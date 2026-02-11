//! Configuration for the plugin system.

use std::path::PathBuf;

use super::loader::PluginLoadSource;

/// Configuration specifying which plugins to load and how.
#[derive(Debug, Clone, Default)]
pub struct PluginSystemConfig {
    /// Paths to bootstrap-phase dynamic libraries.
    pub bootstrap_dll_paths: Vec<PathBuf>,
    /// Paths to normal-phase dynamic libraries.
    pub normal_dll_paths: Vec<PathBuf>,
    /// Normal-phase plugins loaded via custom loaders.
    pub normal_load_sources: Vec<PluginLoadSource>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_system_config_default() {
        let config = PluginSystemConfig::default();
        assert!(config.bootstrap_dll_paths.is_empty());
        assert!(config.normal_dll_paths.is_empty());
        assert!(config.normal_load_sources.is_empty());
    }

    #[test]
    fn test_plugin_system_config_with_paths() {
        let config = PluginSystemConfig {
            bootstrap_dll_paths: vec![PathBuf::from("/lib/bootstrap.so")],
            normal_dll_paths: vec![PathBuf::from("/lib/normal.so")],
            normal_load_sources: vec![PluginLoadSource {
                loader_type: "dll".into(),
                params: std::collections::HashMap::new(),
            }],
        };
        assert_eq!(config.bootstrap_dll_paths.len(), 1);
        assert_eq!(config.normal_dll_paths.len(), 1);
        assert_eq!(config.normal_load_sources.len(), 1);
    }
}
