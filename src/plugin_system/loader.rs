//! Plugin loader trait and source descriptor.

use async_trait::async_trait;
use std::collections::HashMap;

use super::error::PluginError;
use super::traits::Plugin;

/// Descriptor telling the loader where/how to find a plugin.
#[derive(Debug, Clone)]
pub struct PluginLoadSource {
    /// Identifies which loader handles this source (e.g. `"dll"`, `"wasm"`).
    pub loader_type: String,
    /// Loader-specific parameters (e.g. file path, URL).
    pub params: HashMap<String, String>,
}

/// Trait for loading plugins from external sources.
#[async_trait]
pub trait PluginLoader: Send + Sync {
    /// The loader type string this implementation handles.
    fn loader_type(&self) -> &str;

    /// Load a plugin from the given source descriptor.
    async fn load(&self, source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError>;

    /// Whether this loader can handle the given source (default: matches `loader_type`).
    fn can_load(&self, source: &PluginLoadSource) -> bool {
        source.loader_type == self.loader_type()
    }
}
