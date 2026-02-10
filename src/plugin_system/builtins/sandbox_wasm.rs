use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::plugin_system::{Plugin, PluginCategory, PluginContext, PluginError, PluginMetadata, PluginSource};
use crate::sandbox::CodeLanguage;

use xworkflow_sandbox_wasm::{WasmSandbox, WasmSandboxConfig};
use xworkflow_types::{
    CodeSandbox,
    ExecutionConfig,
    LanguageProvider,
    LanguageProviderWrapper,
    LANG_PROVIDE_KEY,
};

/// Create a pair of WASM sandbox plugins (Boot + Normal) sharing the same sandbox instance.
pub fn create_wasm_sandbox_plugins(
    config: WasmSandboxConfig,
) -> (WasmSandboxBootPlugin, WasmSandboxLangPlugin) {
    let sandbox = Arc::new(WasmSandbox::new(config));
    (
        WasmSandboxBootPlugin::new(sandbox.clone()),
        WasmSandboxLangPlugin::new(sandbox),
    )
}

pub struct WasmSandboxBootPlugin {
    metadata: PluginMetadata,
    sandbox: Arc<WasmSandbox>,
}

impl WasmSandboxBootPlugin {
    pub fn new(sandbox: Arc<WasmSandbox>) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.sandbox-wasm.boot".into(),
                name: "WASM Sandbox (Boot)".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                category: PluginCategory::Bootstrap,
                description: "Registers WASM sandbox infrastructure".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
            sandbox,
        }
    }
}

#[async_trait]
impl Plugin for WasmSandboxBootPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_sandbox(CodeLanguage::Wasm, self.sandbox.clone())?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct WasmSandboxLangPlugin {
    metadata: PluginMetadata,
    sandbox: Arc<WasmSandbox>,
}

impl WasmSandboxLangPlugin {
    pub fn new(sandbox: Arc<WasmSandbox>) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.sandbox-wasm.lang".into(),
                name: "WASM Language Provider".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                category: PluginCategory::Normal,
                description: "Provides WASM lang-provide for code node".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
            sandbox,
        }
    }
}

#[async_trait]
impl Plugin for WasmSandboxLangPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let provider: Arc<dyn LanguageProvider> = Arc::new(WasmLanguageProvider {
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

pub struct WasmLanguageProvider {
    sandbox: Arc<WasmSandbox>,
}

impl LanguageProvider for WasmLanguageProvider {
    fn language(&self) -> CodeLanguage {
        CodeLanguage::Wasm
    }

    fn display_name(&self) -> &str {
        "WebAssembly"
    }

    fn sandbox(&self) -> Arc<dyn CodeSandbox> {
        self.sandbox.clone()
    }

    fn default_config(&self) -> ExecutionConfig {
        ExecutionConfig {
            timeout: Duration::from_secs(30),
            max_memory: 64 * 1024 * 1024,
            ..ExecutionConfig::default()
        }
    }

    fn file_extensions(&self) -> Vec<&str> {
        vec![".wasm", ".wat"]
    }

    fn supports_streaming(&self) -> bool {
        false
    }
}
