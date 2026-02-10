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
