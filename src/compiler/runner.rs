use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::application::bootstrap::plugin_bootstrap::{
    new_scheduler_plugin_gate, SchedulerPluginGate,
};
use crate::application::bootstrap::security_bootstrap::{
    new_scheduler_security_gate, SchedulerSecurityGate,
};
use crate::application::workflow_run::WorkflowHandle;
use crate::compiler::compiled_workflow::CompiledWorkflow;
#[cfg(feature = "checkpoint")]
use crate::core::checkpoint::{CheckpointStore, ResumePolicy};
use crate::core::debug::{DebugConfig, DebugHandle};
use crate::core::dispatcher::EngineConfig;
use crate::core::runtime_group::RuntimeGroup;
use crate::core::sub_graph_runner::SubGraphRunner;
use crate::core::workflow_context::WorkflowContext;
use crate::core::SafeStopSignal;
use crate::dsl::schema::WorkflowSchema;
use crate::dsl::validation::ValidationReport;
use crate::error::WorkflowError;
use crate::llm::LlmProviderRegistry;

#[cfg(feature = "security")]
use crate::security::{
    AuditLogger, CredentialProvider, ResourceGovernor, ResourceGroup, SecurityPolicy,
};

pub struct CompiledWorkflowRunnerBuilder {
    compiled: CompiledWorkflow,
    user_inputs: HashMap<String, Value>,
    system_vars: HashMap<String, Value>,
    environment_vars: HashMap<String, Value>,
    conversation_vars: HashMap<String, Value>,
    config: EngineConfig,
    context: WorkflowContext,
    plugin_gate: Box<dyn SchedulerPluginGate>,
    security_gate: Arc<dyn SchedulerSecurityGate>,
    llm_provider_registry: Option<Arc<LlmProviderRegistry>>,
    debug_config: Option<DebugConfig>,
    collect_events: bool,
    #[cfg(feature = "checkpoint")]
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    #[cfg(feature = "checkpoint")]
    workflow_id: Option<String>,
    safe_stop_signal: Option<SafeStopSignal>,
    #[cfg(feature = "checkpoint")]
    resume_policy: ResumePolicy,
}

impl CompiledWorkflowRunnerBuilder {
    pub(crate) fn new(compiled: CompiledWorkflow) -> Self {
        Self {
            compiled,
            user_inputs: HashMap::new(),
            system_vars: HashMap::new(),
            environment_vars: HashMap::new(),
            conversation_vars: HashMap::new(),
            config: EngineConfig::default(),
            context: WorkflowContext::new(Arc::new(RuntimeGroup::default())),
            plugin_gate: new_scheduler_plugin_gate(),
            security_gate: new_scheduler_security_gate(),
            llm_provider_registry: None,
            debug_config: None,
            collect_events: true,
            #[cfg(feature = "checkpoint")]
            checkpoint_store: None,
            #[cfg(feature = "checkpoint")]
            workflow_id: None,
            safe_stop_signal: None,
            #[cfg(feature = "checkpoint")]
            resume_policy: ResumePolicy::Normal,
        }
    }

    pub fn user_inputs(mut self, inputs: HashMap<String, Value>) -> Self {
        self.user_inputs = inputs;
        self
    }

    pub fn system_vars(mut self, vars: HashMap<String, Value>) -> Self {
        self.system_vars = vars;
        self
    }

    pub fn environment_vars(mut self, vars: HashMap<String, Value>) -> Self {
        self.environment_vars = vars;
        self
    }

    pub fn conversation_vars(mut self, vars: HashMap<String, Value>) -> Self {
        self.conversation_vars = vars;
        self
    }

    pub fn collect_events(mut self, collect: bool) -> Self {
        self.collect_events = collect;
        self
    }

    pub fn config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    pub fn context(mut self, context: WorkflowContext) -> Self {
        self.context = context;
        self
    }

    pub fn runtime_group(mut self, runtime_group: Arc<RuntimeGroup>) -> Self {
        self.context = WorkflowContext::new(runtime_group);
        self
    }

    pub fn sub_graph_runner(mut self, runner: Arc<dyn SubGraphRunner>) -> Self {
        self.context = self.context.with_sub_graph_runner(runner);
        self
    }

