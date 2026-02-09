use serde_json::Value;

pub const SCOPE_NODE_ID: &str = "__scope__";

pub fn parse_selector_value(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(arr) => {
            let mut parts = Vec::with_capacity(arr.len());
            for v in arr {
                if let Some(s) = v.as_str() {
                    parts.push(s.to_string());
                } else {
                    return None;
                }
            }
            if parts.is_empty() { None } else { Some(parts) }
        }
        Value::String(s) => {
            let parts: Vec<String> = s
                .split('.')
                .filter(|p| !p.is_empty())
                .map(|p| p.to_string())
                .collect();
            if parts.is_empty() { None } else { Some(parts) }
        }
        _ => None,
    }
}

pub fn normalize_selector(parts: Vec<String>) -> Vec<String> {
    if parts.len() < 2 {
        vec![SCOPE_NODE_ID.to_string(), parts[0].clone()]
    } else {
        parts
    }
}

pub fn selector_from_value(value: &Value) -> Option<Vec<String>> {
    parse_selector_value(value).map(normalize_selector)
}

pub fn selector_from_str(selector: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = selector
        .split('.')
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(normalize_selector(parts))
    }
}
