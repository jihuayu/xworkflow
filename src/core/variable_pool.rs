use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ================================
// Segment – Dify variable type system
// ================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Segment {
    None,
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Object(HashMap<String, Segment>),
    ArrayString(Vec<String>),
    ArrayInteger(Vec<i64>),
    ArrayFloat(Vec<f64>),
    ArrayObject(Vec<HashMap<String, Segment>>),
    ArrayAny(Vec<Segment>),
    File(FileSegment),
    ArrayFile(Vec<FileSegment>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSegment {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub transfer_method: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub extension: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
}

impl Segment {
    /// Convert Segment → serde_json::Value
    pub fn to_value(&self) -> Value {
        match self {
            Segment::None => Value::Null,
            Segment::String(s) => Value::String(s.clone()),
            Segment::Integer(i) => serde_json::json!(*i),
            Segment::Float(f) => serde_json::json!(*f),
            Segment::Boolean(b) => Value::Bool(*b),
            Segment::Object(map) => {
                let m: serde_json::Map<std::string::String, Value> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect();
                Value::Object(m)
            }
            Segment::ArrayString(v) => Value::Array(v.iter().map(|s| Value::String(s.clone())).collect()),
            Segment::ArrayInteger(v) => Value::Array(v.iter().map(|i| serde_json::json!(*i)).collect()),
            Segment::ArrayFloat(v) => Value::Array(v.iter().map(|f| serde_json::json!(*f)).collect()),
            Segment::ArrayObject(v) => {
                Value::Array(v.iter().map(|m| {
                    let obj: serde_json::Map<std::string::String, Value> = m
                        .iter()
                        .map(|(k, seg)| (k.clone(), seg.to_value()))
                        .collect();
                    Value::Object(obj)
                }).collect())
            }
            Segment::ArrayAny(v) => Value::Array(v.iter().map(|s| s.to_value()).collect()),
            Segment::File(f) => serde_json::to_value(f).unwrap_or(Value::Null),
            Segment::ArrayFile(v) => serde_json::to_value(v).unwrap_or(Value::Null),
        }
    }

    /// Create Segment from serde_json::Value
    pub fn from_value(v: &Value) -> Self {
        match v {
            Value::Null => Segment::None,
            Value::Bool(b) => Segment::Boolean(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Segment::Integer(i)
                } else {
                    Segment::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => Segment::String(s.clone()),
            Value::Array(arr) => {
                let segs: Vec<Segment> = arr.iter().map(Segment::from_value).collect();
                Segment::ArrayAny(segs)
            }
            Value::Object(map) => {
                let m: HashMap<String, Segment> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), Segment::from_value(v)))
                    .collect();
                Segment::Object(m)
            }
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Segment::None)
    }

    pub fn as_string(&self) -> Option<String> {
        match self {
            Segment::String(s) => Some(s.clone()),
            Segment::Integer(i) => Some(i.to_string()),
            Segment::Float(f) => Some(f.to_string()),
            Segment::Boolean(b) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn to_display_string(&self) -> String {
        match self {
            Segment::None => String::new(),
            Segment::String(s) => s.clone(),
            Segment::Integer(i) => i.to_string(),
            Segment::Float(f) => f.to_string(),
            Segment::Boolean(b) => b.to_string(),
            other => serde_json::to_string(&other.to_value()).unwrap_or_default(),
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Segment::Integer(i) => Some(*i as f64),
            Segment::Float(f) => Some(*f),
            Segment::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Segment::None => true,
            Segment::String(s) => s.is_empty(),
            Segment::ArrayString(v) => v.is_empty(),
            Segment::ArrayInteger(v) => v.is_empty(),
            Segment::ArrayFloat(v) => v.is_empty(),
            Segment::ArrayObject(v) => v.is_empty(),
            Segment::ArrayAny(v) => v.is_empty(),
            Segment::ArrayFile(v) => v.is_empty(),
            _ => false,
        }
    }
}

impl std::fmt::Display for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_display_string())
    }
}

// ================================
// VariablePool – Dify-compatible
// Key: (node_id, variable_name)
// ================================

#[derive(Debug, Clone)]
pub struct VariablePool {
    variables: HashMap<(String, String), Segment>,
}

impl VariablePool {
    pub fn new() -> Self {
        VariablePool {
            variables: HashMap::new(),
        }
    }

    /// Get variable by selector: ["node_id", "var_name"] or ["sys", "query"]
    pub fn get(&self, selector: &[String]) -> Segment {
        if selector.len() < 2 {
            return Segment::None;
        }
        let key = (selector[0].clone(), selector[1].clone());
        self.variables.get(&key).cloned().unwrap_or(Segment::None)
    }