    #[cfg(feature = "security")]
    pub fn security_policy(mut self, policy: SecurityPolicy) -> Self {
        if self.context.audit_logger().is_none() {
            if let Some(logger) = policy.audit_logger.clone() {
                self.context.set_audit_logger(logger);
            }
        }
        self.context.set_security_policy(policy);
        self
    }

    #[cfg(feature = "security")]
    pub fn resource_group(mut self, group: ResourceGroup) -> Self {
        self.context.set_resource_group(group);
        self
    }

    #[cfg(feature = "security")]
    pub fn resource_governor(mut self, governor: Arc<dyn ResourceGovernor>) -> Self {
        self.context.set_resource_governor(governor);
        self
    }

    #[cfg(feature = "security")]
    pub fn credential_provider(mut self, provider: Arc<dyn CredentialProvider>) -> Self {
        self.context.set_credential_provider(provider);
        self
    }

    #[cfg(feature = "security")]
    pub fn audit_logger(mut self, logger: Arc<dyn AuditLogger>) -> Self {
        self.context.set_audit_logger(logger);
        self
    }

    #[cfg(feature = "plugin-system")]
    pub fn plugin_config(mut self, config: crate::plugin_system::PluginSystemConfig) -> Self {
        self.plugin_gate.set_plugin_config(config);
        self
    }

    #[cfg(feature = "plugin-system")]
    pub fn bootstrap_plugin(mut self, plugin: Box<dyn crate::plugin_system::Plugin>) -> Self {
        self.plugin_gate.add_bootstrap_plugin(plugin);
        self
    }

    #[cfg(feature = "plugin-system")]
    pub fn plugin(mut self, plugin: Box<dyn crate::plugin_system::Plugin>) -> Self {
        self.plugin_gate.add_plugin(plugin);
        self
    }

    pub fn llm_providers(mut self, registry: Arc<LlmProviderRegistry>) -> Self {
        self.llm_provider_registry = Some(registry);
        self
    }

    pub fn debug(mut self, config: DebugConfig) -> Self {
        self.debug_config = Some(config);
        self
    }

    #[cfg(feature = "checkpoint")]
    pub fn checkpoint_store(mut self, store: Arc<dyn CheckpointStore>) -> Self {
        self.checkpoint_store = Some(store);
        self
    }

    #[cfg(feature = "checkpoint")]
    pub fn workflow_id(mut self, id: String) -> Self {
        self.workflow_id = Some(id);
        self
    }

    pub fn safe_stop_signal(mut self, signal: SafeStopSignal) -> Self {
        self.safe_stop_signal = Some(signal);
        self
    }

    #[cfg(feature = "checkpoint")]
    pub fn resume_policy(mut self, policy: ResumePolicy) -> Self {
        self.resume_policy = policy;
        self
    }

    pub fn validate(&self) -> ValidationReport {
        let schema: &WorkflowSchema = self.compiled.schema.as_ref();
        crate::dsl::validate_schema(schema)
    }

    pub async fn run(self) -> Result<WorkflowHandle, WorkflowError> {
        let builder = self;

        let compiled = builder.compiled;
        let spec = crate::application::workflow_run::WorkflowRunSpec {
            schema: Arc::clone(&compiled.schema),
            graph_spec: crate::application::workflow_run::WorkflowGraphSpec::FromTopology(
                Arc::clone(&compiled.graph_template),
            ),
            start_var_types: Arc::clone(&compiled.start_var_types),
            conversation_var_types: Arc::clone(&compiled.conversation_var_types),
            compiled_node_configs: Some(Arc::clone(&compiled.node_configs)),
        };

        let options = crate::application::workflow_run::WorkflowRunOptions {
            user_inputs: builder.user_inputs,
            system_vars: builder.system_vars,
            environment_vars: builder.environment_vars,
            conversation_vars: builder.conversation_vars,
            config: builder.config,
            context: builder.context,
            plugin_gate: builder.plugin_gate,
            security_gate: builder.security_gate,
            llm_provider_registry: builder.llm_provider_registry,
            collect_events: builder.collect_events,
            #[cfg(feature = "checkpoint")]
            checkpoint_store: builder.checkpoint_store,
            #[cfg(feature = "checkpoint")]
            workflow_id: builder.workflow_id,
            safe_stop_signal: builder.safe_stop_signal,
            #[cfg(feature = "checkpoint")]
            resume_policy: builder.resume_policy,
        };

        crate::application::workflow_run::run_workflow(spec, options).await
    }

