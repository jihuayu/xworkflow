use async_trait::async_trait;
use std::any::Any;

use xworkflow::nodes::control_flow::{
    AnswerNodeExecutor, EndNodeExecutor, IfElseNodeExecutor, StartNodeExecutor,
};
use xworkflow::plugin_system::{
    Plugin, PluginCapabilities, PluginCategory, PluginContext, PluginError, PluginMetadata,
    PluginSource,
};

pub struct CoreNodesPlugin {
    metadata: PluginMetadata,
}

impl CoreNodesPlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "builtin-core-nodes".to_string(),
                name: "Builtin Core Nodes".to_string(),
                version: "0.1.0".to_string(),
                category: PluginCategory::Normal,
                description: "Built-in control flow nodes".to_string(),
                source: PluginSource::Host,
                capabilities: Some(PluginCapabilities {
                    register_nodes: true,
                    ..Default::default()
                }),
            },
        }
    }
}

impl Default for CoreNodesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for CoreNodesPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_node_executor("start", Box::new(StartNodeExecutor))?;
        ctx.register_node_executor("end", Box::new(EndNodeExecutor))?;
        ctx.register_node_executor("answer", Box::new(AnswerNodeExecutor))?;
        ctx.register_node_executor("if-else", Box::new(IfElseNodeExecutor))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(CoreNodesPlugin::new())
}
