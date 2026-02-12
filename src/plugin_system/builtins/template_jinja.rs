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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jinja_template_plugin_new() {
        let plugin = JinjaTemplatePlugin::new();
        
        assert_eq!(plugin.metadata().id, "xworkflow.template-jinja");
        assert_eq!(plugin.metadata().name, "Jinja2 Template Engine");
        assert_eq!(plugin.metadata().category, PluginCategory::Bootstrap);
    }

    #[test]
    fn test_jinja_template_plugin_metadata() {
        let plugin = JinjaTemplatePlugin::new();
        
        let metadata = plugin.metadata();
        assert_eq!(metadata.description, "Jinja2 template engine via minijinja");
    }

    #[test]
    fn test_jinja_template_plugin_as_any() {
        let plugin = JinjaTemplatePlugin::new();
        
        assert!(plugin.as_any().is::<JinjaTemplatePlugin>());
    }

    #[test]
    fn test_jinja_template_plugin_version() {
        let plugin = JinjaTemplatePlugin::new();
        
        assert!(!plugin.metadata().version.is_empty());
    }
}
