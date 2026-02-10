use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::dsl::schema::NodeRunResult;
use crate::core::variable_pool::Selector;

/// Reason for pause
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseReason {
    pub node_id: String,
    pub reason: String,
}

/// All events emitted by the graph engine (Dify-compatible)
#[derive(Debug, Clone)]
pub enum GraphEngineEvent {
    // === Graph-level events ===
    GraphRunStarted,
    GraphRunSucceeded {
        outputs: HashMap<String, Value>,
    },
    GraphRunFailed {
        error: String,
        exceptions_count: i32,
    },
    GraphRunPartialSucceeded {
        exceptions_count: i32,
        outputs: HashMap<String, Value>,
    },
    GraphRunAborted {
        reason: Option<String>,
        outputs: HashMap<String, Value>,
    },

    // === Node-level events ===
    NodeRunStarted {
        id: String,
        node_id: String,
        node_type: String,
        node_title: String,
        predecessor_node_id: Option<String>,
    },
    NodeRunSucceeded {
        id: String,
        node_id: String,
        node_type: String,
        node_run_result: NodeRunResult,
    },
    NodeRunFailed {
        id: String,
        node_id: String,
        node_type: String,
        node_run_result: NodeRunResult,
        error: String,
    },
    NodeRunException {
        id: String,
        node_id: String,
        node_type: String,
        node_run_result: NodeRunResult,
        error: String,
    },
    NodeRunStreamChunk {
        id: String,
        node_id: String,
        node_type: String,
        chunk: String,
        selector: Selector,
        is_final: bool,
    },
    NodeRunRetry {
        id: String,
        node_id: String,
        node_type: String,
        node_title: String,
        error: String,
        retry_index: i32,
    },

    // === Error Handler events ===
    ErrorHandlerStarted {
        error: String,
    },
    ErrorHandlerSucceeded {
        outputs: HashMap<String, Value>,
    },
    ErrorHandlerFailed {
        error: String,
    },

    // === Iteration events ===
    IterationStarted {
        node_id: String,
        node_title: String,
        inputs: HashMap<String, Value>,
    },
    IterationNext {
        node_id: String,
        node_title: String,
        index: i32,
    },
    IterationSucceeded {
        node_id: String,
        node_title: String,
        outputs: HashMap<String, Value>,
        steps: i32,
    },
    IterationFailed {
        node_id: String,
        node_title: String,
        error: String,
        steps: i32,
    },

    // === Loop events ===
    LoopStarted {
        node_id: String,
        node_title: String,
        inputs: HashMap<String, Value>,
    },
    LoopNext {
        node_id: String,
        node_title: String,
        index: i32,
    },
    LoopSucceeded {
        node_id: String,
        node_title: String,
        outputs: HashMap<String, Value>,
        steps: i32,
    },
    LoopFailed {
        node_id: String,
        node_title: String,
        error: String,
        steps: i32,
    },

    // === Plugin events ===
    PluginLoaded {
        plugin_id: String,
        name: String,
    },
    PluginUnloaded {
        plugin_id: String,
    },
    PluginEvent {
        plugin_id: String,
        event_type: String,
        data: Value,
    },
    PluginError {
        plugin_id: String,
        error: String,
    },

    // === Debug events ===
    DebugPaused {
        reason: String,
        node_id: String,
        node_type: String,
        node_title: String,
        phase: String,
    },
    DebugResumed,
    DebugBreakpointChanged {
        action: String,
        node_id: Option<String>,
    },
    DebugVariableSnapshot {
        data: Value,
    },
}

