use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::variable_pool::Selector;

pub type VariableSelector = Selector;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Conversation,
    User,
    Global,
}

fn default_memory_scope() -> MemoryScope { MemoryScope::Conversation }
fn default_top_k() -> Option<usize> { Some(10) }

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EnhancedMemoryConfig {
    pub enabled: bool,
    #[serde(default = "default_memory_scope")]
    pub scope: MemoryScope,
    #[serde(default)]
    pub query_selector: Option<VariableSelector>,
    #[serde(default = "default_top_k")]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub filter: Option<Value>,
    #[serde(default)]
    pub injection_position: MemoryInjectionPosition,
    #[serde(default)]
    pub format_template: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryInjectionPosition {
    BeforeSystem,
    #[default]
    AfterSystem,
    BeforeUser,
}

fn default_recall_top_k() -> usize { 5 }

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MemoryRecallNodeData {
    pub scope: MemoryScope,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub query_selector: Option<VariableSelector>,
    #[serde(default = "default_recall_top_k")]
    pub top_k: usize,
    #[serde(default)]
    pub filter: Option<Value>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MemoryStoreNodeData {
    pub scope: MemoryScope,
    pub key: String,
    pub value_selector: VariableSelector,
    #[serde(default)]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(default)]
    pub metadata_selectors: Option<HashMap<String, VariableSelector>>,
}
