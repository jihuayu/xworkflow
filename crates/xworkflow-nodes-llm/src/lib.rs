use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;

use xworkflow::llm::{LlmNodeExecutor, LlmProviderRegistry};
use xworkflow::plugin_system::{
    Plugin, PluginCapabilities, PluginCategory, PluginContext, PluginError, PluginMetadata,
    PluginSource,
};

pub struct LlmNodesPlugin {
    metadata: PluginMetadata,
}

impl LlmNodesPlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "builtin-llm-node".to_string(),
                name: "Builtin LLM Node".to_string(),
                version: "0.1.0".to_string(),
                category: PluginCategory::Normal,
                description: "Built-in LLM node".to_string(),
                source: PluginSource::Host,
                capabilities: Some(PluginCapabilities {
                    register_nodes: true,
                    register_llm_providers: true,
                    ..Default::default()
                }),
            },
        }
    }
}

impl Default for LlmNodesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for LlmNodesPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let registry = Arc::new(LlmProviderRegistry::new());
        ctx.register_node_executor("llm", Box::new(LlmNodeExecutor::new(registry)))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(LlmNodesPlugin::new())
}
