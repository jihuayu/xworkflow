use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::core::event_bus::GraphEngineEvent;
use crate::core::variable_pool::VariablePool;
use super::error::PluginError;

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

#[derive(Debug, Clone)]
pub struct HookPayload {
    pub hook_point: HookPoint,
    pub data: Value,
    pub variable_pool: Option<Arc<VariablePool>>,
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
}

#[async_trait]
pub trait HookHandler: Send + Sync {
    async fn handle(&self, payload: &HookPayload) -> Result<Option<Value>, PluginError>;
    fn name(&self) -> &str;
    fn priority(&self) -> i32 {
        100
    }
}
