use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::core::dispatcher::{EngineConfig, WorkflowDispatcher};
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::graph::types::{Graph, GraphEdge, GraphNode, NodeState};
use crate::nodes::executor::NodeExecutorRegistry;
use crate::nodes::utils::selector_from_str;

/// Sub-graph definition
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubGraphDefinition {
    pub nodes: Vec<SubGraphNode>,
    pub edges: Vec<SubGraphEdge>,
}

/// Sub-graph node
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubGraphNode {
    pub id: String,
    #[serde(rename = "type", default)]
    pub node_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub data: Value,
}

/// Sub-graph edge
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubGraphEdge {
    #[serde(default)]
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, alias = "sourceHandle", alias = "source_handle")]
    pub source_handle: Option<String>,
}

/// Sub-graph errors
#[derive(Debug, thiserror::Error)]
pub enum SubGraphError {
    #[error("Invalid edge reference")]
    InvalidEdge,

    #[error("No start node found in sub-graph")]
    NoStartNode,

    #[error("No end node found in sub-graph")]
    NoEndNode,

    #[error("Sub-graph execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Max iterations exceeded: {0}")]
    MaxIterationsExceeded(usize),

    #[error("Scope error: {0}")]
    ScopeError(String),
}

/// Sub-graph executor
pub struct SubGraphExecutor;

impl SubGraphExecutor {
    pub fn new() -> Self {
        Self
    }

    /// Execute a sub-graph with scoped variables
    pub async fn execute(
        &self,
        sub_graph: &SubGraphDefinition,
        parent_pool: &VariablePool,
        scope_vars: HashMap<String, Value>,
        context: &RuntimeContext,
    ) -> Result<Value, SubGraphError> {
        let mut scoped_pool = parent_pool.clone();
        inject_scope_vars(&mut scoped_pool, scope_vars)?;

        let graph = build_sub_graph(sub_graph)?;
        let registry = NodeExecutorRegistry::new();
        let (tx, _rx) = mpsc::channel(16);

        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            scoped_pool,
            registry,
            tx,
            EngineConfig::default(),
            std::sync::Arc::new(context.clone()),
            None,
        );

        let outputs = dispatcher
            .run()
            .await
            .map_err(|e| SubGraphError::ExecutionFailed(e.to_string()))?;

        Ok(Value::Object(outputs.into_iter().collect()))
    }
}

fn inject_scope_vars(
    pool: &mut VariablePool,
    scope_vars: HashMap<String, Value>,
) -> Result<(), SubGraphError> {
    for (key, value) in scope_vars {
        let selector = if let Some(sel) = selector_from_str(&key) {
            sel
        } else {
            return Err(SubGraphError::ScopeError(format!(
                "Invalid scope key: {}",
                key
            )));
        };

        if selector.len() < 2 {
            return Err(SubGraphError::ScopeError(format!(
                "Invalid scope selector: {}",
                key
            )));
        }
        pool.set(&selector, Segment::from_value(&value));
    }

    Ok(())
}

fn resolve_node_type(node: &SubGraphNode) -> Option<String> {
    if let Some(nt) = &node.node_type {
        return Some(map_node_type_alias(nt));
    }
    if let Value::Object(map) = &node.data {
        if let Some(Value::String(t)) = map.get("type") {
            return Some(map_node_type_alias(t));
        }
    }
    None
}

fn map_node_type_alias(node_type: &str) -> String {
    match node_type {
        "template" => "template-transform".to_string(),
        other => other.to_string(),
    }
}

fn build_sub_graph(sub_graph: &SubGraphDefinition) -> Result<Graph, SubGraphError> {
    let mut nodes = HashMap::new();
    let mut edges = HashMap::new();
    let mut in_edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut out_edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut root_node_id = String::new();

    for (idx, n) in sub_graph.nodes.iter().enumerate() {
        let node_type = resolve_node_type(n).ok_or_else(|| {
            SubGraphError::ExecutionFailed("Sub-graph node type is missing".to_string())
        })?;
        let mut config = serde_json::Map::new();
        config.insert("type".to_string(), Value::String(node_type.clone()));
        if let Some(title) = &n.title {
            config.insert("title".to_string(), Value::String(title.clone()));
        }
        if let Value::Object(map) = &n.data {
            for (k, v) in map {
                if k == "type" {
                    continue;
                }
                config.insert(k.clone(), v.clone());
            }
        }

        let node = GraphNode {
            id: n.id.clone(),
            node_type: node_type.clone(),
            title: n.title.clone().unwrap_or_else(|| format!("node_{}", idx)),
            config: Value::Object(config),
            version: "1".to_string(),
            state: NodeState::Unknown,
        };

        if node_type == "start" {
            root_node_id = n.id.clone();
        }

        in_edges.entry(n.id.clone()).or_default();
        out_edges.entry(n.id.clone()).or_default();
        nodes.insert(n.id.clone(), node);
    }

    if root_node_id.is_empty() {
        return Err(SubGraphError::NoStartNode);
    }

    let mut has_end = false;
    for n in sub_graph.nodes.iter() {
        if let Some(nt) = resolve_node_type(n) {
            if nt == "end" {
                has_end = true;
                break;
            }
        }
    }
    if !has_end {
        return Err(SubGraphError::NoEndNode);
    }

    for (idx, e) in sub_graph.edges.iter().enumerate() {
        let edge_id = if e.id.is_empty() {
            format!("edge_{}", idx)
        } else {
            e.id.clone()
        };
        if !nodes.contains_key(&e.source) || !nodes.contains_key(&e.target) {
            return Err(SubGraphError::InvalidEdge);
        }
        let edge = GraphEdge {
            id: edge_id.clone(),
            source_node_id: e.source.clone(),
            target_node_id: e.target.clone(),
            source_handle: e.source_handle.clone(),
            state: NodeState::Unknown,
        };

        in_edges.entry(e.target.clone()).or_default().push(edge_id.clone());
        out_edges.entry(e.source.clone()).or_default().push(edge_id.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_pool() -> VariablePool {
        let mut pool = VariablePool::new();
        pool.set(
            &["input".to_string(), "value".to_string()],
            Segment::Integer(1),
        );
        pool
    }

    #[tokio::test]
    async fn test_execute_simple_sub_graph() {
        let sub_graph = SubGraphDefinition {
            nodes: vec![
                SubGraphNode {
                    id: "start".to_string(),
                    node_type: Some("start".to_string()),
                    title: None,
                    data: json!({}),
                },
                SubGraphNode {
                    id: "end".to_string(),
                    node_type: Some("end".to_string()),
                    title: None,
                    data: json!({
                        "outputs": [
                            {"variable": "value", "value_selector": ["__scope__", "item"]}
                        ]
                    }),
                },
            ],
            edges: vec![SubGraphEdge {
                id: "e1".to_string(),
                source: "start".to_string(),
                target: "end".to_string(),
                source_handle: None,
            }],
        };

        let mut scope = HashMap::new();
        scope.insert("item".to_string(), json!(42));

        let executor = SubGraphExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute(&sub_graph, &make_pool(), scope, &context).await.unwrap();
        assert_eq!(result.get("value"), Some(&json!(42)));
    }
}
