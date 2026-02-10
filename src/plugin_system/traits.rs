use async_trait::async_trait;
use std::any::Any;
use std::path::PathBuf;

use super::error::PluginError;

/// 插件类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginCategory {
    Bootstrap,
    Normal,
}

/// 插件来源标识
#[derive(Debug, Clone)]
pub enum PluginSource {
    Dll { path: PathBuf },
    Host,
    Custom { loader_type: String, detail: String },
}

/// 插件能力（用于非完全信任插件）
#[derive(Debug, Clone, Default)]
pub struct PluginCapabilities {
    pub read_variables: bool,
    pub write_variables: bool,
    pub emit_events: bool,
    pub http_access: bool,
    pub fs_access: Option<Vec<String>>,
    pub max_memory_pages: Option<u32>,
    pub max_fuel: Option<u64>,
    pub register_nodes: bool,
    pub register_llm_providers: bool,
    pub register_hooks: bool,
}

/// 插件元数据
#[derive(Debug, Clone)]
pub struct PluginMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub category: PluginCategory,
    pub description: String,
    pub source: PluginSource,
    pub capabilities: Option<PluginCapabilities>,
}

impl PluginMetadata {
    pub fn capabilities(&self) -> Option<&PluginCapabilities> {
        self.capabilities.as_ref()
    }
}

/// 统一插件接口
#[async_trait]
pub trait Plugin: Send + Sync {
    fn metadata(&self) -> &PluginMetadata;

    async fn register(&self, context: &mut super::context::PluginContext) -> Result<(), PluginError>;

    async fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }

    fn as_any(&self) -> &dyn Any;
}
