//! Execution status — the canonical definition of workflow execution states.
//!
//! This type was previously defined in `scheduler.rs` and is now the single
//! source of truth for execution status across the engine.

use serde_json::Value;
use std::collections::HashMap;

/// Execution status of a workflow
#[derive(Debug, Clone)]
pub enum ExecutionStatus {
    Running,
    Completed(HashMap<String, Value>),
    Failed(String),
    FailedWithRecovery {
        original_error: String,
        recovered_outputs: HashMap<String, Value>,
    },
    WaitingForInput {
        node_id: String,
        resume_token: String,
        resume_mode: String,
        form_schema: Value,
        prompt_text: Option<String>,
        timeout_at: Option<i64>,
    },
    Paused {
        node_id: String,
        node_title: String,
        prompt: String,
    },
    SafeStopped {
        last_completed_node: Option<String>,
        interrupted_nodes: Vec<String>,
        checkpoint_saved: bool,
    },
}
