use crate::error::WorkflowError;

use super::schema::WorkflowSchema;

/// DSL 格式
#[derive(Debug, Clone, Copy)]
pub enum DslFormat {
    Yaml,
    Json,
}

/// 解析 DSL 内容
///
/// # 参数
/// - `content`: DSL 内容字符串
/// - `format`: DSL 格式（YAML 或 JSON）
///
/// # 返回
/// - `Ok(WorkflowSchema)`: 解析成功
/// - `Err(WorkflowError)`: 解析失败
pub fn parse_dsl(content: &str, format: DslFormat) -> Result<WorkflowSchema, WorkflowError> {
    match format {
        DslFormat::Yaml => serde_yaml::from_str(content)
            .map_err(|e| WorkflowError::DslParseError(format!("YAML parse error: {}", e))),
        DslFormat::Json => serde_json::from_str(content)
            .map_err(|e| WorkflowError::DslParseError(format!("JSON parse error: {}", e))),
    }
}

/// 自动检测格式并解析 DSL
pub fn parse_dsl_auto(content: &str) -> Result<WorkflowSchema, WorkflowError> {
    // 先尝试 JSON
    if let Ok(schema) = parse_dsl(content, DslFormat::Json) {
        return Ok(schema);
    }
    // 再尝试 YAML
    parse_dsl(content, DslFormat::Yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_yaml_dsl() {
        let yaml = r#"
nodes:
  - id: start
    type: start
    data: {}
  - id: end
    type: end
    data: {}
edges:
  - id: e1
    source: start
    target: end
"#;
        let result = parse_dsl(yaml, DslFormat::Yaml);
        assert!(result.is_ok());
        let schema = result.unwrap();
        assert_eq!(schema.nodes.len(), 2);
        assert_eq!(schema.edges.len(), 1);
    }

    #[test]
    fn test_parse_json_dsl() {
        let json = r#"{
            "nodes": [
                {"id": "start", "type": "start", "data": {}},
                {"id": "end", "type": "end", "data": {}}
            ],
            "edges": [
                {"id": "e1", "source": "start", "target": "end"}
            ]
        }"#;
        let result = parse_dsl(json, DslFormat::Json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_dsl_auto_yaml() {
        let yaml = r#"
nodes:
  - id: start
    type: start
    data: {}
edges: []
"#;
        let result = parse_dsl_auto(yaml);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_dsl_invalid() {
        let invalid = "not valid content }{}{}{";
        let result = parse_dsl_auto(invalid);
        assert!(result.is_err());
    }
}
