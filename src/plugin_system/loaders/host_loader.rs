use async_trait::async_trait;

use super::super::error::PluginError;
use super::super::loader::{PluginLoadSource, PluginLoader};
use super::super::traits::Plugin;

pub struct HostPluginLoader;

#[async_trait]
impl PluginLoader for HostPluginLoader {
    fn loader_type(&self) -> &str {
        "host"
    }

    async fn load(&self, _source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError> {
        Err(PluginError::InvalidConfig(
            "HostPluginLoader does not use load(). Use builder API directly.".into(),
        ))
    }
}
