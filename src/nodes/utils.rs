use serde_json::Value;

use crate::core::variable_pool::Selector;

pub fn selector_from_value(value: &Value) -> Option<Selector> {
    Selector::parse_value(value)
}

pub fn selector_from_str(selector: &str) -> Option<Selector> {
    Selector::parse_str(selector)
}
