use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::execution_context::NodeContext;
use crate::core::variable_pool::VariablePool;
use crate::error::NodeError;
use crate::nodes::{NodeExecutionResult, NodeExecutor};

/// 代码执行节点执行器
///
/// 注意：代码执行需要通过外部沙箱服务，这里是 stub 实现。
/// 真实环境中应该通过 gRPC/HTTP 调用沙箱服务。
pub struct CodeNodeExecutor;

#[async_trait]
impl NodeExecutor for CodeNodeExecutor {
    fn validate(&self, _config: &Value) -> Result<(), NodeError> {
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &NodeContext,
        _pool: &Arc<VariablePool>,
        _event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError> {
        // Stub 实现 - 真实环境应调用沙箱服务
        tracing::warn!(
            node_id = %ctx.node_id,
            "Code node execution is stubbed - requires external sandbox service"
        );

        Ok(NodeExecutionResult::Completed(serde_json::json!({
            "output": null,
            "stdout": "",
            "stderr": "Code execution requires sandbox service",
            "execution_time": 0.0,
        })))
    }

    fn node_type(&self) -> &str {
        "code"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event_bus::create_event_channel;
    use serde_json::json;

    #[tokio::test]
    async fn test_code_node_stub() {
        let executor = CodeNodeExecutor;
        let pool = Arc::new(VariablePool::new());
        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "code1".to_string(),
            node_type: "code".to_string(),
            config: json!({
                "language": "python",
                "code": "return 42",
                "output_variable": "result"
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "Code".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        assert!(matches!(result, NodeExecutionResult::Completed(_)));
    }
}
