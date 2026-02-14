use serde::{Deserialize, Serialize};

/// DSL-level type marker for workflow variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentType {
    String,
    Number,
    Boolean,
    Object,
    ArrayString,
    ArrayNumber,
    ArrayObject,
    File,
    ArrayFile,
    Array,
    Any,
}

impl SegmentType {
    pub fn from_dsl_type(t: &str) -> Option<Self> {
        match t.trim().to_lowercase().as_str() {
            "string" => Some(SegmentType::String),
            "number" => Some(SegmentType::Number),
            "boolean" => Some(SegmentType::Boolean),
            "object" => Some(SegmentType::Object),
            "array[string]" => Some(SegmentType::ArrayString),
            "array[number]" => Some(SegmentType::ArrayNumber),
            "array[object]" => Some(SegmentType::ArrayObject),
            "file" => Some(SegmentType::File),
            "array[file]" => Some(SegmentType::ArrayFile),
            _ => None,
        }
    }
}
