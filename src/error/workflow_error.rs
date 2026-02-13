//! Workflow-level error types.

use super::NodeError;
use crate::dsl::validation::ValidationReport;
use thiserror::Error;

/// Workflow-level errors
#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("DSL parse error: {0}")]
    DslParseError(String),
    #[error("Unsupported DSL version: {found}, supported versions: {supported}")]
    UnsupportedVersion { found: String, supported: String },
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
    #[error("Workflow safe-stopped")]
    SafeStopped {
        last_completed_node: Option<String>,
        interrupted_nodes: Vec<String>,
        checkpoint_saved: bool,
    },
    #[error("Node execution error: node={node_id}, error={error}")]
    NodeExecutionError {
        node_id: String,
        error: String,
        error_detail: Option<serde_json::Value>,
    },
    #[cfg(feature = "checkpoint")]
    #[error("Resume rejected for workflow '{workflow_id}':\n{diagnostic}")]
    ResumeRejected {
        workflow_id: String,
        diagnostic: String,
        changes: Box<Vec<crate::core::checkpoint::EnvironmentChange>>,
    },
    #[error("Validation failed")]
    ValidationFailed(Box<ValidationReport>),
    #[error("Node error: {0}")]
    NodeError(Box<NodeError>),
    #[error("Internal error: {0}")]
    InternalError(String),
}

impl From<NodeError> for WorkflowError {
    fn from(value: NodeError) -> Self {
        WorkflowError::NodeError(Box::new(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_error_display() {
        assert_eq!(
            WorkflowError::DslParseError("x".into()).to_string(),
            "DSL parse error: x"
        );
        assert_eq!(
            WorkflowError::UnsupportedVersion {
                found: "1".into(),
                supported: "2".into()
            }
            .to_string(),
            "Unsupported DSL version: 1, supported versions: 2"
        );
        assert_eq!(
            WorkflowError::GraphBuildError("g".into()).to_string(),
            "Graph build error: g"
        );
        assert_eq!(
            WorkflowError::GraphValidationError("gv".into()).to_string(),
            "Graph validation error: gv"
        );
        assert_eq!(
            WorkflowError::NodeNotFound("n".into()).to_string(),
            "Node not found: n"
        );
        assert_eq!(
            WorkflowError::EdgeNotFound("e".into()).to_string(),
            "Edge not found: e"
        );
        assert_eq!(
            WorkflowError::ExecutorNotFound("ex".into()).to_string(),
            "Node executor not found for type: ex"
        );
        assert_eq!(WorkflowError::Timeout.to_string(), "Workflow timeout");
        assert_eq!(
            WorkflowError::ExecutionTimeout.to_string(),
            "Execution timeout"
        );
        assert_eq!(
            WorkflowError::MaxStepsExceeded(100).to_string(),
            "Max steps exceeded: 100"
        );
        assert_eq!(
            WorkflowError::OutputNotFound.to_string(),
            "Output not found"
        );
        assert_eq!(
            WorkflowError::NoStartNode.to_string(),
            "No start node found"
        );
        assert_eq!(WorkflowError::NoEndNode.to_string(), "No end node found");
        assert_eq!(
            WorkflowError::MultipleStartNodes.to_string(),
            "Multiple start nodes found"
        );
        assert_eq!(
            WorkflowError::CycleDetected.to_string(),
            "Cycle detected in graph"
        );
        assert_eq!(
            WorkflowError::Aborted("reason".into()).to_string(),
            "Workflow aborted: reason"
        );
        assert_eq!(
            WorkflowError::InternalError("ie".into()).to_string(),
            "Internal error: ie"
        );
    }

    #[test]
    fn test_workflow_error_node_execution_error() {
        let err = WorkflowError::NodeExecutionError {
            node_id: "node1".into(),
            error: "failed".into(),
            error_detail: Some(serde_json::json!({"key": "val"})),
        };
        let msg = err.to_string();
        assert!(msg.contains("node1"));
        assert!(msg.contains("failed"));
    }

    #[test]
    fn test_workflow_error_node_execution_error_no_detail() {
        let err = WorkflowError::NodeExecutionError {
            node_id: "n2".into(),
            error: "err".into(),
            error_detail: None,
        };
        assert!(err.to_string().contains("n2"));
    }

    #[test]
    fn test_workflow_error_from_node_error() {
        let node_err = NodeError::Timeout;
        let wf_err: WorkflowError = node_err.into();
        assert!(matches!(wf_err, WorkflowError::NodeError(_)));
        assert!(wf_err.to_string().contains("Timeout"));
    }

    #[test]
    fn test_workflow_error_validation_failed() {
        let report = ValidationReport {
            is_valid: true,
            diagnostics: vec![],
        };
        let err = WorkflowError::ValidationFailed(Box::new(report));
        assert_eq!(err.to_string(), "Validation failed");
    }
}
