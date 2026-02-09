pub mod node_error;
pub mod workflow_error;

pub use node_error::NodeError;
pub use workflow_error::WorkflowError;

pub type WorkflowResult<T> = Result<T, WorkflowError>;
pub type NodeResult<T> = Result<T, NodeError>;
