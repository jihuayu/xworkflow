use serde_json::Value;

use crate::error::NodeError;

/// Contains 操作符
pub fn contains(value: &Value, target: &Value) -> Result<bool, NodeError> {
    match (value, target) {
        (Value::String(s), Value::String(t)) => Ok(s.contains(t.as_str())),
        (Value::Array(arr), target) => Ok(arr.contains(target)),
        (Value::String(s), Value::Number(n)) => Ok(s.contains(&n.to_string())),
        _ => Ok(false),
    }
}

/// StartsWith 操作符
pub fn starts_with(value: &Value, target: &Value) -> Result<bool, NodeError> {
    match (value, target) {
        (Value::String(s), Value::String(t)) => Ok(s.starts_with(t.as_str())),
        _ => Ok(false),
    }
}

/// EndsWith 操作符
pub fn ends_with(value: &Value, target: &Value) -> Result<bool, NodeError> {
    match (value, target) {
        (Value::String(s), Value::String(t)) => Ok(s.ends_with(t.as_str())),
        _ => Ok(false),
    }
}

/// IsEmpty 操作符
pub fn is_empty(value: &Value) -> Result<bool, NodeError> {
    match value {
        Value::Null => Ok(true),
        Value::String(s) => Ok(s.is_empty()),
        Value::Array(arr) => Ok(arr.is_empty()),
        Value::Object(obj) => Ok(obj.is_empty()),
        Value::Bool(false) => Ok(true),
        Value::Number(n) => Ok(n.as_f64() == Some(0.0)),
        _ => Ok(false),
    }
}

/// Equal 操作符
pub fn equal(value: &Value, target: &Value) -> Result<bool, NodeError> {
    // 相同类型直接比较
    if value == target {
        return Ok(true);
    }

    // 尝试类型转换后比较
    match (value, target) {
        // 数字比较（处理 int vs float）
        (Value::Number(a), Value::Number(b)) => {
            Ok(a.as_f64() == b.as_f64())
        }
        // 字符串与数字比较
        (Value::String(s), Value::Number(n)) | (Value::Number(n), Value::String(s)) => {
            if let Ok(parsed) = s.parse::<f64>() {
                Ok(Some(parsed) == n.as_f64())
            } else {
                Ok(false)
            }
        }
        // 布尔与字符串比较
        (Value::Bool(b), Value::String(s)) | (Value::String(s), Value::Bool(b)) => {
            match s.to_lowercase().as_str() {
                "true" => Ok(*b),
                "false" => Ok(!*b),
                _ => Ok(false),
            }
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_contains_string() {
        assert!(contains(&json!("hello world"), &json!("world")).unwrap());
        assert!(!contains(&json!("hello world"), &json!("xyz")).unwrap());
    }

    #[test]
    fn test_contains_array() {
        assert!(contains(&json!([1, 2, 3]), &json!(2)).unwrap());
        assert!(!contains(&json!([1, 2, 3]), &json!(4)).unwrap());
    }

    #[test]
    fn test_is_empty_various_types() {
        assert!(is_empty(&json!(null)).unwrap());
        assert!(is_empty(&json!("")).unwrap());
        assert!(is_empty(&json!([])).unwrap());
        assert!(is_empty(&json!({})).unwrap());
        assert!(!is_empty(&json!("hello")).unwrap());
        assert!(!is_empty(&json!([1])).unwrap());
    }

    #[test]
    fn test_equal_cross_type() {
        assert!(equal(&json!("42"), &json!(42)).unwrap());
        assert!(equal(&json!(42), &json!("42")).unwrap());
        assert!(equal(&json!("true"), &json!(true)).unwrap());
    }
}
