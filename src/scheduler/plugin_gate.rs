use async_trait::async_trait;

#[cfg(feature = "plugin-system")]
use std::sync::Arc;

use crate::core::runtime_context::RuntimeContext;
use crate::dsl::schema::WorkflowSchema;
use crate::dsl::validation::ValidationReport;
use crate::error::WorkflowError;
use crate::llm::LlmProviderRegistry;
use crate::nodes::executor::NodeExecutorRegistry;

#[async_trait]
pub trait SchedulerPluginGate: Send {
  async fn init_and_extend_validation(
    &mut self,
    schema: &WorkflowSchema,
    report: &mut ValidationReport,
  ) -> Result<(), WorkflowError>;

  async fn after_dsl_validation(&self, report: &ValidationReport) -> Result<(), WorkflowError>;

  fn apply_node_executors(&mut self, registry: &mut NodeExecutorRegistry);

  fn apply_llm_providers(&self, llm_registry: &mut LlmProviderRegistry);

  fn customize_context(&self, context: &mut RuntimeContext);

  #[cfg(feature = "plugin-system")]
  fn take_plugin_registry_arc(&mut self) -> Option<Arc<crate::plugin_system::PluginRegistry>>;

  #[cfg(feature = "plugin-system")]
  fn set_plugin_config(&mut self, config: crate::plugin_system::PluginSystemConfig);

  #[cfg(feature = "plugin-system")]
  fn add_bootstrap_plugin(&mut self, plugin: Box<dyn crate::plugin_system::Plugin>);

  #[cfg(feature = "plugin-system")]
  fn add_plugin(&mut self, plugin: Box<dyn crate::plugin_system::Plugin>);

}

#[cfg(not(feature = "plugin-system"))]
#[derive(Debug, Default)]
struct NoopSchedulerPluginGate;

#[cfg(not(feature = "plugin-system"))]
#[async_trait]
impl SchedulerPluginGate for NoopSchedulerPluginGate {
  async fn init_and_extend_validation(
    &mut self,
    _schema: &WorkflowSchema,
    _report: &mut ValidationReport,
  ) -> Result<(), WorkflowError> {
    Ok(())
  }

  async fn after_dsl_validation(&self, _report: &ValidationReport) -> Result<(), WorkflowError> {
    Ok(())
  }

  fn apply_node_executors(&mut self, _registry: &mut NodeExecutorRegistry) {}

  fn apply_llm_providers(&self, _llm_registry: &mut LlmProviderRegistry) {}

  fn customize_context(&self, _context: &mut RuntimeContext) {}
}

#[cfg(feature = "plugin-system")]
mod real {
  use super::*;

  use crate::dsl::validation::DiagnosticLevel;
  use crate::plugin_system::{
    HookPayload, HookPoint, Plugin, PluginCategory, PluginLoadSource, PluginRegistry,
    PluginSystemConfig,
  };
  use crate::plugin_system::loaders::{DllPluginLoader, HostPluginLoader};

  #[derive(Default)]
  pub struct RealSchedulerPluginGate {
    plugin_system_config: Option<PluginSystemConfig>,
    host_bootstrap_plugins: Vec<Box<dyn Plugin>>,
    host_normal_plugins: Vec<Box<dyn Plugin>>,
    plugin_registry: Option<PluginRegistry>,
  }

