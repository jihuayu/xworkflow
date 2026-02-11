use serde_json::Value;

use crate::dsl::schema::NodeType;

pub const RESERVED_NAMESPACES: &[&str] = &["sys", "env", "conversation", "loop"];

pub const BRANCH_NODE_TYPES: &[&str] = &["if-else", "question-classifier"];

pub const STUB_NODE_TYPES: &[&str] = &[
    "knowledge-retrieval",
    "question-classifier",
    "parameter-extractor",
    "tool",
    "document-extractor",
    "agent",
    "human-input",
];

pub fn is_known_node_type(node_type: &str) -> bool {
    if node_type.starts_with("plugin.") {
        return true;
    }
    serde_json::from_value::<NodeType>(Value::String(node_type.to_string())).is_ok()
}

pub fn is_stub_node_type(node_type: &str) -> bool {
    STUB_NODE_TYPES.iter().any(|t| *t == node_type)
}
