use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex, RwLock};

use crate::compiler::compiled_workflow::CompiledWorkflow;
use crate::core::debug::{DebugConfig, DebugHandle, InteractiveDebugGate, InteractiveDebugHook, StepMode};
use crate::core::dispatcher::{EngineConfig, EventEmitter, WorkflowDispatcher};
use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_group::RuntimeGroup;
use crate::core::sub_graph_runner::{DefaultSubGraphRunner, SubGraphRunner};
use crate::core::variable_pool::{Segment, SegmentType, VariablePool};
use crate::core::workflow_context::WorkflowContext;
use crate::dsl::schema::{ErrorHandlingMode, WorkflowSchema};
use crate::dsl::validation::ValidationReport;
use crate::error::WorkflowError;
use crate::graph::Graph;
use crate::llm::LlmProviderRegistry;
use crate::nodes::executor::NodeExecutorRegistry;
use crate::scheduler::{
    build_error_context,
    segment_from_type,
    SchedulerPluginGate,
    SchedulerSecurityGate,
};
use crate::scheduler::{new_scheduler_plugin_gate, new_scheduler_security_gate};

#[cfg(feature = "security")]
use crate::security::{AuditLogger, CredentialProvider, ResourceGovernor, ResourceGroup, SecurityPolicy};

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

    pub fn validate(&self) -> ValidationReport {
        let schema: &WorkflowSchema = self.compiled.schema.as_ref();
        crate::dsl::validate_schema(schema)
    }

    pub async fn run(self) -> Result<crate::scheduler::WorkflowHandle, WorkflowError> {
        let mut builder = self;
        let schema = builder.compiled.schema.clone();
        let mut report = builder
            .security_gate
            .validate_schema(schema.as_ref(), &builder.context);
        if !report.is_valid {
            builder
                .security_gate
                .audit_validation_failed(&builder.context, &report)
                .await;
            return Err(WorkflowError::ValidationFailed(report));
        }

        builder
            .plugin_gate
            .init_and_extend_validation(schema.as_ref(), &mut report)
            .await?;
        builder.plugin_gate.after_dsl_validation(&report).await?;

        if !report.is_valid {
            builder
                .security_gate
                .audit_validation_failed(&builder.context, &report)
                .await;
            return Err(WorkflowError::ValidationFailed(report));
        }

        let graph = Graph::from_topology(Arc::clone(&builder.compiled.graph_template));

        let mut pool = VariablePool::new();
        let start_var_types: &HashMap<String, SegmentType> = &builder.compiled.start_var_types;
        let conversation_var_types: &HashMap<String, SegmentType> =
            &builder.compiled.conversation_var_types;
        builder
            .security_gate
            .configure_variable_pool(&builder.context, &mut pool);

        for (k, v) in &builder.system_vars {
            let selector = crate::core::variable_pool::Selector::new("sys", k.clone());
            pool.set(&selector, Segment::from_value(v));
        }

        for (k, v) in &builder.environment_vars {
            let selector = crate::core::variable_pool::Selector::new("env", k.clone());
            pool.set(&selector, Segment::from_value(v));
        }

        for (k, v) in &builder.conversation_vars {
            let selector = crate::core::variable_pool::Selector::new("conversation", k.clone());
            let seg = segment_from_type(v, conversation_var_types.get(k));
            pool.set(&selector, seg);
        }

        let start_node_id = builder.compiled.start_node_id.as_ref();
        for (k, v) in &builder.user_inputs {
            let selector = crate::core::variable_pool::Selector::new(start_node_id, k.clone());
            let seg = segment_from_type(v, start_var_types.get(k));
            pool.set(&selector, seg);
        }

        let mut registry = NodeExecutorRegistry::new();
        builder.plugin_gate.apply_node_executors(&mut registry);

        let mut llm_registry = if let Some(llm_reg) = &builder.llm_provider_registry {
            llm_reg.clone_registry()
        } else {
            LlmProviderRegistry::new()
        };
        builder.plugin_gate.apply_llm_providers(&mut llm_registry);

        let llm_registry = Arc::new(llm_registry);
        registry.set_llm_provider_registry(Arc::clone(&llm_registry));

        let registry = Arc::new(registry);
        builder.context = builder.context.with_node_executor_registry(Arc::clone(&registry));
        builder.context = builder.context.with_llm_provider_registry(Arc::clone(&llm_registry));

        let (tx, mut rx) = mpsc::channel(256);
        let event_active = Arc::new(AtomicBool::new(builder.collect_events));
        let event_emitter = EventEmitter::new(tx.clone(), event_active.clone());
        let error_event_emitter = event_emitter.clone();
        let config = builder
            .security_gate
            .effective_engine_config(&builder.context, builder.config);
        builder.context.strict_template = config.strict_template;

        let workflow_id = builder.security_gate.on_workflow_start(&builder.context).await?;
        builder.context.workflow_id = workflow_id.clone();
        builder.plugin_gate.customize_context(&mut builder.context);

        #[cfg(feature = "plugin-system")]
        let plugin_registry = builder.plugin_gate.take_plugin_registry_arc();
        #[cfg(feature = "plugin-system")]
        let plugin_registry_for_shutdown = plugin_registry.clone();

        let context = Arc::new(builder.context.with_event_tx(tx.clone()));

        let (status_tx, status_rx) = watch::channel(crate::scheduler::ExecutionStatus::Running);
        let events = if builder.collect_events {
            Some(Arc::new(Mutex::new(Vec::new())))
        } else {
            None
        };

        if let Some(events_clone) = events.clone() {
            let active_flag = event_active.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    events_clone.lock().await.push(event);
                }
                active_flag.store(false, Ordering::Relaxed);
            });
        } else {
            event_active.store(false, Ordering::Relaxed);
            drop(rx);
        }

        let status_exec = status_tx.clone();
        let schema_for_err = schema.clone();
        let security_gate = Arc::clone(&builder.security_gate);
        let workflow_id_for_end = workflow_id.clone();
        let compiled_node_configs = Arc::clone(&builder.compiled.node_configs);

        tokio::spawn(async move {
            let mut dispatcher = WorkflowDispatcher::new_with_registry_and_compiled(
                graph,
                pool,
                registry,
                event_emitter,
                config,
                context.clone(),
                compiled_node_configs,
                #[cfg(feature = "plugin-system")]
                plugin_registry,
            );
            match dispatcher.run().await {
                Ok(outputs) => {
                    let _ = status_exec.send(crate::scheduler::ExecutionStatus::Completed(outputs));
                }
                Err(e) => {
                    if let Some(error_handler) = &schema_for_err.error_handler {
                        let partial_outputs = dispatcher.partial_outputs();
                        let pool_snapshot = dispatcher.snapshot_pool().await;
                        let error_context =
                            build_error_context(&e, schema_for_err.as_ref(), &partial_outputs);

                        if error_event_emitter.is_active() {
                            error_event_emitter
                                .emit(GraphEngineEvent::ErrorHandlerStarted {
                                    error: e.to_string(),
                                })
                                .await;
                        }

                        let runner = context
                            .sub_graph_runner()
                            .cloned()
                            .unwrap_or_else(|| Arc::new(DefaultSubGraphRunner));
                        match runner
                            .run_sub_graph(
                                &error_handler.sub_graph,
                                &pool_snapshot,
                                error_context,
                                context.as_ref(),
                            )
                            .await
                        {
                            Ok(handler_outputs) => {
                                let recovered_outputs: HashMap<String, Value> = handler_outputs
                                    .as_object()
                                    .map(|o| {
                                        o.iter()
                                            .map(|(k, v)| (k.clone(), v.clone()))
                                            .collect()
                                    })
                                    .unwrap_or_default();

                                if error_event_emitter.is_active() {
                                    error_event_emitter
                                        .emit(GraphEngineEvent::ErrorHandlerSucceeded {
                                            outputs: recovered_outputs.clone(),
                                        })
                                        .await;
                                }

                                match error_handler.mode {
                                    ErrorHandlingMode::Recover => {
                                        let _ = status_exec
                                            .send(crate::scheduler::ExecutionStatus::FailedWithRecovery {
                                                original_error: e.to_string(),
                                                recovered_outputs,
                                            });
                                    }
                                    ErrorHandlingMode::Notify => {
                                        let _ = status_exec
                                            .send(crate::scheduler::ExecutionStatus::Failed(e.to_string()));
                                    }
                                }
                            }
                            Err(handler_err) => {
                                if error_event_emitter.is_active() {
                                    error_event_emitter
                                        .emit(GraphEngineEvent::ErrorHandlerFailed {
                                            error: handler_err.to_string(),
                                        })
                                        .await;
                                }
                                let _ = status_exec
                                    .send(crate::scheduler::ExecutionStatus::Failed(e.to_string()));
                            }
                        }
                    } else {
                        let _ = status_exec
                            .send(crate::scheduler::ExecutionStatus::Failed(e.to_string()));
                    }
                }
            }

            security_gate
                .record_workflow_end(context.as_ref(), workflow_id_for_end.as_deref())
                .await;

            #[cfg(feature = "plugin-system")]
            if let Some(registry) = plugin_registry_for_shutdown {
                let _ = registry.shutdown_all().await;
            }
        });

        Ok(crate::scheduler::WorkflowHandle::new(
            status_rx,
            events,
            event_active,
        ))
    }

    pub async fn run_debug(
        self,
    ) -> Result<(crate::scheduler::WorkflowHandle, DebugHandle), WorkflowError> {
        let mut builder = self;
        let debug_config = builder.debug_config.clone().unwrap_or_default();
        let schema = builder.compiled.schema.clone();
        let mut report = builder
            .security_gate
            .validate_schema(schema.as_ref(), &builder.context);
        if !report.is_valid {
            return Err(WorkflowError::ValidationFailed(report));
        }

        builder
            .plugin_gate
            .init_and_extend_validation(schema.as_ref(), &mut report)
            .await?;
        builder.plugin_gate.after_dsl_validation(&report).await?;

        if !report.is_valid {
            return Err(WorkflowError::ValidationFailed(report));
        }

        let graph = Graph::from_topology(Arc::clone(&builder.compiled.graph_template));

        let mut pool = VariablePool::new();
        let start_var_types: &HashMap<String, SegmentType> = &builder.compiled.start_var_types;
        let conversation_var_types: &HashMap<String, SegmentType> =
            &builder.compiled.conversation_var_types;

        for (k, v) in &builder.system_vars {
            let selector = crate::core::variable_pool::Selector::new("sys", k.clone());
            pool.set(&selector, Segment::from_value(v));
        }

        for (k, v) in &builder.environment_vars {
            let selector = crate::core::variable_pool::Selector::new("env", k.clone());
            pool.set(&selector, Segment::from_value(v));
        }

        for (k, v) in &builder.conversation_vars {
            let selector = crate::core::variable_pool::Selector::new("conversation", k.clone());
            let seg = segment_from_type(v, conversation_var_types.get(k));
            pool.set(&selector, seg);
        }

        let start_node_id = builder.compiled.start_node_id.as_ref();
        for (k, v) in &builder.user_inputs {
            let selector = crate::core::variable_pool::Selector::new(start_node_id, k.clone());
            let seg = segment_from_type(v, start_var_types.get(k));
            pool.set(&selector, seg);
        }

        let mut registry = NodeExecutorRegistry::new();
        builder.plugin_gate.apply_node_executors(&mut registry);

        let mut llm_registry = if let Some(llm_reg) = &builder.llm_provider_registry {
            llm_reg.clone_registry()
        } else {
            LlmProviderRegistry::new()
        };
        builder.plugin_gate.apply_llm_providers(&mut llm_registry);

        let llm_registry = Arc::new(llm_registry);
        registry.set_llm_provider_registry(Arc::clone(&llm_registry));

        let registry = Arc::new(registry);
        builder.context = builder.context.with_node_executor_registry(Arc::clone(&registry));
        builder.context = builder.context.with_llm_provider_registry(Arc::clone(&llm_registry));

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (debug_evt_tx, debug_evt_rx) = mpsc::channel(256);

        let config_arc = Arc::new(RwLock::new(debug_config.clone()));
        let mode_arc = Arc::new(RwLock::new(if debug_config.break_on_start {
            StepMode::Initial
        } else {
            StepMode::Run
        }));

        let gate = InteractiveDebugGate {
            config: config_arc.clone(),
            mode: mode_arc.clone(),
        };
        let hook = InteractiveDebugHook {
            cmd_rx: Mutex::new(cmd_rx),
            event_tx: debug_evt_tx,
            graph_event_tx: None,
            config: config_arc,
            mode: mode_arc,
            last_pause: Arc::new(RwLock::new(None)),
            step_count: Arc::new(RwLock::new(0)),
        };

        let (tx, mut rx) = mpsc::channel(256);
        let event_active = Arc::new(AtomicBool::new(builder.collect_events));
        let event_emitter = EventEmitter::new(tx.clone(), event_active.clone());
        let error_event_emitter = event_emitter.clone();
        let config = builder
            .security_gate
            .effective_engine_config(&builder.context, builder.config);
        builder.context.strict_template = config.strict_template;

        let workflow_id = builder.security_gate.on_workflow_start(&builder.context).await?;
        builder.context.workflow_id = workflow_id.clone();
        builder.plugin_gate.customize_context(&mut builder.context);

        #[cfg(feature = "plugin-system")]
        let plugin_registry = builder.plugin_gate.take_plugin_registry_arc();
        #[cfg(feature = "plugin-system")]
        let plugin_registry_for_shutdown = plugin_registry.clone();

        let context = Arc::new(builder.context.with_event_tx(tx.clone()));

        let (status_tx, status_rx) = watch::channel(crate::scheduler::ExecutionStatus::Running);
        let events = if builder.collect_events {
            Some(Arc::new(Mutex::new(Vec::new())))
        } else {
            None
        };

        if let Some(events_clone) = events.clone() {
            let active_flag = event_active.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    events_clone.lock().await.push(event);
                }
                active_flag.store(false, Ordering::Relaxed);
            });
        } else {
            event_active.store(false, Ordering::Relaxed);
            drop(rx);
        }

        let status_exec = status_tx.clone();
        let schema_for_err = schema.clone();
        let mut hook = hook;
        hook.graph_event_tx = Some(tx.clone());

        let security_gate = Arc::clone(&builder.security_gate);
        let workflow_id_for_end = workflow_id.clone();
        let compiled_node_configs = Arc::clone(&builder.compiled.node_configs);

        tokio::spawn(async move {
            let mut dispatcher = WorkflowDispatcher::new_with_debug_and_compiled(
                graph,
                pool,
                registry,
                event_emitter,
                config,
                context.clone(),
                compiled_node_configs,
                #[cfg(feature = "plugin-system")]
                plugin_registry,
                gate,
                hook,
            );
            match dispatcher.run().await {
                Ok(outputs) => {
                    let _ = status_exec.send(crate::scheduler::ExecutionStatus::Completed(outputs));
                }
                Err(e) => {
                    if let Some(error_handler) = &schema_for_err.error_handler {
                        let partial_outputs = dispatcher.partial_outputs();
                        let pool_snapshot = dispatcher.snapshot_pool().await;
                        let error_context =
                            build_error_context(&e, schema_for_err.as_ref(), &partial_outputs);

                        if error_event_emitter.is_active() {
                            error_event_emitter
                                .emit(GraphEngineEvent::ErrorHandlerStarted {
                                    error: e.to_string(),
                                })
                                .await;
                        }

                        let runner = context
                            .sub_graph_runner()
                            .cloned()
                            .unwrap_or_else(|| Arc::new(DefaultSubGraphRunner));
                        match runner
                            .run_sub_graph(
                                &error_handler.sub_graph,
                                &pool_snapshot,
                                error_context,
                                context.as_ref(),
                            )
                            .await
                        {
                            Ok(handler_outputs) => {
                                let recovered_outputs: HashMap<String, Value> = handler_outputs
                                    .as_object()
                                    .map(|o| {
                                        o.iter()
                                            .map(|(k, v)| (k.clone(), v.clone()))
                                            .collect()
                                    })
                                    .unwrap_or_default();

                                if error_event_emitter.is_active() {
                                    error_event_emitter
                                        .emit(GraphEngineEvent::ErrorHandlerSucceeded {
                                            outputs: recovered_outputs.clone(),
                                        })
                                        .await;
                                }

                                match error_handler.mode {
                                    ErrorHandlingMode::Recover => {
                                        let _ = status_exec
                                            .send(crate::scheduler::ExecutionStatus::FailedWithRecovery {
                                                original_error: e.to_string(),
                                                recovered_outputs,
                                            });
                                    }
                                    ErrorHandlingMode::Notify => {
                                        let _ = status_exec
                                            .send(crate::scheduler::ExecutionStatus::Failed(e.to_string()));
                                    }
                                }
                            }
                            Err(handler_err) => {
                                if error_event_emitter.is_active() {
                                    error_event_emitter
                                        .emit(GraphEngineEvent::ErrorHandlerFailed {
                                            error: handler_err.to_string(),
                                        })
                                        .await;
                                }
                                let _ = status_exec
                                    .send(crate::scheduler::ExecutionStatus::Failed(e.to_string()));
                            }
                        }
                    } else {
                        let _ = status_exec
                            .send(crate::scheduler::ExecutionStatus::Failed(e.to_string()));
                    }
                }
            }

            security_gate
                .record_workflow_end(context.as_ref(), workflow_id_for_end.as_deref())
                .await;

            #[cfg(feature = "plugin-system")]
            if let Some(registry) = plugin_registry_for_shutdown {
                let _ = registry.shutdown_all().await;
            }
        });

        let workflow_handle = crate::scheduler::WorkflowHandle::new(
            status_rx,
            events,
            event_active,
        );
        let debug_handle = DebugHandle::new(cmd_tx, debug_evt_rx);

        Ok((workflow_handle, debug_handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::compiler::WorkflowCompiler;
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
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .user_inputs(inputs.clone());
        
        assert_eq!(builder.user_inputs.len(), 1);
        assert_eq!(builder.user_inputs.get("key"), Some(&Value::String("value".into())));
    }

    #[test]
    fn test_builder_system_vars() {
        let compiled = create_test_compiled();
        let mut vars = HashMap::new();
        vars.insert("sys_var".to_string(), Value::String("sys_value".into()));
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .system_vars(vars.clone());
        
        assert_eq!(builder.system_vars.len(), 1);
    }

    #[test]
    fn test_builder_environment_vars() {
        let compiled = create_test_compiled();
        let mut vars = HashMap::new();
        vars.insert("env_var".to_string(), Value::String("env_value".into()));
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .environment_vars(vars.clone());
        
        assert_eq!(builder.environment_vars.len(), 1);
    }

    #[test]
    fn test_builder_conversation_vars() {
        let compiled = create_test_compiled();
        let mut vars = HashMap::new();
        vars.insert("conv_var".to_string(), Value::String("conv_value".into()));
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .conversation_vars(vars.clone());
        
        assert_eq!(builder.conversation_vars.len(), 1);
    }

    #[test]
    fn test_builder_collect_events() {
        let compiled = create_test_compiled();
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .collect_events(false);
        
        assert!(!builder.collect_events);
    }

    #[test]
    fn test_builder_config() {
        let compiled = create_test_compiled();
        let config = EngineConfig {
            strict_template: true,
            ..Default::default()
        };
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .config(config.clone());
        
        assert!(builder.config.strict_template);
    }

    #[test]
    fn test_builder_context() {
        let compiled = create_test_compiled();
        let context = WorkflowContext::default();
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .context(context);
        
        assert!(builder.context.workflow_id.is_none());
    }

    #[test]
    fn test_builder_runtime_group() {
        let compiled = create_test_compiled();
        let runtime_group = Arc::new(RuntimeGroup::default());
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .runtime_group(runtime_group);
        
        assert!(builder.context.runtime_group.credential_provider.is_none());
    }

    #[test]
    fn test_builder_sub_graph_runner() {
        let compiled = create_test_compiled();
        let runner = Arc::new(DefaultSubGraphRunner);
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .sub_graph_runner(runner);
        
        assert!(builder.context.sub_graph_runner().is_some());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_builder_security_policy() {
        use crate::security::SecurityPolicy;
        
        let compiled = create_test_compiled();
        let policy = SecurityPolicy::permissive();
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .security_policy(policy);
        
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
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .resource_group(group);
        
        assert!(builder.context.resource_group().is_some());
    }

    #[test]
    fn test_builder_llm_providers() {
        let compiled = create_test_compiled();
        let registry = Arc::new(LlmProviderRegistry::new());
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .llm_providers(registry);
        
        assert!(builder.llm_provider_registry.is_some());
    }

    #[test]
    fn test_builder_debug() {
        let compiled = create_test_compiled();
        let debug_config = DebugConfig {
            break_on_start: true,
            ..Default::default()
        };
        
        let builder = CompiledWorkflowRunnerBuilder::new(compiled)
            .debug(debug_config.clone());
        
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
            crate::scheduler::ExecutionStatus::Completed(_) => {},
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
        
        let handle = compiled.runner()
            .user_inputs(inputs)
            .run()
            .await
            .unwrap();
        
        let status = handle.wait().await;
        match status {
            crate::scheduler::ExecutionStatus::Completed(outputs) => {
                assert_eq!(outputs.get("output"), Some(&Value::String("test".into())));
            },
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
        
        let handle = compiled.runner()
            .system_vars(sys_vars)
            .run()
            .await
            .unwrap();
        
        let status = handle.wait().await;
        match status {
            crate::scheduler::ExecutionStatus::Completed(outputs) => {
                assert_eq!(outputs.get("output"), Some(&Value::String("sys_value".into())));
            },
            other => panic!("Expected Completed, got {:?}", other),
        }
    }
}
