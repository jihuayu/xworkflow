use async_trait::async_trait;
use std::any::Any;

use xworkflow::nodes::data_transform::CodeNodeExecutor;
use xworkflow::plugin_system::{
    Plugin, PluginCapabilities, PluginCategory, PluginContext, PluginError, PluginMetadata,
    PluginSource,
};

pub struct CodeNodePlugin {
    metadata: PluginMetadata,
}

impl CodeNodePlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "builtin-code-node".to_string(),
                name: "Builtin Code Node".to_string(),
                version: "0.1.0".to_string(),
                category: PluginCategory::Normal,
                description: "Built-in code execution node".to_string(),
                source: PluginSource::Host,
                capabilities: Some(PluginCapabilities {
                    register_nodes: true,
                    ..Default::default()
                }),
            },
        }
    }
}

impl Default for CodeNodePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for CodeNodePlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_node_executor("code", Box::new(CodeNodeExecutor::new()))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(CodeNodePlugin::new())
}
