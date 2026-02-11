//! Plugin context provided to plugins during registration.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::runtime_context::{IdGenerator, TimeProvider};
use crate::llm::LlmProvider;
use crate::nodes::executor::NodeExecutor;
use crate::sandbox::{CodeLanguage, CodeSandbox};

use super::error::PluginError;
use super::extensions::{DslValidator, TemplateFunction};
use xworkflow_types::template::TemplateEngine;
use super::hooks::{HookHandler, HookPoint};
use super::loader::PluginLoader;
use super::registry::{PluginPhase, PluginRegistryInner};
use super::traits::PluginCapabilities;

/// Mutable context passed to [`Plugin::register()`](super::Plugin::register).
///
/// Provides methods for registering node executors, LLM providers, sandboxes,
/// hooks, template functions, DSL validators, and other extensions.
pub struct PluginContext<'a> {
    phase: PluginPhase,
    registry_inner: &'a mut PluginRegistryInner,
    loaders: Option<&'a mut HashMap<String, Arc<dyn PluginLoader>>>,
    plugin_id: String,
    capabilities: Option<PluginCapabilities>,
}

impl<'a> PluginContext<'a> {
    pub(crate) fn new(
        phase: PluginPhase,
        registry_inner: &'a mut PluginRegistryInner,
        loaders: Option<&'a mut HashMap<String, Arc<dyn PluginLoader>>>,
        plugin_id: String,
        capabilities: Option<PluginCapabilities>,
    ) -> Self {
        Self {
            phase,
            registry_inner,
            loaders,
            plugin_id,
            capabilities,
        }
    }

    /// Return the current plugin lifecycle phase.
    pub fn phase(&self) -> PluginPhase {
        self.phase
    }

    /// Return the id of the plugin currently being registered.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Emit a structured log line tagged with the plugin id.
    pub fn log(&self, level: tracing::Level, message: &str) {
        match level {
            tracing::Level::TRACE => tracing::trace!(plugin_id = %self.plugin_id, message = %message),
            tracing::Level::DEBUG => tracing::debug!(plugin_id = %self.plugin_id, message = %message),
            tracing::Level::INFO => tracing::info!(plugin_id = %self.plugin_id, message = %message),
            tracing::Level::WARN => tracing::warn!(plugin_id = %self.plugin_id, message = %message),
            tracing::Level::ERROR => tracing::error!(plugin_id = %self.plugin_id, message = %message),
        }
    }

    /// Register a code sandbox for the given language (Bootstrap phase only).
    pub fn register_sandbox(
        &mut self,
        language: CodeLanguage,
        sandbox: Arc<dyn CodeSandbox>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Bootstrap)?;
        self.registry_inner.sandboxes.push((language, sandbox));
        Ok(())
    }

    /// Register a generic service (Normal phase)
    pub fn provide_service(
        &mut self,
        key: &str,
        service: Arc<dyn Any + Send + Sync>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Normal)?;
        self.registry_inner
            .services
            .entry(key.to_string())
            .or_default()
            .push(service);
        Ok(())
    }

    /// Query registered services by key
    pub fn query_services(&self, key: &str) -> &[Arc<dyn Any + Send + Sync>] {
        self.registry_inner
            .services
            .get(key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn register_plugin_loader(
        &mut self,
        loader: Arc<dyn PluginLoader>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Bootstrap)?;
        let loaders = self
            .loaders
            .as_mut()
            .ok_or_else(|| PluginError::RegisterError("Loader registry unavailable".into()))?;
        loaders.insert(loader.loader_type().to_string(), loader);
        Ok(())
    }

    pub fn register_time_provider(
        &mut self,
        provider: Arc<dyn TimeProvider>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Bootstrap)?;
        self.registry_inner.custom_time_provider = Some(provider);
        Ok(())
    }

    pub fn register_id_generator(
        &mut self,
        generator: Arc<dyn IdGenerator>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Bootstrap)?;
        self.registry_inner.custom_id_generator = Some(generator);
        Ok(())
    }

    pub fn register_node_executor(
        &mut self,
        node_type: &str,
        executor: Box<dyn NodeExecutor>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Normal)?;
        if let Some(caps) = &self.capabilities {
            if !caps.register_nodes {
                return Err(PluginError::CapabilityDenied("register_nodes".into()));
            }
        }
        if self.registry_inner.node_executors.contains_key(node_type) {
            return Err(PluginError::ConflictError(format!(
                "Node type '{}' already registered",
                node_type
            )));
        }
        self.registry_inner
            .node_executors
            .insert(node_type.to_string(), executor);
        Ok(())
    }

    pub fn register_llm_provider(
        &mut self,
        provider: Arc<dyn LlmProvider>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Normal)?;
        if let Some(caps) = &self.capabilities {
            if !caps.register_llm_providers {
                return Err(PluginError::CapabilityDenied("register_llm_providers".into()));
            }
        }
        self.registry_inner.llm_providers.push(provider);
        Ok(())
    }

    pub fn register_hook(
        &mut self,
        hook_point: HookPoint,
        handler: Arc<dyn HookHandler>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Normal)?;
        if let Some(caps) = &self.capabilities {
            if !caps.register_hooks {
                return Err(PluginError::CapabilityDenied("register_hooks".into()));
            }
        }
        self.registry_inner
            .hooks
            .entry(hook_point)
            .or_default()
            .push(handler);
        Ok(())
    }

    pub fn register_template_function(
        &mut self,
        name: &str,
        func: Arc<dyn TemplateFunction>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Normal)?;
        self.registry_inner
            .template_functions
            .insert(name.to_string(), func);
        Ok(())
    }

    /// Register template engine (Bootstrap phase)
    pub fn register_template_engine(
        &mut self,
        engine: Arc<dyn TemplateEngine>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Bootstrap)?;
        if self.registry_inner.template_engine.is_some() {
            return Err(PluginError::ConflictError(
                "Template engine already registered".into(),
            ));
        }
        self.registry_inner.template_engine = Some(engine);
        Ok(())
    }

    /// Query template engine (Normal phase)
    pub fn query_template_engine(&self) -> Option<&Arc<dyn TemplateEngine>> {
        self.registry_inner.template_engine.as_ref()
    }

    pub fn register_dsl_validator(
        &mut self,
        validator: Arc<dyn DslValidator>,
    ) -> Result<(), PluginError> {
        self.ensure_phase(PluginPhase::Normal)?;
        self.registry_inner.dsl_validators.push(validator);
        Ok(())
    }

    fn ensure_phase(&self, expected: PluginPhase) -> Result<(), PluginError> {
        if self.phase != expected {
            return Err(PluginError::WrongPhase {
                expected,
                actual: self.phase,
            });
        }
        Ok(())
    }
}
