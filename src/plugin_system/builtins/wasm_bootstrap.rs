use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use tokio::sync::mpsc;

use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::dsl::schema::{EdgeHandle, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::{ErrorCode, ErrorContext, NodeError};
use crate::nodes::executor::NodeExecutor;
use crate::plugin_system::wasm::{
    read_wasm_bytes,
    AllowedCapabilities,
    EventEmitter,
    PluginError as WasmPluginError,
    PluginHook,
    PluginHookType,
    PluginManifest,
    PluginNodeType,
    PluginRuntime,
    PluginState,
    VariableAccess,
    WasmEngine,
};

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
    engine: WasmEngine,
    config: WasmPluginConfig,
}

impl WasmPluginLoader {
    pub fn new(config: WasmPluginConfig) -> Result<Self, PluginError> {
        let engine = WasmEngine::new()
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
        let wasm_bytes = read_wasm_bytes(&wasm_path)
            .map_err(|e| PluginError::LoadError(e.to_string()))?;

        let runtime = PluginRuntime::new_with_engine(
            &self.engine,
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
                manifest: self.manifest.clone(),
            });
            ctx.register_node_executor(&node.node_type, executor)?;
        }

        for hook in &self.manifest.hooks {
            if let Some(point) = map_hook_point(&hook.hook_type) {
                let handler = Arc::new(WasmHookHandlerAdapter {
                    runtime: self.runtime.clone(),
                    hook: hook.clone(),
                    manifest: self.manifest.clone(),
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

struct VariablePoolAccess {
    pool: Arc<RwLock<VariablePool>>,
}

impl VariableAccess for VariablePoolAccess {
    fn get(&self, selector: &Value) -> Result<Value> {
        let selector: Selector = serde_json::from_value(selector.clone())
            .map_err(|e| anyhow!(e.to_string()))?;
        let pool = self.pool.read().map_err(|_| anyhow!("Variable pool poisoned"))?;
        Ok(pool.get(&selector).snapshot_to_value())
    }

    fn set(&self, selector: &Value, value: &Value) -> Result<()> {
        let selector: Selector = serde_json::from_value(selector.clone())
            .map_err(|e| anyhow!(e.to_string()))?;
        let mut pool = self.pool.write().map_err(|_| anyhow!("Variable pool poisoned"))?;
        pool.set(&selector, Segment::from_value(value));
        Ok(())
    }
}

struct GraphEventEmitter {
    tx: mpsc::Sender<GraphEngineEvent>,
    plugin_id: String,
}

impl EventEmitter for GraphEventEmitter {
    fn emit(&self, event_type: &str, data: Value) -> Result<()> {
        let _ = self.tx.try_send(GraphEngineEvent::PluginEvent {
            plugin_id: self.plugin_id.clone(),
            event_type: event_type.to_string(),
            data,
        });
        Ok(())
    }
}

struct WasmNodeExecutorAdapter {
    runtime: Arc<PluginRuntime>,
    node_type: PluginNodeType,
    manifest: PluginManifest,
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
        let pool = Arc::new(RwLock::new(variable_pool.clone()));
        let state = build_state(&self.manifest, Some(pool), context.event_tx().cloned());
        let output = self
            .runtime
            .execute_node(&self.node_type, config, state)
            .map_err(map_wasm_error)?;

        let mut outputs = std::collections::HashMap::new();
        if let Value::Object(obj) = &output {
            for (k, v) in obj {
                outputs.insert(k.clone(), v.clone());
            }
        } else {
            outputs.insert("result".to_string(), output);
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

struct WasmHookHandlerAdapter {
    runtime: Arc<PluginRuntime>,
    hook: PluginHook,
    manifest: PluginManifest,
}

#[async_trait]
impl HookHandler for WasmHookHandlerAdapter {
    async fn handle(&self, payload: &HookPayload) -> Result<Option<Value>, PluginError> {
        let pool = payload
            .variable_pool
            .as_ref()
            .map(|p| Arc::new(RwLock::new((**p).clone())));
        let state = build_state(&self.manifest, pool, payload.event_tx.clone());
        let output = self
            .runtime
            .execute_hook(self.hook.hook_type.clone(), &payload.data, state)
            .map_err(|e| PluginError::LoadError(e.to_string()))?;
        Ok(output)
    }

    fn name(&self) -> &str {
        &self.hook.handler
    }
}

fn build_state(
    manifest: &PluginManifest,
    variable_pool: Option<Arc<RwLock<VariablePool>>>,
    event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
) -> PluginState {
    let mut state = PluginState::new(manifest.id.clone(), manifest.capabilities.clone());

    if let Some(pool) = variable_pool {
        state = state.with_variable_access(Arc::new(VariablePoolAccess { pool }));
    }

    if let Some(tx) = event_tx {
        state = state.with_event_emitter(Arc::new(GraphEventEmitter {
            tx,
            plugin_id: manifest.id.clone(),
        }));
    }

    state
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

fn map_wasm_error(err: WasmPluginError) -> NodeError {
    let context = match &err {
        WasmPluginError::NotFound(_) => {
            ErrorContext::non_retryable(ErrorCode::PluginNotFound, err.to_string())
        }
        WasmPluginError::Timeout | WasmPluginError::FuelExhausted => {
            ErrorContext::retryable(ErrorCode::PluginExecutionError, err.to_string())
        }
        WasmPluginError::MemoryLimitExceeded => {
            ErrorContext::non_retryable(ErrorCode::PluginResourceLimit, err.to_string())
        }
        WasmPluginError::CapabilityDenied(_) => {
            ErrorContext::non_retryable(ErrorCode::PluginCapabilityDenied, err.to_string())
        }
        _ => ErrorContext::non_retryable(ErrorCode::PluginExecutionError, err.to_string()),
    };

    NodeError::ExecutionError(err.to_string()).with_context(context)
}
