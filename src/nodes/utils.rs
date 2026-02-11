//! Utility functions for node implementations.

use serde_json::Value;

use crate::core::variable_pool::Selector;

/// Parse a variable selector from a JSON value (string or string array).
pub fn selector_from_value(value: &Value) -> Option<Selector> {
    Selector::parse_value(value)
}

/// Parse a variable selector from a dot-separated string.
pub fn selector_from_str(selector: &str) -> Option<Selector> {
    Selector::parse_str(selector)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selector_from_value_array() {
        let val = serde_json::json!(["sys", "user_id"]);
        let sel = selector_from_value(&val);
        assert!(sel.is_some());
    }

    #[test]
    fn test_selector_from_value_invalid() {
        let val = serde_json::json!(42);
        let sel = selector_from_value(&val);
        assert!(sel.is_none());
    }

    #[test]
    fn test_selector_from_str_valid() {
        let sel = selector_from_str("sys.user_id");
        assert!(sel.is_some());
    }

    #[test]
    fn test_selector_from_str_empty() {
        let sel = selector_from_str("");
        assert!(sel.is_none());
    }
}
