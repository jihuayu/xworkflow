use thiserror::Error;
use super::NodeError;
use crate::dsl::validation::ValidationReport;

/// Workflow-level errors
#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("DSL parse error: {0}")]
    DslParseError(String),
    #[error("Unsupported DSL version: {found}, supported versions: {supported}")]
    UnsupportedVersion {
        found: String,
        supported: String,
    },
    #[error("Graph build error: {0}")]
    GraphBuildError(String),
    #[error("Graph validation error: {0}")]
    GraphValidationError(String),
    #[error("Node not found: {0}")]
    NodeNotFound(String),
    #[error("Edge not found: {0}")]
    EdgeNotFound(String),
    #[error("Node executor not found for type: {0}")]
    ExecutorNotFound(String),
    #[error("Workflow timeout")]
    Timeout,
    #[error("Execution timeout")]
    ExecutionTimeout,
    #[error("Max steps exceeded: {0}")]
    MaxStepsExceeded(i32),
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
    #[error("Workflow aborted: {0}")]
    Aborted(String),
    #[error("Node execution error: node={node_id}, error={error}")]
    NodeExecutionError {
        node_id: String,
        error: String,
        error_detail: Option<serde_json::Value>,
    },
    #[error("Validation failed")]
    ValidationFailed(ValidationReport),
    #[error("Node error: {0}")]
    NodeError(#[from] NodeError),
    #[error("Internal error: {0}")]
    InternalError(String),
}
