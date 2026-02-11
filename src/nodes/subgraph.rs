use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::runtime_context::RuntimeContext;
use crate::core::sub_graph_runner::DefaultSubGraphRunner;
use crate::core::variable_pool::VariablePool;

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
        let runner = context
            .sub_graph_runner()
            .cloned()
            .unwrap_or_else(|| Arc::new(DefaultSubGraphRunner));

        runner
            .run_sub_graph(sub_graph, parent_pool, scope_vars, context)
            .await
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::{Segment, Selector};
    use serde_json::json;

    fn make_pool() -> VariablePool {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("input", "value"), Segment::Integer(1));
        pool
    }

    #[cfg(all(feature = "builtin-core-nodes", feature = "builtin-subgraph-nodes"))]
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