    pub async fn run_debug(self) -> Result<(WorkflowHandle, DebugHandle), WorkflowError> {
        let builder = self;

        let debug_config = builder.debug_config.unwrap_or_default();

        let compiled = builder.compiled;
        let spec = crate::application::workflow_run::WorkflowRunSpec {
            schema: Arc::clone(&compiled.schema),
            graph_spec: crate::application::workflow_run::WorkflowGraphSpec::FromTopology(
                Arc::clone(&compiled.graph_template),
            ),
            start_var_types: Arc::clone(&compiled.start_var_types),
            conversation_var_types: Arc::clone(&compiled.conversation_var_types),
            compiled_node_configs: Some(Arc::clone(&compiled.node_configs)),
        };

        let options = crate::application::workflow_run::WorkflowRunOptions {
            user_inputs: builder.user_inputs,
            system_vars: builder.system_vars,
            environment_vars: builder.environment_vars,
            conversation_vars: builder.conversation_vars,
            config: builder.config,
            context: builder.context,
            plugin_gate: builder.plugin_gate,
            security_gate: builder.security_gate,
            llm_provider_registry: builder.llm_provider_registry,
            collect_events: builder.collect_events,
            #[cfg(feature = "checkpoint")]
            checkpoint_store: builder.checkpoint_store,
            #[cfg(feature = "checkpoint")]
            workflow_id: builder.workflow_id,
            safe_stop_signal: builder.safe_stop_signal,
            #[cfg(feature = "checkpoint")]
            resume_policy: builder.resume_policy,
        };

        crate::application::workflow_run::run_workflow_debug(spec, options, debug_config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::compiler::WorkflowCompiler;
    use crate::core::sub_graph_runner::DefaultSubGraphRunner;
    use crate::dsl::DslFormat;

    fn create_test_compiled() -> CompiledWorkflow {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: end
    data:
      type: end
      title: End
      outputs: []
edges:
  - source: start
    target: end
"#;
        WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap()
    }

    #[test]
    fn test_builder_new() {
        let compiled = create_test_compiled();
        let builder = CompiledWorkflowRunnerBuilder::new(compiled);
        assert!(builder.user_inputs.is_empty());
        assert!(builder.system_vars.is_empty());
        assert!(builder.collect_events);
    }

    #[test]
    fn test_builder_user_inputs() {
        let compiled = create_test_compiled();
        let mut inputs = HashMap::new();
        inputs.insert("key".to_string(), Value::String("value".into()));

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).user_inputs(inputs.clone());

        assert_eq!(builder.user_inputs.len(), 1);
        assert_eq!(
            builder.user_inputs.get("key"),
            Some(&Value::String("value".into()))
        );
    }

    #[test]
    fn test_builder_system_vars() {
        let compiled = create_test_compiled();
        let mut vars = HashMap::new();
        vars.insert("sys_var".to_string(), Value::String("sys_value".into()));

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).system_vars(vars.clone());

        assert_eq!(builder.system_vars.len(), 1);
    }

    #[test]
    fn test_builder_environment_vars() {
        let compiled = create_test_compiled();
        let mut vars = HashMap::new();
        vars.insert("env_var".to_string(), Value::String("env_value".into()));

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).environment_vars(vars.clone());

