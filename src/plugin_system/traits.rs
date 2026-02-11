use async_trait::async_trait;
use std::any::Any;
use std::path::PathBuf;

use super::error::PluginError;

/// Category of a plugin, determining its initialization phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginCategory {
    /// Loaded first; may register sandboxes and template engines.
    Bootstrap,
    /// Loaded after bootstrap; may register nodes, hooks, providers.
    Normal,
}

/// Origin of a plugin binary or module.
#[derive(Debug, Clone)]
pub enum PluginSource {
    /// Loaded from a dynamic library at the given path.
    Dll { path: PathBuf },
    /// Compiled into the host binary.
    Host,
    /// Loaded via a custom [`PluginLoader`](super::PluginLoader).
    Custom { loader_type: String, detail: String },
}

/// Capability flags for sandboxed (non-fully-trusted) plugins.
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

/// Metadata describing a plugin: identity, version, category, and capabilities.
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
    /// Return the plugin’s capability set, if specified.
    pub fn capabilities(&self) -> Option<&PluginCapabilities> {
        self.capabilities.as_ref()
    }
}

/// The core plugin trait.
///
/// Every plugin must provide [`metadata()`](Self::metadata) and
/// [`register()`](Self::register). Optionally override
/// [`shutdown()`](Self::shutdown) for cleanup.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Return this plugin’s metadata.
    fn metadata(&self) -> &PluginMetadata;

    /// Called during initialization. Use `context` to register extensions.
    async fn register(&self, context: &mut super::context::PluginContext) -> Result<(), PluginError>;

    /// Called when the plugin system is shutting down.
    async fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }

    /// Downcast helper for concrete plugin types.
    fn as_any(&self) -> &dyn Any;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_plugin_category_eq() {
        assert_eq!(PluginCategory::Bootstrap, PluginCategory::Bootstrap);
        assert_eq!(PluginCategory::Normal, PluginCategory::Normal);
        assert_ne!(PluginCategory::Bootstrap, PluginCategory::Normal);
    }

    #[test]
    fn test_plugin_source_dll() {
        let source = PluginSource::Dll { path: PathBuf::from("/lib/plugin.so") };
        match &source {
            PluginSource::Dll { path } => assert_eq!(path, &PathBuf::from("/lib/plugin.so")),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_plugin_source_host() {
        let source = PluginSource::Host;
        assert!(matches!(source, PluginSource::Host));
    }

    #[test]
    fn test_plugin_source_custom() {
        let source = PluginSource::Custom {
            loader_type: "wasm".into(),
            detail: "path.wasm".into(),
        };
        match &source {
            PluginSource::Custom { loader_type, detail } => {
                assert_eq!(loader_type, "wasm");
                assert_eq!(detail, "path.wasm");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_plugin_capabilities_default() {
        let caps = PluginCapabilities::default();
        assert!(!caps.read_variables);
        assert!(!caps.write_variables);
        assert!(!caps.emit_events);
        assert!(!caps.http_access);
        assert!(caps.fs_access.is_none());
        assert!(caps.max_memory_pages.is_none());
        assert!(caps.max_fuel.is_none());
        assert!(!caps.register_nodes);
        assert!(!caps.register_llm_providers);
        assert!(!caps.register_hooks);
    }

    #[test]
    fn test_plugin_metadata_capabilities() {
        let meta = PluginMetadata {
            id: "test".into(),
            name: "Test".into(),
            version: "1.0".into(),
            category: PluginCategory::Normal,
            description: "test plugin".into(),
            source: PluginSource::Host,
            capabilities: None,
        };
        assert!(meta.capabilities().is_none());

        let meta2 = PluginMetadata {
            capabilities: Some(PluginCapabilities {
                read_variables: true,
                ..PluginCapabilities::default()
            }),
            ..meta
        };
        assert!(meta2.capabilities().unwrap().read_variables);
    }
}
