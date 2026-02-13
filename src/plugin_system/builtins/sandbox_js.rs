use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::plugin_system::{Plugin, PluginCategory, PluginContext, PluginError, PluginMetadata, PluginSource};
use crate::sandbox::CodeLanguage;

use xworkflow_sandbox_js::{BuiltinSandbox, BuiltinSandboxConfig};
use xworkflow_types::{
    CodeSandbox,
    ExecutionConfig,
    LanguageProvider,
    LanguageProviderWrapper,
    LANG_PROVIDE_KEY,
};

/// Create a pair of JS sandbox plugins (Boot + Normal) sharing the same sandbox instance.
pub fn create_js_sandbox_plugins(
    config: BuiltinSandboxConfig,
) -> (JsSandboxBootPlugin, JsSandboxLangPlugin) {
    let sandbox = Arc::new(BuiltinSandbox::new(config));
    (
        JsSandboxBootPlugin::new(sandbox.clone()),
        JsSandboxLangPlugin::new(sandbox),
    )
}

pub struct JsSandboxBootPlugin {
    metadata: PluginMetadata,
    sandbox: Arc<BuiltinSandbox>,
}

impl JsSandboxBootPlugin {
    pub fn new(sandbox: Arc<BuiltinSandbox>) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.sandbox-js.boot".into(),
                name: "JavaScript Sandbox (Boot)".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                category: PluginCategory::Bootstrap,
                description: "Registers JS sandbox infrastructure".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
            sandbox,
        }
    }
}

#[async_trait]
impl Plugin for JsSandboxBootPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_sandbox(CodeLanguage::JavaScript, self.sandbox.clone())?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct JsSandboxLangPlugin {
    metadata: PluginMetadata,
    sandbox: Arc<BuiltinSandbox>,
}

impl JsSandboxLangPlugin {
    pub fn new(sandbox: Arc<BuiltinSandbox>) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.sandbox-js.lang".into(),
                name: "JavaScript Language Provider".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                category: PluginCategory::Normal,
                description: "Provides JavaScript lang-provide for code node".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
            sandbox,
        }
    }
}

#[async_trait]
impl Plugin for JsSandboxLangPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let provider: Arc<dyn LanguageProvider> = Arc::new(JsLanguageProvider {
            sandbox: self.sandbox.clone(),
        });
        ctx.provide_service(
            LANG_PROVIDE_KEY,
            Arc::new(LanguageProviderWrapper(provider)),
        )?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct JsLanguageProvider {
    sandbox: Arc<BuiltinSandbox>,
}

impl LanguageProvider for JsLanguageProvider {
    fn language(&self) -> CodeLanguage {
        CodeLanguage::JavaScript
    }

    fn display_name(&self) -> &str {
        "JavaScript (boa)"
    }

    fn sandbox(&self) -> Arc<dyn CodeSandbox> {
        self.sandbox.clone()
    }

    fn default_config(&self) -> ExecutionConfig {
        ExecutionConfig {
            timeout: Duration::from_secs(30),
            max_memory: 32 * 1024 * 1024,
            ..ExecutionConfig::default()
        }
    }

    fn file_extensions(&self) -> Vec<&str> {
        vec![".js", ".mjs"]
    }

    fn supports_streaming(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_js_sandbox_plugins() {
        let config = BuiltinSandboxConfig::default();
        let (boot, lang) = create_js_sandbox_plugins(config);
        
        assert_eq!(boot.metadata().id, "xworkflow.sandbox-js.boot");
        assert_eq!(boot.metadata().category, PluginCategory::Bootstrap);
        assert_eq!(lang.metadata().id, "xworkflow.sandbox-js.lang");
        assert_eq!(lang.metadata().category, PluginCategory::Normal);
    }

    #[test]
    fn test_js_sandbox_boot_plugin_new() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let plugin = JsSandboxBootPlugin::new(sandbox);
        
        assert_eq!(plugin.metadata().name, "JavaScript Sandbox (Boot)");
    }

    #[test]
    fn test_js_sandbox_boot_plugin_metadata() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let plugin = JsSandboxBootPlugin::new(sandbox);
        
        let metadata = plugin.metadata();
        assert_eq!(metadata.id, "xworkflow.sandbox-js.boot");
        assert_eq!(metadata.category, PluginCategory::Bootstrap);
    }

    #[test]
    fn test_js_sandbox_boot_plugin_as_any() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let plugin = JsSandboxBootPlugin::new(sandbox);
        
        assert!(plugin.as_any().is::<JsSandboxBootPlugin>());
    }

    #[test]
    fn test_js_sandbox_lang_plugin_new() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let plugin = JsSandboxLangPlugin::new(sandbox);
        
        assert_eq!(plugin.metadata().name, "JavaScript Language Provider");
        assert_eq!(plugin.metadata().category, PluginCategory::Normal);
    }

    #[test]
    fn test_js_sandbox_lang_plugin_metadata() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let plugin = JsSandboxLangPlugin::new(sandbox);
        
        let metadata = plugin.metadata();
        assert_eq!(metadata.id, "xworkflow.sandbox-js.lang");
        assert_eq!(metadata.description, "Provides JavaScript lang-provide for code node");
    }

    #[test]
    fn test_js_sandbox_lang_plugin_as_any() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let plugin = JsSandboxLangPlugin::new(sandbox);
        
        assert!(plugin.as_any().is::<JsSandboxLangPlugin>());
    }

    #[test]
    fn test_js_language_provider_language() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let provider = JsLanguageProvider { sandbox };
        
        assert_eq!(provider.language(), CodeLanguage::JavaScript);
    }

    #[test]
    fn test_js_language_provider_display_name() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let provider = JsLanguageProvider { sandbox };
        
        assert_eq!(provider.display_name(), "JavaScript (boa)");
    }

    #[test]
    fn test_js_language_provider_default_config() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let provider = JsLanguageProvider { sandbox };
        
        let config = provider.default_config();
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.max_memory, 32 * 1024 * 1024);
    }

    #[test]
    fn test_js_language_provider_file_extensions() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let provider = JsLanguageProvider { sandbox };
        
        let exts = provider.file_extensions();
        assert_eq!(exts, vec![".js", ".mjs"]);
    }

    #[test]
    fn test_js_language_provider_supports_streaming() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let provider = JsLanguageProvider { sandbox };
        
        assert!(provider.supports_streaming());
    }

    #[test]
    fn test_js_language_provider_sandbox() {
        let sandbox = Arc::new(BuiltinSandbox::new(BuiltinSandboxConfig::default()));
        let provider = JsLanguageProvider { sandbox: sandbox.clone() };
        
        let _sandbox_ref = provider.sandbox();
    }
}
