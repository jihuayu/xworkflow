//! DSL parser: converts raw YAML/JSON/TOML text into [`WorkflowSchema`].

use super::schema::WorkflowSchema;
use crate::error::WorkflowError;

/// Supported DSL input formats.
#[derive(Debug, Clone, Copy)]
pub enum DslFormat {
    /// YAML format (`.yaml` / `.yml`).
    Yaml,
    /// JSON format (`.json`).
    Json,
    /// TOML format (`.toml`).
    Toml,
}

/// Parse DSL content into WorkflowSchema
pub fn parse_dsl(content: &str, format: DslFormat) -> Result<WorkflowSchema, WorkflowError> {
    match format {
        DslFormat::Yaml => serde_saphyr::from_str(content)
            .map_err(|e| WorkflowError::DslParseError(e.to_string())),
        DslFormat::Json => serde_json::from_str(content)
            .map_err(|e| WorkflowError::DslParseError(e.to_string())),
        DslFormat::Toml => {
            // Parse TOML → toml::Value, then convert to serde_json::Value,
            // and finally deserialize into WorkflowSchema.  This two-step
            // conversion ensures fields typed as serde_json::Value (e.g.
            // condition values) are handled correctly.
            let toml_val: toml::Value = toml::from_str(content)
                .map_err(|e| WorkflowError::DslParseError(e.to_string()))?;
            let json_val = toml_value_to_json(toml_val);
            serde_json::from_value(json_val)
                .map_err(|e| WorkflowError::DslParseError(e.to_string()))
        }
    }
}

/// Convert a [`toml::Value`] into a [`serde_json::Value`].
///
/// TOML does not have a null type, so `Datetime` values are stringified.
fn toml_value_to_json(val: toml::Value) -> serde_json::Value {
    match val {
        toml::Value::String(s) => serde_json::Value::String(s),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(b),
        toml::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(tbl) => {
            let map: serde_json::Map<String, serde_json::Value> = tbl
                .into_iter()
                .map(|(k, v)| (k, toml_value_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_yaml() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start_1
    data:
      type: start
      title: Start
edges: []
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        assert_eq!(schema.version, "0.1.0");
        assert_eq!(schema.nodes.len(), 1);
        assert_eq!(schema.nodes[0].data.node_type, "start");
    }

    #[test]
    fn test_parse_json() {
        let json = r#"{"version":"0.1.0","nodes":[{"id":"s","data":{"type":"start","title":"S"}}],"edges":[]}"#;
        let schema = parse_dsl(json, DslFormat::Json).unwrap();
        assert_eq!(schema.version, "0.1.0");
        assert_eq!(schema.nodes.len(), 1);
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_dsl("{{{invalid", DslFormat::Json).is_err());
    }

    #[test]
    fn test_parse_toml() {
        let toml_str = r#"
version = "0.1.0"
edges = []

[[nodes]]
id = "start_1"
[nodes.data]
type = "start"
title = "Start"
"#;
        let schema = parse_dsl(toml_str, DslFormat::Toml).unwrap();
        assert_eq!(schema.version, "0.1.0");
        assert_eq!(schema.nodes.len(), 1);
        assert_eq!(schema.nodes[0].data.node_type, "start");
    }

    #[test]
    fn test_parse_toml_invalid() {
        assert!(parse_dsl("[[[bad", DslFormat::Toml).is_err());
    }
}
