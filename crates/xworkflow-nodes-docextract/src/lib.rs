use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;

use xworkflow::nodes::document_extract::{DocumentExtractorExecutor, ExtractorRouter};
use xworkflow::plugin_system::{
    Plugin, PluginCapabilities, PluginCategory, PluginContext, PluginError, PluginMetadata,
    PluginSource,
};
use xworkflow_types::{
    DocumentExtractorProvider, DocumentExtractorProviderWrapper, DOC_EXTRACT_PROVIDE_KEY,
};

pub struct DocumentExtractorPlugin {
    metadata: PluginMetadata,
}

impl DocumentExtractorPlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "builtin-document-extractor".to_string(),
                name: "Builtin Document Extractor".to_string(),
                version: "0.1.0".to_string(),
                category: PluginCategory::Normal,
                description: "Built-in document extractor node".to_string(),
                source: PluginSource::Host,
                capabilities: Some(PluginCapabilities {
                    register_nodes: true,
                    ..Default::default()
                }),
            },
        }
    }
}

impl Default for DocumentExtractorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for DocumentExtractorPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let providers = query_doc_extract_providers(ctx);
        let router = ExtractorRouter::from_providers(&providers);
        ctx.register_node_executor(
            "document-extractor",
            Box::new(DocumentExtractorExecutor::new(Arc::new(router))),
        )?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(DocumentExtractorPlugin::new())
}

/// Register a document extractor provider via the generic service registry.
pub fn register_doc_extract_provider(
    ctx: &mut PluginContext,
    provider: Arc<dyn DocumentExtractorProvider>,
) -> Result<(), PluginError> {
    ctx.provide_service(
        DOC_EXTRACT_PROVIDE_KEY,
        Arc::new(DocumentExtractorProviderWrapper(provider)),
    )
}

/// Query document extractor providers registered via the service registry.
pub fn query_doc_extract_providers(ctx: &PluginContext) -> Vec<Arc<dyn DocumentExtractorProvider>> {
    ctx.query_services(DOC_EXTRACT_PROVIDE_KEY)
        .iter()
        .filter_map(|service| {
            service
                .clone()
                .downcast::<DocumentExtractorProviderWrapper>()
                .ok()
                .map(|wrapper| wrapper.0.clone())
        })
        .collect()
}
