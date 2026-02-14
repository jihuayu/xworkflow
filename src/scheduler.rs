//! High-level workflow runner and builder.
//!
//! [`WorkflowRunner`] (constructed via [`WorkflowRunnerBuilder`]) is the main
//! entry point for executing a parsed workflow schema. It wires together the
//! graph engine, variable pool, node executors, LLM providers, debug hooks,
//! plugin system, and security layer.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex, RwLock};

mod plugin_gate;
mod security_gate;

// Re-export from application::bootstrap (canonical location)
pub(crate) use crate::application::bootstrap::plugin_bootstrap::{
    new_scheduler_plugin_gate, SchedulerPluginGate,
};
pub(crate) use crate::application::bootstrap::security_bootstrap::{
    new_scheduler_security_gate, SchedulerSecurityGate,
};

#[cfg(feature = "checkpoint")]
use crate::core::checkpoint::{CheckpointStore, ResumePolicy};
use crate::core::debug::{
    DebugConfig, DebugHandle, InteractiveDebugGate, InteractiveDebugHook, StepMode,
};
use crate::core::dispatcher::{Command, EngineConfig, EventEmitter, WorkflowDispatcher};
use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_group::RuntimeGroup;
use crate::core::sub_graph_runner::{DefaultSubGraphRunner, SubGraphRunner};
use crate::core::variable_pool::{Segment, VariablePool};
use crate::core::workflow_context::WorkflowContext;
use crate::core::SafeStopSignal;
use crate::dsl::schema::{ErrorHandlingMode, HumanInputDecision, WorkflowSchema};
use crate::dsl::validation::{validate_schema, ValidationReport};
use crate::error::WorkflowError;
use crate::graph::build_graph;
use crate::llm::LlmProviderRegistry;
#[cfg(feature = "builtin-agent-node")]
use crate::mcp::pool::McpConnectionPool;
use crate::nodes::executor::NodeExecutorRegistry;
#[cfg(feature = "security")]
use crate::security::{
    AuditLogger, CredentialProvider, ResourceGovernor, ResourceGroup, SecurityPolicy,
};

// ExecutionStatus is now defined in domain::execution::status
// Re-export for backward compatibility.
pub use crate::domain::execution::ExecutionStatus;

/// Handle to a running or completed workflow.
///
/// Allows polling [`status()`](Self::status), blocking on completion via
/// [`wait()`](Self::wait), and retrieving collected engine events.
pub struct WorkflowHandle {
    status_rx: watch::Receiver<ExecutionStatus>,
    events: Option<Arc<Mutex<Vec<GraphEngineEvent>>>>,
    event_active: Arc<AtomicBool>,
    command_tx: mpsc::Sender<Command>,
}

impl WorkflowHandle {
    pub(crate) fn new(
        status_rx: watch::Receiver<ExecutionStatus>,
        events: Option<Arc<Mutex<Vec<GraphEngineEvent>>>>,
        event_active: Arc<AtomicBool>,
        command_tx: mpsc::Sender<Command>,
    ) -> Self {
        Self {
            status_rx,
            events,
            event_active,
            command_tx,
        }
    }

    /// Return the current execution status (non-blocking).
    pub async fn status(&self) -> ExecutionStatus {
        self.status_rx.borrow().clone()
    }

    /// Return a snapshot of all collected engine events so far.
    pub async fn events(&self) -> Vec<GraphEngineEvent> {
        match &self.events {
            Some(events) => events.lock().await.clone(),
            None => Vec::new(),
        }
    }

    /// Block until the workflow reaches a terminal status.
    pub async fn wait(&self) -> ExecutionStatus {
        let mut rx = self.status_rx.clone();
        loop {
            let status = rx.borrow().clone();
            match status {
                ExecutionStatus::Running => {
                    if rx.changed().await.is_err() {
                        return rx.borrow().clone();
                    }
                }
                _ => return status,
            }
        }
    }

    pub async fn wait_or_paused(&self) -> ExecutionStatus {
        self.wait().await
    }

    pub async fn resume_with_input(
        &self,
        input: HashMap<String, Value>,
    ) -> Result<(), WorkflowError> {
        self.command_tx
            .send(Command::ResumeWithInput { input })
            .await
            .map_err(|_| WorkflowError::InternalError("Workflow already terminated".to_string()))
    }

    pub async fn resume_human_input(
        &self,
        node_id: &str,
        resume_token: &str,
        decision: Option<HumanInputDecision>,
        form_data: HashMap<String, Value>,
    ) -> Result<(), WorkflowError> {
        self.command_tx
            .send(Command::ResumeHumanInput {
                node_id: node_id.to_string(),
                resume_token: resume_token.to_string(),
                decision,
                form_data,
            })
            .await
            .map_err(|_| WorkflowError::InternalError("Workflow already terminated".to_string()))
    }

    pub async fn safe_stop(&self) -> Result<(), WorkflowError> {
        self.command_tx
            .send(Command::SafeStop)
            .await
            .map_err(|_| WorkflowError::InternalError("Workflow already terminated".to_string()))
    }

    /// Whether event collection is still active.
    pub fn events_active(&self) -> bool {
        self.event_active.load(Ordering::Relaxed)
    }
}

/// Workflow runner with builder-based configuration.
///
/// Use [`WorkflowRunner::builder(schema)`](Self::builder) to obtain a
/// [`WorkflowRunnerBuilder`].
#[allow(dead_code)]
pub struct WorkflowRunner {
    schema: WorkflowSchema,
    user_inputs: HashMap<String, Value>,
    system_vars: HashMap<String, Value>,
    environment_vars: HashMap<String, Value>,
    conversation_vars: HashMap<String, Value>,
    config: EngineConfig,
    context: WorkflowContext,
    plugin_gate: Box<dyn SchedulerPluginGate>,
    security_gate: Arc<dyn SchedulerSecurityGate>,
    llm_provider_registry: Option<Arc<LlmProviderRegistry>>,
    collect_events: bool,
}

