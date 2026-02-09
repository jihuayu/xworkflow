use thiserror::Error;

use super::NodeError;

/// 工作流级错误
#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("DSL parse error: {0}")]
    DslParseError(String),

    #[error("Graph build error: {0}")]
    GraphBuildError(String),

    #[error("Graph validation error: {0}")]
    GraphValidationError(String),

    #[error("Node not found: {0}")]
    NodeNotFound(String),

    #[error("Node executor not found for type: {0}")]
    ExecutorNotFound(String),

    #[error("Node execution failed: node={0}, error={1}")]
    NodeExecutionFailed(String, String),

    #[error("Workflow timeout")]
    Timeout,

    #[error("Output not found")]
    OutputNotFound,

    #[error("No start node found")]
    NoStartNode,

    #[error("No end node found")]
    NoEndNode,

    #[error("Multiple start nodes found")]
    MultipleStartNodes,

    #[error("Cycle detected in graph")]
    CycleDetected,

    #[error("Node error: {0}")]
    NodeError(#[from] NodeError),

    #[error("Internal error: {0}")]
    InternalError(String),
}
