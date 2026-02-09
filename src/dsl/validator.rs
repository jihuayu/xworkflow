use crate::error::WorkflowError;

use super::schema::WorkflowSchema;

/// 验证工作流 DSL 的合法性
pub fn validate_workflow_schema(schema: &WorkflowSchema) -> Result<(), WorkflowError> {
    // 1. 检查必须有至少一个节点
    if schema.nodes.is_empty() {
        return Err(WorkflowError::GraphValidationError(
            "Workflow must have at least one node".to_string(),
        ));
    }

    // 2. 检查是否有 Start 节点
    let start_nodes: Vec<_> = schema
        .nodes
        .iter()
        .filter(|n| n.node_type == "start")
        .collect();

    if start_nodes.is_empty() {
        return Err(WorkflowError::NoStartNode);
    }

    if start_nodes.len() > 1 {
        return Err(WorkflowError::MultipleStartNodes);
    }

    // 3. 检查是否有 End 节点
    let end_nodes: Vec<_> = schema
        .nodes
        .iter()
        .filter(|n| n.node_type == "end")
        .collect();

    if end_nodes.is_empty() {
        return Err(WorkflowError::NoEndNode);
    }

    // 4. 检查节点 ID 唯一性
    let mut seen_ids = std::collections::HashSet::new();
    for node in &schema.nodes {
        if !seen_ids.insert(&node.id) {
            return Err(WorkflowError::GraphValidationError(format!(
                "Duplicate node ID: {}",
                node.id
            )));
        }
    }

    // 5. 检查边引用的节点是否存在
    let node_ids: std::collections::HashSet<_> =
        schema.nodes.iter().map(|n| n.id.as_str()).collect();

    for edge in &schema.edges {
        if !node_ids.contains(edge.source.as_str()) {
            return Err(WorkflowError::GraphValidationError(format!(
                "Edge source node not found: {}",
                edge.source
            )));
        }
        if !node_ids.contains(edge.target.as_str()) {
            return Err(WorkflowError::GraphValidationError(format!(
                "Edge target node not found: {}",
                edge.target
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::schema::*;
    use serde_json::json;

    fn make_node(id: &str, node_type: &str) -> NodeSchema {
        NodeSchema {
            id: id.to_string(),
            node_type: node_type.to_string(),
            data: json!({}),
            title: id.to_string(),
            position: None,
        }
    }

    fn make_edge(source: &str, target: &str) -> EdgeSchema {
        EdgeSchema {
            id: format!("{}_{}", source, target),
            source: source.to_string(),
            target: target.to_string(),
            source_handle: None,
        }
    }

    #[test]
    fn test_validate_valid_schema() {
        let schema = WorkflowSchema {
            nodes: vec![make_node("start", "start"), make_node("end", "end")],
            edges: vec![make_edge("start", "end")],
            variables: vec![],
            environment_variables: vec![],
            conversation_variables: vec![],
        };
        assert!(validate_workflow_schema(&schema).is_ok());
    }

    #[test]
    fn test_validate_no_start_node() {
        let schema = WorkflowSchema {
            nodes: vec![make_node("end", "end")],
            edges: vec![],
            variables: vec![],
            environment_variables: vec![],
            conversation_variables: vec![],
        };
        assert!(matches!(
            validate_workflow_schema(&schema),
            Err(WorkflowError::NoStartNode)
        ));
    }

    #[test]
    fn test_validate_no_end_node() {
        let schema = WorkflowSchema {
            nodes: vec![make_node("start", "start")],
            edges: vec![],
            variables: vec![],
            environment_variables: vec![],
            conversation_variables: vec![],
        };
        assert!(matches!(
            validate_workflow_schema(&schema),
            Err(WorkflowError::NoEndNode)
        ));
    }

    #[test]
    fn test_validate_duplicate_node_id() {
        let schema = WorkflowSchema {
            nodes: vec![
                make_node("start", "start"),
                make_node("start", "template"),
                make_node("end", "end"),
            ],
            edges: vec![],
            variables: vec![],
            environment_variables: vec![],
            conversation_variables: vec![],
        };
        assert!(matches!(
            validate_workflow_schema(&schema),
            Err(WorkflowError::GraphValidationError(_))
        ));
    }

    #[test]
    fn test_validate_edge_references_missing_node() {
        let schema = WorkflowSchema {
            nodes: vec![make_node("start", "start"), make_node("end", "end")],
            edges: vec![make_edge("start", "nonexistent")],
            variables: vec![],
            environment_variables: vec![],
            conversation_variables: vec![],
        };
        assert!(matches!(
            validate_workflow_schema(&schema),
            Err(WorkflowError::GraphValidationError(_))
        ));
    }
}
