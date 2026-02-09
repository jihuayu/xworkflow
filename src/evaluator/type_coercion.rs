use serde_json::Value;

use crate::error::NodeError;

/// 将 Value 转换为 f64
pub fn to_f64(value: &Value) -> Result<f64, NodeError> {
    match value {
        Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| NodeError::TypeError("Cannot convert number to f64".to_string())),
        Value::String(s) => s
            .parse::<f64>()
            .map_err(|e| NodeError::TypeError(format!("Cannot convert '{}' to number: {}", s, e))),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Null => Ok(0.0),
        _ => Err(NodeError::TypeError(format!(
            "Cannot convert {:?} to number",
            value
        ))),
    }
}

/// 数值比较
pub fn compare_numeric<F>(value: &Value, target: &Value, compare_fn: F) -> Result<bool, NodeError>
where
    F: Fn(f64, f64) -> bool,
{
    let a = to_f64(value)?;
    let b = to_f64(target)?;
    Ok(compare_fn(a, b))
}

/// 将 Value 转换为字符串
pub fn to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_to_f64() {
        assert_eq!(to_f64(&json!(42)).unwrap(), 42.0);
        assert_eq!(to_f64(&json!(3.14)).unwrap(), 3.14);
        assert_eq!(to_f64(&json!("100")).unwrap(), 100.0);
        assert_eq!(to_f64(&json!(true)).unwrap(), 1.0);
        assert_eq!(to_f64(&json!(false)).unwrap(), 0.0);
        assert_eq!(to_f64(&json!(null)).unwrap(), 0.0);
    }

    #[test]
    fn test_to_f64_invalid() {
        assert!(to_f64(&json!("not a number")).is_err());
        assert!(to_f64(&json!([1, 2, 3])).is_err());
    }

    #[test]
    fn test_compare_numeric() {
        assert!(compare_numeric(&json!(10), &json!(5), |a, b| a > b).unwrap());
        assert!(!compare_numeric(&json!(3), &json!(5), |a, b| a > b).unwrap());
        assert!(compare_numeric(&json!("10"), &json!(5), |a, b| a > b).unwrap());
    }

    #[test]
    fn test_to_string() {
        assert_eq!(to_string(&json!("hello")), "hello");
        assert_eq!(to_string(&json!(42)), "42");
        assert_eq!(to_string(&json!(true)), "true");
        assert_eq!(to_string(&json!(null)), "");
    }
}
