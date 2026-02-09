use serde_json::Value;
use std::sync::{Arc, RwLock};

use tokio::sync::mpsc;
use wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};

use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::{NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::NodeError;

use super::error::PluginError;
use super::host_functions::{register_host_functions, PluginState};
use super::manifest::{PluginHookType, PluginManifest, PluginNodeType};
use wasmtime_wasi::{preview1, WasiCtxBuilder};

#[derive(Debug, Clone, PartialEq)]
pub enum PluginStatus {
    Loaded,
    Ready,
    Running,
    Error(String),
    Unloaded,
}

/// Plugin runtime instance
pub struct PluginRuntime {
    manifest: PluginManifest,
    engine: Engine,
    module: Module,
    linker: Linker<PluginState>,
    status: Arc<RwLock<PluginStatus>>,
}

impl PluginRuntime {
    pub fn new(manifest: PluginManifest, wasm_bytes: &[u8]) -> Result<Self, PluginError> {
        let engine = build_engine()?;
        Self::new_with_engine(engine, manifest, wasm_bytes)
    }

    pub fn new_with_engine(
        engine: Engine,
        manifest: PluginManifest,
        wasm_bytes: &[u8],
    ) -> Result<Self, PluginError> {
        let module = Module::new(&engine, wasm_bytes)
            .map_err(|e| PluginError::CompilationError(e.to_string()))?;

        let mut linker = Linker::new(&engine);
        preview1::add_to_linker_sync(&mut linker, |state: &mut PluginState| &mut state.wasi)
            .map_err(|e: anyhow::Error| PluginError::InstantiationError(e.to_string()))?;
        register_host_functions(&mut linker)?;

        Ok(Self {
            manifest,
            engine,
            module,
            linker,
            status: Arc::new(RwLock::new(PluginStatus::Loaded)),
        })
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn status(&self) -> PluginStatus {
        self.status
            .read()
            .map(|s| s.clone())
            .unwrap_or(PluginStatus::Error("Status lock poisoned".into()))
    }

    pub fn call_function(
        &self,
        function_name: &str,
        input: &Value,
        mut state: PluginState,
    ) -> Result<Value, PluginError> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(state.capabilities.max_memory_pages.unwrap_or(256) as usize * 64 * 1024)
            .build();
        state.limits = limits;

        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);

        if let Some(fuel) = store.data().capabilities.max_fuel {
            store
                .set_fuel(fuel)
                .map_err(|e: anyhow::Error| PluginError::ExecutionError(e.to_string()))?;
        }

        let instance = self
            .linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| PluginError::InstantiationError(e.to_string()))?;

        if let Ok(init) = instance.get_typed_func::<(), i32>(&mut store, "plugin_init") {
            let code = init
                .call(&mut store, ())
                .map_err(|e| PluginError::ExecutionError(e.to_string()))?;
            if code != 0 {
                return Err(PluginError::ExecutionError(format!(
                    "plugin_init failed: {}",
                    code
                )));
            }
        }

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| PluginError::MissingExport("memory".into()))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "alloc")
            .map_err(|_| PluginError::MissingExport("alloc".into()))?;
        let dealloc = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "dealloc")
            .map_err(|_| PluginError::MissingExport("dealloc".into()))?;
        let handler = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, function_name)
            .map_err(|_| PluginError::MissingExport(function_name.to_string()))?;

        let input_bytes = serde_json::to_vec(input)
            .map_err(|e| PluginError::SerializationError(e.to_string()))?;
        let input_len = input_bytes.len() as i32;
        let input_ptr = alloc
            .call(&mut store, input_len)
            .map_err(|e| map_wasm_error(&e))?;
        memory
            .write(&mut store, input_ptr as usize, &input_bytes)
            .map_err(|e| PluginError::ExecutionError(e.to_string()))?;

        let result_ptr = handler
            .call(&mut store, (input_ptr, input_len))
            .map_err(|e| map_wasm_error(&e))?;

        let mut header = [0u8; 8];
        memory
            .read(&mut store, result_ptr as usize, &mut header)
            .map_err(|e| PluginError::ExecutionError(e.to_string()))?;
        let out_ptr = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let out_len = i32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;

        let mut out_bytes = vec![0u8; out_len];
        memory
            .read(&mut store, out_ptr, &mut out_bytes)
            .map_err(|e| PluginError::ExecutionError(e.to_string()))?;
        let output: Value = serde_json::from_slice(&out_bytes)
            .map_err(|e| PluginError::SerializationError(e.to_string()))?;

        let _ = dealloc.call(&mut store, (input_ptr, input_len));
        let _ = dealloc.call(&mut store, (out_ptr as i32, out_len as i32));
        let _ = dealloc.call(&mut store, (result_ptr, 8));

        if let Ok(destroy) = instance.get_typed_func::<(), ()>(&mut store, "plugin_destroy") {
            let _ = destroy.call(&mut store, ());
        }

        Ok(output)
    }

    pub async fn execute_node(
        &self,
        node_type: &PluginNodeType,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let pool = Arc::new(RwLock::new(variable_pool.clone()));
        let state = PluginState {
            wasi: WasiCtxBuilder::new().build_p1(),
            variable_pool: Some(pool),
            event_tx: None,
            capabilities: self.manifest.capabilities.clone(),
            limits: StoreLimitsBuilder::new().build(),
            plugin_id: self.manifest.id.clone(),
            host_buffers: Vec::new(),
        };

        let output = self
            .call_function(&node_type.handler, config, state)
            .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

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
            outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }

    pub async fn execute_hook(
        &self,
        hook_type: PluginHookType,
        payload: &Value,
        variable_pool: Arc<RwLock<VariablePool>>,
        event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    ) -> Result<Option<Value>, PluginError> {
        let hook = self
            .manifest
            .hooks
            .iter()
            .find(|h| h.hook_type == hook_type)
            .ok_or_else(|| PluginError::MissingExport(format!("hook:{:?}", hook_type)))?;

        let state = PluginState {
            wasi: WasiCtxBuilder::new().build_p1(),
            variable_pool: Some(variable_pool),
            event_tx,
            capabilities: self.manifest.capabilities.clone(),
            limits: StoreLimitsBuilder::new().build(),
            plugin_id: self.manifest.id.clone(),
            host_buffers: Vec::new(),
        };

        let output = self.call_function(&hook.handler, payload, state)?;
        Ok(Some(output))
    }
}

fn build_engine() -> Result<Engine, PluginError> {
    let mut cfg = wasmtime::Config::new();
    cfg.consume_fuel(true);
    Engine::new(&cfg).map_err(|e| PluginError::CompilationError(e.to_string()))
}

fn map_wasm_error(err: &anyhow::Error) -> PluginError {
    let msg = err.to_string();
    if msg.to_lowercase().contains("fuel") {
        PluginError::FuelExhausted
    } else if msg.to_lowercase().contains("memory") {
        PluginError::MemoryLimitExceeded
    } else {
        PluginError::ExecutionError(msg)
    }
}