    /// Set a single variable
    pub fn set(&mut self, selector: &[String], value: Segment) {
        if selector.len() >= 2 {
            let key = (selector[0].clone(), selector[1].clone());
            self.variables.insert(key, value);
        }
    }

    /// Set node outputs as (node_id, key) -> value
    pub fn set_node_outputs(&mut self, node_id: &str, outputs: &HashMap<String, Value>) {
        for (key, val) in outputs {
            let seg = Segment::from_value(val);
            self.variables.insert((node_id.to_string(), key.clone()), seg);
        }
    }

    /// Set node outputs from Segment map
    pub fn set_node_segment_outputs(&mut self, node_id: &str, outputs: &HashMap<String, Segment>) {
        for (key, val) in outputs {
            self.variables.insert((node_id.to_string(), key.clone()), val.clone());
        }
    }

    /// Check if variable exists and is not None
    pub fn has(&self, selector: &[String]) -> bool {
        if selector.len() < 2 {
            return false;
        }
        let key = (selector[0].clone(), selector[1].clone());
        self.variables.get(&key).map_or(false, |s| !s.is_none())
    }

    /// Get all variables for a given node_id
    pub fn get_node_variables(&self, node_id: &str) -> HashMap<String, Segment> {
        self.variables
            .iter()
            .filter(|((nid, _), _)| nid == node_id)
            .map(|((_, key), val)| (key.clone(), val.clone()))
            .collect()
    }

    /// Remove all variables for a given node
    pub fn remove_node(&mut self, node_id: &str) {
        self.variables.retain(|(nid, _), _| nid != node_id);
    }

    /// Append value to an existing array variable
    pub fn append(&mut self, selector: &[String], value: Segment) {
        if selector.len() < 2 {
            return;
        }
        let key = (selector[0].clone(), selector[1].clone());
        let existing = self.variables.entry(key).or_insert(Segment::ArrayAny(vec![]));
        match existing {
            Segment::ArrayAny(arr) => arr.push(value),
            Segment::ArrayString(arr) => {
                if let Segment::String(s) = value {
                    arr.push(s);
                }
            }
            Segment::String(s) => {
                *s += &value.to_display_string();
            }
            _ => {
                // Convert to ArrayAny
                let old = std::mem::replace(existing, Segment::None);
                *existing = Segment::ArrayAny(vec![old, value]);
            }
        }
    }

    /// Clear a variable (set to None)
    pub fn clear(&mut self, selector: &[String]) {
        if selector.len() >= 2 {
            let key = (selector[0].clone(), selector[1].clone());
            self.variables.insert(key, Segment::None);
        }
    }

    /// Snapshot entire pool for serialization
    pub fn snapshot(&self) -> HashMap<(String, String), Segment> {
        self.variables.clone()
    }
}

impl Default for VariablePool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_variable_pool_basic() {
        let mut pool = VariablePool::new();
        let sel = vec!["node1".to_string(), "output".to_string()];
        pool.set(&sel, Segment::String("hello".to_string()));
        let val = pool.get(&sel);
        assert!(matches!(val, Segment::String(s) if s == "hello"));
    }

    #[test]
    fn test_variable_pool_sys() {
        let mut pool = VariablePool::new();
        let sel = vec!["sys".to_string(), "query".to_string()];
        pool.set(&sel, Segment::String("test query".to_string()));
        assert!(pool.has(&sel));
        let val = pool.get(&sel);
        assert_eq!(val.to_display_string(), "test query");
    }

    #[test]
    fn test_variable_pool_missing() {
        let pool = VariablePool::new();
        let sel = vec!["nonexistent".to_string(), "var".to_string()];
        assert!(pool.get(&sel).is_none());
        assert!(!pool.has(&sel));
    }

    #[test]
    fn test_set_node_outputs() {
        let mut pool = VariablePool::new();
        let mut outputs = HashMap::new();
        outputs.insert("text".to_string(), Value::String("result".to_string()));
        outputs.insert("count".to_string(), serde_json::json!(42));
        pool.set_node_outputs("node_llm", &outputs);

        let text = pool.get(&["node_llm".to_string(), "text".to_string()]);
        assert!(matches!(text, Segment::String(s) if s == "result"));

        let count = pool.get(&["node_llm".to_string(), "count".to_string()]);
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
    fn test_append() {
        let mut pool = VariablePool::new();
        let sel = vec!["n".to_string(), "arr".to_string()];
        pool.set(&sel, Segment::ArrayAny(vec![]));
        pool.append(&sel, Segment::Integer(1));
        pool.append(&sel, Segment::Integer(2));
        match pool.get(&sel) {
            Segment::ArrayAny(v) => assert_eq!(v.len(), 2),
            _ => panic!("Expected ArrayAny"),
        }
    }
}
