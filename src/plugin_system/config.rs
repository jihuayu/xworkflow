use std::path::PathBuf;

use super::loader::PluginLoadSource;

#[derive(Debug, Clone, Default)]
pub struct PluginSystemConfig {
    pub bootstrap_dll_paths: Vec<PathBuf>,
    pub normal_dll_paths: Vec<PathBuf>,
    pub normal_load_sources: Vec<PluginLoadSource>,
}
