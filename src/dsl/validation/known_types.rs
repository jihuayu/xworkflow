use serde_json::Value;

use crate::dsl::schema::NodeType;

pub const RESERVED_NAMESPACES: &[&str] = &["sys", "env", "conversation", "loop"];

pub const BRANCH_NODE_TYPES: &[&str] = &["if-else", "question-classifier"];

#[cfg(all(feature = "builtin-docextract-node", feature = "builtin-agent-node"))]
pub const STUB_NODE_TYPES: &[&str] = &["knowledge-retrieval", "parameter-extractor"];

#[cfg(all(
    not(feature = "builtin-docextract-node"),
    feature = "builtin-agent-node"
))]
pub const STUB_NODE_TYPES: &[&str] = &[
    "knowledge-retrieval",
    "parameter-extractor",
    "document-extractor",
];

#[cfg(all(
    feature = "builtin-docextract-node",
    not(feature = "builtin-agent-node")
))]
pub const STUB_NODE_TYPES: &[&str] = &[
    "knowledge-retrieval",
    "parameter-extractor",
    "tool",
    "agent",
];

#[cfg(all(
    not(feature = "builtin-docextract-node"),
    not(feature = "builtin-agent-node")
))]
pub const STUB_NODE_TYPES: &[&str] = &[
    "knowledge-retrieval",
    "parameter-extractor",
    "tool",
    "document-extractor",
    "agent",
];

pub fn is_known_node_type(node_type: &str) -> bool {
    if node_type.starts_with("plugin.") {
        return true;
    }
    serde_json::from_value::<NodeType>(Value::String(node_type.to_string())).is_ok()
}

pub fn is_stub_node_type(node_type: &str) -> bool {
    if node_type == "question-classifier" {
        return !cfg!(feature = "builtin-llm-node");
    }
    STUB_NODE_TYPES.contains(&node_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reserved_namespaces() {
        assert!(RESERVED_NAMESPACES.contains(&"sys"));
        assert!(RESERVED_NAMESPACES.contains(&"env"));
        assert!(RESERVED_NAMESPACES.contains(&"conversation"));
        assert!(RESERVED_NAMESPACES.contains(&"loop"));
        assert!(!RESERVED_NAMESPACES.contains(&"custom"));
    }

    #[test]
    fn test_branch_node_types() {
        assert!(BRANCH_NODE_TYPES.contains(&"if-else"));
        assert!(BRANCH_NODE_TYPES.contains(&"question-classifier"));
        assert!(!BRANCH_NODE_TYPES.contains(&"start"));
    }

    #[test]
    fn test_stub_node_types() {
        assert!(STUB_NODE_TYPES.contains(&"knowledge-retrieval"));
        assert!(STUB_NODE_TYPES.contains(&"parameter-extractor"));

        #[cfg(not(feature = "builtin-agent-node"))]
        assert!(STUB_NODE_TYPES.contains(&"tool"));

        #[cfg(not(feature = "builtin-docextract-node"))]
        assert!(STUB_NODE_TYPES.contains(&"document-extractor"));

        #[cfg(not(feature = "builtin-agent-node"))]
        assert!(STUB_NODE_TYPES.contains(&"agent"));

        assert!(!STUB_NODE_TYPES.contains(&"start"));
    }

    #[test]
    fn test_is_known_node_type_builtin() {
        assert!(is_known_node_type("start"));
        assert!(is_known_node_type("end"));
        assert!(is_known_node_type("if-else"));
        assert!(is_known_node_type("code"));
        assert!(is_known_node_type("template-transform"));
        assert!(is_known_node_type("http-request"));
        assert!(is_known_node_type("llm"));
        assert!(is_known_node_type("answer"));
        assert!(is_known_node_type("gather"));
    }

    #[test]
    fn test_is_known_node_type_plugin() {
        assert!(is_known_node_type("plugin.custom_node"));
        assert!(is_known_node_type("plugin.anything"));
    }

    #[test]
    fn test_is_known_node_type_unknown() {
        assert!(!is_known_node_type("nonexistent"));
        assert!(!is_known_node_type("random"));
    }

    #[test]
    fn test_is_stub_node_type_true() {
        assert!(is_stub_node_type("knowledge-retrieval"));

        #[cfg(not(feature = "builtin-llm-node"))]
        assert!(is_stub_node_type("question-classifier"));

        #[cfg(not(feature = "builtin-agent-node"))]
        assert!(is_stub_node_type("tool"));

        #[cfg(not(feature = "builtin-agent-node"))]
        assert!(is_stub_node_type("agent"));
    }

    #[test]
    fn test_is_stub_node_type_false() {
        assert!(!is_stub_node_type("start"));
        assert!(!is_stub_node_type("end"));
        assert!(!is_stub_node_type("code"));
        assert!(!is_stub_node_type("plugin.something"));

        #[cfg(feature = "builtin-llm-node")]
        assert!(!is_stub_node_type("question-classifier"));
    }
}
