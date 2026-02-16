//! Public API layer — stable entry points for external consumers.
//!
//! This module provides the primary public interface for the XWorkflow engine.
//! All externally visible types are re-exported here.

mod handle;
mod runner;

pub use handle::WorkflowHandle;
pub use runner::{WorkflowRunner, WorkflowRunnerBuilder};