        assert_eq!(builder.environment_vars.len(), 1);
    }

    #[test]
    fn test_builder_conversation_vars() {
        let compiled = create_test_compiled();
        let mut vars = HashMap::new();
        vars.insert("conv_var".to_string(), Value::String("conv_value".into()));

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).conversation_vars(vars.clone());

        assert_eq!(builder.conversation_vars.len(), 1);
    }

    #[test]
    fn test_builder_collect_events() {
        let compiled = create_test_compiled();
        let builder = CompiledWorkflowRunnerBuilder::new(compiled).collect_events(false);

        assert!(!builder.collect_events);
    }

    #[test]
    fn test_builder_config() {
        let compiled = create_test_compiled();
        let config = EngineConfig {
            strict_template: true,
            ..Default::default()
        };

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).config(config.clone());

        assert!(builder.config.strict_template);
    }

    #[test]
    fn test_builder_context() {
        let compiled = create_test_compiled();
        let context = WorkflowContext::default();

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).context(context);

        assert!(builder.context.workflow_id.is_none());
    }

    #[test]
    fn test_builder_runtime_group() {
        let compiled = create_test_compiled();
        let runtime_group = Arc::new(RuntimeGroup::default());

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).runtime_group(runtime_group);

        assert!(builder.context.runtime_group.credential_provider.is_none());
    }

    #[test]
    fn test_builder_sub_graph_runner() {
        let compiled = create_test_compiled();
        let runner = Arc::new(DefaultSubGraphRunner);

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).sub_graph_runner(runner);

        assert!(builder.context.sub_graph_runner().is_some());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_builder_security_policy() {
        use crate::security::SecurityPolicy;

        let compiled = create_test_compiled();
        let policy = SecurityPolicy::permissive();

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).security_policy(policy);

        assert!(builder.context.security_policy().is_some());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_builder_resource_group() {
        use crate::security::{ResourceGroup, ResourceQuota, SecurityLevel};

        let compiled = create_test_compiled();
        let group = ResourceGroup {
            group_id: "test".into(),
            group_name: None,
            security_level: SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).resource_group(group);

        assert!(builder.context.resource_group().is_some());
    }

    #[test]
    fn test_builder_llm_providers() {
        let compiled = create_test_compiled();
        let registry = Arc::new(LlmProviderRegistry::new());

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).llm_providers(registry);

        assert!(builder.llm_provider_registry.is_some());
    }

    #[test]
    fn test_builder_debug() {
        let compiled = create_test_compiled();
        let debug_config = DebugConfig {
            break_on_start: true,
            ..Default::default()
        };

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).debug(debug_config.clone());

        assert!(builder.debug_config.is_some());
    }

    #[test]
    fn test_builder_validate() {
        let compiled = create_test_compiled();
        let builder = CompiledWorkflowRunnerBuilder::new(compiled);

        let report = builder.validate();
        assert!(report.is_valid);
    }

    #[cfg(all(test, feature = "builtin-core-nodes"))]
    #[tokio::test]
    async fn test_builder_run_basic() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: end
    data:
      type: end
      title: End
      outputs: []
edges:
  - source: start
    target: end
