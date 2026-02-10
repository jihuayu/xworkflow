use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Plugin manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    pub wasm_file: String,
    pub capabilities: PluginCapabilities,
    #[serde(default)]
    pub node_types: Vec<PluginNodeType>,
    #[serde(default)]
    pub hooks: Vec<PluginHook>,
}

/// Plugin capabilities
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginCapabilities {
    #[serde(default)]
    pub read_variables: bool,
    #[serde(default)]
    pub write_variables: bool,
    #[serde(default)]
    pub emit_events: bool,
    #[serde(default)]
    pub http_access: bool,
    #[serde(default)]
    pub fs_access: Option<Vec<String>>,
    #[serde(default)]
    pub max_memory_pages: Option<u32>,
    #[serde(default)]
    pub max_fuel: Option<u64>,
}

/// Plugin node type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginNodeType {
    pub node_type: String,
    pub label: String,
    #[serde(default)]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub output_schema: Option<Value>,
    pub handler: String,
}

/// Plugin hook
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHook {
    pub hook_type: PluginHookType,
    pub handler: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PluginHookType {
    BeforeWorkflowRun,
    AfterWorkflowRun,
    BeforeNodeExecute,
    AfterNodeExecute,
    BeforeVariableWrite,
}

/// Allowed capability whitelist
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AllowedCapabilities {
    #[serde(default)]
    pub read_variables: bool,
    #[serde(default)]
    pub write_variables: bool,
    #[serde(default)]
    pub emit_events: bool,
    #[serde(default)]
    pub http_access: bool,
    #[serde(default)]
    pub fs_access: bool,
}
