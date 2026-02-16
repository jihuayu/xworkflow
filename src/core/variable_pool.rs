//! Variable pool storage and selector indexing.
//!
//! Segment and stream value types live in `domain::execution` and are re-exported
//! here for compatibility within the crate.

use compact_str::CompactString;
use im::HashMap as ImHashMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::domain::execution::{
    FileCategory, FileSegment, FileTransferMethod, Segment, SegmentArray, SegmentObject,
    SegmentStream, StreamEvent, StreamLimits, StreamReader, StreamStateWeak, StreamStatus,
    StreamWriter,
};
pub use crate::domain::model::{SegmentType, Selector, SCOPE_NODE_ID};

/// Copy-on-write variable storage for a workflow execution.
///
/// Variables are keyed by `"node_id:variable_name"` and stored as [`Segment`]
/// values. The pool uses [`im::HashMap`] for efficient structural sharing when
/// forking pools (e.g. for sub-graph / iteration execution).
#[derive(Debug, Clone)]
pub struct VariablePool {
    variables: ImHashMap<CompactString, Segment>,
    #[cfg(feature = "security")]
    selector_validation: Option<crate::security::validation::SelectorValidation>,
    #[cfg(not(feature = "security"))]
    selector_validation: Option<()>,
}

impl VariablePool {
    /// Create an empty variable pool.
    pub fn new() -> Self {
        VariablePool {
            variables: ImHashMap::new(),
            #[cfg(feature = "security")]
            selector_validation: None,
            #[cfg(not(feature = "security"))]
            selector_validation: None,
        }
    }

    #[cfg(feature = "security")]
    pub fn new_with_selector_validation(
        selector_validation: Option<crate::security::validation::SelectorValidation>,
    ) -> Self {
        VariablePool {
            variables: ImHashMap::new(),
            selector_validation,
        }
    }

    #[cfg(not(feature = "security"))]
    pub fn new_with_selector_validation(_selector_validation: Option<()>) -> Self {
        VariablePool {
            variables: ImHashMap::new(),
            selector_validation: None,
        }
    }

    #[cfg(feature = "security")]
    pub fn set_selector_validation(
        &mut self,
        selector_validation: Option<crate::security::validation::SelectorValidation>,
    ) {
        self.selector_validation = selector_validation;
    }

    /// Return the number of variables in the pool.
    pub fn len(&self) -> usize {
        self.variables.len()
    }

    /// Return whether the pool contains no variables.
    pub fn is_empty(&self) -> bool {
        self.variables.is_empty()
    }

    /// Estimate the total memory footprint of all variables in bytes.
    pub fn estimate_total_bytes(&self) -> usize {
        self.variables
            .iter()
            .map(|(k, v)| k.len() + v.estimate_bytes())
            .sum()
    }

    /// Build key from node_id and variable name.
    pub fn make_key(node_id: &str, var_name: &str) -> CompactString {
        let mut key = CompactString::with_capacity(node_id.len() + 1 + var_name.len());
        key.push_str(node_id);
        key.push(':');
        key.push_str(var_name);
        key
    }

    fn key_prefix(node_id: &str) -> CompactString {
        let mut prefix = CompactString::with_capacity(node_id.len() + 1);
        prefix.push_str(node_id);
        prefix.push(':');
        prefix
    }

    #[cfg(feature = "security")]
    fn selector_allowed(&self, selector: &Selector) -> bool {
        let Some(cfg) = &self.selector_validation else {
            return true;
        };
        if 2 > cfg.max_depth {
            return false;
        }

        let total_len = selector.node_id().len() + 1 + selector.variable_name().len();
        if total_len > cfg.max_length {
            return false;
        }

        if cfg.allowed_prefixes.contains("*") {
            return true;
        }
        let prefix = selector.node_id();
        cfg.allowed_prefixes.contains(prefix)
    }

    /// Get variable by selector: (node_id, var_name)
    pub fn get(&self, selector: &Selector) -> Segment {
        #[cfg(feature = "security")]
        if !self.selector_allowed(selector) {
            return Segment::None;
        }
        self.variables
            .get(&selector.pool_key())
            .cloned()
            .unwrap_or(Segment::None)
    }

    /// Get variable and resolve Stream by collecting it.
    pub async fn get_resolved(&self, selector: &Selector) -> Segment {
        match self.get(selector) {
            Segment::Stream(stream) => stream.collect().await.unwrap_or(Segment::None),
            other => other,
        }
    }

    /// Get variable and resolve Stream, returning serde_json::Value.
    pub async fn get_resolved_value(&self, selector: &Selector) -> Value {
        self.get_resolved(selector).await.to_value()
    }

    /// Set a single variable
    pub fn set(&mut self, selector: &Selector, value: Segment) {
        self.variables.insert(selector.pool_key(), value);
    }