impl GraphEngineEvent {
    /// Serialize event to JSON for Python interop
    pub fn to_json(&self) -> Value {
        match self {
            GraphEngineEvent::GraphRunStarted => serde_json::json!({
                "type": "graph_run_started"
            }),
            GraphEngineEvent::GraphRunSucceeded { outputs } => serde_json::json!({
                "type": "graph_run_succeeded",
                "data": { "outputs": outputs }
            }),
            GraphEngineEvent::GraphRunFailed { error, exceptions_count } => serde_json::json!({
                "type": "graph_run_failed",
                "data": { "error": error, "exceptions_count": exceptions_count }
            }),
            GraphEngineEvent::GraphRunPartialSucceeded { exceptions_count, outputs } => serde_json::json!({
                "type": "graph_run_partial_succeeded",
                "data": { "exceptions_count": exceptions_count, "outputs": outputs }
            }),
            GraphEngineEvent::GraphRunAborted { reason, outputs } => serde_json::json!({
                "type": "graph_run_aborted",
                "data": { "reason": reason, "outputs": outputs }
            }),
            GraphEngineEvent::NodeRunStarted { id, node_id, node_type, node_title, predecessor_node_id } => serde_json::json!({
                "type": "node_run_started",
                "data": { "id": id, "node_id": node_id, "node_type": node_type, "node_title": node_title, "predecessor_node_id": predecessor_node_id }
            }),
            GraphEngineEvent::NodeRunSucceeded { id, node_id, node_type, node_run_result } => serde_json::json!({
                "type": "node_run_succeeded",
                "data": { "id": id, "node_id": node_id, "node_type": node_type, "status": "succeeded", "outputs": node_run_result.outputs.ready() }
            }),
            GraphEngineEvent::NodeRunFailed { id, node_id, node_type, node_run_result, error } => serde_json::json!({
                "type": "node_run_failed",
                "data": {
                    "id": id,
                    "node_id": node_id,
                    "node_type": node_type,
                    "error": error,
                    "error_type": node_run_result.error.as_ref().and_then(|e| e.error_type.clone()),
                    "error_detail": node_run_result.error.as_ref().and_then(|e| e.detail.clone()),
                }
            }),
            GraphEngineEvent::NodeRunException { id, node_id, node_type, node_run_result, error } => serde_json::json!({
                "type": "node_run_exception",
                "data": {
                    "id": id,
                    "node_id": node_id,
                    "node_type": node_type,
                    "error": error,
                    "error_type": node_run_result.error.as_ref().and_then(|e| e.error_type.clone()),
                    "error_detail": node_run_result.error.as_ref().and_then(|e| e.detail.clone()),
                }
            }),
            GraphEngineEvent::NodeRunStreamChunk { id, node_id, node_type, chunk, selector, is_final } => serde_json::json!({
                "type": "node_run_stream_chunk",
                "data": { "id": id, "node_id": node_id, "node_type": node_type, "chunk": chunk, "selector": selector, "is_final": is_final }
            }),
            GraphEngineEvent::NodeRunRetry { id, node_id, node_type, node_title, error, retry_index } => serde_json::json!({
                "type": "node_run_retry",
                "data": { "id": id, "node_id": node_id, "node_type": node_type, "node_title": node_title, "error": error, "retry_index": retry_index }
            }),
            GraphEngineEvent::ErrorHandlerStarted { error } => serde_json::json!({
                "type": "error_handler_started",
                "data": { "error": error }
            }),
            GraphEngineEvent::ErrorHandlerSucceeded { outputs } => serde_json::json!({
                "type": "error_handler_succeeded",
                "data": { "outputs": outputs }
            }),
            GraphEngineEvent::ErrorHandlerFailed { error } => serde_json::json!({
                "type": "error_handler_failed",
                "data": { "error": error }
            }),
            GraphEngineEvent::IterationStarted { node_id, node_title, inputs } => serde_json::json!({
                "type": "iteration_started",
                "data": { "node_id": node_id, "node_title": node_title, "inputs": inputs }
            }),
            GraphEngineEvent::IterationNext { node_id, node_title, index } => serde_json::json!({
                "type": "iteration_next",
                "data": { "node_id": node_id, "node_title": node_title, "index": index }
            }),
            GraphEngineEvent::IterationSucceeded { node_id, node_title, outputs, steps } => serde_json::json!({
                "type": "iteration_succeeded",
                "data": { "node_id": node_id, "node_title": node_title, "outputs": outputs, "steps": steps }
            }),
            GraphEngineEvent::IterationFailed { node_id, node_title, error, steps } => serde_json::json!({
                "type": "iteration_failed",
                "data": { "node_id": node_id, "node_title": node_title, "error": error, "steps": steps }
            }),
            GraphEngineEvent::LoopStarted { node_id, node_title, inputs } => serde_json::json!({
                "type": "loop_started",
                "data": { "node_id": node_id, "node_title": node_title, "inputs": inputs }
            }),
            GraphEngineEvent::LoopNext { node_id, node_title, index } => serde_json::json!({
                "type": "loop_next",
                "data": { "node_id": node_id, "node_title": node_title, "index": index }
            }),
            GraphEngineEvent::LoopSucceeded { node_id, node_title, outputs, steps } => serde_json::json!({
                "type": "loop_succeeded",
                "data": { "node_id": node_id, "node_title": node_title, "outputs": outputs, "steps": steps }
            }),
            GraphEngineEvent::LoopFailed { node_id, node_title, error, steps } => serde_json::json!({
                "type": "loop_failed",
                "data": { "node_id": node_id, "node_title": node_title, "error": error, "steps": steps }
            }),
            GraphEngineEvent::PluginLoaded { plugin_id, name } => serde_json::json!({
                "type": "plugin_loaded",
                "data": { "plugin_id": plugin_id, "name": name }
            }),
            GraphEngineEvent::PluginUnloaded { plugin_id } => serde_json::json!({
                "type": "plugin_unloaded",
                "data": { "plugin_id": plugin_id }
            }),
            GraphEngineEvent::PluginEvent { plugin_id, event_type, data } => serde_json::json!({
                "type": "plugin_event",
                "data": { "plugin_id": plugin_id, "event_type": event_type, "data": data }
            }),
            GraphEngineEvent::PluginError { plugin_id, error } => serde_json::json!({
                "type": "plugin_error",
                "data": { "plugin_id": plugin_id, "error": error }
            }),
            GraphEngineEvent::DebugPaused { reason, node_id, node_type, node_title, phase } => serde_json::json!({
                "type": "debug_paused",
                "data": {
                    "reason": reason,
                    "node_id": node_id,
                    "node_type": node_type,
                    "node_title": node_title,
                    "phase": phase
                }
            }),
            GraphEngineEvent::DebugResumed => serde_json::json!({
                "type": "debug_resumed"
            }),
            GraphEngineEvent::DebugBreakpointChanged { action, node_id } => serde_json::json!({
                "type": "debug_breakpoint_changed",
                "data": { "action": action, "node_id": node_id }
            }),
            GraphEngineEvent::DebugVariableSnapshot { data } => serde_json::json!({
                "type": "debug_variable_snapshot",
                "data": { "data": data }
            }),
        }
    }
}
