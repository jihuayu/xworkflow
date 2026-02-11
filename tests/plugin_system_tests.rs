#![cfg(feature = "wasm-runtime")]

use std::fs;

use tempfile::TempDir;

use xworkflow::plugin_system::builtins::{WasmBootstrapPlugin, WasmPluginConfig};
use xworkflow::plugin_system::wasm::{
    AllowedCapabilities,
    parse_wat_str,
    PluginCapabilities,
    PluginHook,
    PluginHookType,
    PluginManifest,
    PluginNodeType,
};
use xworkflow::plugin_system::{PluginLoadSource, PluginRegistry};

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

#[tokio::test]
async fn test_plugin_discover() {
    let temp = TempDir::new().unwrap();
    let plugin_dir = temp.path().join("com.example.basic");
    fs::create_dir_all(&plugin_dir).unwrap();

    let wasm_bytes = parse_wat_str(basic_plugin_wat()).unwrap();
    fs::write(plugin_dir.join("plugin.wasm"), wasm_bytes).unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&basic_manifest()).unwrap(),
    )
    .unwrap();

    let mut registry = PluginRegistry::new();
    let config = WasmPluginConfig {
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
    registry
        .run_bootstrap_phase(Vec::new(), vec![Box::new(WasmBootstrapPlugin::new(config))])
        .await
        .unwrap();

    let mut params = std::collections::HashMap::new();
    params.insert(
        "dir".to_string(),
        plugin_dir.to_string_lossy().into_owned(),
    );
    let sources = vec![PluginLoadSource {
        loader_type: "wasm".to_string(),
        params,
    }];

    registry
        .run_normal_phase(sources, Vec::new())
        .await
        .unwrap();

    let executors = registry.take_node_executors();
    assert!(executors.contains_key("plugin.echo"));
}

#[tokio::test]
async fn test_plugin_capability_deny() {
    let temp = TempDir::new().unwrap();
    let plugin_dir = temp.path().join("com.example.basic");
    fs::create_dir_all(&plugin_dir).unwrap();

    let wasm_bytes = parse_wat_str(basic_plugin_wat()).unwrap();
    fs::write(plugin_dir.join("plugin.wasm"), wasm_bytes).unwrap();
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&basic_manifest()).unwrap(),
    )
    .unwrap();

    let mut registry = PluginRegistry::new();
    let config = WasmPluginConfig {
        auto_discover: false,
        default_max_memory_pages: 64,
        default_max_fuel: 100_000,
        allowed_capabilities: AllowedCapabilities::default(),
    };
    registry
        .run_bootstrap_phase(Vec::new(), vec![Box::new(WasmBootstrapPlugin::new(config))])
        .await
        .unwrap();

    let mut params = std::collections::HashMap::new();
    params.insert(
        "dir".to_string(),
        plugin_dir.to_string_lossy().into_owned(),
    );
    let sources = vec![PluginLoadSource {
        loader_type: "wasm".to_string(),
        params,
    }];

    let result = registry.run_normal_phase(sources, Vec::new()).await;
    assert!(result.is_err());
}
