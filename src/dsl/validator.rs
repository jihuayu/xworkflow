use super::schema::{WorkflowSchema, SUPPORTED_DSL_VERSIONS};
use crate::error::WorkflowError;
use std::collections::HashSet;

/// Validate a parsed WorkflowSchema
pub fn validate_workflow_schema(schema: &WorkflowSchema) -> Result<(), WorkflowError> {
    // Check DSL version
    if !SUPPORTED_DSL_VERSIONS.contains(&schema.version.as_str()) {
        return Err(WorkflowError::UnsupportedVersion {
            found: schema.version.clone(),
            supported: SUPPORTED_DSL_VERSIONS.join(", "),
        });
    }

    if schema.nodes.is_empty() {
        return Err(WorkflowError::GraphValidationError("No nodes defined".into()));
    }

    // Check exactly one start node
    let start_count = schema.nodes.iter().filter(|n| n.data.node_type == "start").count();
    if start_count == 0 {
        return Err(WorkflowError::NoStartNode);
    }
    if start_count > 1 {
        return Err(WorkflowError::MultipleStartNodes);
    }

    // Check at least one end/answer node
    let has_end = schema.nodes.iter().any(|n| n.data.node_type == "end" || n.data.node_type == "answer");
    if !has_end {
        return Err(WorkflowError::NoEndNode);
    }

    // Check unique node IDs
    let mut ids = HashSet::new();
    for node in &schema.nodes {
        if !ids.insert(&node.id) {
            return Err(WorkflowError::GraphValidationError(
                format!("Duplicate node ID: {}", node.id),
            ));
        }
    }

    // Check edge references
    for edge in &schema.edges {
        if !ids.contains(&edge.source) {
            return Err(WorkflowError::GraphValidationError(
                format!("Edge source not found: {}", edge.source),
            ));
        }
        if !ids.contains(&edge.target) {
            return Err(WorkflowError::GraphValidationError(
                format!("Edge target not found: {}", edge.target),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{parse_dsl, DslFormat};

    #[test]
    fn test_valid_schema() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        assert!(validate_workflow_schema(&schema).is_ok());
    }

    #[test]
    fn test_no_start() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: e
    data: { type: end, title: E }
edges: []
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        assert!(validate_workflow_schema(&schema).is_err());
    }

    #[test]
    fn test_no_end() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
edges: []
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        assert!(validate_workflow_schema(&schema).is_err());
    }

    #[test]
    fn test_unsupported_version() {
        let yaml = r#"
version: "99.0.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let err = validate_workflow_schema(&schema).unwrap_err();
        assert!(err.to_string().contains("Unsupported DSL version"));
    }
}
