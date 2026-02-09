use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::execution_context::NodeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::{IfElseNodeConfig, LogicalOperator};
use crate::error::NodeError;
use crate::evaluator::ConditionEvaluator;
use crate::nodes::{NodeExecutionResult, NodeExecutor};

/// If/Else 条件分支节点执行器
pub struct IfElseNodeExecutor {
    evaluator: ConditionEvaluator,
}

impl IfElseNodeExecutor {
    pub fn new() -> Self {
        IfElseNodeExecutor {
            evaluator: ConditionEvaluator::new(),
        }
    }
}

impl Default for IfElseNodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeExecutor for IfElseNodeExecutor {
    fn validate(&self, config: &Value) -> Result<(), NodeError> {
        let _config: IfElseNodeConfig = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(format!("Invalid if-else config: {}", e)))?;
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &NodeContext,
        pool: &Arc<VariablePool>,
        _event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError> {
        let config: IfElseNodeConfig = serde_json::from_value(ctx.config.clone())
            .map_err(|e| NodeError::ConfigError(format!("Invalid if-else config: {}", e)))?;

        // 评估所有条件
        let mut condition_results = Vec::new();

        for condition in &config.conditions {
            let value = pool
                .get_value(&condition.variable_selector)
                .unwrap_or(Value::Null);

            let result = self.evaluator.evaluate(
                &value,
                &condition.comparison_operator,
                &condition.value,
            )?;

            condition_results.push(result);
        }

        // 根据逻辑运算符计算最终结果
        let final_result = match config.logical_operator {
            LogicalOperator::And => condition_results.iter().all(|&r| r),
            LogicalOperator::Or => condition_results.iter().any(|&r| r),
        };

        // 返回选中的分支
        let selected_branch = if final_result { "true" } else { "false" };
        Ok(NodeExecutionResult::BranchSelected(
            selected_branch.to_string(),
        ))
    }

    fn node_type(&self) -> &str {
        "if-else"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event_bus::create_event_channel;
    use serde_json::json;

    #[tokio::test]
    async fn test_if_else_true() {
        let executor = IfElseNodeExecutor::new();
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("input", json!({"score": 75}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "if1".to_string(),
            node_type: "if-else".to_string(),
            config: json!({
                "conditions": [
                    {
                        "variable_selector": "input.score",
                        "comparison_operator": "greater_than",
                        "value": 60
                    }
                ],
                "logical_operator": "and"
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "If Score > 60".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::BranchSelected(branch) => {
                assert_eq!(branch, "true");
            }
            _ => panic!("Expected BranchSelected"),
        }
    }

    #[tokio::test]
    async fn test_if_else_false() {
        let executor = IfElseNodeExecutor::new();
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("input", json!({"score": 40}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "if1".to_string(),
            node_type: "if-else".to_string(),
            config: json!({
                "conditions": [
                    {
                        "variable_selector": "input.score",
                        "comparison_operator": "greater_than",
                        "value": 60
                    }
                ],
                "logical_operator": "and"
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "If Score > 60".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::BranchSelected(branch) => {
                assert_eq!(branch, "false");
            }
            _ => panic!("Expected BranchSelected"),
        }
    }

    #[tokio::test]
    async fn test_if_else_and_operator() {
        let executor = IfElseNodeExecutor::new();
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("input", json!({"score": 75, "name": "Alice"}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "if1".to_string(),
            node_type: "if-else".to_string(),
            config: json!({
                "conditions": [
                    {
                        "variable_selector": "input.score",
                        "comparison_operator": "greater_than",
                        "value": 60
                    },
                    {
                        "variable_selector": "input.name",
                        "comparison_operator": "equal",
                        "value": "Alice"
                    }
                ],
                "logical_operator": "and"
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "And condition".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::BranchSelected(branch) => {
                assert_eq!(branch, "true");
            }
            _ => panic!("Expected BranchSelected"),
        }
    }

    #[tokio::test]
    async fn test_if_else_or_operator() {
        let executor = IfElseNodeExecutor::new();
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("input", json!({"score": 40, "name": "Alice"}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "if1".to_string(),
            node_type: "if-else".to_string(),
            config: json!({
                "conditions": [
                    {
                        "variable_selector": "input.score",
                        "comparison_operator": "greater_than",
                        "value": 60
                    },
                    {
                        "variable_selector": "input.name",
                        "comparison_operator": "equal",
                        "value": "Alice"
                    }
                ],
                "logical_operator": "or"
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "Or condition".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::BranchSelected(branch) => {
                // score < 60 (false) OR name == Alice (true) => true
                assert_eq!(branch, "true");
            }
            _ => panic!("Expected BranchSelected"),
        }
    }
}
