use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::core::event_bus::GraphEngineEvent;
use crate::core::variable_pool::VariablePool;

use super::error::PluginError;
use super::manifest::{
    AllowedCapabilities, PluginHookType, PluginManifest, PluginNodeType,
};
use super::runtime::{PluginRuntime, PluginStatus};

/// Plugin manager configuration
#[derive(Debug, Clone)]
pub struct PluginManagerConfig {
    pub plugin_dir: PathBuf,
    pub auto_discover: bool,
    pub default_max_memory_pages: u32,
    pub default_max_fuel: u64,
    pub allowed_capabilities: AllowedCapabilities,
}

/// Plugin manager
pub struct PluginManager {
    plugins: HashMap<String, Arc<PluginRuntime>>,
    node_type_registry: HashMap<String, String>,
    hook_registry: HashMap<PluginHookType, Vec<(String, String)>>,
    engine: wasmtime::Engine,
    config: PluginManagerConfig,
}

impl PluginManager {
    pub fn new(config: PluginManagerConfig) -> Result<Self, PluginError> {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(true);
        let engine = wasmtime::Engine::new(&cfg)
            .map_err(|e| PluginError::CompilationError(e.to_string()))?;

        let mut manager = Self {
            plugins: HashMap::new(),
            node_type_registry: HashMap::new(),
            hook_registry: HashMap::new(),
            engine,
            config,
        };

        if manager.config.auto_discover {
            let _ = manager.discover_and_load();
        }

        Ok(manager)
    }

    pub fn discover_and_load(&mut self) -> Result<Vec<String>, PluginError> {
        let mut loaded = Vec::new();
        if !self.config.plugin_dir.exists() {
            return Ok(loaded);
        }
        for entry in fs::read_dir(&self.config.plugin_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Ok(plugin_id) = self.load_plugin(&path) {
                    loaded.push(plugin_id);
                }
            }
        }
        Ok(loaded)
    }

    pub fn load_plugin(&mut self, plugin_dir: &Path) -> Result<String, PluginError> {
        let manifest_path = plugin_dir.join("manifest.json");
        let manifest_str = fs::read_to_string(&manifest_path)?;
        let mut manifest: PluginManifest = serde_json::from_str(&manifest_str)
            .map_err(|e| PluginError::InvalidManifest(e.to_string()))?;

        if self.plugins.contains_key(&manifest.id) {
            return Err(PluginError::AlreadyLoaded(manifest.id));
        }

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
        )?;

        let plugin_id = manifest.id.clone();
        let runtime = Arc::new(runtime);
        self.plugins.insert(plugin_id.clone(), runtime);

        // Register node types
        for node in &manifest.node_types {
            self.node_type_registry
                .insert(node.node_type.clone(), plugin_id.clone());
        }

        // Register hooks
        for hook in &manifest.hooks {
            self.hook_registry
                .entry(hook.hook_type.clone())
                .or_default()
                .push((plugin_id.clone(), hook.handler.clone()));
        }

        Ok(plugin_id)
    }

    pub fn unload_plugin(&mut self, plugin_id: &str) -> Result<(), PluginError> {
        self.plugins
            .remove(plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;
        self.node_type_registry
            .retain(|_, v| v != plugin_id);
        for handlers in self.hook_registry.values_mut() {
            handlers.retain(|(id, _)| id != plugin_id);
        }
        Ok(())
    }

    pub fn get_plugin_node_types(&self) -> Vec<(String, PluginNodeType)> {
        let mut types = Vec::new();
        for runtime in self.plugins.values() {
            for node in &runtime.manifest().node_types {
                types.push((node.node_type.clone(), node.clone()));
            }
        }
        types
    }

    pub fn get_executor_for_node_type(&self, node_type: &str) -> Option<Arc<PluginRuntime>> {
        let plugin_id = self.node_type_registry.get(node_type)?;
        self.plugins.get(plugin_id).cloned()
    }

    pub async fn execute_hooks(
        &self,
        hook_type: PluginHookType,
        payload: &Value,
        variable_pool: Arc<std::sync::RwLock<VariablePool>>,
        event_tx: Option<tokio::sync::mpsc::Sender<GraphEngineEvent>>,
    ) -> Result<Vec<Value>, PluginError> {
        let mut results = Vec::new();
        if let Some(handlers) = self.hook_registry.get(&hook_type) {
            for (plugin_id, _handler_name) in handlers {
                if let Some(runtime) = self.plugins.get(plugin_id) {
                    if let Some(value) = runtime
                        .execute_hook(hook_type.clone(), payload, variable_pool.clone(), event_tx.clone())
                        .await?
                    {
                        results.push(value);
                    }
                }
            }
        }
        Ok(results)
    }

    pub fn list_plugins(&self) -> Vec<&PluginManifest> {
        self.plugins.values().map(|r| r.manifest()).collect()
    }

    pub fn get_plugin_status(&self, plugin_id: &str) -> Option<PluginStatus> {
        self.plugins.get(plugin_id).map(|p| p.status())
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

fn read_wasm_bytes(path: &Path) -> Result<Vec<u8>, PluginError> {
    let bytes = fs::read(path)?;
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("wat") {
            let text = String::from_utf8_lossy(&bytes);
            return wat::parse_str(&text)
                .map_err(|e| PluginError::CompilationError(e.to_string()));
        }
    }
    Ok(bytes)
}
