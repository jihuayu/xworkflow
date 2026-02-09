use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::execution_context::NodeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::VariableAssignerNodeConfig;
use crate::error::NodeError;
use crate::nodes::{NodeExecutionResult, NodeExecutor};

/// 变量赋值节点执行器
pub struct VariableAssignerNodeExecutor;

#[async_trait]
impl NodeExecutor for VariableAssignerNodeExecutor {
    fn validate(&self, _config: &Value) -> Result<(), NodeError> {
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &NodeContext,
        pool: &Arc<VariablePool>,
        _event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError> {
        let config: Option<VariableAssignerNodeConfig> =
            serde_json::from_value(ctx.config.clone()).ok();

        let mut outputs = serde_json::Map::new();

        if let Some(config) = config {
            for assignment in &config.assignments {
                let value = if assignment.from_variable {
                    // 从变量池获取值
                    if let Some(selector) = &assignment.variable_selector {
                        pool.get_value(selector).unwrap_or(Value::Null)
                    } else {
                        assignment.value.clone()
                    }
                } else {
                    assignment.value.clone()
                };

                // 写入对话变量
                pool.set_conversation_var(&assignment.variable, value.clone());
                outputs.insert(assignment.variable.clone(), value);
            }
        }

        Ok(NodeExecutionResult::Completed(Value::Object(outputs)))
    }

    fn node_type(&self) -> &str {
        "variable-assigner"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event_bus::create_event_channel;
    use serde_json::json;

    #[tokio::test]
    async fn test_variable_assigner() {
        let executor = VariableAssignerNodeExecutor;
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("node1", json!({"text": "hello"}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "assigner1".to_string(),
            node_type: "variable-assigner".to_string(),
            config: json!({
                "assignments": [
                    {
                        "variable": "greeting",
                        "value": null,
                        "from_variable": true,
                        "variable_selector": "node1.text"
                    }
                ]
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "Assigner".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::Completed(value) => {
                assert_eq!(value["greeting"], "hello");
            }
            _ => panic!("Expected Completed"),
        }

        // 验证对话变量已设置
        assert_eq!(pool.get_value("greeting"), Some(json!("hello")));
    }
}
