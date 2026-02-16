//! Hook points and handlers for the plugin lifecycle.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::error::PluginError;
use crate::core::event_bus::GraphEngineEvent;
use crate::core::variable_pool::VariablePool;

/// Points in the workflow lifecycle where hooks can be invoked.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookPoint {
    BeforeWorkflowRun,
    AfterWorkflowRun,
    BeforeNodeExecute,
    AfterNodeExecute,
    BeforeVariableWrite,
    AfterDslValidation,
    AfterPluginLoaded,
}

/// Payload delivered to a [`HookHandler`] when a hook fires.
#[derive(Debug, Clone)]
pub struct HookPayload {
    pub hook_point: HookPoint,
    pub data: Value,
    pub variable_pool: Option<Arc<VariablePool>>,
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
}

/// Handler that reacts to a [`HookPoint`] event.
#[async_trait]
pub trait HookHandler: Send + Sync {
    /// Process the hook event and optionally return modified data.
    async fn handle(&self, payload: &HookPayload) -> Result<Option<Value>, PluginError>;
    /// Human-readable name for logging.
    fn name(&self) -> &str;
    /// Execution priority (lower runs first). Default is 100.
    fn priority(&self) -> i32 {
        100
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_point_eq() {
        assert_eq!(HookPoint::BeforeWorkflowRun, HookPoint::BeforeWorkflowRun);
        assert_eq!(HookPoint::AfterWorkflowRun, HookPoint::AfterWorkflowRun);
        assert_eq!(HookPoint::BeforeNodeExecute, HookPoint::BeforeNodeExecute);
        assert_eq!(HookPoint::AfterNodeExecute, HookPoint::AfterNodeExecute);
        assert_eq!(
            HookPoint::BeforeVariableWrite,
            HookPoint::BeforeVariableWrite
        );
        assert_eq!(HookPoint::AfterDslValidation, HookPoint::AfterDslValidation);
        assert_eq!(HookPoint::AfterPluginLoaded, HookPoint::AfterPluginLoaded);
        assert_ne!(HookPoint::BeforeWorkflowRun, HookPoint::AfterWorkflowRun);
    }

    #[test]
    fn test_hook_payload_creation() {
        let payload = HookPayload {
            hook_point: HookPoint::BeforeNodeExecute,
            data: serde_json::json!({"node_id": "n1"}),
            variable_pool: None,
            event_tx: None,
        };
        assert_eq!(payload.hook_point, HookPoint::BeforeNodeExecute);
        assert!(payload.variable_pool.is_none());
        assert!(payload.event_tx.is_none());
    }

    #[test]
    fn test_hook_point_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(HookPoint::BeforeWorkflowRun);
        set.insert(HookPoint::AfterWorkflowRun);
        set.insert(HookPoint::BeforeWorkflowRun); // duplicate
        assert_eq!(set.len(), 2);
    }
}
