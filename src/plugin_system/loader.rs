use async_trait::async_trait;
use std::collections::HashMap;

use super::error::PluginError;
use super::traits::Plugin;

#[derive(Debug, Clone)]
pub struct PluginLoadSource {
    pub loader_type: String,
    pub params: HashMap<String, String>,
}

#[async_trait]
pub trait PluginLoader: Send + Sync {
    fn loader_type(&self) -> &str;

    async fn load(&self, source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError>;

    fn can_load(&self, source: &PluginLoadSource) -> bool {
        source.loader_type == self.loader_type()
    }
}
