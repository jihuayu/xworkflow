pub mod error;
pub mod host_functions;
pub mod manifest;
pub mod manager;
pub mod runtime;

pub use error::PluginError;
pub use host_functions::{PluginState, HOST_MODULE};
pub use manifest::{
    AllowedCapabilities, PluginCapabilities, PluginHook, PluginHookType, PluginManifest,
    PluginNodeType,
};
pub use manager::{PluginManager, PluginManagerConfig};
pub use runtime::{PluginRuntime, PluginStatus};

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::NodeRunResult;
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;

/// Plugin node executor
pub struct PluginNodeExecutor {
    plugin_manager: Arc<PluginManager>,
    node_type_override: Option<String>,
}

impl PluginNodeExecutor {
    pub fn new(plugin_manager: Arc<PluginManager>, node_type_override: Option<String>) -> Self {
        Self {
            plugin_manager,
            node_type_override,
        }
    }
}

#[async_trait]
impl NodeExecutor for PluginNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let node_type = config
            .get("plugin_node_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| self.node_type_override.clone())
            .ok_or_else(|| NodeError::ConfigError("Missing plugin_node_type".into()))?;

        let runtime = self
            .plugin_manager
            .get_executor_for_node_type(&node_type)
            .ok_or_else(|| NodeError::ConfigError(format!("No plugin for node type: {}", node_type)))?;

        let plugin_node = runtime
            .manifest()
            .node_types
            .iter()
            .find(|n| n.node_type == node_type)
            .ok_or_else(|| NodeError::ConfigError("Node type not found in plugin".into()))?;

        runtime
            .execute_node(plugin_node, config, variable_pool, context)
            .await
    }
}
