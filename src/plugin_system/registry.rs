use std::collections::HashMap;
use std::sync::Arc;

use crate::core::runtime_context::{IdGenerator, TimeProvider};
use crate::llm::LlmProvider;
use crate::nodes::executor::NodeExecutor;
use crate::sandbox::{CodeLanguage, CodeSandbox};

use super::context::PluginContext;
use super::error::PluginError;
use super::extensions::{DslValidator, TemplateFunction};
use super::hooks::{HookHandler, HookPoint};
use super::loader::{PluginLoadSource, PluginLoader};
use super::traits::{Plugin, PluginCategory, PluginCapabilities, PluginMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginPhase {
    Uninitialized,
    Bootstrap,
    Normal,
    Ready,
}

pub struct PluginRegistry {
    plugins: HashMap<String, Arc<dyn Plugin>>,
    bootstrap_plugin_ids: Vec<String>,
    normal_plugin_ids: Vec<String>,
    loaders: HashMap<String, Arc<dyn PluginLoader>>,
    phase: PluginPhase,
    inner: PluginRegistryInner,
}

pub(crate) struct PluginRegistryInner {
    pub(crate) node_executors: HashMap<String, Box<dyn NodeExecutor>>,
    pub(crate) llm_providers: Vec<Arc<dyn LlmProvider>>,
    pub(crate) sandboxes: Vec<(CodeLanguage, Arc<dyn CodeSandbox>)>,
    pub(crate) hooks: HashMap<HookPoint, Vec<Arc<dyn HookHandler>>>,
    pub(crate) template_functions: HashMap<String, Arc<dyn TemplateFunction>>,
    pub(crate) dsl_validators: Vec<Arc<dyn DslValidator>>,
    pub(crate) custom_time_provider: Option<Arc<dyn TimeProvider>>,
    pub(crate) custom_id_generator: Option<Arc<dyn IdGenerator>>,
}

impl PluginRegistryInner {
    fn new() -> Self {
        Self {
            node_executors: HashMap::new(),
            llm_providers: Vec::new(),
            sandboxes: Vec::new(),
            hooks: HashMap::new(),
            template_functions: HashMap::new(),
            dsl_validators: Vec::new(),
            custom_time_provider: None,
            custom_id_generator: None,
        }
    }
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            bootstrap_plugin_ids: Vec::new(),
            normal_plugin_ids: Vec::new(),
            loaders: HashMap::new(),
            phase: PluginPhase::Uninitialized,
            inner: PluginRegistryInner::new(),
        }
    }

    pub fn register_loader(&mut self, loader: Arc<dyn PluginLoader>) {
        self.loaders
            .insert(loader.loader_type().to_string(), loader);
    }

    pub async fn run_bootstrap_phase(
        &mut self,
        bootstrap_sources: Vec<PluginLoadSource>,
        host_bootstrap_plugins: Vec<Box<dyn Plugin>>,
    ) -> Result<(), PluginError> {
        self.phase = PluginPhase::Bootstrap;

        for plugin in host_bootstrap_plugins {
            self.register_plugin(plugin, PluginPhase::Bootstrap).await?;
        }

        for source in bootstrap_sources {
            let loader = self
                .loaders
                .get(&source.loader_type)
                .ok_or_else(|| PluginError::InvalidConfig(format!(
                    "Loader not found: {}",
                    source.loader_type
                )))?
                .clone();
            let plugin = loader.load(&source).await?;
            self.register_plugin(plugin, PluginPhase::Bootstrap).await?;
        }

        Ok(())
    }

    pub async fn run_normal_phase(
        &mut self,
        normal_sources: Vec<PluginLoadSource>,
        host_normal_plugins: Vec<Box<dyn Plugin>>,
    ) -> Result<(), PluginError> {
        self.phase = PluginPhase::Normal;

        for plugin in host_normal_plugins {
            self.register_plugin(plugin, PluginPhase::Normal).await?;
        }

        for source in normal_sources {
            let loader = self
                .loaders
                .get(&source.loader_type)
                .ok_or_else(|| PluginError::InvalidConfig(format!(
                    "Loader not found: {}",
                    source.loader_type
                )))?
                .clone();
            let plugin = loader.load(&source).await?;
            self.register_plugin(plugin, PluginPhase::Normal).await?;
        }

        self.phase = PluginPhase::Ready;
        Ok(())
    }

    pub fn take_node_executors(&mut self) -> HashMap<String, Box<dyn NodeExecutor>> {
        std::mem::take(&mut self.inner.node_executors)
    }

    pub fn llm_providers(&self) -> &[Arc<dyn LlmProvider>] {
        &self.inner.llm_providers
    }

    pub fn sandboxes(&self) -> &[(CodeLanguage, Arc<dyn CodeSandbox>)] {
        &self.inner.sandboxes
    }

    pub fn hooks(&self, point: &HookPoint) -> Vec<Arc<dyn HookHandler>> {
        self.inner
            .hooks
            .get(point)
            .cloned()
            .unwrap_or_default()
    }

    pub fn plugin_metadata(&self) -> Vec<PluginMetadata> {
        self.plugins
            .values()
            .map(|plugin| plugin.metadata().clone())
            .collect()
    }

    pub fn template_functions(&self) -> &HashMap<String, Arc<dyn TemplateFunction>> {
        &self.inner.template_functions
    }

    pub fn dsl_validators(&self) -> &[Arc<dyn DslValidator>] {
        &self.inner.dsl_validators
    }

    pub fn custom_time_provider(&self) -> Option<Arc<dyn TimeProvider>> {
        self.inner.custom_time_provider.clone()
    }

    pub fn custom_id_generator(&self) -> Option<Arc<dyn IdGenerator>> {
        self.inner.custom_id_generator.clone()
    }

    pub async fn shutdown_all(&mut self) -> Result<(), PluginError> {
        for plugin_id in self.normal_plugin_ids.iter().rev() {
            if let Some(plugin) = self.plugins.get(plugin_id) {
                plugin.shutdown().await?;
            }
        }
        for plugin_id in self.bootstrap_plugin_ids.iter().rev() {
            if let Some(plugin) = self.plugins.get(plugin_id) {
                plugin.shutdown().await?;
            }
        }
        Ok(())
    }

    async fn register_plugin(
        &mut self,
        plugin: Box<dyn Plugin>,
        expected_phase: PluginPhase,
    ) -> Result<(), PluginError> {
        let metadata = plugin.metadata().clone();
        let category = metadata.category;
        let expected_category = match expected_phase {
            PluginPhase::Bootstrap => PluginCategory::Bootstrap,
            PluginPhase::Normal => PluginCategory::Normal,
            _ => PluginCategory::Normal,
        };
        if category != expected_category {
            return Err(PluginError::InvalidConfig(format!(
                "Plugin category mismatch: expected {:?}, got {:?}",
                expected_category, category
            )));
        }

        if self.plugins.contains_key(&metadata.id) {
            return Err(PluginError::ConflictError(format!(
                "Plugin '{}' already registered",
                metadata.id
            )));
        }

        let capabilities: Option<PluginCapabilities> = metadata.capabilities.clone();
        let mut context = PluginContext::new(
            expected_phase,
            &mut self.inner,
            if expected_phase == PluginPhase::Bootstrap {
                Some(&mut self.loaders)
            } else {
                None
            },
            metadata.id.clone(),
            capabilities,
        );

        plugin.register(&mut context).await?;

        let plugin = Arc::from(plugin);
        self.plugins.insert(metadata.id.clone(), plugin);
        match expected_phase {
            PluginPhase::Bootstrap => self.bootstrap_plugin_ids.push(metadata.id),
            PluginPhase::Normal => self.normal_plugin_ids.push(metadata.id),
            _ => {}
        }

        Ok(())
    }
}
