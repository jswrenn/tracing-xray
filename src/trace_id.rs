//! Utilities for generating/parsing AWS X-Ray `trace_id`s.
use tracing_core::span::Attributes;
use tracing_core::subscriber::Subscriber;
use tracing_subscriber::registry::{LookupSpan, SpanRef};

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
            ROOT_KEY => {
                root = Some(v.to_owned());
            }
            PARENT_KEY => {
                parent = Some(v.to_owned());
            }
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

/// Extract an AWS X-Ray `trace_id` from a tracing `Span`
pub fn from_span<'a, S>(span: &SpanRef<'a, S>, attr: &Attributes<'_>) -> Option<String>
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