"#;
        let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap();
        let handle = compiled.runner().run().await.unwrap();

        let status = handle.wait().await;
        match status {
            crate::domain::execution::ExecutionStatus::Completed(_) => {}
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[cfg(all(test, feature = "builtin-core-nodes"))]
    #[tokio::test]
    async fn test_builder_run_with_inputs() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: input
          label: Input
          type: string
          required: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: output
          value_selector: ["start", "input"]
edges:
  - source: start
    target: end
"#;
        let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap();
        let mut inputs = HashMap::new();
        inputs.insert("input".to_string(), Value::String("test".into()));

        let handle = compiled.runner().user_inputs(inputs).run().await.unwrap();

        let status = handle.wait().await;
        match status {
            crate::domain::execution::ExecutionStatus::Completed(outputs) => {
                assert_eq!(outputs.get("output"), Some(&Value::String("test".into())));
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[cfg(all(test, feature = "builtin-core-nodes"))]
    #[tokio::test]
    async fn test_builder_run_with_system_vars() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: output
          value_selector: ["sys", "test_var"]
edges:
  - source: start
    target: end
"#;
        let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap();
        let mut sys_vars = HashMap::new();
        sys_vars.insert("test_var".to_string(), Value::String("sys_value".into()));

        let handle = compiled.runner().system_vars(sys_vars).run().await.unwrap();

        let status = handle.wait().await;
        match status {
            crate::domain::execution::ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("output"),
                    Some(&Value::String("sys_value".into()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[cfg(all(test, feature = "builtin-core-nodes"))]
    #[tokio::test]
    async fn test_builder_run_with_environment_vars() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: output
          value_selector: ["env", "env_var"]
edges:
  - source: start
    target: end
"#;
        let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap();
        let mut env_vars = HashMap::new();
        env_vars.insert("env_var".to_string(), Value::String("env_value".into()));

        let handle = compiled
            .runner()
            .environment_vars(env_vars)
            .run()
            .await
            .unwrap();

        let status = handle.wait().await;
        match status {
            crate::domain::execution::ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("output"),
                    Some(&Value::String("env_value".into()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[cfg(all(test, feature = "builtin-core-nodes"))]
    #[tokio::test]
    async fn test_builder_run_with_conversation_vars() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: output
          value_selector: ["conversation", "conv_var"]
edges:
  - source: start
    target: end
"#;
        let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap();
        let mut conv_vars = HashMap::new();
        conv_vars.insert("conv_var".to_string(), Value::String("conv_value".into()));

        let handle = compiled
            .runner()
            .conversation_vars(conv_vars)
            .run()
            .await
            .unwrap();

        let status = handle.wait().await;
        match status {
            crate::domain::execution::ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("output"),
                    Some(&Value::String("conv_value".into()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_builder_resource_governor() {
        use crate::security::{InMemoryResourceGovernor, ResourceQuota};

        let compiled = create_test_compiled();
        let mut quotas = HashMap::new();
        quotas.insert("default".to_string(), ResourceQuota::default());
        let governor = Arc::new(InMemoryResourceGovernor::new(quotas));

        let builder =
            CompiledWorkflowRunnerBuilder::new(compiled).resource_governor(governor.clone());

        assert!(builder.context.resource_governor().is_some());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_builder_credential_provider() {
        use crate::security::CredentialProvider;

        let compiled = create_test_compiled();

        struct MockCredentialProvider;

        #[async_trait::async_trait]
        impl CredentialProvider for MockCredentialProvider {
            async fn get_credentials(
                &self,
                _group_id: &str,
                _provider: &str,
            ) -> Result<HashMap<String, String>, crate::security::CredentialError> {
                Ok(HashMap::new())
            }
        }

        let provider = Arc::new(MockCredentialProvider);

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).credential_provider(provider);

        assert!(builder.context.credential_provider().is_some());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_builder_audit_logger() {
        use crate::security::TracingAuditLogger;

        let compiled = create_test_compiled();
        let logger = Arc::new(TracingAuditLogger);

        let builder = CompiledWorkflowRunnerBuilder::new(compiled).audit_logger(logger);

        assert!(builder.context.audit_logger().is_some());
    }

    #[cfg(all(test, feature = "builtin-core-nodes"))]
    #[tokio::test]
    async fn test_builder_run_with_config() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: end
    data:
      type: end
      title: End
      outputs: []
edges:
  - source: start
    target: end
"#;
        let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap();
        let config = EngineConfig {
            strict_template: true,
            max_steps: 100,
            ..Default::default()
        };

        let handle = compiled.runner().config(config).run().await.unwrap();

        let status = handle.wait().await;
        assert!(matches!(
            status,
            crate::domain::execution::ExecutionStatus::Completed(_)
        ));
    }

    #[cfg(all(test, feature = "builtin-core-nodes"))]
    #[tokio::test]
    async fn test_builder_run_collect_events_disabled() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: end
    data:
      type: end
      title: End
      outputs: []
edges:
  - source: start
    target: end
"#;
        let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap();

        let handle = compiled.runner().collect_events(false).run().await.unwrap();

        let events = handle.events().await;
        assert!(events.is_empty());
    }

    #[cfg(all(test, feature = "plugin-system"))]
    #[test]
    fn test_builder_plugin_config() {
        use crate::plugin_system::PluginSystemConfig;

        let compiled = create_test_compiled();
        let plugin_config = PluginSystemConfig::default();

        let _builder = CompiledWorkflowRunnerBuilder::new(compiled).plugin_config(plugin_config);

        // Plugin config is set internally
    }
}
