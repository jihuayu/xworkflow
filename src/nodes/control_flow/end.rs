use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::execution_context::NodeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::EndNodeConfig;
use crate::error::NodeError;
use crate::nodes::{NodeExecutionResult, NodeExecutor};

/// 结束节点执行器
pub struct EndNodeExecutor;

#[async_trait]
impl NodeExecutor for EndNodeExecutor {
    fn validate(&self, _config: &Value) -> Result<(), NodeError> {
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &NodeContext,
        pool: &Arc<VariablePool>,
        _event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError> {
        // 尝试解析配置获取输出变量列表
        let config: Option<EndNodeConfig> = serde_json::from_value(ctx.config.clone()).ok();

        let mut outputs = serde_json::Map::new();

        if let Some(config) = config {
            for output_var in &config.outputs {
                if let Some(value) = pool.get_value(&output_var.variable_selector) {
                    outputs.insert(output_var.name.clone(), value);
                }
            }
        }

        // 如果没有定义输出变量，返回所有节点输出的汇总
        if outputs.is_empty() {
            // 返回空对象
            return Ok(NodeExecutionResult::Completed(Value::Object(outputs)));
        }

        Ok(NodeExecutionResult::Completed(Value::Object(outputs)))
    }

    fn node_type(&self) -> &str {
        "end"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event_bus::create_event_channel;
    use serde_json::json;

    #[tokio::test]
    async fn test_end_node_with_outputs() {
        let executor = EndNodeExecutor;
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("template1", json!({"output": "Hello World"}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "end".to_string(),
            node_type: "end".to_string(),
            config: json!({
                "outputs": [
                    {"name": "result", "variable_selector": "template1.output"}
                ]
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "End".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::Completed(value) => {
                assert_eq!(value["result"], "Hello World");
            }
            _ => panic!("Expected Completed"),
        }
    }

    #[tokio::test]
    async fn test_end_node_without_config() {
        let executor = EndNodeExecutor;
        let pool = Arc::new(VariablePool::new());
        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "end".to_string(),
            node_type: "end".to_string(),
            config: json!({}),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "End".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        assert!(matches!(result, NodeExecutionResult::Completed(_)));
    }
}
