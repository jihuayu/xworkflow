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
use super::hooks::{HookHandler, HookPoint};
use super::loader::PluginLoader;
use super::registry::{PluginPhase, PluginRegistryInner};
use super::traits::PluginCapabilities;
use xworkflow_types::template::TemplateEngine;

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
            tracing::Level::TRACE => {
                tracing::trace!(plugin_id = %self.plugin_id, message = %message)
            }
            tracing::Level::DEBUG => {
                tracing::debug!(plugin_id = %self.plugin_id, message = %message)
            }
            tracing::Level::INFO => tracing::info!(plugin_id = %self.plugin_id, message = %message),
            tracing::Level::WARN => tracing::warn!(plugin_id = %self.plugin_id, message = %message),
            tracing::Level::ERROR => {
                tracing::error!(plugin_id = %self.plugin_id, message = %message)
            }
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
                return Err(PluginError::CapabilityDenied(
                    "register_llm_providers".into(),
                ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runtime_context::{RealIdGenerator, RealTimeProvider};
    use crate::sandbox::{CodeLanguage, CodeSandbox};
    use async_trait::async_trait;

    struct MockSandbox;

    #[async_trait]
    impl CodeSandbox for MockSandbox {
        async fn execute(
            &self,
            _request: xworkflow_types::SandboxRequest,
        ) -> Result<xworkflow_types::SandboxResult, xworkflow_types::SandboxError> {
            Ok(xworkflow_types::SandboxResult {
                success: true,
                output: "mock".into(),
                stdout: "mock".into(),
                stderr: String::new(),
                execution_time: std::time::Duration::from_secs(0),
                memory_used: 0,
                error: None,
            })
        }

        fn sandbox_type(&self) -> xworkflow_types::SandboxType {
            xworkflow_types::SandboxType::Builtin
        }

        fn supported_languages(&self) -> Vec<xworkflow_types::CodeLanguage> {
            vec![xworkflow_types::CodeLanguage::JavaScript]
        }
    }

    struct MockNodeExecutor;

    #[async_trait]
    impl NodeExecutor for MockNodeExecutor {
        async fn execute(
            &self,
            _node_id: &str,
            _config: &serde_json::Value,
            _variable_pool: &crate::core::variable_pool::VariablePool,
            _context: &crate::core::runtime_context::RuntimeContext,
        ) -> Result<crate::dsl::schema::NodeRunResult, crate::error::NodeError> {
            Ok(crate::dsl::schema::NodeRunResult::default())
        }
    }

    struct MockLlmProvider;

    #[async_trait]
    impl LlmProvider for MockLlmProvider {
        fn id(&self) -> &str {
            "mock"
        }

        fn info(&self) -> crate::llm::types::ProviderInfo {
            crate::llm::types::ProviderInfo {
                id: "mock".into(),
                name: "Mock".into(),
                models: vec![],
            }
        }

        async fn chat_completion(
            &self,
            _request: crate::llm::types::ChatCompletionRequest,
        ) -> Result<crate::llm::types::ChatCompletionResponse, crate::llm::error::LlmError>
        {
            Ok(crate::llm::types::ChatCompletionResponse {
                content: "mock response".into(),
                usage: Default::default(),
                model: "mock".into(),
                finish_reason: Some("stop".into()),
                tool_calls: vec![],
            })
        }

        async fn chat_completion_stream(
            &self,
            _request: crate::llm::types::ChatCompletionRequest,
            _chunk_tx: tokio::sync::mpsc::Sender<crate::llm::types::StreamChunk>,
        ) -> Result<crate::llm::types::ChatCompletionResponse, crate::llm::error::LlmError>
        {
            Ok(crate::llm::types::ChatCompletionResponse {
                content: "mock response".into(),
                usage: Default::default(),
                model: "mock".into(),
                finish_reason: Some("stop".into()),
                tool_calls: vec![],
            })
        }
    }

    struct MockTemplateEngine;

    impl TemplateEngine for MockTemplateEngine {
        fn render(
            &self,
            _template: &str,
            _variables: &HashMap<String, serde_json::Value>,
            _functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
        ) -> Result<String, String> {
            Ok("rendered".into())
        }

        fn compile(
            &self,
            _template: &str,
            _functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
        ) -> Result<Box<dyn xworkflow_types::template::CompiledTemplateHandle>, String> {
            Err("not implemented".into())
        }

        fn engine_name(&self) -> &str {
            "mock"
        }
    }

    struct MockHookHandler;

    #[async_trait]
    impl HookHandler for MockHookHandler {
        async fn handle(
            &self,
            _payload: &crate::plugin_system::hooks::HookPayload,
        ) -> Result<Option<serde_json::Value>, PluginError> {
            Ok(None)
        }

        fn name(&self) -> &str {
            "mock"
        }
    }

    struct MockTemplateFunction;

    impl TemplateFunction for MockTemplateFunction {
        fn call(&self, _args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
            Ok(serde_json::Value::String("result".into()))
        }

        fn name(&self) -> &str {
            "mock_func"
        }
    }

    struct MockDslValidator;

    impl DslValidator for MockDslValidator {
        fn name(&self) -> &str {
            "mock_validator"
        }

        fn validate(
            &self,
            _schema: &crate::dsl::schema::WorkflowSchema,
        ) -> Vec<crate::dsl::Diagnostic> {
            Vec::new()
        }
    }

    fn create_test_inner() -> PluginRegistryInner {
        PluginRegistryInner {
            node_executors: HashMap::new(),
            llm_providers: Vec::new(),
            sandboxes: Vec::new(),
            hooks: HashMap::new(),
            template_functions: HashMap::new(),
            services: HashMap::new(),
            template_engine: None,
            dsl_validators: Vec::new(),
            custom_time_provider: None,
            custom_id_generator: None,
        }
    }

    fn create_test_context<'a>(
        phase: PluginPhase,
        inner: &'a mut PluginRegistryInner,
        loaders: Option<&'a mut HashMap<String, Arc<dyn PluginLoader>>>,
    ) -> PluginContext<'a> {
        PluginContext::new(phase, inner, loaders, "test_plugin".into(), None)
    }

    #[test]
    fn test_context_phase() {
        let mut inner = create_test_inner();
        let ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);
        assert_eq!(ctx.phase(), PluginPhase::Bootstrap);
    }

    #[test]
    fn test_context_plugin_id() {
        let mut inner = create_test_inner();
        let ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);
        assert_eq!(ctx.plugin_id(), "test_plugin");
    }

    #[test]
    fn test_context_log() {
        let mut inner = create_test_inner();
        let ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);
        ctx.log(tracing::Level::INFO, "test message");
    }

    #[test]
    fn test_register_sandbox_bootstrap_phase() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let result = ctx.register_sandbox(CodeLanguage::JavaScript, Arc::new(MockSandbox));
        assert!(result.is_ok());
        assert_eq!(inner.sandboxes.len(), 1);
    }

    #[test]
    fn test_register_sandbox_wrong_phase() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let result = ctx.register_sandbox(CodeLanguage::JavaScript, Arc::new(MockSandbox));
        assert!(result.is_err());
        match result {
            Err(PluginError::WrongPhase { expected, actual }) => {
                assert_eq!(expected, PluginPhase::Bootstrap);
                assert_eq!(actual, PluginPhase::Normal);
            }
            _ => panic!("Expected WrongPhase error"),
        }
    }

    #[test]
    fn test_provide_service() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let service: Arc<dyn Any + Send + Sync> = Arc::new(42i32);
        let result = ctx.provide_service("test_service", service);
        assert!(result.is_ok());
        assert_eq!(inner.services.get("test_service").unwrap().len(), 1);
    }

    #[test]
    fn test_provide_service_wrong_phase() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let service: Arc<dyn Any + Send + Sync> = Arc::new(42i32);
        let result = ctx.provide_service("test_service", service);
        assert!(result.is_err());
    }

    #[test]
    fn test_query_services() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let service: Arc<dyn Any + Send + Sync> = Arc::new(42i32);
        ctx.provide_service("test_service", service).unwrap();

        let services = ctx.query_services("test_service");
        assert_eq!(services.len(), 1);
    }

    #[test]
    fn test_query_services_not_found() {
        let mut inner = create_test_inner();
        let ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let services = ctx.query_services("nonexistent");
        assert_eq!(services.len(), 0);
    }

    #[test]
    fn test_register_time_provider() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let provider = Arc::new(RealTimeProvider::new());
        let result = ctx.register_time_provider(provider);
        assert!(result.is_ok());
        assert!(inner.custom_time_provider.is_some());
    }

    #[test]
    fn test_register_id_generator() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let generator = Arc::new(RealIdGenerator);
        let result = ctx.register_id_generator(generator);
        assert!(result.is_ok());
        assert!(inner.custom_id_generator.is_some());
    }

    #[test]
    fn test_register_node_executor() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let executor = Box::new(MockNodeExecutor);
        let result = ctx.register_node_executor("test_node", executor);
        assert!(result.is_ok());
        assert!(inner.node_executors.contains_key("test_node"));
    }

    #[test]
    fn test_register_node_executor_wrong_phase() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let executor = Box::new(MockNodeExecutor);
        let result = ctx.register_node_executor("test_node", executor);
        assert!(result.is_err());
    }

    #[test]
    fn test_register_node_executor_conflict() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let executor1 = Box::new(MockNodeExecutor);
        ctx.register_node_executor("test_node", executor1).unwrap();

        let executor2 = Box::new(MockNodeExecutor);
        let result = ctx.register_node_executor("test_node", executor2);
        assert!(result.is_err());
        match result {
            Err(PluginError::ConflictError(_)) => {}
            _ => panic!("Expected ConflictError"),
        }
    }

    #[test]
    fn test_register_node_executor_capability_denied() {
        let mut inner = create_test_inner();
        let capabilities = PluginCapabilities {
            read_variables: false,
            write_variables: false,
            emit_events: false,
            http_access: false,
            fs_access: None,
            max_memory_pages: None,
            max_fuel: None,
            register_nodes: false,
            register_llm_providers: true,
            register_hooks: true,
        };
        let mut ctx = PluginContext::new(
            PluginPhase::Normal,
            &mut inner,
            None,
            "test_plugin".into(),
            Some(capabilities),
        );

        let executor = Box::new(MockNodeExecutor);
        let result = ctx.register_node_executor("test_node", executor);
        assert!(result.is_err());
        match result {
            Err(PluginError::CapabilityDenied(_)) => {}
            _ => panic!("Expected CapabilityDenied error"),
        }
    }

    #[test]
    fn test_register_llm_provider() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let provider = Arc::new(MockLlmProvider);
        let result = ctx.register_llm_provider(provider);
        assert!(result.is_ok());
        assert_eq!(inner.llm_providers.len(), 1);
    }

    #[test]
    fn test_register_llm_provider_wrong_phase() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let provider = Arc::new(MockLlmProvider);
        let result = ctx.register_llm_provider(provider);
        assert!(result.is_err());
    }

    #[test]
    fn test_register_llm_provider_capability_denied() {
        let mut inner = create_test_inner();
        let capabilities = PluginCapabilities {
            read_variables: false,
            write_variables: false,
            emit_events: false,
            http_access: false,
            fs_access: None,
            max_memory_pages: None,
            max_fuel: None,
            register_nodes: true,
            register_llm_providers: false,
            register_hooks: true,
        };
        let mut ctx = PluginContext::new(
            PluginPhase::Normal,
            &mut inner,
            None,
            "test_plugin".into(),
            Some(capabilities),
        );

        let provider = Arc::new(MockLlmProvider);
        let result = ctx.register_llm_provider(provider);
        assert!(result.is_err());
    }

    #[test]
    fn test_register_hook() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let handler = Arc::new(MockHookHandler);
        let result = ctx.register_hook(HookPoint::BeforeNodeExecute, handler);
        assert!(result.is_ok());
        assert!(inner.hooks.contains_key(&HookPoint::BeforeNodeExecute));
    }

    #[test]
    fn test_register_hook_wrong_phase() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let handler = Arc::new(MockHookHandler);
        let result = ctx.register_hook(HookPoint::BeforeNodeExecute, handler);
        assert!(result.is_err());
    }

    #[test]
    fn test_register_hook_capability_denied() {
        let mut inner = create_test_inner();
        let capabilities = PluginCapabilities {
            read_variables: false,
            write_variables: false,
            emit_events: false,
            http_access: false,
            fs_access: None,
            max_memory_pages: None,
            max_fuel: None,
            register_nodes: true,
            register_llm_providers: true,
            register_hooks: false,
        };
        let mut ctx = PluginContext::new(
            PluginPhase::Normal,
            &mut inner,
            None,
            "test_plugin".into(),
            Some(capabilities),
        );

        let handler = Arc::new(MockHookHandler);
        let result = ctx.register_hook(HookPoint::BeforeNodeExecute, handler);
        assert!(result.is_err());
    }

    #[test]
    fn test_register_template_function() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let func = Arc::new(MockTemplateFunction);
        let result = ctx.register_template_function("test_func", func);
        assert!(result.is_ok());
        assert!(inner.template_functions.contains_key("test_func"));
    }

    #[test]
    fn test_register_template_engine() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let engine = Arc::new(MockTemplateEngine);
        let result = ctx.register_template_engine(engine);
        assert!(result.is_ok());
        assert!(inner.template_engine.is_some());
    }

    #[test]
    fn test_register_template_engine_conflict() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let engine1 = Arc::new(MockTemplateEngine);
        ctx.register_template_engine(engine1).unwrap();

        let engine2 = Arc::new(MockTemplateEngine);
        let result = ctx.register_template_engine(engine2);
        assert!(result.is_err());
        match result {
            Err(PluginError::ConflictError(_)) => {}
            _ => panic!("Expected ConflictError"),
        }
    }

    #[test]
    fn test_query_template_engine() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Bootstrap, &mut inner, None);

        let engine = Arc::new(MockTemplateEngine);
        ctx.register_template_engine(engine).unwrap();

        let queried = ctx.query_template_engine();
        assert!(queried.is_some());
    }

    #[test]
    fn test_register_dsl_validator() {
        let mut inner = create_test_inner();
        let mut ctx = create_test_context(PluginPhase::Normal, &mut inner, None);

        let validator = Arc::new(MockDslValidator);
        let result = ctx.register_dsl_validator(validator);
        assert!(result.is_ok());
        assert_eq!(inner.dsl_validators.len(), 1);
    }
}