    #[cfg(feature = "security")]
    pub fn set_checked(
        &mut self,
        selector: &Selector,
        value: Segment,
        max_entries: usize,
        max_memory_bytes: usize,
    ) -> Result<(), crate::security::QuotaError> {
        if self.variables.len() >= max_entries {
            return Err(crate::security::QuotaError::VariablePoolTooLarge {
                max_entries,
                current: self.variables.len(),
            });
        }
        let estimated_size = self.estimate_total_bytes() + value.estimate_bytes();
        if estimated_size > max_memory_bytes {
            return Err(crate::security::QuotaError::VariablePoolMemoryExceeded {
                max_bytes: max_memory_bytes,
                current: estimated_size,
            });
        }
        self.set(selector, value);
        Ok(())
    }

    /// Set node outputs as (node_id, key) -> value
    pub fn set_node_value_outputs(&mut self, node_id: &str, outputs: &HashMap<String, Value>) {
        for (key, val) in outputs {
            let seg = Segment::from_value(val);
            self.variables.insert(Self::make_key(node_id, key), seg);
        }
    }

    /// Set node outputs from Segment map
    pub fn set_node_outputs(&mut self, node_id: &str, outputs: &HashMap<String, Segment>) {
        for (key, val) in outputs {
            self.variables
                .insert(Self::make_key(node_id, key), val.clone());
        }
    }

    /// Check if variable exists and is not None
    pub fn has(&self, selector: &Selector) -> bool {
        #[cfg(feature = "security")]
        if !self.selector_allowed(selector) {
            return false;
        }
        self.variables
            .get(&selector.pool_key())
            .is_some_and(|s| !s.is_none())
    }

    /// Get all variables for a given node_id
    pub fn get_node_variables(&self, node_id: &str) -> HashMap<String, Segment> {
        let prefix = Self::key_prefix(node_id);
        self.variables
            .iter()
            .filter(|(key, _)| key.as_str().starts_with(prefix.as_str()))
            .filter_map(|(key, val)| {
                key.split_once(':')
                    .map(|(_, var)| (var.to_string(), val.clone()))
            })
            .collect()
    }

    /// Remove all variables for a given node
    pub fn remove_node(&mut self, node_id: &str) {
        let prefix = Self::key_prefix(node_id);
        self.variables
            .retain(|key, _| !key.as_str().starts_with(prefix.as_str()));
    }

    /// Append value to an existing array variable
    pub fn append(&mut self, selector: &Selector, value: Segment) {
        let key = selector.pool_key();
        let existing = self
            .variables
            .entry(key)
            .or_insert_with(|| Segment::Array(Arc::new(SegmentArray::new(Vec::new()))));
        match existing {
            Segment::Array(arr) => {
                let arr = Arc::make_mut(arr);
                arr.push(value);
            }
            Segment::ArrayString(arr) => match value {
                Segment::String(s) => {
                    let arr = Arc::make_mut(arr);
                    arr.push(s);
                }
                other => {
                    let mut promoted: Vec<Segment> =
                        arr.iter().cloned().map(Segment::String).collect();
                    promoted.push(other);
                    *existing = Segment::Array(Arc::new(SegmentArray::new(promoted)));
                }
            },
            Segment::String(s) => {
                *s += &value.to_display_string();
            }
            Segment::None => {
                *existing = Segment::Array(Arc::new(SegmentArray::new(vec![value])));
            }
            _ => {
                let old = std::mem::replace(existing, Segment::None);
                *existing = Segment::Array(Arc::new(SegmentArray::new(vec![old, value])));
            }
        }
    }

    /// Clear a variable (set to None)
    pub fn clear(&mut self, selector: &Selector) {
        self.variables.insert(selector.pool_key(), Segment::None);
    }

    /// Snapshot entire pool for serialization
    pub fn snapshot(&self) -> HashMap<String, Segment> {
        self.variables
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }
}

impl Default for VariablePool {
    fn default() -> Self {
        Self::new()
    }
}

pub fn snapshot_for_checkpoint(pool: &VariablePool) -> HashMap<String, Value> {
    pool.snapshot()
        .into_iter()
        .filter_map(|(key, segment)| match segment {
            Segment::Stream(stream) => {
                if stream.status() == StreamStatus::Completed {
                    Some((key, stream.snapshot_segment().to_value()))
                } else {
                    None
                }
            }
            other => Some((key, other.to_value())),
        })
        .collect()
}

