use async_trait::async_trait;
use std::any::Any;

use xworkflow::nodes::subgraph_nodes::{
    IterationNodeExecutor, ListOperatorNodeExecutor, LoopNodeExecutor,
};
use xworkflow::plugin_system::{
    Plugin, PluginCapabilities, PluginCategory, PluginContext, PluginError, PluginMetadata,
    PluginSource,
};

pub struct SubgraphNodesPlugin {
    metadata: PluginMetadata,
}

impl SubgraphNodesPlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "builtin-subgraph-nodes".to_string(),
                name: "Builtin Subgraph Nodes".to_string(),
                version: "0.1.0".to_string(),
                category: PluginCategory::Normal,
                description: "Built-in subgraph control nodes".to_string(),
                source: PluginSource::Host,
                capabilities: Some(PluginCapabilities {
                    register_nodes: true,
                    ..Default::default()
                }),
            },
        }
    }
}

impl Default for SubgraphNodesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SubgraphNodesPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_node_executor(
            "list-operator",
            Box::new(ListOperatorNodeExecutor::new()),
        )?;
        ctx.register_node_executor(
            "iteration",
            Box::new(IterationNodeExecutor::new()),
        )?;
        ctx.register_node_executor("loop", Box::new(LoopNodeExecutor::new()))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(SubgraphNodesPlugin::new())
}
