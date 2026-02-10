use async_trait::async_trait;
use std::any::Any;

use xworkflow::nodes::data_transform::HttpRequestExecutor;
use xworkflow::plugin_system::{
    Plugin, PluginCapabilities, PluginCategory, PluginContext, PluginError, PluginMetadata,
    PluginSource,
};

pub struct HttpNodePlugin {
    metadata: PluginMetadata,
}

impl HttpNodePlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "builtin-http-node".to_string(),
                name: "Builtin HTTP Node".to_string(),
                version: "0.1.0".to_string(),
                category: PluginCategory::Normal,
                description: "Built-in HTTP request node".to_string(),
                source: PluginSource::Host,
                capabilities: Some(PluginCapabilities {
                    register_nodes: true,
                    http_access: true,
                    ..Default::default()
                }),
            },
        }
    }
}

impl Default for HttpNodePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for HttpNodePlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_node_executor("http-request", Box::new(HttpRequestExecutor))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(HttpNodePlugin::new())
}
