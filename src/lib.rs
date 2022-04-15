use std::io;
use std::string::ToString;
use std::time::{Duration, SystemTime};
use tokio::runtime::Handle;
use tracing_core::field::Visit;
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::subscriber::Subscriber;
use tracing_core::Field;
use tracing_serde::AsSerde;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

mod xray_daemon;

/// Add `aws.xray.trace_id` as a field to a tracing span to designate it as an
/// X-Ray segment. Set the value of this field to either a
/// [fresh trace id][trace_id::new], or one
/// [parsed from HTTP headers][trace_id::from_headers].
pub const TRACE_ID_FIELD: &'static str = "aws.xray.trace_id";

/// A [tracing_subscriber] [`Layer`][tracing_subscriber::layer::Layer] that
/// emits traces to an [AWS X-Ray daemon].
///
/// This layer assumes the X-Ray daemon is running locally, and listening on
/// port 2000.
///
/// [AWS X-Ray daemon]: https://docs.aws.amazon.com/xray/latest/devguide/xray-daemon.html
pub struct Layer {
    handle: Handle,
    connection: &'static xray_daemon::DaemonClient<xray_daemon::Connected>,
    service_name: String,
}

impl Layer {
    /// Constructs a new [`Layer`].
    ///
    /// The given `service_name` is used as the `name` for segment documentes
    /// emitted by this layer.
    pub async fn new(service_name: impl ToString) -> io::Result<Self> {
        Ok(Self {
            handle: Handle::current(),
            connection: Box::leak(Box::new(
                xray_daemon::DaemonClient::default().connect().await?,
            )),
            service_name: service_name.to_string(),
        })
    }

    /// Emit a given [`model::Segment`].
    fn send(&self, segment: &model::Segment) {
        let connection = self.connection;
        let message = serde_json::to_vec(segment).unwrap();
        let _ = self
            .handle
            .spawn(async move { connection.send(&message[..]).await });
    }
}

/// Utilities for generating/parsing AWS X-Ray `trace_id`s.
pub mod trace_id {
    /// Generate a fresh X-Ray trace id.
    pub fn new() -> String {
        use rand::prelude::*;
        use std::time::{Duration, SystemTime};

        let time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();

        let mut rng = rand::thread_rng();
        let a: u32 = rng.gen();
        let b: u32 = rng.gen();
        let c: u32 = rng.gen();

        format!("1-{time:08x}-{a:08x}{b:08x}{c:08x}")
    }

    pub enum SamplingDecision {
        Sampled,
        NotSampled,
        Requested,
        Unknown,
    }

    impl SamplingDecision {
        fn from_str(s: &str) -> Self {
            match s {
                "1" => Self::Sampled,
                "0" => Self::NotSampled,
                "?" => Self::Requested,
                _ => Self::Unknown,
            }
        }
    }

    /// The result of [`from_headers`].
    pub struct FromHeaders {
        pub root: String,
        pub parent: Option<String>,
        pub sampled: SamplingDecision,
    }

    /// Parse an [AWS X-Ray tracing header] from the given http headers.
    ///
    /// [AWS X-Ray tracing header]: https://docs.aws.amazon.com/xray/latest/devguide/xray-concepts.html#xray-concepts-tracingheader
    pub fn from_headers(headers: &http::header::HeaderMap) -> Option<FromHeaders> {
        const AWS_XRAY_HEADER: &str = "X-Amzn-Trace-Id";
        const ROOT_KEY: &str = "Root";
        const PARENT_KEY: &str = "Parent";
        const SAMPLED_KEY: &str = "Sampled";

        let header = headers.get(AWS_XRAY_HEADER)?.to_str().ok()?;

        let mut root = None;
        let mut parent = None;
        let mut sampled = SamplingDecision::Unknown;

        for entry in header.trim().split_terminator(';') {
            let mut kv = entry.trim().split('=');
            let k = kv.next()?.trim_end();
            let v = kv.next()?.trim_start();
            match k {
                ROOT_KEY => drop(root.insert(v.to_owned())),
                PARENT_KEY => drop(parent.insert(v.to_owned())),
                SAMPLED_KEY => {
                    sampled = SamplingDecision::from_str(v);
                }
                _ => return None,
            };
        }

        Some(FromHeaders {
            root: root?,
            parent,
            sampled,
        })
    }
    
    use tracing_core::span::Attributes;
    use tracing_core::subscriber::Subscriber;
    use tracing_subscriber::registry::{
        LookupSpan,
        SpanRef,
    };