pub fn restore_from_checkpoint(variables: &HashMap<String, Value>) -> VariablePool {
    let mut pool = VariablePool::new();
    for (key, value) in variables {
        if let Some(selector) = Selector::from_pool_key(key) {
            pool.set(&selector, Segment::from_value(value));
        }
    }
    pool
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_variable_pool_basic() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("node1", "output");
        pool.set(&sel, Segment::String("hello".to_string()));
        let val = pool.get(&sel);
        assert!(matches!(val, Segment::String(s) if s == "hello"));
    }

    #[test]
    fn test_variable_pool_sys() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("sys", "query");
        pool.set(&sel, Segment::String("test query".to_string()));
        assert!(pool.has(&sel));
        let val = pool.get(&sel);
        assert_eq!(val.to_display_string(), "test query");
    }

    #[test]
    fn test_variable_pool_missing() {
        let pool = VariablePool::new();
        let sel = Selector::new("nonexistent", "var");
        assert!(pool.get(&sel).is_none());
        assert!(!pool.has(&sel));
    }

    #[test]
    fn test_set_node_outputs() {
        let mut pool = VariablePool::new();
        let mut outputs = HashMap::new();
        outputs.insert("text".to_string(), Value::String("result".to_string()));
        outputs.insert("count".to_string(), serde_json::json!(42));
        pool.set_node_value_outputs("node_llm", &outputs);

        let text = pool.get(&Selector::new("node_llm", "text"));
        assert!(matches!(text, Segment::String(s) if s == "result"));

        let count = pool.get(&Selector::new("node_llm", "count"));
        assert!(matches!(count, Segment::Integer(42)));
    }

    #[test]
    fn test_segment_conversion() {
        let seg = Segment::Integer(42);
        let val = seg.to_value();
        assert_eq!(val, serde_json::json!(42));

        let back = Segment::from_value(&val);
        assert!(matches!(back, Segment::Integer(42)));
    }

    #[test]
    fn test_selector_from_pool_key() {
        let selector = Selector::from_pool_key("node1:output").unwrap();
        assert_eq!(selector.node_id(), "node1");
        assert_eq!(selector.variable_name(), "output");
        assert!(Selector::from_pool_key("invalid").is_none());
    }

    #[test]
    fn test_snapshot_restore_checkpoint_helpers() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "a"), Segment::Integer(10));
        pool.set(&Selector::new("n1", "b"), Segment::String("x".into()));

        let snap = snapshot_for_checkpoint(&pool);
        assert_eq!(snap.get("n1:a"), Some(&serde_json::json!(10)));
        assert_eq!(snap.get("n1:b"), Some(&serde_json::json!("x")));

        let restored = restore_from_checkpoint(&snap);
        assert_eq!(
            restored.get(&Selector::new("n1", "a")),
            Segment::Integer(10)
        );
        assert_eq!(
            restored.get(&Selector::new("n1", "b")),
            Segment::String("x".into())
        );
    }

    #[tokio::test]
    async fn test_snapshot_for_checkpoint_skips_running_stream() {
        let (stream, writer) = SegmentStream::channel();
        writer.send(Segment::String("chunk".into())).await;

        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "stream"), Segment::Stream(stream));

        let snap = snapshot_for_checkpoint(&pool);
        assert!(!snap.contains_key("n1:stream"));
    }

    #[test]
    fn test_append() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "arr");
        pool.set(
            &sel,
            Segment::Array(Arc::new(SegmentArray::new(Vec::new()))),
        );
        pool.append(&sel, Segment::Integer(1));
        pool.append(&sel, Segment::Integer(2));
        match pool.get(&sel) {
            Segment::Array(v) => assert_eq!(v.len(), 2),
            _ => panic!("Expected Array"),
        }
    }

    #[test]
    fn test_from_value_array_inference() {
        let seg = Segment::from_value(&serde_json::json!(["a", "b"]));
        assert!(matches!(seg, Segment::ArrayString(_)));

        let seg = Segment::from_value(&serde_json::json!([1, 2, 3]));
        assert!(matches!(seg, Segment::Array(_)));

        let seg = Segment::from_value(&serde_json::json!([1, "a"]));
        assert!(matches!(seg, Segment::Array(_)));

        let seg = Segment::from_value(&serde_json::json!([]));
        assert!(matches!(seg, Segment::Array(_)));
    }

    #[test]
    fn test_file_segment_roundtrip() {
        let file = FileSegment {
            name: "a.txt".into(),
            size: 12,
            mime_type: "text/plain".into(),
            transfer_method: FileTransferMethod::LocalFile,
            id: Some("/tmp/a.txt".into()),
            ..Default::default()
        };
        let seg = file.to_segment();
        assert!(matches!(seg, Segment::File(_)));

        let back = FileSegment::from_segment(&seg).unwrap();
        assert_eq!(back.id, file.id);
        assert_eq!(back.transfer_method, file.transfer_method);
    }

    #[test]
    fn test_append_array_string_promote() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "arr");
        pool.set(&sel, Segment::ArrayString(Arc::new(vec!["a".into()])));
        pool.append(&sel, Segment::Integer(1));
        match pool.get(&sel) {
            Segment::Array(v) => assert_eq!(v.len(), 2),
            _ => panic!("Expected Array after promotion"),
        }
    }

    #[test]
    fn test_segment_type_mapping() {
        assert_eq!(
            SegmentType::from_dsl_type("string"),
            Some(SegmentType::String)
        );
        assert_eq!(
            SegmentType::from_dsl_type("number"),
            Some(SegmentType::Number)
        );
        assert_eq!(SegmentType::from_dsl_type("file"), Some(SegmentType::File));
        assert_eq!(SegmentType::from_dsl_type("invalid"), None);

        assert!(Segment::Integer(42).matches_type(&SegmentType::Number));
        assert!(Segment::Float(2.5).matches_type(&SegmentType::Number));
        assert!(!Segment::String("42".into()).matches_type(&SegmentType::Number));
    }

    #[tokio::test]
    async fn test_stream_basic() {
        let (stream, writer) = SegmentStream::channel();
        writer.send(Segment::String("hello ".into())).await;
        writer.send(Segment::String("world".into())).await;
        writer.end(Segment::String("hello world".into())).await;

        let result = stream.collect().await.unwrap();
        assert_eq!(result.to_display_string(), "hello world");
    }

    #[tokio::test]
    async fn test_stream_multiple_readers() {
        let (stream, writer) = SegmentStream::channel();
        let mut reader1 = stream.reader();
        let mut reader2 = stream.reader();

        writer.send(Segment::String("a".into())).await;
        let e1 = reader1.next().await;
        let e2 = reader2.next().await;
        assert!(matches!(e1, Some(StreamEvent::Chunk(_))));
        assert!(matches!(e2, Some(StreamEvent::Chunk(_))));
    }

    #[tokio::test]
    async fn test_stream_error() {
        let (stream, writer) = SegmentStream::channel();
        writer.error("timeout".into()).await;
        let err = stream.collect().await.unwrap_err();
        assert!(err.contains("timeout"));
    }

    #[tokio::test]
    async fn test_get_resolved_stream() {
        let (stream, writer) = SegmentStream::channel();
        let mut pool = VariablePool::new();
        let sel = Selector::new("n1", "text");
        pool.set(&sel, Segment::Stream(stream));

        tokio::spawn(async move {
            writer.send(Segment::String("hello".into())).await;
            writer.end(Segment::String("hello".into())).await;
        });

        let resolved = pool.get_resolved(&sel).await;
        assert_eq!(resolved.to_display_string(), "hello");
    }

    // ---- Selector tests ----

    #[test]
    fn test_selector_parse_str() {
        let sel = Selector::parse_str("node1.output").unwrap();
        assert_eq!(sel.node_id(), "node1");
        assert_eq!(sel.variable_name(), "output");
    }

    #[test]
    fn test_selector_parse_str_single() {
        let sel = Selector::parse_str("query").unwrap();
        assert_eq!(sel.node_id(), SCOPE_NODE_ID);
        assert_eq!(sel.variable_name(), "query");
    }

    #[test]
    fn test_selector_parse_str_too_deep() {
        assert!(Selector::parse_str("a.b.c").is_none());
    }

    #[test]
    fn test_selector_parse_str_empty() {
        assert!(Selector::parse_str("").is_none());
    }

    #[test]
    fn test_selector_parse_value_array() {
        let val = serde_json::json!(["node1", "output"]);
        let sel = Selector::parse_value(&val).unwrap();
        assert_eq!(sel.node_id(), "node1");
        assert_eq!(sel.variable_name(), "output");
    }

    #[test]
    fn test_selector_parse_value_string() {
        let val = serde_json::json!("node1.output");
        let sel = Selector::parse_value(&val).unwrap();
        assert_eq!(sel.node_id(), "node1");
    }

    #[test]
    fn test_selector_parse_value_invalid() {
        assert!(Selector::parse_value(&serde_json::json!(42)).is_none());
    }

    #[test]
    fn test_selector_is_empty() {
        let sel = Selector::new("", "var");
        assert!(sel.is_empty());
        let sel = Selector::new("node", "");
        assert!(sel.is_empty());
        let sel = Selector::new("node", "var");
        assert!(!sel.is_empty());
    }

    #[test]
    fn test_selector_serde_roundtrip() {
        let sel = Selector::new("n1", "out");
        let json = serde_json::to_string(&sel).unwrap();
        let back: Selector = serde_json::from_str(&json).unwrap();
        assert_eq!(sel, back);
    }

    #[test]
    fn test_selector_deserialize_from_string() {
        let back: Selector = serde_json::from_str(r#""n1.out""#).unwrap();
        assert_eq!(back.node_id(), "n1");
        assert_eq!(back.variable_name(), "out");
    }

    // ---- Segment tests ----

    #[test]
    fn test_segment_as_string() {
        assert_eq!(Segment::String("hi".into()).as_string(), Some("hi".into()));
        assert_eq!(Segment::Integer(42).as_string(), Some("42".into()));
        assert_eq!(Segment::Float(2.5).as_string(), Some("2.5".into()));
        assert_eq!(Segment::Boolean(true).as_string(), Some("true".into()));
        assert_eq!(Segment::None.as_string(), None);
    }

    #[test]
    fn test_segment_as_f64() {
        assert_eq!(Segment::Integer(42).as_f64(), Some(42.0));
        assert_eq!(Segment::Float(2.5).as_f64(), Some(2.5));
        assert_eq!(Segment::String("2.5".into()).as_f64(), Some(2.5));
        assert_eq!(Segment::String("not_num".into()).as_f64(), None);
        assert_eq!(Segment::None.as_f64(), None);
    }

    #[test]
    fn test_segment_is_empty() {
        assert!(Segment::None.is_empty());
        assert!(Segment::String("".into()).is_empty());
        assert!(!Segment::String("a".into()).is_empty());
        assert!(Segment::ArrayString(Arc::new(vec![])).is_empty());
        assert!(!Segment::ArrayString(Arc::new(vec!["a".into()])).is_empty());
        assert!(!Segment::Integer(0).is_empty());
    }

    #[test]
    fn test_segment_estimate_bytes() {
        assert_eq!(Segment::None.estimate_bytes(), 0);
        assert_eq!(Segment::String("hello".into()).estimate_bytes(), 5);
        assert_eq!(Segment::Integer(42).estimate_bytes(), 8);
        assert_eq!(Segment::Boolean(true).estimate_bytes(), 8);
        assert_eq!(
            Segment::ArrayString(Arc::new(vec!["ab".into(), "cd".into()])).estimate_bytes(),
            4
        );
    }

    #[test]
    fn test_segment_display() {
        assert_eq!(format!("{}", Segment::String("hi".into())), "hi");
        assert_eq!(format!("{}", Segment::Integer(42)), "42");
        assert_eq!(format!("{}", Segment::None), "");
    }

    #[test]
    fn test_segment_partial_eq() {
        assert_eq!(Segment::None, Segment::None);
        assert_eq!(Segment::String("a".into()), Segment::String("a".into()));
        assert_ne!(Segment::String("a".into()), Segment::String("b".into()));
        assert_eq!(Segment::Integer(1), Segment::Integer(1));
        assert_eq!(Segment::Float(1.0), Segment::Float(1.0));
    }

    #[test]
    fn test_segment_string_array() {
        let seg = Segment::string_array(vec!["a".into(), "b".into()]);
        match seg {
            Segment::ArrayString(arr) => assert_eq!(arr.len(), 2),
            _ => panic!("expected ArrayString"),
        }
    }

    #[test]
    fn test_segment_into_value() {
        assert_eq!(Segment::None.into_value(), Value::Null);
        assert_eq!(
            Segment::String("a".into()).into_value(),
            Value::String("a".into())
        );
        assert_eq!(Segment::Integer(1).into_value(), serde_json::json!(1));
        assert_eq!(Segment::Float(2.5).into_value(), serde_json::json!(2.5));
        assert_eq!(Segment::Boolean(true).into_value(), Value::Bool(true));
        let arr = Segment::ArrayString(Arc::new(vec!["x".into()]));
        assert_eq!(arr.into_value(), serde_json::json!(["x"]));
    }

    #[test]
    fn test_segment_from_value_object() {
        let val = serde_json::json!({"key": "val"});
        let seg = Segment::from_value(&val);
        assert!(matches!(seg, Segment::Object(_)));
        assert_eq!(seg.to_value(), val);
    }

    #[test]
    fn test_segment_from_value_float() {
        let val = serde_json::json!(2.5);
        let seg = Segment::from_value(&val);
        assert!(matches!(seg, Segment::Float(_)));
    }

    #[test]
    fn test_segment_type() {
        assert_eq!(Segment::None.segment_type(), SegmentType::Any);
        assert_eq!(
            Segment::String("a".into()).segment_type(),
            SegmentType::String
        );
        assert_eq!(Segment::Integer(1).segment_type(), SegmentType::Number);
        assert_eq!(Segment::Float(1.0).segment_type(), SegmentType::Number);
        assert_eq!(Segment::Boolean(true).segment_type(), SegmentType::Boolean);
    }

    #[test]
    fn test_segment_matches_type() {
        assert!(Segment::Integer(1).matches_type(&SegmentType::Any));
        assert!(Segment::Integer(1).matches_type(&SegmentType::Number));
        assert!(!Segment::Integer(1).matches_type(&SegmentType::String));
        let arr = Segment::Array(Arc::new(SegmentArray::new(vec![])));
        assert!(arr.matches_type(&SegmentType::ArrayNumber));
        assert!(arr.matches_type(&SegmentType::Array));
    }

    #[test]
    fn test_segment_serde() {
        let seg = Segment::Integer(42);
        let json = serde_json::to_value(&seg).unwrap();
        let back: Segment = serde_json::from_value(json).unwrap();
        assert_eq!(back, seg);
    }

    // ---- VariablePool additional tests ----

    #[test]
    fn test_pool_len_and_estimate_bytes() {
        let mut pool = VariablePool::new();
        assert_eq!(pool.len(), 0);
        assert_eq!(pool.estimate_total_bytes(), 0);
        pool.set(&Selector::new("n", "a"), Segment::String("hello".into()));
        assert_eq!(pool.len(), 1);
        assert!(pool.estimate_total_bytes() > 0);
    }

    #[test]
    fn test_pool_set_node_segment_outputs() {
        let mut pool = VariablePool::new();
        let mut outputs = HashMap::new();
        outputs.insert("x".to_string(), Segment::Integer(10));
        outputs.insert("y".to_string(), Segment::String("hi".into()));
        pool.set_node_outputs("node1", &outputs);
        assert_eq!(pool.get(&Selector::new("node1", "x")), Segment::Integer(10));
        assert_eq!(
            pool.get(&Selector::new("node1", "y")),
            Segment::String("hi".into())
        );
    }

    #[test]
    fn test_pool_get_node_variables() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "a"), Segment::Integer(1));
        pool.set(&Selector::new("n1", "b"), Segment::Integer(2));
        pool.set(&Selector::new("n2", "c"), Segment::Integer(3));
        let vars = pool.get_node_variables("n1");
        assert_eq!(vars.len(), 2);
        assert!(vars.contains_key("a"));
        assert!(vars.contains_key("b"));
    }

    #[test]
    fn test_pool_remove_node() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "a"), Segment::Integer(1));
        pool.set(&Selector::new("n1", "b"), Segment::Integer(2));
        pool.set(&Selector::new("n2", "c"), Segment::Integer(3));
        pool.remove_node("n1");
        assert!(pool.get(&Selector::new("n1", "a")).is_none());
        assert!(pool.get(&Selector::new("n1", "b")).is_none());
        assert_eq!(pool.get(&Selector::new("n2", "c")), Segment::Integer(3));
    }

    #[test]
    fn test_pool_clear() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "x");
        pool.set(&sel, Segment::Integer(42));
        pool.clear(&sel);
        assert!(pool.get(&sel).is_none());
    }

    #[test]
    fn test_pool_snapshot() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n", "a"), Segment::Integer(1));
        pool.set(&Selector::new("n", "b"), Segment::String("x".into()));
        let snap = pool.snapshot();
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn test_pool_append_to_none() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "arr");
        pool.append(&sel, Segment::Integer(1));
        match pool.get(&sel) {
            Segment::Array(v) => assert_eq!(v.len(), 1),
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_pool_append_string() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "s");
        pool.set(&sel, Segment::String("hello".into()));
        pool.append(&sel, Segment::String(" world".into()));
        assert_eq!(pool.get(&sel).to_display_string(), "hello world");
    }

    #[test]
    fn test_pool_append_non_array() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "x");
        pool.set(&sel, Segment::Integer(1));
        pool.append(&sel, Segment::Integer(2));
        match pool.get(&sel) {
            Segment::Array(v) => assert_eq!(v.len(), 2),
            _ => panic!("expected Array after appending to non-array"),
        }
    }

    #[test]
    fn test_pool_append_array_string_same_type() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "arr");
        pool.set(&sel, Segment::ArrayString(Arc::new(vec!["a".into()])));
        pool.append(&sel, Segment::String("b".into()));
        match pool.get(&sel) {
            Segment::ArrayString(v) => assert_eq!(v.len(), 2),
            _ => panic!("expected ArrayString"),
        }
    }

    #[tokio::test]
    async fn test_pool_get_resolved_value() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n", "x"), Segment::Integer(42));
        let val = pool.get_resolved_value(&Selector::new("n", "x")).await;
        assert_eq!(val, serde_json::json!(42));
    }

    #[test]
    fn test_pool_default() {
        let pool = VariablePool::default();
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_segment_type_from_dsl_type_all() {
        assert_eq!(
            SegmentType::from_dsl_type("string"),
            Some(SegmentType::String)
        );
        assert_eq!(
            SegmentType::from_dsl_type("number"),
            Some(SegmentType::Number)
        );
        assert_eq!(
            SegmentType::from_dsl_type("boolean"),
            Some(SegmentType::Boolean)
        );
        assert_eq!(
            SegmentType::from_dsl_type("object"),
            Some(SegmentType::Object)
        );
        assert_eq!(
            SegmentType::from_dsl_type("array[string]"),
            Some(SegmentType::ArrayString)
        );
        assert_eq!(
            SegmentType::from_dsl_type("array[number]"),
            Some(SegmentType::ArrayNumber)
        );
        assert_eq!(
            SegmentType::from_dsl_type("array[object]"),
            Some(SegmentType::ArrayObject)
        );
        assert_eq!(SegmentType::from_dsl_type("file"), Some(SegmentType::File));
        assert_eq!(
            SegmentType::from_dsl_type("array[file]"),
            Some(SegmentType::ArrayFile)
        );
        assert_eq!(SegmentType::from_dsl_type("unknown"), None);
    }

    #[tokio::test]
    async fn test_stream_writer_drop_without_end() {
        let (stream, writer) = SegmentStream::channel();
        drop(writer);
        let err = stream.collect().await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_stream_with_limits_max_chunks() {
        let limits = StreamLimits {
            max_chunks: Some(1),
            max_buffer_bytes: None,
        };
        let (stream, writer) = SegmentStream::channel_with_limits(limits);
        writer.send(Segment::String("a".into())).await;
        writer.send(Segment::String("b".into())).await;
        let result = stream.collect().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_stream_with_limits_max_bytes() {
        let limits = StreamLimits {
            max_chunks: None,
            max_buffer_bytes: Some(5),
        };
        let (stream, writer) = SegmentStream::channel_with_limits(limits);
        writer.send(Segment::String("hello".into())).await;
        writer.send(Segment::String("world".into())).await;
        let result = stream.collect().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_stream_multiple_writers() {
        let (stream, writer) = SegmentStream::channel();
        let writer2 = writer.clone();
        writer.send(Segment::String("a".into())).await;
        writer2.send(Segment::String("b".into())).await;
        drop(writer);
        writer2.end(Segment::String("ab".into())).await;
        let result = stream.collect().await.unwrap();
        assert_eq!(result.to_display_string(), "ab");
    }

    #[tokio::test]
    async fn test_stream_status_and_chunks() {
        let (stream, writer) = SegmentStream::channel();
        assert_eq!(stream.status(), StreamStatus::Running);
        writer.send(Segment::String("a".into())).await;
        assert_eq!(stream.chunks().len(), 1);
        writer.end(Segment::String("a".into())).await;
        assert_eq!(stream.status(), StreamStatus::Completed);
    }

    #[tokio::test]
    async fn test_stream_snapshot_segment() {
        let (stream, writer) = SegmentStream::channel();
        writer.send(Segment::String("a".into())).await;
        let snap = stream.snapshot_segment();
        assert!(matches!(snap, Segment::Array(_)));
        writer.end(Segment::String("done".into())).await;
        let snap2 = stream.snapshot_segment();
        assert_eq!(snap2.to_display_string(), "done");
    }

    #[test]
    fn test_segment_display_stream_running() {
        let (stream, _writer) = SegmentStream::channel();
        let seg = Segment::Stream(stream);
        // display should not panic on running stream
        let _ = seg.to_display_string();
    }

    #[test]
    fn test_segment_snapshot_to_value_non_stream() {
        let seg = Segment::Integer(42);
        assert_eq!(seg.snapshot_to_value(), serde_json::json!(42));
    }

    #[test]
    fn test_file_segment_default() {
        let fs = FileSegment::default();
        assert!(fs.url.is_none());
        assert!(fs.name.is_empty());
        assert_eq!(fs.transfer_method, FileTransferMethod::RemoteUrl);
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_pool_set_checked() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "a");
        assert!(pool
            .set_checked(&sel, Segment::Integer(1), 10, 10000)
            .is_ok());
        assert_eq!(pool.len(), 1);
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_pool_set_checked_too_many_entries() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "a");
        pool.set(&sel, Segment::Integer(1));
        let sel2 = Selector::new("n", "b");
        let result = pool.set_checked(&sel2, Segment::Integer(2), 1, 10000);
        assert!(result.is_err());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_pool_selector_validation() {
        use crate::security::validation::SelectorValidation;
        let cfg = SelectorValidation {
            max_depth: 2,
            max_length: 10,
            allowed_prefixes: std::collections::HashSet::from(["n1".to_string()]),
        };
        let mut pool = VariablePool::new_with_selector_validation(Some(cfg));
        pool.set(&Selector::new("n1", "a"), Segment::Integer(1));
        assert_eq!(pool.get(&Selector::new("n1", "a")), Segment::Integer(1));
        // Disallowed prefix
        assert!(pool.get(&Selector::new("n2", "a")).is_none());
        assert!(!pool.has(&Selector::new("n2", "a")));
    }

    #[test]
    fn test_stream_writer_send_after_end() {
        let (stream, writer) = SegmentStream::channel();
        let w = writer;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            w.send(Segment::String("chunk1".into())).await;
            w.end(Segment::String("final".into())).await;
            // Send after end should be a no-op (status != Running)
            w.send(Segment::String("nope".into())).await;
        });
        let (status, chunks, _, _) = stream.snapshot_status();
        assert_eq!(chunks.len(), 1);
        assert!(matches!(status, StreamStatus::Completed));
    }

    #[test]
    fn test_stream_writer_double_end() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.end(Segment::String("a".into())).await;
            writer.end(Segment::String("b".into())).await;
        });
        let (status, _, final_val, _) = stream.snapshot_status();
        assert!(matches!(status, StreamStatus::Completed));
        // second end should be no-op, keep first value
        assert_eq!(final_val, Some(Segment::String("a".into())));
    }

    #[test]
    fn test_stream_writer_error_after_error() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.error("first error".to_string()).await;
            writer.error("second error".to_string()).await;
        });
        let (status, _, _, err) = stream.snapshot_status();
        assert!(matches!(status, StreamStatus::Failed));
        assert_eq!(err, Some("first error".to_string()));
    }

    #[test]
    fn test_segment_into_value_shared_arc() {
        // When Arc has multiple owners, into_value should still work (clone path)
        let arr = Arc::new(SegmentArray::new(vec![
            Segment::Integer(1),
            Segment::Integer(2),
        ]));
        let arr_clone = arr.clone(); // create second owner
        let seg = Segment::Array(arr);
        let val = seg.into_value();
        assert_eq!(val, serde_json::json!([1, 2]));
        // arr_clone is still alive
        assert_eq!(arr_clone.len(), 2);
    }

    #[test]
    fn test_segment_into_value_object_shared_arc() {
        let mut map = std::collections::HashMap::new();
        map.insert("key".to_string(), Segment::String("val".into()));
        let obj = Arc::new(SegmentObject::new(map));
        let obj_clone = obj.clone();
        let seg = Segment::Object(obj);
        let val = seg.into_value();
        assert_eq!(val, serde_json::json!({"key": "val"}));
        assert_eq!(obj_clone.len(), 1);
    }

    #[test]
    fn test_segment_display_stream_failed() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.error("some error".to_string()).await;
        });
        let seg = Segment::Stream(stream);
        let display = seg.to_display_string();
        assert!(display.contains("stream error"));
        assert!(display.contains("some error"));
    }

    #[test]
    fn test_segment_display_stream_completed_no_final() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.end(Segment::None).await;
        });
        let seg = Segment::Stream(stream);
        let display = seg.to_display_string();
        // Completed with None final_value should display empty
        assert!(display.is_empty());
    }

    #[test]
    fn test_file_segment_from_segment_non_object() {
        let seg = Segment::String("not an object".into());
        let file = FileSegment::from_segment(&seg);
        assert!(file.is_none());
    }

    #[test]
    fn test_segment_array_into_value_cached() {
        let arr = SegmentArray::new(vec![Segment::Integer(1)]);
        // First call computes value
        let v1 = arr.to_value();
        // Second call should use cache
        let v2 = arr.to_value();
        assert_eq!(v1, v2);
        // into_value when cache is present
        let v3 = arr.into_value();
        assert_eq!(v1, v3);
    }

    #[test]
    fn test_segment_object_into_value_cached() {
        let mut map = std::collections::HashMap::new();
        map.insert("a".to_string(), Segment::Integer(1));
        let obj = SegmentObject::new(map);
        let v1 = obj.to_value();
        let v2 = obj.to_value();
        assert_eq!(v1, v2);
        let v3 = obj.into_value();
        assert_eq!(v1, v3);
    }

    #[test]
    fn test_stream_reader_clone_drop() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.send(Segment::String("a".into())).await;
            writer.end(Segment::None).await;
        });
        // Just test that clone/drop works without panic
        let reader1 = stream.reader();
        let _reader2 = reader1.clone();
        drop(_reader2);
        // reader1 is still alive
        let chunks = stream.chunks();
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_segment_snapshot_to_value_stream() {
        let (stream, writer) = SegmentStream::channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            writer.send(Segment::String("hello".into())).await;
            writer.send(Segment::String(" world".into())).await;
            writer.end(Segment::String("hello world".into())).await;
        });
        let seg = Segment::Stream(stream);
        let val = seg.snapshot_to_value();
        // Completed stream with final value should return that value
        assert_eq!(val, serde_json::json!("hello world"));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_pool_set_checked_memory_limit() {
        let mut pool = VariablePool::new();
        let sel = Selector::new("n", "x");
        // set_checked with very small memory limit
        let result = pool.set_checked(
            &sel,
            Segment::String("a very long string that uses bytes".into()),
            100,
            1,
        );
        assert!(result.is_err());
    }

    // ---- Stream memory/lifecycle tests (moved from tests/memory/) ----

    #[tokio::test]
    async fn test_stream_writer_drop_without_end_collect_fails() {
        let (stream, writer) = SegmentStream::channel();
        writer.send(Segment::String("chunk1".into())).await;
        drop(writer);

        let result = stream.collect().await;
        assert!(result.is_err(), "expected stream to fail after writer drop");
    }

    #[tokio::test]
    async fn test_stream_reader_drop_decrements_count() {
        let (stream, writer) = SegmentStream::channel();
        let r1 = stream.reader();
        let r2 = stream.reader();
        let r3 = stream.reader();

        assert_eq!(stream.debug_readers_count(), 3);

        drop(r1);
        assert_eq!(stream.debug_readers_count(), 2);
        drop(r2);
        assert_eq!(stream.debug_readers_count(), 1);
        drop(r3);
        assert_eq!(stream.debug_readers_count(), 0);

        writer.end(Segment::String("done".into())).await;
        let result = stream.collect().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stream_arc_cleanup_after_completion() {
        let (stream, writer) = SegmentStream::channel();
        let weak = stream.debug_state_weak();

        writer.end(Segment::String("final".into())).await;
        let _ = stream.collect().await;

        drop(writer);
        drop(stream);

        assert!(weak.is_dropped(), "stream state Arc should be released");
    }

    #[tokio::test]
    async fn test_stream_multiple_readers_partial_drop() {
        let (stream, writer) = SegmentStream::channel();
        let mut r1 = stream.reader();
        let r2 = stream.reader();

        writer.send(Segment::String("chunk".into())).await;
        writer.end(Segment::String("done".into())).await;

        let mut events = Vec::new();
        while let Some(e) = r1.next().await {
            events.push(e);
            if matches!(events.last(), Some(StreamEvent::End(_))) {
                break;
            }
        }

        assert_eq!(events.len(), 2);

        drop(r1);
        drop(r2);
        assert_eq!(stream.debug_readers_count(), 0);
    }

    // ---- Pool structural sharing test (moved from tests/memory/) ----

    #[tokio::test]
    async fn test_pool_clone_structural_sharing() {
        let mut pool = VariablePool::new();
        let mut entries = std::collections::HashMap::new();
        entries.insert("k".to_string(), Segment::String("v".into()));
        let obj = Arc::new(SegmentObject::new(entries));

        pool.set(&Selector::new("node", "obj"), Segment::Object(obj.clone()));
        assert_eq!(Arc::strong_count(&obj), 2);

        let mut clone = pool.clone();
        assert_eq!(Arc::strong_count(&obj), 2);

        clone.set(
            &Selector::new("node", "obj"),
            Segment::String("override".into()),
        );

        assert_eq!(Arc::strong_count(&obj), 2);
    }
}
