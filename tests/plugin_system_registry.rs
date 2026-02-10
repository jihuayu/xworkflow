#![cfg(feature = "plugin-system")]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tempfile::TempDir;

use xworkflow::dsl::schema::{NodeRunResult, WorkflowNodeExecutionStatus};
use xworkflow::error::NodeError;
use xworkflow::nodes::executor::NodeExecutor;
use xworkflow::plugin_system::wasm::{
    PluginCapabilities, PluginManifest, PluginNodeType,
};
use xworkflow::plugin_system::builtins::{WasmBootstrapPlugin, WasmPluginConfig};
use xworkflow::plugin_system::loaders::{DllPluginLoader, HostPluginLoader};
use xworkflow::plugin_system::{
    Plugin,
    PluginCategory,
    PluginContext,
    PluginError,
    PluginLoadSource,
    PluginLoader,
    PluginMetadata,
    PluginRegistry,
    PluginSource,
};

fn base_metadata(id: &str, name: &str, category: PluginCategory) -> PluginMetadata {
    PluginMetadata {
        id: id.to_string(),
        name: name.to_string(),
        version: "0.1.0".to_string(),
        category,
        description: format!("test plugin {}", name),
        source: PluginSource::Host,
        capabilities: None,
    }
}

struct LoaderBootstrapPlugin {
    metadata: PluginMetadata,
}

impl LoaderBootstrapPlugin {
    fn new() -> Self {
        Self {
            metadata: base_metadata(
                "test.bootstrap.loader",
                "Loader Bootstrap",
                PluginCategory::Bootstrap,
            ),
        }
    }
}

#[async_trait]
impl Plugin for LoaderBootstrapPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_plugin_loader(Arc::new(TestLoader))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct TestLoader;

#[async_trait]
impl PluginLoader for TestLoader {
    fn loader_type(&self) -> &str {
        "test"
    }

    async fn load(&self, _source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError> {
        Ok(Box::new(LoadedPlugin::new()))
    }
}

struct LoadedPlugin {
    metadata: PluginMetadata,
}

impl LoadedPlugin {
    fn new() -> Self {
        Self {
            metadata: base_metadata("test.loaded.plugin", "Loaded Plugin", PluginCategory::Normal),
        }
    }
}

#[async_trait]
impl Plugin for LoadedPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_node_executor("test.node", Box::new(TestNodeExecutor))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct TestNodeExecutor;

#[async_trait]
impl NodeExecutor for TestNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        _config: &serde_json::Value,
        _variable_pool: &xworkflow::core::variable_pool::VariablePool,
        _context: &xworkflow::RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn test_bootstrap_registers_loader_and_loads_plugin() {
    let mut registry = PluginRegistry::new();
    registry.register_loader(Arc::new(DllPluginLoader::new()));
    registry.register_loader(Arc::new(HostPluginLoader));

    registry
        .run_bootstrap_phase(Vec::new(), vec![Box::new(LoaderBootstrapPlugin::new())])
        .await
        .expect("bootstrap phase failed");

    let mut params = HashMap::new();
    params.insert("noop".to_string(), "true".to_string());
    let sources = vec![PluginLoadSource {
        loader_type: "test".to_string(),
        params,
    }];

    registry
        .run_normal_phase(sources, Vec::new())
        .await
        .expect("normal phase failed");

    let executors = registry.take_node_executors();
    assert!(executors.contains_key("test.node"));
}

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
)"#
}

#[tokio::test]
async fn test_wasm_bootstrap_loader() {
    let temp = TempDir::new().expect("temp dir");
    let plugin_dir = temp.path().join("com.example.echo");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");

    let wasm_bytes = wat::parse_str(basic_plugin_wat()).expect("parse wat");
    std::fs::write(plugin_dir.join("plugin.wasm"), wasm_bytes).expect("write wasm");

    let manifest = PluginManifest {
        id: "com.example.echo".to_string(),
        version: "1.0.0".to_string(),
        name: "Echo Plugin".to_string(),
        description: "Test plugin".to_string(),
        author: Some("tests".to_string()),
        wasm_file: "plugin.wasm".to_string(),
        capabilities: PluginCapabilities {
            read_variables: false,
            write_variables: false,
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
        hooks: vec![],
    };

    std::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");

    let mut registry = PluginRegistry::new();
    registry.register_loader(Arc::new(DllPluginLoader::new()));
    registry.register_loader(Arc::new(HostPluginLoader));

    registry
        .run_bootstrap_phase(
            Vec::new(),
            vec![Box::new(WasmBootstrapPlugin::new(WasmPluginConfig::default()))],
        )
        .await
        .expect("bootstrap phase failed");

    let mut params = HashMap::new();
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
        .expect("normal phase failed");

    let executors = registry.take_node_executors();
    assert!(executors.contains_key("plugin.echo"));
}