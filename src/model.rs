use serde::{Serialize, Serializer};
use std::time::{Duration, SystemTime};
use tracing_core::span::Record;
use tracing_serde::AsSerde;

#[derive(Serialize)]
pub(crate) enum Kind {
    Segment,
    #[serde(rename = "subsegment")]
    Subsegment,
}

impl Kind {
    fn is_segment(&self) -> bool {
        matches!(self, Self::Segment)
    }
}

#[derive(Serialize)]
pub(crate) struct Segment {
    pub(crate) name: String,
    pub(crate) id: Id,
    #[serde(serialize_with = "serialize_time")]
    pub(crate) start_time: SystemTime,
    pub(crate) trace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_id: Option<Id>,
    #[serde(rename = "type", skip_serializing_if = "Kind::is_segment")]
    pub(crate) kind: Kind,
    pub(crate) metadata: Metadata,
    pub(crate) annotations: Annotations,
    #[serde(flatten)]
    pub(crate) rest: Rest,
}

impl Segment {
    /// Complete this segment.
    pub(crate) fn complete(&mut self) {
        self.rest = Rest::Completed(Completed {
            end_time: SystemTime::now(),
        });
    }
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
            if field == crate::TRACE_ID_FIELD {
                // don't add the TRACE_ID span field to either metadata or annotations;
                // it's already reflected as a top-level item in the segment document
                continue;
            } else if let Some(("", field)) = field.split_once(crate::ANNOTATION_PREFIX) {
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
    #[serde(serialize_with = "serialize_time")]
    end_time: SystemTime,
}

/// Serialize the given `SystemTime` as `f64` seconds-since-the-unix-epoch.
fn serialize_time<S>(time: &SystemTime, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64();
    s.serialize_f64(secs)
}

#[derive(Serialize)]
pub(crate) struct Id(#[serde(serialize_with = "serialize_id")] pub(crate) tracing_core::span::Id);

fn serialize_id<S>(id: &tracing_core::span::Id, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(format!("{:016x}", id.into_u64()).as_str())
}