    /// Extrace an AWS X-Ray `trace_id` from a tracing `Span`
    pub fn from_span<'a, S>(
        span: &SpanRef<'a, S>,
        attr: &Attributes<'_>,
    ) -> Option<String>
    where
        S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync,
    {
        #[derive(Clone)]
        pub struct TraceId(String);
        let mut visitor = crate::TraceIdVisitor { trace_id: None };
        attr.record(&mut visitor);
        
        let mut extensions = span.extensions_mut();
        if let Some(trace_id) = visitor.trace_id {
            // If so, that's our trace_id. We insert `trace_id` into
            // this span's associated data, so that descendents can
            // more easily look it up.
            extensions.insert(TraceId(trace_id.clone()));
            Some(trace_id)
        } else {
            // otherwise, walk up the tree till we find a TraceId
            let trace_id = span
                .scope()
                .skip(1)
                .find_map(|span| span.extensions().get::<TraceId>().cloned())
                .map(|trace_id| trace_id.0);
            trace_id
        }
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

        let record = Record::new(attr.values());
        let (mut metadata, mut annotations) = crate::model::metadata_and_annotations_from(&record);

        let kind = match attr.fields().field(TRACE_ID_FIELD).is_some() {
            true => model::Kind::Segment,
            false => model::Kind::Subsegment,
        };

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
            id: {
                // What the docs say:
                // A 64-bit identifier for the segment, unique among segments in
                // the same trace, in 16 hexadecimal digits.
                //
                // What we do:
                // Convert `Id` to a `u64`, then format it as hex.
                format!("{:016x}", id.into_u64())
            },
            start_time: {
                // What the docs say:
                // number that is the time the segment was created, in floating
                // point seconds in epoch time.
                //
                // What we do:
                // Compute the duration, in floating point seconds, between
                // the current `SystemTime` and the `UNIX_EPOCH`. If the system
                // time is earlier than `UNIX_EPOCH`, clamp to `0`.
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
                    .as_secs_f64()
            },
            trace_id,
            parent_id: { span.parent().map(|p| format!("{:016x}", p.id().into_u64())) },
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
            segment.rest = model::Rest::Completed(model::Completed {
                end_time: {
                    SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or(Duration::ZERO)
                        .as_secs_f64()
                },
            });
            // send the completed segment
            let _ = self.send(&segment);
        }
    }
}

pub(crate) mod model {
    use serde::Serialize;

    use super::*;

    #[derive(Serialize)]
    pub(crate) enum Kind {
        Segment,
        #[serde(rename = "subsegment")]
        Subsegment,
    }

    impl Kind {
        fn is_segment(&self) -> bool {
            if let Self::Segment = &self {
                true
            } else {
                false
            }
        }
    }

    #[derive(Serialize)]
    pub(crate) struct Segment {
        pub(crate) name: String,
        pub(crate) id: String,
        pub(crate) start_time: f64,
        pub(crate) trace_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) parent_id: Option<String>,
        #[serde(rename = "type", skip_serializing_if = "Kind::is_segment")]
        pub(crate) kind: Kind,
        pub(crate) metadata: Metadata,
        pub(crate) annotations: Annotations,
        #[serde(flatten)]
        pub(crate) rest: Rest,
    }

    #[derive(Serialize, Default)]
    pub(crate) struct Metadata {
        #[serde(flatten)]
        pub(crate) fields: Fields,
    }

    impl Metadata {
        pub(crate) fn update(&mut self, updates: Self) {
            self.fields.update(updates.fields)
        }
    }

    #[derive(Serialize, Default)]
    pub(crate) struct Annotations {
        #[serde(flatten)]
        pub(crate) fields: Fields,
    }
    
    impl Annotations {
        pub(crate) fn update(&mut self, updates: Self) {
            self.fields.update(updates.fields)
        }
    }

    pub(crate) fn metadata_and_annotations_from(record: &Record<'_>) -> (Metadata, Annotations) {
        use serde_json::Value::Object;
        let json = serde_json::to_value(record.as_serde()).expect("impossible, right?");

        let mut annotations = serde_json::Map::new();
        let mut metadata = serde_json::Map::new();
        if let Object(map) = json {
            for (field, value) in map {
                if field == TRACE_ID_FIELD {
                    // don't add the TRACE_ID span field to either metadata or annotations;
                    // it's already reflected as a top-level item in the segment document
                    continue;
                } else if let Some(("", field)) = field.split_once("aws.xray.annotations.") {
                    // `key` is an annotation
                    annotations.insert(field.to_owned(), value);
                } else {
                    // `key` is metadata
                    metadata.insert(field, value);
                }
            }
        }
        let metadata = Metadata {
            fields: Fields(Object(metadata)),
        };
        let annotations = Annotations {
            fields: Fields(Object(annotations)),
        };
        (metadata, annotations)
    }

    #[derive(Serialize, Default)]
    pub(crate) struct Fields(serde_json::Value);

    impl Fields {
        pub(crate) fn add<K, V>(&mut self, name: K, value: V)
        where
            K: std::string::ToString,
            V: Serialize,
        {
            use serde_json::Value::Object;
            if let Object(map) = &mut self.0 {
                let value = serde_json::to_value(value).unwrap();
                map.insert(name.to_string(), value);
            }
        }

        pub(crate) fn update(&mut self, update: Self) {
            use serde_json::Value::Object;
            match (&mut self.0, update.0) {
                (Object(base), Object(extension)) => {
                    base.extend(extension.into_iter());
                }
                (base, extension) => {
                    *base = extension;
                }
            }
        }
    }

    #[derive(Serialize)]
    #[serde(untagged)]
    pub(crate) enum Rest {
        InProgress(InProgress),
        Completed(Completed),
    }

    pub(crate) struct InProgress;

    impl Serialize for InProgress {
        #[inline]
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            use serde::ser::SerializeStruct;
            let mut in_progress = serializer.serialize_struct("InProgress", 3)?;
            in_progress.serialize_field("in_progress", &true)?;
            in_progress.end()
        }
    }

    #[derive(Serialize)]
    pub(crate) struct Completed {
        pub(crate) end_time: f64,
    }
}
