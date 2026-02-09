pub mod node_error;
pub mod workflow_error;

pub use node_error::NodeError;
pub use workflow_error::WorkflowError;

/// 通用 Result 类型
pub type WorkflowResult<T> = Result<T, WorkflowError>;
pub type NodeResult<T> = Result<T, NodeError>;
