use serde_json::Value;
use std::sync::{Arc, RwLock};

use wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};
use wasmtime_wasi::preview1;

use super::error::PluginError;
use super::host_functions::{register_host_functions, PluginState};
use super::manifest::{PluginHookType, PluginManifest, PluginNodeType};

#[derive(Debug, Clone, PartialEq)]
pub enum PluginStatus {
    Loaded,
    Ready,
    Running,
    Error(String),
    Unloaded,
}

#[derive(Clone)]
pub struct WasmEngine {
    engine: Engine,
}

impl WasmEngine {
    pub fn new() -> Result<Self, PluginError> {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(true);
        let engine = Engine::new(&cfg).map_err(|e| PluginError::CompilationError(e.to_string()))?;
        Ok(Self { engine })
    }
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
        let engine = WasmEngine::new()?;
        Self::new_with_engine(&engine, manifest, wasm_bytes)
    }

    pub fn new_with_engine(
        engine: &WasmEngine,
        manifest: PluginManifest,
        wasm_bytes: &[u8],
    ) -> Result<Self, PluginError> {
        let module = Module::new(&engine.engine, wasm_bytes)
            .map_err(|e| PluginError::CompilationError(e.to_string()))?;

        let mut linker = Linker::new(&engine.engine);
        preview1::add_to_linker_sync(&mut linker, |state: &mut PluginState| &mut state.wasi)
            .map_err(|e: anyhow::Error| PluginError::InstantiationError(e.to_string()))?;
        register_host_functions(&mut linker)?;

        Ok(Self {
            manifest,
            engine: engine.engine.clone(),
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

    pub fn execute_node(
        &self,
        node_type: &PluginNodeType,
        config: &Value,
        state: PluginState,
    ) -> Result<Value, PluginError> {
        self.call_function(&node_type.handler, config, state)
    }

    pub fn execute_hook(
        &self,
        hook_type: PluginHookType,
        payload: &Value,
        state: PluginState,
    ) -> Result<Option<Value>, PluginError> {
        let hook = self
            .manifest
            .hooks
            .iter()
            .find(|h| h.hook_type == hook_type)
            .ok_or_else(|| PluginError::MissingExport(format!("hook:{:?}", hook_type)))?;

        let output = self.call_function(&hook.handler, payload, state)?;
        Ok(Some(output))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::RwLock;

    use anyhow::{anyhow, Result};
    use crate::host_functions::VariableAccess;

    fn basic_plugin_wat() -> &'static str {
        r#"(module
    (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
    (func $alloc (export "alloc") (param $size i32) (result i32)
    (local $addr i32)
    global.get $heap
    local.set $addr
    global.get $heap
    local.get $size
    i32.add
    global.set $heap
    local.get $addr)
    (func $dealloc (export "dealloc") (param i32 i32))
    (data (i32.const 0) "{\"echo\":\"ok\"}")
    (data (i32.const 64) "{\"hook\":\"before\"}")
    (func (export "handle_echo") (param i32 i32) (result i32)
    (local $struct i32)
    (local.set $struct (call $alloc (i32.const 8)))
    (local.get $struct)
    (i32.const 0)
    i32.store
    (local.get $struct)
    (i32.const 13)
    i32.store offset=4
    (local.get $struct)
  )
    (func (export "on_before_run") (param i32 i32) (result i32)
    (local $struct i32)
    (local.set $struct (call $alloc (i32.const 8)))
    (local.get $struct)
    (i32.const 64)
    i32.store
    (local.get $struct)
    (i32.const 17)
    i32.store offset=4
    (local.get $struct)
  )
)"#
    }

    fn host_plugin_wat() -> &'static str {
        r#"(module
    (import "xworkflow" "var_get" (func $var_get (param i32 i32) (result i32 i32)))
    (import "xworkflow" "var_set" (func $var_set (param i32 i32 i32 i32) (result i32)))
    (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
    (func $alloc (export "alloc") (param $size i32) (result i32)
    (local $addr i32)
    global.get $heap
    local.set $addr
    global.get $heap
    local.get $size
    i32.add
    global.set $heap
    local.get $addr)
    (func $dealloc (export "dealloc") (param i32 i32))
    (data (i32.const 0) "[\"node1\",\"key1\"]")
    (data (i32.const 64) "\"new-value\"")
    (data (i32.const 128) "{\"ok\":true}")
    (func (export "handle_var_get") (param i32 i32) (result i32)
    (local $out_ptr i32)
    (local $out_len i32)
    (local $struct i32)
    (call $var_get (i32.const 0) (i32.const 16))
    local.set $out_len
    local.set $out_ptr
    (local.set $struct (call $alloc (i32.const 8)))
    (local.get $struct)
    (local.get $out_ptr)
    i32.store
    (local.get $struct)
    (local.get $out_len)
    i32.store offset=4
    (local.get $struct)
  )
    (func (export "handle_var_set") (param i32 i32) (result i32)
    (local $struct i32)
    (call $var_set (i32.const 0) (i32.const 16) (i32.const 64) (i32.const 11))
    drop
    (local.set $struct (call $alloc (i32.const 8)))
    (local.get $struct)
    (i32.const 128)
    i32.store
    (local.get $struct)
    (i32.const 11)
    i32.store offset=4
    (local.get $struct)
  )
)"#
    }

    fn basic_manifest() -> PluginManifest {
        PluginManifest {
            id: "com.example.basic".to_string(),
            version: "1.0.0".to_string(),
            name: "Basic Plugin".to_string(),
            description: "Test plugin".to_string(),
            author: Some("tester".to_string()),
            wasm_file: "plugin.wasm".to_string(),
            capabilities: super::super::manifest::PluginCapabilities {
                read_variables: true,
                write_variables: true,
                emit_events: false,
                http_access: false,
                fs_access: None,
                max_memory_pages: Some(64),
                max_fuel: Some(100_000),
            },
            node_types: vec![PluginNodeType {
                node_type: "plugin.echo".to_string(),
                label: "Echo".to_string(),
                input_schema: None,
                output_schema: None,
                handler: "handle_echo".to_string(),
            }],
            hooks: vec![super::super::manifest::PluginHook {
                hook_type: PluginHookType::BeforeWorkflowRun,
                handler: "on_before_run".to_string(),
            }],
        }
    }

    #[derive(Default)]
    struct TestVarStore {
        data: RwLock<HashMap<(String, String), Value>>,
    }

    impl TestVarStore {
        fn set_direct(&self, node: &str, key: &str, value: Value) {
            let mut guard = self.data.write().unwrap();
            guard.insert((node.to_string(), key.to_string()), value);
        }

        fn get_direct(&self, node: &str, key: &str) -> Option<Value> {
            let guard = self.data.read().unwrap();
            guard.get(&(node.to_string(), key.to_string())).cloned()
        }
    }

    impl VariableAccess for TestVarStore {
        fn get(&self, selector: &Value) -> Result<Value> {
            let (node, key) = selector_key(selector)?;
            Ok(self.get_direct(&node, &key).unwrap_or(Value::Null))
        }

        fn set(&self, selector: &Value, value: &Value) -> Result<()> {
            let (node, key) = selector_key(selector)?;
            self.set_direct(&node, &key, value.clone());
            Ok(())
        }
    }

    fn selector_key(selector: &Value) -> Result<(String, String)> {
        let arr = selector
            .as_array()
            .ok_or_else(|| anyhow!("selector must be an array"))?;
        if arr.len() != 2 {
            return Err(anyhow!("selector must have 2 elements"));
        }
        let node = arr[0]
            .as_str()
            .ok_or_else(|| anyhow!("selector node must be string"))?;
        let key = arr[1]
            .as_str()
            .ok_or_else(|| anyhow!("selector key must be string"))?;
        Ok((node.to_string(), key.to_string()))
    }

    #[test]
    fn test_plugin_host_var_get() {
        let wasm_bytes = crate::parse_wat_str(host_plugin_wat()).unwrap();
        let manifest = PluginManifest {
            id: "com.example.host".to_string(),
            version: "1.0.0".to_string(),
            name: "Host Plugin".to_string(),
            description: "Test plugin".to_string(),
            author: None,
            wasm_file: "plugin.wasm".to_string(),
            capabilities: super::super::manifest::PluginCapabilities {
                read_variables: true,
                write_variables: true,
                emit_events: false,
                http_access: false,
                fs_access: None,
                max_memory_pages: Some(64),
                max_fuel: Some(100_000),
            },
            node_types: vec![],
            hooks: vec![],
        };

        let runtime = PluginRuntime::new(manifest.clone(), &wasm_bytes).unwrap();
        let store = Arc::new(TestVarStore::default());
        store.set_direct("node1", "key1", Value::String("value1".to_string()));

        let state = PluginState::new(manifest.id.clone(), manifest.capabilities.clone())
            .with_variable_access(store);

        let output = runtime
            .call_function("handle_var_get", &Value::Null, state)
            .unwrap();
        assert_eq!(output, Value::String("value1".to_string()));
    }

    #[test]
    fn test_plugin_host_var_set() {
        let wasm_bytes = crate::parse_wat_str(host_plugin_wat()).unwrap();
        let manifest = PluginManifest {
            id: "com.example.host".to_string(),
            version: "1.0.0".to_string(),
            name: "Host Plugin".to_string(),
            description: "Test plugin".to_string(),
            author: None,
            wasm_file: "plugin.wasm".to_string(),
            capabilities: super::super::manifest::PluginCapabilities {
                read_variables: true,
                write_variables: true,
                emit_events: false,
                http_access: false,
                fs_access: None,
                max_memory_pages: Some(64),
                max_fuel: Some(100_000),
            },
            node_types: vec![],
            hooks: vec![],
        };

        let runtime = PluginRuntime::new(manifest.clone(), &wasm_bytes).unwrap();
        let store = Arc::new(TestVarStore::default());

        let state = PluginState::new(manifest.id.clone(), manifest.capabilities.clone())
            .with_variable_access(store.clone());

        let _ = runtime
            .call_function("handle_var_set", &Value::Null, state)
            .unwrap();

        assert_eq!(
            store.get_direct("node1", "key1"),
            Some(Value::String("new-value".to_string()))
        );
    }

    #[test]
    fn test_plugin_custom_node_and_hook() {
        let wasm_bytes = crate::parse_wat_str(basic_plugin_wat()).unwrap();
        let manifest = basic_manifest();
        let runtime = PluginRuntime::new(manifest.clone(), &wasm_bytes).unwrap();

        let node_type = &manifest.node_types[0];
        let state = PluginState::new(manifest.id.clone(), manifest.capabilities.clone());
        let result = runtime.execute_node(node_type, &Value::Null, state).unwrap();
        assert_eq!(result.get("echo"), Some(&Value::String("ok".to_string())));

        let hook_state = PluginState::new(manifest.id.clone(), manifest.capabilities.clone());
        let hook_output = runtime
            .execute_hook(PluginHookType::BeforeWorkflowRun, &Value::Null, hook_state)
            .unwrap();
        assert!(hook_output.is_some());
    }
}
