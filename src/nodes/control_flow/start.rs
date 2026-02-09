use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::execution_context::NodeContext;
use crate::core::variable_pool::VariablePool;
use crate::error::NodeError;
use crate::nodes::{NodeExecutionResult, NodeExecutor};

/// 开始节点执行器
pub struct StartNodeExecutor;

#[async_trait]
impl NodeExecutor for StartNodeExecutor {
    fn validate(&self, _config: &Value) -> Result<(), NodeError> {
        // Start 节点不需要特殊配置验证
        Ok(())
    }

    async fn execute(
        &self,
        _ctx: &NodeContext,
        _pool: &Arc<VariablePool>,
        _event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError> {
        // Start 节点只需返回成功
        // 输入已在运行时初始化时写入变量池
        Ok(NodeExecutionResult::Completed(serde_json::json!({})))
    }

    fn node_type(&self) -> &str {
        "start"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event_bus::create_event_channel;

    #[tokio::test]
    async fn test_start_node() {
        let executor = StartNodeExecutor;
        let pool = Arc::new(VariablePool::new());
        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "start".to_string(),
            node_type: "start".to_string(),
            config: serde_json::json!({}),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "Start".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        assert!(matches!(result, NodeExecutionResult::Completed(_)));
    }
}
