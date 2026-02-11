//! Error types for the workflow engine.
//!
//! - [`NodeError`] — Errors raised during individual node execution.
//! - [`WorkflowError`] — Top-level errors for workflow parsing, building, and running.
//! - [`ErrorContext`] — Structured error metadata (code, retryability, severity).

pub mod node_error;
pub mod error_context;
pub mod workflow_error;

pub use node_error::NodeError;
pub use error_context::{ErrorCode, ErrorContext, ErrorRetryability, ErrorSeverity};
pub use workflow_error::WorkflowError;

/// Convenience alias for workflow-level results.
pub type WorkflowResult<T> = Result<T, WorkflowError>;
/// Convenience alias for node-level results.
pub type NodeResult<T> = Result<T, NodeError>;
