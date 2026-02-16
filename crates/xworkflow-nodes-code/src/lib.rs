use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;

use xworkflow::nodes::transform::CodeNodeExecutor;
use xworkflow::plugin_system::{
    Plugin, PluginCapabilities, PluginCategory, PluginContext, PluginError, PluginMetadata,
    PluginSource,
};
use xworkflow::sandbox::{SandboxManager, SandboxManagerConfig};
use xworkflow_types::{LanguageProvider, LanguageProviderWrapper, LANG_PROVIDE_KEY};

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
        let providers = query_language_providers(ctx);
        let manager = if providers.is_empty() {
            SandboxManager::new_empty(SandboxManagerConfig::default())
        } else {
            SandboxManager::from_providers(&providers)
        };
        ctx.register_node_executor(
            "code",
            Box::new(CodeNodeExecutor::new_with_manager(Arc::new(manager))),
        )?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(CodeNodePlugin::new())
}

/// Register a language provider via the generic service registry.
pub fn register_language_provider(
    ctx: &mut PluginContext,
    provider: Arc<dyn LanguageProvider>,
) -> Result<(), PluginError> {
    ctx.provide_service(
        LANG_PROVIDE_KEY,
        Arc::new(LanguageProviderWrapper(provider)),
    )
}

/// Query language providers registered via the generic service registry.
pub fn query_language_providers(ctx: &PluginContext) -> Vec<Arc<dyn LanguageProvider>> {
    ctx.query_services(LANG_PROVIDE_KEY)
        .iter()
        .filter_map(|service| {
            service
                .clone()
                .downcast::<LanguageProviderWrapper>()
                .ok()
                .map(|wrapper| wrapper.0.clone())
        })
        .collect()
}
