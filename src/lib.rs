use std::io;
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

pub struct Layer {
    handle: Handle,
    connection: &'static xray_daemon::DaemonClient<xray_daemon::Connected>,
}

impl Layer {
    pub async fn new() -> io::Result<Self> {
        Ok(Self {
            handle: Handle::current(),
            connection: 
                Box::leak(Box::new(xray_daemon::DaemonClient::default().connect().await?)),
        })
    }

    fn send(&self, segment: &model::Segment) {
        let connection = self.connection;
        let message = serde_json::to_vec(segment).unwrap();
        let _ = self.handle.spawn(async move {
            connection.send(&message[..]).await
        });
    }
}

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

    pub struct FromHeaders {
        pub root: String,
        pub parent: Option<String>,
        pub sampled: SamplingDecision,
    }

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
}

struct TraceIdVisitor {
    trace_id: Option<String>,
}

impl Visit for TraceIdVisitor {
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "AWS_XRAY_TRACE_ID" {
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
        let parent = span.parent();
        let mut extensions = span.extensions_mut();

        let segment = model::Segment {
            name: {
                // What the docs say:
                // The logical name of the service that handled the request, up
                // to 200 characters. Names can contain Unicode letters,
                // numbers, and whitespace, and the following symbols: _, ., :,
                // /, %, &, #, =, +, \, -, @.

                // What we do:
                // Use the static name ascribed by the user to the span.

                // TODO:
                // Sanitize `name` to meet the X-Ray requirements?
                attr.metadata().name()
            },
            id: {
                // What the docs say:
                // A 64-bit identifier for the segment, unique among segments in
                // the same trace, in 16 hexadecimal digits.
                //
                // What we do:
                // Convert `Id` to a `u64`, then format it as hex.
                format!("{:08x}", id.into_u64())
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
            trace_id: {
                // What the docs say:
                // A trace_id consists of three numbers separated by hyphens.
                // For example, 1-58406520-a006649127e371903a2de979.
                // This includes:
                // - The version number, that is, 1.
                // - The time of the original request, in Unix epoch time
                //   (seconds), in 8 hexadecimal digits.
                // - A 96-bit identifier for the trace, globally unique, in 24
                //   hexadecimal digits.
                //
                // What we do:
                // First, check to see if the current span was created with the
                // field `AWS-XRAY-TRACE-ID`.
                #[derive(Clone)]
                pub struct TraceId(String);
                let mut visitor = TraceIdVisitor { trace_id: None };
                attr.record(&mut visitor);

                if let Some(trace_id) = visitor.trace_id {
                    // If so, that's our trace_id. We insert `trace_id` into
                    // this span's associated data, so that descendents can
                    // more easily look it up.
                    extensions.insert(TraceId(trace_id.clone()));
                    trace_id
                } else {
                    // otherwise, walk up the tree till we find a TraceId
                    span.scope()
                        .skip(1)
                        .find_map(|span| span.extensions().get::<TraceId>().cloned())
                        .map(|trace_id| trace_id.0)
                        .unwrap_or("TODO".to_string())
                }
            },
            parent_id: { parent.map(|p| format!("{:08x}", p.id().into_u64())) },
            kind: match attr.fields().field("AWS_XRAY_TRACE_ID").is_some() {
                true => model::Kind::Segment,
                false => model::Kind::Subsegment,
            },
            metadata: model::Metadata {
                fields: model::Fields::from(attr),
            },
            rest: model::Rest::InProgress(model::InProgress),
        };
        let _ = self.send(&segment);
        extensions.insert(segment);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found, this is a bug");
        let mut extensions = span.extensions_mut();
        if let Some(segment) = extensions.get_mut::<model::Segment>() {
            segment.metadata.fields.update(values);
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
        pub(crate) name: &'static str,
        pub(crate) id: String,
        pub(crate) start_time: f64,
        pub(crate) trace_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(crate) parent_id: Option<String>,
        #[serde(rename = "type", skip_serializing_if = "Kind::is_segment")]
        pub(crate) kind: Kind,
        pub(crate) metadata: Metadata,
        #[serde(flatten)]
        pub(crate) rest: Rest,
    }

    #[derive(Serialize, Default)]
    pub(crate) struct Metadata {
        pub(crate) fields: Fields,
    }

    #[derive(Serialize, Default)]
    pub(crate) struct Fields(serde_json::Value);

    impl Fields {
        pub(crate) fn from(attr: &Attributes<'_>) -> Self {
            Self(
                serde_json::to_value(Record::new(attr.values()).as_serde())
                    .expect("impossible, right?"),
            )
        }

        pub(crate) fn update(&mut self, record: &Record<'_>) {
            use serde_json::value::Value::Object;
            let extension = serde_json::to_value(record.as_serde()).expect("impossible, right?");
            match (&mut self.0, extension) {
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
