use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::NodeRunResult;
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::plugin::manifest::{AllowedCapabilities, PluginHookType, PluginManifest, PluginNodeType};
use crate::plugin::runtime::PluginRuntime;

use super::super::context::PluginContext;
use super::super::error::PluginError;
use super::super::hooks::{HookHandler, HookPayload, HookPoint};
use super::super::loader::{PluginLoadSource, PluginLoader};
use super::super::traits::{Plugin, PluginCategory, PluginCapabilities, PluginMetadata, PluginSource};

#[derive(Debug, Clone)]
pub struct WasmPluginConfig {
    pub auto_discover: bool,
    pub default_max_memory_pages: u32,
    pub default_max_fuel: u64,
    pub allowed_capabilities: AllowedCapabilities,
}

impl Default for WasmPluginConfig {
    fn default() -> Self {
        Self {
            auto_discover: true,
            default_max_memory_pages: 64,
            default_max_fuel: 100_000,
            allowed_capabilities: AllowedCapabilities::default(),
        }
    }
}

pub struct WasmBootstrapPlugin {
    metadata: PluginMetadata,
    config: WasmPluginConfig,
}

impl WasmBootstrapPlugin {
    pub fn new(config: WasmPluginConfig) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.wasm-loader".into(),
                name: "WASM Plugin Loader".into(),
                version: "0.1.0".into(),
                category: PluginCategory::Bootstrap,
                description: "Registers WasmPluginLoader for loading WASM plugins".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
            config,
        }
    }
}

#[async_trait]
impl Plugin for WasmBootstrapPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let loader = WasmPluginLoader::new(self.config.clone())?;
        ctx.register_plugin_loader(Arc::new(loader))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct WasmPluginLoader {
    engine: wasmtime::Engine,
    config: WasmPluginConfig,
}

impl WasmPluginLoader {
    pub fn new(config: WasmPluginConfig) -> Result<Self, PluginError> {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(true);
        let engine = wasmtime::Engine::new(&cfg)
            .map_err(|e| PluginError::LoadError(format!("Failed to create WASM engine: {}", e)))?;
        Ok(Self { engine, config })
    }

    fn manifest_to_metadata(&self, manifest: &PluginManifest, manifest_path: &Path) -> PluginMetadata {
        let mut capabilities = PluginCapabilities::default();
        capabilities.read_variables = manifest.capabilities.read_variables;
        capabilities.write_variables = manifest.capabilities.write_variables;
        capabilities.emit_events = manifest.capabilities.emit_events;
        capabilities.http_access = manifest.capabilities.http_access;
        capabilities.fs_access = manifest.capabilities.fs_access.clone();
        capabilities.max_memory_pages = manifest.capabilities.max_memory_pages;
        capabilities.max_fuel = manifest.capabilities.max_fuel;
        capabilities.register_nodes = !manifest.node_types.is_empty();
        capabilities.register_hooks = !manifest.hooks.is_empty();
        capabilities.register_llm_providers = false;

        PluginMetadata {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            category: PluginCategory::Normal,
            description: manifest.description.clone(),
            source: PluginSource::Custom {
                loader_type: "wasm".into(),
                detail: manifest_path.display().to_string(),
            },
            capabilities: Some(capabilities),
        }
    }

    fn validate_capabilities(&self, manifest: &PluginManifest) -> Result<(), PluginError> {
        let caps = &manifest.capabilities;
        let allowed = &self.config.allowed_capabilities;
        if caps.read_variables && !allowed.read_variables {
            return Err(PluginError::CapabilityDenied("read_variables".into()));
        }
        if caps.write_variables && !allowed.write_variables {
            return Err(PluginError::CapabilityDenied("write_variables".into()));
        }
        if caps.emit_events && !allowed.emit_events {
            return Err(PluginError::CapabilityDenied("emit_events".into()));
        }
        if caps.http_access && !allowed.http_access {
            return Err(PluginError::CapabilityDenied("http_access".into()));
        }
        if caps.fs_access.is_some() && !allowed.fs_access {
            return Err(PluginError::CapabilityDenied("fs_access".into()));
        }
        Ok(())
    }
}

#[async_trait]
impl PluginLoader for WasmPluginLoader {
    fn loader_type(&self) -> &str {
        "wasm"
    }

