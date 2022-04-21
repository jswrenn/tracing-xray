use std::io;
use std::string::ToString;
use std::time::SystemTime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing_core::field::Visit;
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::subscriber::Subscriber;
use tracing_core::Field;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

mod model;
pub mod trace_id;
mod xray_daemon;

/// Add `aws.xray.trace_id` as a field to a tracing span to designate it as an
/// X-Ray segment. Set the value of this field to either a
/// [fresh trace id][trace_id::new], or one
/// [parsed from HTTP headers][trace_id::from_headers].
pub const TRACE_ID_FIELD: &str = "aws.xray.trace_id";

/// Prefix span fields that you'd like to classify as X-Ray [annotations] with
/// `aws.xray.annotations.`.
pub const ANNOTATION_PREFIX: &str = "aws.xray.annotations.";

/// A [tracing_subscriber] [`Layer`][tracing_subscriber::layer::Layer] that
/// emits traces to an [AWS X-Ray daemon].
///
/// This layer assumes the X-Ray daemon is running locally, and listening on
/// port 2000.
///
/// [AWS X-Ray daemon]: https://docs.aws.amazon.com/xray/latest/devguide/xray-daemon.html
pub struct Layer {
    handle: JoinHandle<io::Result<()>>,
    sender: mpsc::Sender<model::Segment>,
    service_name: String,
}

impl std::ops::Drop for Layer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Layer {
    /// Constructs a new [`Layer`].
    ///
    /// The given `service_name` is used as the `name` for segment documentes
    /// emitted by this layer.
    pub async fn new(service_name: impl ToString) -> io::Result<Self> {
        let connection = xray_daemon::DaemonClient::default().connect().await?;
        let (sender, mut receiver) = mpsc::channel::<model::Segment>(1000);
        Ok(Self {
            handle: tokio::spawn(async move {
                while let Some(segment) = receiver.recv().await {
                    let message = serde_json::to_vec(&segment).unwrap();
                    connection.send(&message[..]).await?;
                }
                Ok(())
            }),
            sender,
            service_name: service_name.to_string(),
        })
    }

    /// Emit a given [`model::Segment`].
    fn send(&self, segment: &model::Segment) {
        let _ = self.sender.try_send(segment.to_owned());
    }
}

/// A [visitor][Visit] that searches for fields named
/// [`aws.xray.trace_id`][TRACE_ID_FIELD] and records their value.
struct TraceIdVisitor {
    trace_id: Option<String>,
}

impl Visit for TraceIdVisitor {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
    fn record_str(&mut self, field: &Field, value: &str) {
        // if the field's name matches `TRACE_ID_FIELD`, record its value as
        // the `trace_id`.
        if field.name() == TRACE_ID_FIELD {
            let _ = self.trace_id.insert(value.to_owned());
        }
    }
}

impl<S> tracing_subscriber::layer::Layer<S> for Layer
where
    S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync,
{
    fn on_new_span(&self, attr: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        // try to identify a `trace_id` for this span
        let trace_id = if let Some(trace_id) = trace_id::from_span(&span, attr) {
            trace_id
        } else {
            // if none can be found, don't emit a Segment
            return;
        };

        let kind = match attr.fields().field(TRACE_ID_FIELD).is_some() {
            true => model::Kind::Segment,
            false => model::Kind::Subsegment,
        };

        // prepare X-Ray annotations and metadata for this span
        let record = Record::new(attr.values());
        let (mut metadata, mut annotations) = crate::model::metadata_and_annotations_from(&record);

        annotations
            .fields
            .add("tracing.target", attr.metadata().target());
        metadata.fields.add("tracing.file", attr.metadata().file());
        metadata.fields.add("tracing.line", attr.metadata().line());

        // if we're creating a segment, retain the tracing span name as an annotation
        // otherwise, it'll be available as the name of the subsegment.
        if let model::Kind::Segment = kind {
            annotations
                .fields
                .add("tracing.name", attr.metadata().name());
        }

        let segment = model::Segment {
            name: {
                match kind {
                    // for segments, use the logical name of the service
                    model::Kind::Segment => self.service_name.clone(),
                    // for subsegments, use the tracing span name
                    model::Kind::Subsegment => attr.metadata().name().to_owned(),
                }
            },
            id: model::Id(id.to_owned()),
            start_time: SystemTime::now(),
            trace_id,
            parent_id: span.parent().map(|p| model::Id(p.id())),
            kind,
            metadata,
            annotations,
            rest: model::Rest::InProgress(model::InProgress),
        };
        let _ = self.send(&segment);
        span.extensions_mut().insert(segment);
    }

    // fields starting with `aws.xray.annotations.FIELD_NAME` become a field in
    // `annotations` with `FIELD_NAME`. Other fields go to `metadata`.
    fn on_record(&self, id: &Id, record: &Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        let (metadata, annotations) = crate::model::metadata_and_annotations_from(record);
        if let Some(segment) = extensions.get_mut::<model::Segment>() {
            segment.metadata.update(metadata);
            segment.annotations.update(annotations);
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if let Some(segment) = extensions.get_mut::<model::Segment>() {
            // complete the segment
            segment.complete();
            // send the completed segment
            let _ = self.send(segment);
        }
    }
}
