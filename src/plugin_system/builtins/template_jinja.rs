use std::sync::Arc;

use async_trait::async_trait;

use crate::plugin_system::{Plugin, PluginCategory, PluginContext, PluginError, PluginMetadata, PluginSource};

use crate::template::jinja::JinjaTemplateEngine;

pub struct JinjaTemplatePlugin {
    metadata: PluginMetadata,
}

impl JinjaTemplatePlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.template-jinja".into(),
                name: "Jinja2 Template Engine".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                category: PluginCategory::Bootstrap,
                description: "Jinja2 template engine via minijinja".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
        }
    }
}

#[async_trait]
impl Plugin for JinjaTemplatePlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let engine = Arc::new(JinjaTemplateEngine::new());
        ctx.register_template_engine(engine)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
