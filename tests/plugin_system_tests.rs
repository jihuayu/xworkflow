use std::fs;
use std::sync::{Arc, RwLock};

use serde_json::Value;
use tempfile::TempDir;

use xworkflow::core::variable_pool::{Segment, VariablePool};
use xworkflow::plugin::{
    AllowedCapabilities, PluginCapabilities, PluginHook, PluginHookType, PluginManifest,
    PluginManager, PluginManagerConfig, PluginNodeType, PluginRuntime, PluginState,
};

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
        capabilities: PluginCapabilities {
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
        hooks: vec![PluginHook {
            hook_type: PluginHookType::BeforeWorkflowRun,
            handler: "on_before_run".to_string(),
        }],
    }
}

#[test]
fn test_plugin_load_manifest() {
    let manifest = basic_manifest();
    assert_eq!(manifest.id, "com.example.basic");
    assert_eq!(manifest.node_types.len(), 1);
}

#[test]
fn test_plugin_discover() {
    let temp = TempDir::new().unwrap();
    let plugin_dir = temp.path().join("com.example.basic");
    fs::create_dir_all(&plugin_dir).unwrap();

    let wasm_bytes = wat::parse_str(basic_plugin_wat()).unwrap();
    fs::write(plugin_dir.join("plugin.wasm"), wasm_bytes).unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&basic_manifest()).unwrap(),
    )
    .unwrap();

    let config = PluginManagerConfig {
        plugin_dir: temp.path().to_path_buf(),
        auto_discover: false,
        default_max_memory_pages: 64,
        default_max_fuel: 100_000,
        allowed_capabilities: AllowedCapabilities {
            read_variables: true,
            write_variables: true,
            emit_events: false,
            http_access: false,
            fs_access: false,
        },
    };

    let mut manager = PluginManager::new(config).unwrap();
    let loaded = manager.discover_and_load().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(manager.list_plugins().len(), 1);
}

#[test]
fn test_plugin_capability_deny() {
    let temp = TempDir::new().unwrap();
    let plugin_dir = temp.path().join("com.example.basic");
    fs::create_dir_all(&plugin_dir).unwrap();

    let wasm_bytes = wat::parse_str(basic_plugin_wat()).unwrap();
    fs::write(plugin_dir.join("plugin.wasm"), wasm_bytes).unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&basic_manifest()).unwrap(),
    )
    .unwrap();

    let config = PluginManagerConfig {
        plugin_dir: temp.path().to_path_buf(),
        auto_discover: false,
        default_max_memory_pages: 64,
        default_max_fuel: 100_000,
        allowed_capabilities: AllowedCapabilities::default(),
    };

    let mut manager = PluginManager::new(config).unwrap();
    let result = manager.load_plugin(&plugin_dir);
    assert!(result.is_err());
}

#[test]
fn test_plugin_host_var_get() {
    let wasm_bytes = wat::parse_str(host_plugin_wat()).unwrap();
    let manifest = PluginManifest {
        id: "com.example.host".to_string(),
        version: "1.0.0".to_string(),
        name: "Host Plugin".to_string(),
        description: "Test plugin".to_string(),
        author: None,
        wasm_file: "plugin.wasm".to_string(),
        capabilities: PluginCapabilities {
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
    let mut pool = VariablePool::new();
    pool.set(
        &["node1".to_string(), "key1".to_string()],
        Segment::String("value1".to_string()),
    );

    let state = PluginState {
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
        variable_pool: Some(Arc::new(RwLock::new(pool))),
        event_tx: None,
        capabilities: manifest.capabilities.clone(),
        limits: wasmtime::StoreLimitsBuilder::new().build(),
        plugin_id: manifest.id.clone(),
        host_buffers: Vec::new(),
    };

    let output = runtime
        .call_function("handle_var_get", &Value::Null, state)
        .unwrap();
    assert_eq!(output, Value::String("value1".to_string()));
}

#[test]
fn test_plugin_host_var_set() {
    let wasm_bytes = wat::parse_str(host_plugin_wat()).unwrap();
    let manifest = PluginManifest {
        id: "com.example.host".to_string(),
        version: "1.0.0".to_string(),
        name: "Host Plugin".to_string(),
        description: "Test plugin".to_string(),
        author: None,
        wasm_file: "plugin.wasm".to_string(),
        capabilities: PluginCapabilities {
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
    let pool = Arc::new(RwLock::new(VariablePool::new()));

    let state = PluginState {
        wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
        variable_pool: Some(pool.clone()),
        event_tx: None,
        capabilities: manifest.capabilities.clone(),
        limits: wasmtime::StoreLimitsBuilder::new().build(),
        plugin_id: manifest.id.clone(),
        host_buffers: Vec::new(),
    };

    let _ = runtime
        .call_function("handle_var_set", &Value::Null, state)
        .unwrap();

    let pool_guard = pool.read().unwrap();
    let value = pool_guard.get(&["node1".to_string(), "key1".to_string()]);
    assert_eq!(value.to_value(), Value::String("new-value".to_string()));
}

#[tokio::test]
async fn test_plugin_custom_node_and_hook() {
    let wasm_bytes = wat::parse_str(basic_plugin_wat()).unwrap();
    let manifest = basic_manifest();
    let runtime = PluginRuntime::new(manifest.clone(), &wasm_bytes).unwrap();

    let node_type = &manifest.node_types[0];
    let pool = VariablePool::new();
    let context = xworkflow::RuntimeContext::default();
    let result = runtime
        .execute_node(node_type, &Value::Null, &pool, &context)
        .await
        .unwrap();
    assert_eq!(result.outputs.get("echo"), Some(&Value::String("ok".to_string())));

    let hook_output = runtime
        .execute_hook(
            PluginHookType::BeforeWorkflowRun,
            &Value::Null,
            Arc::new(RwLock::new(VariablePool::new())),
            None,
        )
        .await
        .unwrap();
    assert!(hook_output.is_some());
}

#[test]
fn test_plugin_unload() {
    let temp = TempDir::new().unwrap();
    let plugin_dir = temp.path().join("com.example.basic");
    fs::create_dir_all(&plugin_dir).unwrap();

    let wasm_bytes = wat::parse_str(basic_plugin_wat()).unwrap();
    fs::write(plugin_dir.join("plugin.wasm"), wasm_bytes).unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&basic_manifest()).unwrap(),
    )
    .unwrap();

    let config = PluginManagerConfig {
        plugin_dir: temp.path().to_path_buf(),
        auto_discover: false,
        default_max_memory_pages: 64,
        default_max_fuel: 100_000,
        allowed_capabilities: AllowedCapabilities {
            read_variables: true,
            write_variables: true,
            emit_events: false,
            http_access: false,
            fs_access: false,
        },
    };

    let mut manager = PluginManager::new(config).unwrap();
    let plugin_id = manager.load_plugin(&plugin_dir).unwrap();
    manager.unload_plugin(&plugin_id).unwrap();
    assert!(manager.list_plugins().is_empty());
}
