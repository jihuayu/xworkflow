use super::schema::WorkflowSchema;
use crate::error::WorkflowError;

#[derive(Debug, Clone, Copy)]
pub enum DslFormat {
    Yaml,
    Json,
}

/// Parse DSL content into WorkflowSchema
pub fn parse_dsl(content: &str, format: DslFormat) -> Result<WorkflowSchema, WorkflowError> {
    match format {
        DslFormat::Yaml => serde_yaml::from_str(content)
            .map_err(|e| WorkflowError::DslParseError(e.to_string())),
        DslFormat::Json => serde_json::from_str(content)
            .map_err(|e| WorkflowError::DslParseError(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_yaml() {
        let yaml = r#"
nodes:
  - id: start_1
    data:
      type: start
      title: Start
edges: []
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        assert_eq!(schema.nodes.len(), 1);
        assert_eq!(schema.nodes[0].data.node_type, "start");
    }

    #[test]
    fn test_parse_json() {
        let json = r#"{"nodes":[{"id":"s","data":{"type":"start","title":"S"}}],"edges":[]}"#;
        let schema = parse_dsl(json, DslFormat::Json).unwrap();
        assert_eq!(schema.nodes.len(), 1);
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_dsl("{{{invalid", DslFormat::Json).is_err());
    }
}
