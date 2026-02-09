pub mod control_flow;
pub mod data_transform;
pub mod registry;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::variable_pool::VariablePool;
use crate::core::execution_context::NodeContext;
use crate::error::NodeError;

pub use registry::NodeRegistry;

/// 节点执行结果
#[derive(Debug, Clone)]
pub enum NodeExecutionResult {
    /// 正常完成，返回输出数据
    Completed(Value),

    /// 分支选择（用于 If/Else 节点）
    BranchSelected(String),

    /// 迭代完成（用于 Iteration 节点）
    IterationCompleted(Vec<Value>),

    /// 执行失败
    Failed(String),

    /// 节点被跳过
    Skipped,
}

/// 节点执行器 Trait - 所有节点类型必须实现此接口
#[async_trait]
pub trait NodeExecutor: Send + Sync {
    /// 静态验证：在工作流启动前检查配置是否合法
    fn validate(&self, config: &Value) -> Result<(), NodeError>;

    /// 执行节点逻辑
    async fn execute(
        &self,
        ctx: &NodeContext,
        pool: &Arc<VariablePool>,
        event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError>;

    /// 获取节点类型名称
    fn node_type(&self) -> &str;
}
