use serde_json::Value;
use std::collections::HashMap;

use crate::dsl::schema::{WorkflowSchema, NodeSchema};
use crate::error::WorkflowError;
use super::types::{Graph, GraphNode, GraphEdge, NodeState};

/// Build a Graph from a validated WorkflowSchema
pub fn build_graph(schema: &WorkflowSchema) -> Result<Graph, WorkflowError> {
    let mut nodes = HashMap::new();
    let mut edges = HashMap::new();
    let mut in_edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut out_edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut root_node_id = String::new();

    // Build nodes
    for ns in &schema.nodes {
        let config = build_node_config(ns);
        let node = GraphNode {
            id: ns.id.clone(),
            node_type: ns.data.node_type.clone(),
            title: ns.data.title.clone(),
            config,
            version: ns.data.version.clone().unwrap_or_else(|| "1".to_string()),
            state: NodeState::Unknown,
        };

        if ns.data.node_type == "start" {
            root_node_id = ns.id.clone();
        }

        // Ensure in_edges/out_edges maps exist for all nodes
        in_edges.entry(ns.id.clone()).or_default();
        out_edges.entry(ns.id.clone()).or_default();

        nodes.insert(ns.id.clone(), node);
    }

    if root_node_id.is_empty() {
        return Err(WorkflowError::NoStartNode);
    }

    // Build edges
    for (idx, es) in schema.edges.iter().enumerate() {
        let edge_id = if es.id.is_empty() {
            format!("edge_{}", idx)
        } else {
            es.id.clone()
        };

        let edge = GraphEdge {
            id: edge_id.clone(),
            source_node_id: es.source.clone(),
            target_node_id: es.target.clone(),
            source_handle: es.source_handle.clone(),
            state: NodeState::Unknown,
        };

        in_edges.entry(es.target.clone()).or_default().push(edge_id.clone());
        out_edges.entry(es.source.clone()).or_default().push(edge_id.clone());

        edges.insert(edge_id, edge);
    }

    Ok(Graph {
        nodes,
        edges,
        in_edges,
        out_edges,
        root_node_id,
    })
}

/// Build the config JSON for a node from its schema
fn build_node_config(ns: &NodeSchema) -> Value {
    // Merge the extra fields into a single JSON object
    let mut config = serde_json::Map::new();
    config.insert("type".to_string(), Value::String(ns.data.node_type.clone()));
    config.insert("title".to_string(), Value::String(ns.data.title.clone()));
    for (k, v) in &ns.data.extra {
        config.insert(k.clone(), v.clone());
    }
    if let Some(es) = &ns.data.error_strategy {
        config.insert("error_strategy".to_string(), serde_json::to_value(es).unwrap_or(Value::Null));
    }
    if let Some(rc) = &ns.data.retry_config {
        config.insert("retry_config".to_string(), serde_json::to_value(rc).unwrap_or(Value::Null));
    }
    Value::Object(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{parse_dsl, DslFormat};

    #[test]
    fn test_build_simple_graph() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start_1
    data:
      type: start
      title: Start
  - id: end_1
    data:
      type: end
      title: End
edges:
  - source: start_1
    target: end_1
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.root_node_id, "start_1");
        assert!(graph.is_node_ready("start_1"));
    }

    #[test]
    fn test_build_branch_graph() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: if1
    data:
      type: if-else
      title: Check
      cases:
        - case_id: case1
          logical_operator: and
          conditions:
            - variable_selector: ["start", "x"]
              comparison_operator: greater_than
              value: 5
  - id: a
    data: { type: code, title: A }
  - id: b
    data: { type: code, title: B }
  - id: end
    data: { type: end, title: End }
edges:
  - source: start
    target: if1
  - source: if1
    target: a
    sourceHandle: case1
  - source: if1
    target: b
    sourceHandle: "false"
  - source: a
    target: end
  - source: b
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();
        assert_eq!(graph.nodes.len(), 5);
        assert!(graph.is_branch_node("if1"));
    }
}