  impl std::fmt::Debug for RealSchedulerPluginGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.debug_struct("RealSchedulerPluginGate")
        .field("has_config", &self.plugin_system_config.is_some())
        .field("bootstrap_plugins", &self.host_bootstrap_plugins.len())
        .field("normal_plugins", &self.host_normal_plugins.len())
        .field("has_registry", &self.plugin_registry.is_some())
        .finish()
    }
  }

  impl RealSchedulerPluginGate {
    fn should_init(&self) -> bool {
      self.plugin_system_config.is_some()
        || !self.host_bootstrap_plugins.is_empty()
        || !self.host_normal_plugins.is_empty()
    }

    fn collect_bootstrap_sources(&self) -> Vec<PluginLoadSource> {
      let mut sources = Vec::new();
      if let Some(config) = &self.plugin_system_config {
        for path in &config.bootstrap_dll_paths {
          sources.push(PluginLoadSource {
            loader_type: "dll".into(),
            params: [(
              "path".into(),
              path.to_string_lossy().into_owned(),
            )]
            .into_iter()
            .collect(),
          });
        }
      }
      sources
    }

    fn collect_normal_sources(&self) -> Vec<PluginLoadSource> {
      let mut sources = Vec::new();
      if let Some(config) = &self.plugin_system_config {
        for path in &config.normal_dll_paths {
          sources.push(PluginLoadSource {
            loader_type: "dll".into(),
            params: [(
              "path".into(),
              path.to_string_lossy().into_owned(),
            )]
            .into_iter()
            .collect(),
          });
        }
        sources.extend(config.normal_load_sources.clone());
      }
      sources
    }

    async fn execute_registry_hooks(
      registry: &PluginRegistry,
      hook_point: HookPoint,
      data: serde_json::Value,
    ) -> Result<(), WorkflowError> {
      let mut handlers = registry.hooks(&hook_point);
      if handlers.is_empty() {
        return Ok(());
      }

      handlers.sort_by_key(|h| h.priority());
      let payload = HookPayload {
        hook_point,
        data,
        variable_pool: None,
        event_tx: None,
      };

      for handler in handlers {
        if let Err(err) = handler.handle(&payload).await {
          tracing::warn!(
            plugin_id = %handler.name(),
            error = %err,
            "plugin hook failed"
          );
        }
      }

      Ok(())
    }

    async fn init_plugins(&mut self) -> Result<(), crate::plugin_system::PluginError> {
      let mut registry = PluginRegistry::new();
      registry.register_loader(Arc::new(DllPluginLoader::new()));
      registry.register_loader(Arc::new(HostPluginLoader));

      let bootstrap_sources = self.collect_bootstrap_sources();
      let mut bootstrap_plugins: Vec<Box<dyn Plugin>> = Vec::new();

      #[cfg(feature = "builtin-sandbox-js")]
      {
        let (boot, lang) = crate::plugin_system::builtins::create_js_sandbox_plugins(
          crate::sandbox::BuiltinSandboxConfig::default(),
        );
        bootstrap_plugins.push(Box::new(boot));
        self.host_normal_plugins.insert(0, Box::new(lang));
      }

      #[cfg(feature = "builtin-sandbox-wasm")]
      {
        let (boot, lang) = crate::plugin_system::builtins::create_wasm_sandbox_plugins(
          crate::sandbox::WasmSandboxConfig::default(),
        );
        bootstrap_plugins.push(Box::new(boot));
        self.host_normal_plugins.insert(0, Box::new(lang));
      }

      #[cfg(feature = "builtin-template-jinja")]
      {
        bootstrap_plugins.push(Box::new(
          crate::plugin_system::builtins::JinjaTemplatePlugin::new(),
        ));
      }

      bootstrap_plugins.extend(std::mem::take(&mut self.host_bootstrap_plugins));
      registry
        .run_bootstrap_phase(bootstrap_sources, bootstrap_plugins)
        .await?;

      let normal_sources = self.collect_normal_sources();
      let normal_plugins = std::mem::take(&mut self.host_normal_plugins);
      registry
        .run_normal_phase(normal_sources, normal_plugins)
        .await?;

      self.plugin_registry = Some(registry);
      Ok(())
    }
  }

  #[async_trait]
  impl SchedulerPluginGate for RealSchedulerPluginGate {
    async fn init_and_extend_validation(
      &mut self,
      schema: &WorkflowSchema,
      report: &mut ValidationReport,
    ) -> Result<(), WorkflowError> {
      if !self.should_init() {
        return Ok(());
      }

      self
        .init_plugins()
        .await
        .map_err(|e| WorkflowError::InternalError(e.to_string()))?;

      if let Some(reg) = &self.plugin_registry {
        let plugin_ids = reg
          .plugin_metadata()
          .into_iter()
          .map(|meta| meta.id)
          .collect::<Vec<_>>();
        let payload = serde_json::json!({
          "event": "after_plugin_loaded",
          "plugins": plugin_ids,
        });
        Self::execute_registry_hooks(reg, HookPoint::AfterPluginLoaded, payload).await?;
      }

      if let Some(reg) = &self.plugin_registry {
        let mut extra = Vec::new();
        for validator in reg.dsl_validators() {
          extra.extend(validator.validate(schema));
        }
        if !extra.is_empty() {
          report.diagnostics.extend(extra);
          report.is_valid = report
            .diagnostics
            .iter()
            .all(|d| d.level != DiagnosticLevel::Error);
        }
      }

      Ok(())
    }

    async fn after_dsl_validation(&self, report: &ValidationReport) -> Result<(), WorkflowError> {
      if let Some(reg) = &self.plugin_registry {
        let payload = serde_json::json!({
          "event": "after_dsl_validation",
          "report": report,
        });
        Self::execute_registry_hooks(reg, HookPoint::AfterDslValidation, payload).await?;
      }
      Ok(())
    }

    fn apply_node_executors(&mut self, registry: &mut NodeExecutorRegistry) {
      if let Some(reg) = self.plugin_registry.as_mut() {
        registry.apply_plugin_executors(reg.take_node_executors());
      }
    }

    fn apply_llm_providers(&self, llm_registry: &mut LlmProviderRegistry) {
      if let Some(reg) = &self.plugin_registry {
        llm_registry.apply_plugin_providers(reg.llm_providers());
      }
    }

    fn customize_context(&self, context: &mut RuntimeContext) {
      if let Some(reg) = &self.plugin_registry {
        if let Some(tp) = reg.custom_time_provider() {
          context.time_provider = tp;
        }
        if let Some(id_gen) = reg.custom_id_generator() {
          context.id_generator = id_gen;
        }
        if !reg.template_functions().is_empty() {
          context.extensions.template_functions =
            Some(Arc::new(reg.template_functions().clone()));
        }
      }
    }

    fn take_plugin_registry_arc(&mut self) -> Option<Arc<PluginRegistry>> {
      self.plugin_registry.take().map(Arc::new)
    }

    fn set_plugin_config(&mut self, config: PluginSystemConfig) {
      self.plugin_system_config = Some(config);
    }

    fn add_bootstrap_plugin(&mut self, plugin: Box<dyn Plugin>) {
      assert_eq!(plugin.metadata().category, PluginCategory::Bootstrap);
      self.host_bootstrap_plugins.push(plugin);
    }

    fn add_plugin(&mut self, plugin: Box<dyn Plugin>) {
      assert_eq!(plugin.metadata().category, PluginCategory::Normal);
      self.host_normal_plugins.push(plugin);
    }

  }

  pub fn new_gate() -> Box<dyn SchedulerPluginGate> {
    Box::new(RealSchedulerPluginGate::default())
  }
}

#[cfg(feature = "plugin-system")]
pub use real::new_gate as new_scheduler_plugin_gate;

#[cfg(not(feature = "plugin-system"))]
pub fn new_scheduler_plugin_gate() -> Box<dyn SchedulerPluginGate> {
  Box::new(NoopSchedulerPluginGate)
}