    async fn load(&self, source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError> {
        let dir = source
            .params
            .get("dir")
            .ok_or_else(|| PluginError::InvalidConfig("WASM plugin dir not specified".into()))?;
        let plugin_dir = PathBuf::from(dir);
        let manifest_path = plugin_dir.join("manifest.json");
        let manifest_str = std::fs::read_to_string(&manifest_path)
            .map_err(|e| PluginError::LoadError(format!("Failed to read manifest: {}", e)))?;
        let mut manifest: PluginManifest = serde_json::from_str(&manifest_str)
            .map_err(|e| PluginError::LoadError(format!("Invalid manifest: {}", e)))?;

        self.validate_capabilities(&manifest)?;

        if manifest.capabilities.max_memory_pages.is_none() {
            manifest.capabilities.max_memory_pages = Some(self.config.default_max_memory_pages);
        }
        if manifest.capabilities.max_fuel.is_none() {
            manifest.capabilities.max_fuel = Some(self.config.default_max_fuel);
        }

        let wasm_path = plugin_dir.join(&manifest.wasm_file);
        let wasm_bytes = read_wasm_bytes(&wasm_path)?;

        let runtime = PluginRuntime::new_with_engine(
            self.engine.clone(),
            manifest.clone(),
            &wasm_bytes,
        )
        .map_err(|e| PluginError::LoadError(e.to_string()))?;

        let metadata = self.manifest_to_metadata(&manifest, &manifest_path);
        let plugin = WasmPlugin {
            metadata,
            runtime: Arc::new(runtime),
            manifest,
        };
        Ok(Box::new(plugin))
    }
}

struct WasmPlugin {
    metadata: PluginMetadata,
    runtime: Arc<PluginRuntime>,
    manifest: PluginManifest,
}

#[async_trait]
impl Plugin for WasmPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        for node in &self.manifest.node_types {
            let executor = Box::new(WasmNodeExecutorAdapter {
                runtime: self.runtime.clone(),
                node_type: node.clone(),
            });
            ctx.register_node_executor(&node.node_type, executor)?;
        }

        for hook in &self.manifest.hooks {
            if let Some(point) = map_hook_point(&hook.hook_type) {
                let handler = Arc::new(WasmHookHandlerAdapter {
                    runtime: self.runtime.clone(),
                    hook: hook.clone(),
                });
                ctx.register_hook(point, handler)?;
            }
        }

        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct WasmNodeExecutorAdapter {
    runtime: Arc<PluginRuntime>,
    node_type: PluginNodeType,
}

#[async_trait]
impl NodeExecutor for WasmNodeExecutorAdapter {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        self.runtime
            .execute_node(&self.node_type, config, variable_pool, context)
            .await
    }
}

struct WasmHookHandlerAdapter {
    runtime: Arc<PluginRuntime>,
    hook: crate::plugin::manifest::PluginHook,
}

#[async_trait]
impl HookHandler for WasmHookHandlerAdapter {
    async fn handle(&self, payload: &HookPayload) -> Result<Option<Value>, PluginError> {
        let pool = payload
            .variable_pool
            .as_ref()
            .map(|p| Arc::new(std::sync::RwLock::new((**p).clone())));
        let output = self
            .runtime
            .execute_hook(
                self.hook.hook_type.clone(),
                &payload.data,
                pool.unwrap_or_else(|| Arc::new(std::sync::RwLock::new(VariablePool::new()))),
                payload.event_tx.clone(),
            )
            .await
            .map_err(|e| PluginError::LoadError(e.to_string()))?;
        Ok(output)
    }

    fn name(&self) -> &str {
        &self.hook.handler
    }
}

fn map_hook_point(hook_type: &PluginHookType) -> Option<HookPoint> {
    match hook_type {
        PluginHookType::BeforeWorkflowRun => Some(HookPoint::BeforeWorkflowRun),
        PluginHookType::AfterWorkflowRun => Some(HookPoint::AfterWorkflowRun),
        PluginHookType::BeforeNodeExecute => Some(HookPoint::BeforeNodeExecute),
        PluginHookType::AfterNodeExecute => Some(HookPoint::AfterNodeExecute),
        PluginHookType::BeforeVariableWrite => Some(HookPoint::BeforeVariableWrite),
    }
}

fn read_wasm_bytes(path: &Path) -> Result<Vec<u8>, PluginError> {
    let bytes = std::fs::read(path)
        .map_err(|e| PluginError::LoadError(format!("Failed to read wasm: {}", e)))?;
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("wat") {
            let text = String::from_utf8_lossy(&bytes);
            return wat::parse_str(&text)
                .map_err(|e| PluginError::LoadError(format!("Failed to parse wat: {}", e)));
        }
    }
    Ok(bytes)
}
