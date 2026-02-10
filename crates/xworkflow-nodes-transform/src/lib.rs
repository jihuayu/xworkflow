use async_trait::async_trait;
use std::any::Any;

use xworkflow::nodes::data_transform::{
    LegacyVariableAggregatorExecutor, TemplateTransformExecutor, VariableAggregatorExecutor,
    VariableAssignerExecutor,
};
use xworkflow::plugin_system::{
    Plugin, PluginCapabilities, PluginCategory, PluginContext, PluginError, PluginMetadata,
    PluginSource,
};

pub struct TransformNodesPlugin {
    metadata: PluginMetadata,
}

impl TransformNodesPlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "builtin-transform-nodes".to_string(),
                name: "Builtin Transform Nodes".to_string(),
                version: "0.1.0".to_string(),
                category: PluginCategory::Normal,
                description: "Built-in data transform nodes".to_string(),
                source: PluginSource::Host,
                capabilities: Some(PluginCapabilities {
                    register_nodes: true,
                    ..Default::default()
                }),
            },
        }
    }
}

impl Default for TransformNodesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for TransformNodesPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let template_executor = if let Some(engine) = ctx.query_template_engine().cloned() {
            TemplateTransformExecutor::new_with_engine(engine)
        } else {
            TemplateTransformExecutor::new()
        };
        ctx.register_node_executor("template-transform", Box::new(template_executor))?;
        ctx.register_node_executor(
            "variable-aggregator",
            Box::new(VariableAggregatorExecutor),
        )?;
        ctx.register_node_executor(
            "variable-assigner",
            Box::new(LegacyVariableAggregatorExecutor),
        )?;
        ctx.register_node_executor("assigner", Box::new(VariableAssignerExecutor))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(TransformNodesPlugin::new())
}
