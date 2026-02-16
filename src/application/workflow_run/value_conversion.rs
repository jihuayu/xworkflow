use serde_json::Value;
use std::sync::Arc;

use crate::domain::execution::{FileSegment, Segment};
use crate::domain::model::SegmentType;

pub(crate) fn segment_from_type(value: &Value, seg_type: Option<&SegmentType>) -> Segment {
    match seg_type {
        Some(SegmentType::ArrayString) => {
            if let Value::Array(items) = value {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    if let Some(s) = item.as_str() {
                        values.push(s.to_string());
                    } else {
                        return Segment::from_value(value);
                    }
                }
                return Segment::string_array(values);
            }
        }
        Some(SegmentType::File) => {
            if let Ok(file) = serde_json::from_value::<FileSegment>(value.clone()) {
                return Segment::File(Arc::new(file));
            }
        }
        Some(SegmentType::ArrayFile) => {
            if let Value::Array(items) = value {
                let mut files = Vec::with_capacity(items.len());
                for item in items {
                    match serde_json::from_value::<FileSegment>(item.clone()) {
                        Ok(file) => files.push(file),
                        Err(_) => return Segment::from_value(value),
                    }
                }
                return Segment::ArrayFile(Arc::new(files));
            }
        }
        _ => {}
    }

    Segment::from_value(value)
}
