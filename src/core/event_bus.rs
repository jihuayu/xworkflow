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

/// All events emitted by the graph engine
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pause_reason_serde() {
        let pr = PauseReason {
            node_id: "n1".into(),
            reason: "breakpoint".into(),
        };
        let json = serde_json::to_string(&pr).unwrap();
        let de: PauseReason = serde_json::from_str(&json).unwrap();
        assert_eq!(de.node_id, "n1");
        assert_eq!(de.reason, "breakpoint");
    }

    #[test]
    fn test_graph_run_started() {
        let e = GraphEngineEvent::GraphRunStarted;
        let j = e.to_json();
        assert_eq!(j["type"], "graph_run_started");
    }

    #[test]
    fn test_graph_run_succeeded() {
        let mut outputs = HashMap::new();
        outputs.insert("key".into(), Value::String("val".into()));
        let e = GraphEngineEvent::GraphRunSucceeded { outputs };
        let j = e.to_json();
        assert_eq!(j["type"], "graph_run_succeeded");
        assert_eq!(j["data"]["outputs"]["key"], "val");
    }

    #[test]
    fn test_graph_run_failed() {
        let e = GraphEngineEvent::GraphRunFailed {
            error: "boom".into(),
            exceptions_count: 2,
        };
        let j = e.to_json();
        assert_eq!(j["type"], "graph_run_failed");
        assert_eq!(j["data"]["error"], "boom");
        assert_eq!(j["data"]["exceptions_count"], 2);
    }

    #[test]
    fn test_graph_run_partial_succeeded() {
        let e = GraphEngineEvent::GraphRunPartialSucceeded {
            exceptions_count: 1,
            outputs: HashMap::new(),
        };
        let j = e.to_json();
        assert_eq!(j["type"], "graph_run_partial_succeeded");
        assert_eq!(j["data"]["exceptions_count"], 1);
    }

    #[test]
    fn test_graph_run_aborted() {
        let e = GraphEngineEvent::GraphRunAborted {
            reason: Some("timeout".into()),
            outputs: HashMap::new(),
        };
        let j = e.to_json();
        assert_eq!(j["type"], "graph_run_aborted");
        assert_eq!(j["data"]["reason"], "timeout");
    }

    #[test]
    fn test_node_run_started() {
        let e = GraphEngineEvent::NodeRunStarted {
            id: "r1".into(),
            node_id: "n1".into(),
            node_type: "code".into(),
            node_title: "Code Node".into(),
            predecessor_node_id: Some("n0".into()),
        };
        let j = e.to_json();
        assert_eq!(j["type"], "node_run_started");
        assert_eq!(j["data"]["node_id"], "n1");
        assert_eq!(j["data"]["predecessor_node_id"], "n0");
    }

    #[test]
    fn test_node_run_succeeded() {
        let e = GraphEngineEvent::NodeRunSucceeded {
            id: "r1".into(),
            node_id: "n1".into(),
            node_type: "code".into(),
            node_run_result: NodeRunResult::default(),
        };
        let j = e.to_json();
        assert_eq!(j["type"], "node_run_succeeded");
        assert_eq!(j["data"]["status"], "succeeded");
    }

    #[test]
    fn test_node_run_failed() {
        let result = NodeRunResult {
            error: Some(crate::dsl::schema::NodeErrorInfo {
                message: "err".into(),
                error_type: Some("RuntimeError".into()),
                detail: Some(Value::String("detail".into())),
            }),
            ..Default::default()
        };
        let e = GraphEngineEvent::NodeRunFailed {
            id: "r1".into(),
            node_id: "n1".into(),
            node_type: "code".into(),
            node_run_result: result,
            error: "err".into(),
        };
        let j = e.to_json();
        assert_eq!(j["type"], "node_run_failed");
        assert_eq!(j["data"]["error_type"], "RuntimeError");
    }

    #[test]
    fn test_node_run_exception() {
        let e = GraphEngineEvent::NodeRunException {
            id: "r1".into(),
            node_id: "n1".into(),
            node_type: "code".into(),
            node_run_result: NodeRunResult::default(),
            error: "exception".into(),
        };
        let j = e.to_json();
        assert_eq!(j["type"], "node_run_exception");
    }

    #[test]
    fn test_node_run_stream_chunk() {
        let e = GraphEngineEvent::NodeRunStreamChunk {
            id: "r1".into(),
            node_id: "n1".into(),
            node_type: "answer".into(),
            chunk: "hello".into(),
            selector: Selector::new("n1", "text"),
            is_final: false,
        };
        let j = e.to_json();
        assert_eq!(j["type"], "node_run_stream_chunk");
        assert_eq!(j["data"]["chunk"], "hello");
        assert_eq!(j["data"]["is_final"], false);
    }

    #[test]
    fn test_node_run_retry() {
        let e = GraphEngineEvent::NodeRunRetry {
            id: "r1".into(),
            node_id: "n1".into(),
            node_type: "http-request".into(),
            node_title: "HTTP".into(),
            error: "timeout".into(),
            retry_index: 2,
        };
        let j = e.to_json();
        assert_eq!(j["type"], "node_run_retry");
        assert_eq!(j["data"]["retry_index"], 2);
    }

    #[test]
    fn test_error_handler_events() {
        let e = GraphEngineEvent::ErrorHandlerStarted { error: "e".into() };
        assert_eq!(e.to_json()["type"], "error_handler_started");

        let e = GraphEngineEvent::ErrorHandlerSucceeded { outputs: HashMap::new() };
        assert_eq!(e.to_json()["type"], "error_handler_succeeded");

        let e = GraphEngineEvent::ErrorHandlerFailed { error: "e".into() };
        assert_eq!(e.to_json()["type"], "error_handler_failed");
    }

    #[test]
    fn test_iteration_events() {
        let e = GraphEngineEvent::IterationStarted {
            node_id: "n1".into(),
            node_title: "Iter".into(),
            inputs: HashMap::new(),
        };
        assert_eq!(e.to_json()["type"], "iteration_started");

        let e = GraphEngineEvent::IterationNext {
            node_id: "n1".into(),
            node_title: "Iter".into(),
            index: 3,
        };
        let j = e.to_json();
        assert_eq!(j["type"], "iteration_next");
        assert_eq!(j["data"]["index"], 3);

        let e = GraphEngineEvent::IterationSucceeded {
            node_id: "n1".into(),
            node_title: "Iter".into(),
            outputs: HashMap::new(),
            steps: 5,
        };
        assert_eq!(e.to_json()["data"]["steps"], 5);

        let e = GraphEngineEvent::IterationFailed {
            node_id: "n1".into(),
            node_title: "Iter".into(),
            error: "fail".into(),
            steps: 2,
        };
        assert_eq!(e.to_json()["type"], "iteration_failed");
    }

    #[test]
    fn test_loop_events() {
        let e = GraphEngineEvent::LoopStarted {
            node_id: "n1".into(),
            node_title: "L".into(),
            inputs: HashMap::new(),
        };
        assert_eq!(e.to_json()["type"], "loop_started");

        let e = GraphEngineEvent::LoopNext {
            node_id: "n1".into(),
            node_title: "L".into(),
            index: 1,
        };
        assert_eq!(e.to_json()["type"], "loop_next");

        let e = GraphEngineEvent::LoopSucceeded {
            node_id: "n1".into(),
            node_title: "L".into(),
            outputs: HashMap::new(),
            steps: 10,
        };
        assert_eq!(e.to_json()["data"]["steps"], 10);

        let e = GraphEngineEvent::LoopFailed {
            node_id: "n1".into(),
            node_title: "L".into(),
            error: "err".into(),
            steps: 3,
        };
        assert_eq!(e.to_json()["type"], "loop_failed");
    }

    #[test]
    fn test_plugin_events() {
        let e = GraphEngineEvent::PluginLoaded {
            plugin_id: "p1".into(),
            name: "MyPlugin".into(),
        };
        assert_eq!(e.to_json()["data"]["name"], "MyPlugin");

        let e = GraphEngineEvent::PluginUnloaded { plugin_id: "p1".into() };
        assert_eq!(e.to_json()["type"], "plugin_unloaded");

        let e = GraphEngineEvent::PluginEvent {
            plugin_id: "p1".into(),
            event_type: "custom".into(),
            data: Value::Bool(true),
        };
        assert_eq!(e.to_json()["data"]["event_type"], "custom");

        let e = GraphEngineEvent::PluginError {
            plugin_id: "p1".into(),
            error: "crash".into(),
        };
        assert_eq!(e.to_json()["type"], "plugin_error");
    }

    #[test]
    fn test_debug_events() {
        let e = GraphEngineEvent::DebugPaused {
            reason: "breakpoint".into(),
            node_id: "n1".into(),
            node_type: "code".into(),
            node_title: "Code".into(),
            phase: "before".into(),
        };
        let j = e.to_json();
        assert_eq!(j["type"], "debug_paused");
        assert_eq!(j["data"]["phase"], "before");

        let e = GraphEngineEvent::DebugResumed;
        assert_eq!(e.to_json()["type"], "debug_resumed");

        let e = GraphEngineEvent::DebugBreakpointChanged {
            action: "add".into(),
            node_id: Some("n1".into()),
        };
        assert_eq!(e.to_json()["data"]["action"], "add");

        let e = GraphEngineEvent::DebugVariableSnapshot {
            data: serde_json::json!({"x": 1}),
        };
        assert_eq!(e.to_json()["data"]["data"]["x"], 1);
    }
}