impl WorkflowRunner {
    /// Create a new builder from a parsed workflow schema.
    pub fn builder(schema: WorkflowSchema) -> WorkflowRunnerBuilder {
        WorkflowRunnerBuilder {
            schema,
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

    /// Compile workflow DSL content into a reusable artifact.
    pub fn compile(
        content: &str,
        format: crate::dsl::DslFormat,
    ) -> Result<crate::compiler::CompiledWorkflow, WorkflowError> {
        crate::compiler::WorkflowCompiler::compile(content, format)
    }

    /// Compile a parsed workflow schema into a reusable artifact.
    pub fn compile_schema(
        schema: WorkflowSchema,
    ) -> Result<crate::compiler::CompiledWorkflow, WorkflowError> {
        crate::compiler::WorkflowCompiler::compile_schema(schema)
    }
}

// Helper functions are now in domain::compile::helpers.
// Re-export for backward compatibility within the crate.
pub(crate) use crate::application::workflow_run::segment_from_type;
#[allow(unused_imports)]
pub(crate) use crate::domain::compile::helpers::extract_error_node_info;
pub(crate) use crate::domain::compile::{
    build_error_context, collect_conversation_variable_types, collect_start_variable_types,
};

/// Builder for configuring and launching a [`WorkflowRunner`].
pub struct WorkflowRunnerBuilder {
    schema: WorkflowSchema,
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

impl WorkflowRunnerBuilder {
    /// Set user-supplied input variables.
    pub fn user_inputs(mut self, inputs: HashMap<String, Value>) -> Self {
        self.user_inputs = inputs;
        self
    }

    /// Set system variables (e.g. `sys.user_id`).
    pub fn system_vars(mut self, vars: HashMap<String, Value>) -> Self {
        self.system_vars = vars;
        self
    }

    /// Set environment variables accessible to the workflow.
    pub fn environment_vars(mut self, vars: HashMap<String, Value>) -> Self {
        self.environment_vars = vars;
        self
    }

    /// Set conversation variables for stateful chat workflows.
    pub fn conversation_vars(mut self, vars: HashMap<String, Value>) -> Self {
        self.conversation_vars = vars;
        self
    }

    /// Enable or disable event collection.
    pub fn collect_events(mut self, collect: bool) -> Self {
        self.collect_events = collect;
        self
    }

    /// Set the engine configuration (timeouts, max steps, etc.).
    pub fn config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    /// Set a custom workflow context.
    pub fn context(mut self, context: WorkflowContext) -> Self {
        self.context = context;
        self
    }

    /// Set a shared runtime group and reset the workflow context.
    pub fn runtime_group(mut self, runtime_group: Arc<RuntimeGroup>) -> Self {
        self.context = WorkflowContext::new(runtime_group);
        self
    }

    /// Set a custom sub-graph runner for iteration/loop nodes.
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

    /// Set the LLM provider registry.
    pub fn llm_providers(mut self, registry: Arc<LlmProviderRegistry>) -> Self {
        self.llm_provider_registry = Some(registry);
        self
    }

    /// Enable interactive debugging with the given config.
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

    /// Validate the workflow schema without running it.
    pub fn validate(&self) -> ValidationReport {
        validate_schema(&self.schema)
    }

    /// Build and launch the workflow, returning a [`WorkflowHandle`].
    #[allow(unused_mut)]
    pub async fn run(self) -> Result<WorkflowHandle, WorkflowError> {
        let mut builder = self;
        let mut report = builder
            .security_gate
            .validate_schema(&builder.schema, &builder.context);
        if !report.is_valid {
            builder
                .security_gate
                .audit_validation_failed(&builder.context, &report)
                .await;
            return Err(WorkflowError::ValidationFailed(Box::new(report)));
        }

        builder
            .plugin_gate
            .init_and_extend_validation(&builder.schema, &mut report)
            .await?;
        builder.plugin_gate.after_dsl_validation(&report).await?;

        if !report.is_valid {
            builder
                .security_gate
                .audit_validation_failed(&builder.context, &report)
                .await;
            return Err(WorkflowError::ValidationFailed(Box::new(report)));
        }

        let graph = build_graph(&builder.schema)?;

        // Build variable pool
        let mut pool = VariablePool::new();
        let start_var_types = collect_start_variable_types(&builder.schema);
        let conversation_var_types = collect_conversation_variable_types(&builder.schema);
        builder
            .security_gate
            .configure_variable_pool(&builder.context, &mut pool);

        // Set system variables
        #[cfg(feature = "builtin-agent-node")]
        if !builder.schema.mcp_servers.is_empty() {
            builder
                .system_vars
                .entry("__mcp_servers".to_string())
                .or_insert_with(|| {
                    serde_json::to_value(&builder.schema.mcp_servers)
                        .unwrap_or(Value::Object(serde_json::Map::new()))
                });
        }

        for (k, v) in &builder.system_vars {
            let selector = crate::core::variable_pool::Selector::new("sys", k.clone());
            pool.set(&selector, Segment::from_value(v));
        }

        // Set environment variables
        for (k, v) in &builder.environment_vars {
            let selector = crate::core::variable_pool::Selector::new("env", k.clone());
            pool.set(&selector, Segment::from_value(v));
        }

        // Set conversation variables
        for (k, v) in &builder.conversation_vars {
            let selector = crate::core::variable_pool::Selector::new("conversation", k.clone());
            let seg = segment_from_type(v, conversation_var_types.get(k));
            pool.set(&selector, seg);
        }

        // Set user inputs mapped to start node
        let start_node_id = graph.root_node_id().to_string();
        for (k, v) in &builder.user_inputs {
            let selector =
                crate::core::variable_pool::Selector::new(start_node_id.clone(), k.clone());
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
        #[cfg(feature = "builtin-agent-node")]
        {
            let mcp_pool = Arc::new(RwLock::new(McpConnectionPool::new()));
            registry.set_mcp_pool(mcp_pool);
        }

        let registry = Arc::new(registry);
        builder.context = builder
            .context
            .with_node_executor_registry(Arc::clone(&registry));
        builder.context = builder
            .context
            .with_llm_provider_registry(Arc::clone(&llm_registry));

        let (tx, mut rx) = mpsc::channel(256);
        let event_active = Arc::new(AtomicBool::new(builder.collect_events));
        let event_emitter = EventEmitter::new(tx.clone(), event_active.clone());
        let error_event_emitter = event_emitter.clone();
        let config = builder
            .security_gate
            .effective_engine_config(&builder.context, builder.config);
        builder.context.strict_template = config.strict_template;

        let workflow_id = builder
            .security_gate
            .on_workflow_start(&builder.context)
            .await?;
        builder.context.workflow_id = workflow_id.clone();
        builder.plugin_gate.customize_context(&mut builder.context);

        #[cfg(feature = "plugin-system")]
        let plugin_registry = builder.plugin_gate.take_plugin_registry_arc();
        #[cfg(feature = "plugin-system")]
        let plugin_registry_for_shutdown = plugin_registry.clone();

        let context = Arc::new(builder.context.with_event_tx(tx.clone()));
        let (command_tx, command_rx) = mpsc::channel(64);

        #[cfg(feature = "checkpoint")]
        let checkpoint_store = builder.checkpoint_store.clone();
        #[cfg(feature = "checkpoint")]
        let checkpoint_workflow_id = builder
            .workflow_id
            .clone()
            .or_else(|| workflow_id.clone())
            .or_else(|| Some(context.execution_id.clone()));
        #[cfg(feature = "checkpoint")]
        let checkpoint_execution_id = Some(context.execution_id.clone());
        #[cfg(feature = "checkpoint")]
        let checkpoint_resume_policy = builder.resume_policy;
        let safe_stop_signal = builder.safe_stop_signal.clone();
        #[cfg(feature = "checkpoint")]
        let checkpoint_schema_hash = crate::core::checkpoint::hash_json(&builder.schema);
        #[cfg(feature = "checkpoint")]
        let checkpoint_engine_config_hash = crate::core::checkpoint::hash_json(&config);

        let (status_tx, status_rx) = watch::channel(ExecutionStatus::Running);
        let events = if builder.collect_events {
            Some(Arc::new(Mutex::new(Vec::new())))
        } else {
            None
        };

        if let Some(events_clone) = events.clone() {
            let active_flag = event_active.clone();
            // Spawn event collector
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

        // Spawn workflow execution
        let status_exec = status_tx.clone();
        let schema = builder.schema.clone();
        let security_gate = Arc::clone(&builder.security_gate);
        let workflow_id_for_end = workflow_id.clone();
        tokio::spawn(async move {
            let mut dispatcher = WorkflowDispatcher::new_with_registry(
                graph,
                pool,
                registry,
                event_emitter,
                config,
                context.clone(),
                #[cfg(feature = "plugin-system")]
                plugin_registry,
            );
            dispatcher.set_control_channels(status_exec.clone(), command_rx);
            dispatcher.set_safe_stop_signal(safe_stop_signal);
            #[cfg(feature = "checkpoint")]
            dispatcher.set_checkpoint_options(
                checkpoint_store,
                checkpoint_workflow_id,
                checkpoint_execution_id,
                checkpoint_resume_policy,
                checkpoint_schema_hash,
                checkpoint_engine_config_hash,
            );
            match dispatcher.run().await {
                Ok(outputs) => {
                    let _ = status_exec.send(ExecutionStatus::Completed(outputs));
                }
                Err(e) => {
                    if let WorkflowError::SafeStopped {
                        last_completed_node,
                        interrupted_nodes,
                        checkpoint_saved,
                    } = &e
                    {
                        let _ = status_exec.send(ExecutionStatus::SafeStopped {
                            last_completed_node: last_completed_node.clone(),
                            interrupted_nodes: interrupted_nodes.clone(),
                            checkpoint_saved: *checkpoint_saved,
                        });
                        return;
                    }

                    if let Some(error_handler) = &schema.error_handler {
                        let partial_outputs = dispatcher.partial_outputs();
                        let pool_snapshot = dispatcher.snapshot_pool().await;
                        let error_context = build_error_context(&e, &schema, &partial_outputs);

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
                                        o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
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
                                        let _ =
                                            status_exec.send(ExecutionStatus::FailedWithRecovery {
                                                original_error: e.to_string(),
                                                recovered_outputs,
                                            });
                                    }
                                    ErrorHandlingMode::Notify => {
                                        let _ = status_exec
                                            .send(ExecutionStatus::Failed(e.to_string()));
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
                                let _ = status_exec.send(ExecutionStatus::Failed(e.to_string()));
                            }
                        }
                    } else {
                        let _ = status_exec.send(ExecutionStatus::Failed(e.to_string()));
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

        Ok(WorkflowHandle {
            status_rx,
            events,
            event_active,
            command_tx,
        })
    }

    #[allow(unused_mut)]
    pub async fn run_debug(self) -> Result<(WorkflowHandle, DebugHandle), WorkflowError> {
        let mut builder = self;
        let debug_config = builder.debug_config.clone().unwrap_or_default();
        let mut report = builder
            .security_gate
            .validate_schema(&builder.schema, &builder.context);
        if !report.is_valid {
            return Err(WorkflowError::ValidationFailed(Box::new(report)));
        }

        builder
            .plugin_gate
            .init_and_extend_validation(&builder.schema, &mut report)
            .await?;
        builder.plugin_gate.after_dsl_validation(&report).await?;

        if !report.is_valid {
            return Err(WorkflowError::ValidationFailed(Box::new(report)));
        }

        let graph = build_graph(&builder.schema)?;

        let mut pool = VariablePool::new();
        let start_var_types = collect_start_variable_types(&builder.schema);
        let conversation_var_types = collect_conversation_variable_types(&builder.schema);

        #[cfg(feature = "builtin-agent-node")]
        if !builder.schema.mcp_servers.is_empty() {
            builder
                .system_vars
                .entry("__mcp_servers".to_string())
                .or_insert_with(|| {
                    serde_json::to_value(&builder.schema.mcp_servers)
                        .unwrap_or(Value::Object(serde_json::Map::new()))
                });
        }

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

        let start_node_id = graph.root_node_id().to_string();
        for (k, v) in &builder.user_inputs {
            let selector =
                crate::core::variable_pool::Selector::new(start_node_id.clone(), k.clone());
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
        #[cfg(feature = "builtin-agent-node")]
        {
            let mcp_pool = Arc::new(RwLock::new(McpConnectionPool::new()));
            registry.set_mcp_pool(mcp_pool);
        }

        let registry = Arc::new(registry);
        builder.context = builder
            .context
            .with_node_executor_registry(Arc::clone(&registry));
        builder.context = builder
            .context
            .with_llm_provider_registry(Arc::clone(&llm_registry));

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

        let workflow_id = builder
            .security_gate
            .on_workflow_start(&builder.context)
            .await?;
        builder.context.workflow_id = workflow_id.clone();
        builder.plugin_gate.customize_context(&mut builder.context);

        #[cfg(feature = "plugin-system")]
        let plugin_registry = builder.plugin_gate.take_plugin_registry_arc();
        #[cfg(feature = "plugin-system")]
        let plugin_registry_for_shutdown = plugin_registry.clone();

        let context = Arc::new(builder.context.with_event_tx(tx.clone()));
        let (command_tx, command_rx) = mpsc::channel(64);

        #[cfg(feature = "checkpoint")]
        let checkpoint_store = builder.checkpoint_store.clone();
        #[cfg(feature = "checkpoint")]
        let checkpoint_workflow_id = builder
            .workflow_id
            .clone()
            .or_else(|| workflow_id.clone())
            .or_else(|| Some(context.execution_id.clone()));
        #[cfg(feature = "checkpoint")]
        let checkpoint_execution_id = Some(context.execution_id.clone());
        #[cfg(feature = "checkpoint")]
        let checkpoint_resume_policy = builder.resume_policy;
        let safe_stop_signal = builder.safe_stop_signal.clone();
        #[cfg(feature = "checkpoint")]
        let checkpoint_schema_hash = crate::core::checkpoint::hash_json(&builder.schema);
        #[cfg(feature = "checkpoint")]
        let checkpoint_engine_config_hash = crate::core::checkpoint::hash_json(&config);

        let (status_tx, status_rx) = watch::channel(ExecutionStatus::Running);
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
        let schema = builder.schema.clone();
        let mut hook = hook;
        hook.graph_event_tx = Some(tx.clone());

        let security_gate = Arc::clone(&builder.security_gate);
        let workflow_id_for_end = workflow_id.clone();

        tokio::spawn(async move {
            let mut dispatcher = WorkflowDispatcher::new_with_debug(
                graph,
                pool,
                registry,
                event_emitter,
                config,
                context.clone(),
                #[cfg(feature = "plugin-system")]
                plugin_registry,
                gate,
                hook,
            );
            dispatcher.set_control_channels(status_exec.clone(), command_rx);
            dispatcher.set_safe_stop_signal(safe_stop_signal);
            #[cfg(feature = "checkpoint")]
            dispatcher.set_checkpoint_options(
                checkpoint_store,
                checkpoint_workflow_id,
                checkpoint_execution_id,
                checkpoint_resume_policy,
                checkpoint_schema_hash,
                checkpoint_engine_config_hash,
            );
            match dispatcher.run().await {
                Ok(outputs) => {
                    let _ = status_exec.send(ExecutionStatus::Completed(outputs));
                }
                Err(e) => {
                    if let WorkflowError::SafeStopped {
                        last_completed_node,
                        interrupted_nodes,
                        checkpoint_saved,
                    } = &e
                    {
                        let _ = status_exec.send(ExecutionStatus::SafeStopped {
                            last_completed_node: last_completed_node.clone(),
                            interrupted_nodes: interrupted_nodes.clone(),
                            checkpoint_saved: *checkpoint_saved,
                        });
                        return;
                    }

                    if let Some(error_handler) = &schema.error_handler {
                        let partial_outputs = dispatcher.partial_outputs();
                        let pool_snapshot = dispatcher.snapshot_pool().await;
                        let error_context = build_error_context(&e, &schema, &partial_outputs);

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
                                        o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
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
                                        let _ =
                                            status_exec.send(ExecutionStatus::FailedWithRecovery {
                                                original_error: e.to_string(),
                                                recovered_outputs,
                                            });
                                    }
                                    ErrorHandlingMode::Notify => {
                                        let _ = status_exec
                                            .send(ExecutionStatus::Failed(e.to_string()));
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
                                let _ = status_exec.send(ExecutionStatus::Failed(e.to_string()));
                            }
                        }
                    } else {
                        let _ = status_exec.send(ExecutionStatus::Failed(e.to_string()));
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

        let workflow_handle = WorkflowHandle {
            status_rx,
            events,
            event_active,
            command_tx,
        };
        let debug_handle = DebugHandle::new(cmd_tx, debug_evt_rx);

        Ok((workflow_handle, debug_handle))
    }
}

#[cfg(all(test, feature = "builtin-core-nodes"))]
mod tests {
    use super::*;
    use crate::core::debug::{
        DebugConfig, DebugEvent, PauseLocation, PauseReason as DebugPauseReason,
    };
    use crate::domain::compile::helpers::error_type_name;
    use crate::dsl::{parse_dsl, DslFormat};

    #[tokio::test]
    async fn test_scheduler_basic() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          label: Q
          type: string
          required: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["start", "query"]
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();

        let mut inputs = HashMap::new();
        inputs.insert("query".to_string(), Value::String("test".into()));

        let mut sys = HashMap::new();
        sys.insert("query".to_string(), Value::String("test".into()));

        let handle = WorkflowRunner::builder(schema)
            .user_inputs(inputs)
            .system_vars(sys)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;

        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(outputs.get("result"), Some(&Value::String("test".into())));
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_scheduler_with_branch() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: if1
    data:
      type: if-else
      title: Check
      cases:
        - case_id: "yes"
          logical_operator: and
          conditions:
            - variable_selector: ["start", "n"]
              comparison_operator: greater_than
              value: 5
  - id: end_a
    data:
      type: end
      title: End A
      outputs:
        - variable: path
          value_selector: ["start", "n"]
  - id: end_b
    data:
      type: end
      title: End B
      outputs:
        - variable: path
          value_selector: ["start", "n"]
edges:
  - source: start
    target: if1
  - source: if1
    target: end_a
    sourceHandle: "yes"
  - source: if1
    target: end_b
    sourceHandle: "false"
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();

        let mut inputs = HashMap::new();
        inputs.insert("n".to_string(), serde_json::json!(10));

        let handle = WorkflowRunner::builder(schema)
            .user_inputs(inputs)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;

        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(outputs.get("path"), Some(&serde_json::json!(10)));
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_scheduler_run_debug_break_on_start() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          label: Q
          type: string
          required: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["start", "query"]
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();

        let mut inputs = HashMap::new();
        inputs.insert("query".to_string(), Value::String("test".into()));

        let dbg = DebugConfig {
            break_on_start: true,
            auto_snapshot: true,
            ..DebugConfig::default()
        };

        let (_handle, debug) = WorkflowRunner::builder(schema)
            .user_inputs(inputs)
            .debug(dbg)
            .run_debug()
            .await
            .unwrap();

        let pause = debug.wait_for_pause().await.unwrap();
        match pause {
            DebugEvent::Paused { reason, location } => {
                assert!(matches!(reason, DebugPauseReason::Initial));
                match location {
                    PauseLocation::BeforeNode { node_id, .. } => assert_eq!(node_id, "start"),
                    other => panic!("Expected pause before node, got {:?}", other),
                }
            }
            other => panic!("Expected paused event, got {:?}", other),
        }

        debug.inspect_variables().await.unwrap();

        let snapshot = loop {
            match debug.next_event().await {
                Some(DebugEvent::VariableSnapshot { variables }) => break variables,
                Some(_) => continue,
                None => panic!("Missing variable snapshot event"),
            }
        };
        assert!(snapshot.contains_key(&VariablePool::make_key("start", "query").to_string()));

        debug.continue_run().await.unwrap();
    }

    #[tokio::test]
    async fn test_scheduler_collect_events_disabled() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          label: Q
          type: string
          required: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["start", "query"]
edges:
  - source: start
    target: end
"#;

        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let mut inputs = HashMap::new();
        inputs.insert("query".to_string(), Value::String("test".into()));

        let handle = WorkflowRunner::builder(schema)
            .user_inputs(inputs)
            .collect_events(false)
            .run()
            .await
            .unwrap();

        let _ = handle.wait().await;
        let events = handle.events().await;
        assert!(events.is_empty());
    }

    #[test]
    fn test_error_type_name_variants() {
        assert_eq!(
            error_type_name(&WorkflowError::NodeExecutionError {
                node_id: "n".into(),
                error: "e".into(),
                error_detail: None,
            }),
            "NodeExecutionError"
        );
        assert_eq!(error_type_name(&WorkflowError::Timeout), "Timeout");
        assert_eq!(error_type_name(&WorkflowError::ExecutionTimeout), "Timeout");
        assert_eq!(
            error_type_name(&WorkflowError::MaxStepsExceeded(100)),
            "MaxStepsExceeded"
        );
        assert_eq!(
            error_type_name(&WorkflowError::Aborted("x".into())),
            "Aborted"
        );
        assert_eq!(
            error_type_name(&WorkflowError::InternalError("x".into())),
            "InternalError"
        );
    }

    #[test]
    fn test_build_error_context_with_node_error() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: code1
    data: { type: code, title: Code, code: "x", language: javascript }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: code1
  - source: code1
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let error = WorkflowError::NodeExecutionError {
            node_id: "code1".into(),
            error: "boom".into(),
            error_detail: None,
        };
        let mut partial = HashMap::new();
        partial.insert("foo".to_string(), Value::String("bar".into()));
        let ctx = build_error_context(&error, &schema, &partial);
        assert!(ctx
            .get("sys.error_message")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("boom"));
        assert_eq!(
            ctx.get("sys.error_node_id").unwrap(),
            &Value::String("code1".into())
        );
        assert_eq!(
            ctx.get("sys.error_node_type").unwrap(),
            &Value::String("code".into())
        );
        assert_eq!(
            ctx.get("sys.error_type").unwrap(),
            &Value::String("NodeExecutionError".into())
        );
        assert!(ctx.get("sys.workflow_outputs").unwrap().is_object());
    }

    #[test]
    fn test_build_error_context_non_node_error() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let error = WorkflowError::Timeout;
        let ctx = build_error_context(&error, &schema, &HashMap::new());
        assert_eq!(
            ctx.get("sys.error_node_id").unwrap(),
            &Value::String("".into())
        );
        assert_eq!(
            ctx.get("sys.error_node_type").unwrap(),
            &Value::String("".into())
        );
        assert_eq!(
            ctx.get("sys.error_type").unwrap(),
            &Value::String("Timeout".into())
        );
    }

    #[test]
    fn test_segment_from_type_array_string() {
        use crate::core::variable_pool::SegmentType;
        let val = serde_json::json!(["a", "b", "c"]);
        let seg = segment_from_type(&val, Some(&SegmentType::ArrayString));
        assert!(matches!(seg, Segment::ArrayString(_)));
    }

    #[test]
    fn test_segment_from_type_array_string_non_string_items() {
        use crate::core::variable_pool::SegmentType;
        let val = serde_json::json!([1, 2, 3]);
        let seg = segment_from_type(&val, Some(&SegmentType::ArrayString));
        // Mixed items fall back to from_value
        assert!(!matches!(seg, Segment::ArrayString(_)));
    }

    #[test]
    fn test_segment_from_type_none() {
        let val = serde_json::json!("hello");
        let seg = segment_from_type(&val, None);
        assert!(matches!(seg, Segment::String(_)));
    }

    #[test]
    fn test_segment_from_type_file() {
        let val = serde_json::json!({
          "name": "report.pdf",
          "size": 123,
          "mime_type": "application/pdf",
          "transfer_method": "remote_url",
          "url": "https://example.com/report.pdf"
        });
        let seg = segment_from_type(&val, Some(&crate::core::variable_pool::SegmentType::File));
        assert!(matches!(seg, Segment::File(_)));
    }

    #[test]
    fn test_segment_from_type_array_file() {
        let val = serde_json::json!([
          {
            "name": "a.txt",
            "size": 10,
            "mime_type": "text/plain",
            "transfer_method": "local_file",
            "id": "C:/tmp/a.txt"
          },
          {
            "name": "b.txt",
            "size": 20,
            "mime_type": "text/plain",
            "transfer_method": "remote_url",
            "url": "https://example.com/b.txt"
          }
        ]);
        let seg = segment_from_type(
            &val,
            Some(&crate::core::variable_pool::SegmentType::ArrayFile),
        );
        assert!(matches!(seg, Segment::ArrayFile(_)));
    }

    #[test]
    fn test_collect_start_variable_types() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: name
          label: Name
          type: string
          required: true
        - variable: items
          label: Items
          type: array[string]
          required: false
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let types = collect_start_variable_types(&schema);
        assert!(types.contains_key("name"));
        assert!(types.contains_key("items"));
    }

    #[test]
    fn test_collect_conversation_variable_types() {
        let yaml = r#"
version: "0.1.0"
conversation_variables:
  - name: greeting
    type: string
  - name: count
    type: number
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let types = collect_conversation_variable_types(&schema);
        assert!(types.contains_key("greeting"));
        assert!(types.contains_key("count"));
    }

    #[test]
    fn test_extract_error_node_info_node_error() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let error = WorkflowError::NodeExecutionError {
            node_id: "start".into(),
            error: "e".into(),
            error_detail: None,
        };
        let (nid, ntype) = extract_error_node_info(&error, &schema);
        assert_eq!(nid.unwrap(), "start");
        assert_eq!(ntype.unwrap(), "start");
    }

    #[test]
    fn test_extract_error_node_info_non_node_error() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let (nid, ntype) = extract_error_node_info(&WorkflowError::Timeout, &schema);
        assert!(nid.is_none());
        assert!(ntype.is_none());
    }

    #[tokio::test]
    async fn test_execution_status_debug() {
        let s = ExecutionStatus::Running;
        let debug_str = format!("{:?}", s);
        assert!(debug_str.contains("Running"));

        let s2 = ExecutionStatus::Completed(HashMap::new());
        let debug_str2 = format!("{:?}", s2);
        assert!(debug_str2.contains("Completed"));

        let s3 = ExecutionStatus::Failed("err".into());
        let debug_str3 = format!("{:?}", s3);
        assert!(debug_str3.contains("Failed"));

        let s4 = ExecutionStatus::FailedWithRecovery {
            original_error: "orig".into(),
            recovered_outputs: HashMap::new(),
        };
        let debug_str4 = format!("{:?}", s4);
        assert!(debug_str4.contains("FailedWithRecovery"));
    }

    #[tokio::test]
    async fn test_builder_environment_and_conversation_vars() {
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
        - variable: env_val
          value_selector: ["env", "api_key"]
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let mut env_vars = HashMap::new();
        env_vars.insert("api_key".to_string(), Value::String("secret".into()));
        let mut conv_vars = HashMap::new();
        conv_vars.insert("history".to_string(), Value::Array(vec![]));

        let handle = WorkflowRunner::builder(schema)
            .environment_vars(env_vars)
            .conversation_vars(conv_vars)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("env_val"),
                    Some(&Value::String("secret".into()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[test]
    fn test_validate_reports_issues() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
edges: []
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let builder = WorkflowRunner::builder(schema);
        let report = builder.validate();
        // Missing end node, no edges etc
        assert!(!report.diagnostics.is_empty());
    }

    #[test]
    fn test_segment_from_type_integer() {
        let val = serde_json::json!(42);
        let seg = segment_from_type(&val, Some(&crate::core::variable_pool::SegmentType::Number));
        assert_eq!(seg, crate::core::variable_pool::Segment::Integer(42));
    }

    #[test]
    fn test_segment_from_type_string() {
        let val = serde_json::json!("hello");
        let seg = segment_from_type(&val, Some(&crate::core::variable_pool::SegmentType::String));
        assert_eq!(
            seg,
            crate::core::variable_pool::Segment::String("hello".into())
        );
    }

    #[test]
    fn test_segment_from_type_boolean() {
        let val = serde_json::json!(true);
        let seg = segment_from_type(
            &val,
            Some(&crate::core::variable_pool::SegmentType::Boolean),
        );
        assert_eq!(seg, crate::core::variable_pool::Segment::Boolean(true));
    }

    #[test]
    fn test_segment_from_type_float() {
        let val = serde_json::json!(2.5);
        let seg = segment_from_type(&val, Some(&crate::core::variable_pool::SegmentType::Number));
        match seg {
            crate::core::variable_pool::Segment::Float(f) => assert!((f - 2.5).abs() < 0.001),
            _ => panic!("Expected Float"),
        }
    }

    #[test]
    fn test_segment_from_type_array_string_valid() {
        let val = serde_json::json!(["a", "b"]);
        let seg = segment_from_type(
            &val,
            Some(&crate::core::variable_pool::SegmentType::ArrayString),
        );
        match seg {
            crate::core::variable_pool::Segment::ArrayString(_) => {}
            _ => panic!("Expected ArrayString, got {:?}", seg),
        }
    }

    #[tokio::test]
    async fn test_builder_llm_providers() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let registry = crate::llm::LlmProviderRegistry::new();
        // Register a mock provider is not possible without the trait, but
        // the builder path is exercised
        let handle = WorkflowRunner::builder(schema)
            .llm_providers(Arc::new(registry))
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[tokio::test]
    async fn test_handle_events_active() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema)
            .collect_events(true)
            .run()
            .await
            .unwrap();
        assert!(handle.events_active());
    }

    #[tokio::test]
    async fn test_collect_start_variable_types_unmapped() {
        // Test that variables with unmapped types are skipped
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: known
          type: string
          required: false
        - variable: exotic
          type: some-unknown-type
          required: false
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let types = collect_start_variable_types(&schema);
        assert!(types.contains_key("known"));
        // exotic type should be skipped
        assert!(!types.contains_key("exotic"));
    }

    #[tokio::test]
    async fn test_scheduler_workflow_with_template_transform() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: name
          type: string
          required: true
  - id: tt
    data:
      type: template-transform
      title: TT
      template: "Hello {{ name }}"
      variables:
        - variable: name
          value_selector: ["start", "name"]
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["tt", "output"]
edges:
  - source: start
    target: tt
  - source: tt
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let mut input_vars = HashMap::new();
        input_vars.insert("name".to_string(), Value::String("World".into()));
        let handle = WorkflowRunner::builder(schema)
            .user_inputs(input_vars)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("result"),
                    Some(&Value::String("Hello World".into()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_builder_security_policy() {
        use crate::security::policy::SecurityPolicy;
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let policy = SecurityPolicy::permissive();
        let handle = WorkflowRunner::builder(schema)
            .security_policy(policy)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_builder_resource_group() {
        use crate::security::resource_group::{ResourceGroup, ResourceQuota};
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let group = ResourceGroup {
            group_id: "g1".into(),
            group_name: Some("Group1".into()),
            security_level: crate::security::policy::SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };
        let handle = WorkflowRunner::builder(schema)
            .resource_group(group)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_builder_resource_governor() {
        use crate::security::governor::InMemoryResourceGovernor;
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let governor = Arc::new(InMemoryResourceGovernor::new(HashMap::new()));
        let handle = WorkflowRunner::builder(schema)
            .resource_governor(governor)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_builder_credential_provider() {
        // Create a simple credential provider implementation
        struct TestCredProvider;
        #[async_trait::async_trait]
        impl crate::security::credential::CredentialProvider for TestCredProvider {
            async fn get_credentials(
                &self,
                _group_id: &str,
                _provider: &str,
            ) -> Result<HashMap<String, String>, crate::security::credential::CredentialError>
            {
                Ok(HashMap::new())
            }
        }
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let provider = Arc::new(TestCredProvider);
        let handle = WorkflowRunner::builder(schema)
            .credential_provider(provider)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_builder_audit_logger() {
        use crate::security::audit::TracingAuditLogger;
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let logger = Arc::new(TracingAuditLogger);
        let handle = WorkflowRunner::builder(schema)
            .audit_logger(logger)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_builder_all_security_combined() {
        use crate::security::audit::TracingAuditLogger;
        use crate::security::governor::InMemoryResourceGovernor;
        use crate::security::policy::SecurityPolicy;
        use crate::security::resource_group::{ResourceGroup, ResourceQuota};
        struct TestCredProvider2;
        #[async_trait::async_trait]
        impl crate::security::credential::CredentialProvider for TestCredProvider2 {
            async fn get_credentials(
                &self,
                _group_id: &str,
                _provider: &str,
            ) -> Result<HashMap<String, String>, crate::security::credential::CredentialError>
            {
                Ok(HashMap::new())
            }
        }
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema)
            .security_policy(SecurityPolicy::permissive())
            .resource_group(ResourceGroup {
                group_id: "g1".into(),
                group_name: Some("G1".into()),
                security_level: crate::security::policy::SecurityLevel::Standard,
                quota: ResourceQuota::default(),
                credential_refs: HashMap::new(),
            })
            .resource_governor(Arc::new(InMemoryResourceGovernor::new(HashMap::new())))
            .credential_provider(Arc::new(TestCredProvider2))
            .audit_logger(Arc::new(TracingAuditLogger))
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[tokio::test]
    async fn test_scheduler_workflow_failure_no_handler() {
        // A workflow with a node that will fail and no error handler
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: code
    data:
      type: code
      title: Code
      code: "function main(inputs) { throw new Error('intentional failure'); }"
      language: javascript
  - id: end
    data:
      type: end
      title: End
      outputs: []
edges:
  - source: start
    target: code
  - source: code
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema).run().await.unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Failed(_)));
    }

    #[tokio::test]
    async fn test_scheduler_with_config() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let config = EngineConfig {
            max_steps: 50,
            max_execution_time_secs: 10,
            ..EngineConfig::default()
        };
        let handle = WorkflowRunner::builder(schema)
            .config(config)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[tokio::test]
    async fn test_scheduler_with_context() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let context = WorkflowContext::default();
        let handle = WorkflowRunner::builder(schema)
            .context(context)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[tokio::test]
    async fn test_scheduler_no_events_collection() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema)
            .collect_events(false)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
        let events = handle.events().await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn test_workflow_handle_events_active_after_completion() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema).run().await.unwrap();
        let _status = handle.wait().await;
        // After completion, events_active should eventually become false
        // (event_active is set to false after tokio task completes)
        // Give a small delay for the task to wrap up
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!handle.events_active());
    }

    #[tokio::test]
    async fn test_scheduler_with_sub_graph_runner() {
        use crate::core::sub_graph_runner::DefaultSubGraphRunner;
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let runner = Arc::new(DefaultSubGraphRunner);
        let handle = WorkflowRunner::builder(schema)
            .sub_graph_runner(runner)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[tokio::test]
    async fn test_scheduler_with_conversation_vars() {
        let yaml = r#"
version: "0.1.0"
conversation_variables:
  - name: history
    type: string
nodes:
  - id: s
    data:
      type: start
      title: S
  - id: e
    data:
      type: end
      title: E
      outputs:
        - variable: result
          value_selector: ["conversation", "history"]
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let mut conv_vars = HashMap::new();
        conv_vars.insert("history".to_string(), Value::String("prev msg".into()));
        let handle = WorkflowRunner::builder(schema)
            .conversation_vars(conv_vars)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        assert!(matches!(status, ExecutionStatus::Completed(_)));
    }

    #[tokio::test]
    async fn test_scheduler_with_env_vars() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data:
      type: end
      title: E
      outputs:
        - variable: result
          value_selector: ["env", "api_key"]
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let mut env_vars = HashMap::new();
        env_vars.insert("api_key".to_string(), Value::String("secret123".into()));
        let handle = WorkflowRunner::builder(schema)
            .environment_vars(env_vars)
            .run()
            .await
            .unwrap();
        let status = handle.wait().await;
        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("result"),
                    Some(&Value::String("secret123".into()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_scheduler_workflow_with_events() {
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
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema)
            .collect_events(true)
            .run()
            .await
            .unwrap();
        let _ = handle.wait().await;
        // Give a moment for events to flush
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let events = handle.events().await;
        assert!(!events.is_empty());
    }

    async fn wait_until_terminal(handle: &WorkflowHandle) -> ExecutionStatus {
        for _ in 0..80 {
            let status = handle.status().await;
            match status {
                ExecutionStatus::Completed(_)
                | ExecutionStatus::Failed(_)
                | ExecutionStatus::FailedWithRecovery { .. }
                | ExecutionStatus::SafeStopped { .. } => return status,
                _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        handle.status().await
    }

    #[tokio::test]
    async fn test_scheduler_human_input_form_resume() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: human
    data:
      type: human-input
      title: Human
      resume_mode: form
      prompt_text: "Please input city"
      form_fields:
        - variable: city
          label: City
          field_type: text
          required: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: city
          value_selector: ["human", "city"]
edges:
  - source: start
    target: human
  - source: human
    target: end
"#;

        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema).run().await.unwrap();
        let paused = handle.wait_or_paused().await;

        let (node_id, resume_token) = match paused {
            ExecutionStatus::WaitingForInput {
                node_id,
                resume_token,
                ..
            } => (node_id, resume_token),
            other => panic!("Expected WaitingForInput, got {:?}", other),
        };

        let mut form_data = HashMap::new();
        form_data.insert("city".to_string(), Value::String("Shanghai".to_string()));
        handle
            .resume_human_input(&node_id, &resume_token, None, form_data)
            .await
            .unwrap();

        let status = wait_until_terminal(&handle).await;
        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("city"),
                    Some(&Value::String("Shanghai".to_string()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_scheduler_human_input_approval_branch() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: human
    data:
      type: human-input
      title: Approval
      resume_mode: approval
      prompt_text: "Approve this request?"
      form_fields:
        - variable: comment
          label: Comment
          field_type: textarea
          required: false
  - id: approved_end
    data:
      type: end
      title: Approved
      outputs:
        - variable: decision
          value_selector: ["human", "__decision"]
  - id: rejected_end
    data:
      type: end
      title: Rejected
      outputs:
        - variable: decision
          value_selector: ["human", "__decision"]
edges:
  - source: start
    target: human
  - source: human
    target: approved_end
    sourceHandle: approve
  - source: human
    target: rejected_end
    sourceHandle: reject
"#;

        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema).run().await.unwrap();
        let paused = handle.wait_or_paused().await;

        let (node_id, resume_token) = match paused {
            ExecutionStatus::WaitingForInput {
                node_id,
                resume_token,
                ..
            } => (node_id, resume_token),
            other => panic!("Expected WaitingForInput, got {:?}", other),
        };

        let mut form_data = HashMap::new();
        form_data.insert("comment".to_string(), Value::String("LGTM".to_string()));
        handle
            .resume_human_input(
                &node_id,
                &resume_token,
                Some(HumanInputDecision::Approve),
                form_data,
            )
            .await
            .unwrap();

        let status = wait_until_terminal(&handle).await;
        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("decision"),
                    Some(&Value::String("approve".to_string()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_scheduler_human_input_timeout_default_value() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: human
    data:
      type: human-input
      title: Human
      resume_mode: form
      timeout_secs: 1
      timeout_action: default_value
      timeout_default_values:
        city: "Beijing"
      form_fields:
        - variable: city
          label: City
          field_type: text
          required: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: city
          value_selector: ["human", "city"]
edges:
  - source: start
    target: human
  - source: human
    target: end
"#;

        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let handle = WorkflowRunner::builder(schema).run().await.unwrap();
        let paused = handle.wait_or_paused().await;
        assert!(matches!(paused, ExecutionStatus::WaitingForInput { .. }));

        let status = wait_until_terminal(&handle).await;
        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(
                    outputs.get("city"),
                    Some(&Value::String("Beijing".to_string()))
                );
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }
}
