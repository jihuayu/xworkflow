use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::execution_context::NodeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::VariableAggregatorNodeConfig;
use crate::error::NodeError;
use crate::nodes::{NodeExecutionResult, NodeExecutor};

/// 变量聚合器节点执行器
pub struct VariableAggregatorNodeExecutor;

#[async_trait]
impl NodeExecutor for VariableAggregatorNodeExecutor {
    fn validate(&self, _config: &Value) -> Result<(), NodeError> {
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &NodeContext,
        pool: &Arc<VariablePool>,
        _event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError> {
        let config: Option<VariableAggregatorNodeConfig> =
            serde_json::from_value(ctx.config.clone()).ok();

        if let Some(config) = config {
            let mut aggregated = Vec::new();

            for selector in &config.variables {
                if let Some(value) = pool.get_value(selector) {
                    aggregated.push(value);
                }
            }

            let output = serde_json::json!({
                config.output_variable: aggregated,
            });

            Ok(NodeExecutionResult::Completed(output))
        } else {
            Ok(NodeExecutionResult::Completed(serde_json::json!({})))
        }
    }

    fn node_type(&self) -> &str {
        "variable-aggregator"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event_bus::create_event_channel;
    use serde_json::json;

    #[tokio::test]
    async fn test_variable_aggregator() {
        let executor = VariableAggregatorNodeExecutor;
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("branch_a", json!({"text": "hello"}));
        pool.set_node_output("branch_b", json!({"text": "world"}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "agg1".to_string(),
            node_type: "variable-aggregator".to_string(),
            config: json!({
                "variables": ["branch_a.text", "branch_b.text"],
                "output_variable": "merged"
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "Aggregator".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::Completed(value) => {
                let merged = value["merged"].as_array().unwrap();
                assert_eq!(merged.len(), 2);
                assert_eq!(merged[0], "hello");
                assert_eq!(merged[1], "world");
            }
            _ => panic!("Expected Completed"),
        }
    }
}
