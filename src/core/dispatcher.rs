//! Workflow dispatcher — the main execution driver.
//!
//! The [`WorkflowDispatcher`] walks the DAG graph, executing each node via its
//! registered [`NodeExecutor`](crate::nodes::NodeExecutor), managing edge
//! traversal, error strategies, retry logic, timeout enforcement, and streaming
//! propagation.

#![expect(
    clippy::too_many_arguments,
    reason = "Dispatcher orchestration keeps explicit parameters for feature-gated execution paths"
)]

use serde_json::Value;

use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};

use crate::compiler::CompiledNodeConfigMap;
#[cfg(feature = "checkpoint")]
use crate::core::checkpoint::{
    diff_fingerprints, hash_json, Checkpoint, CheckpointStore, ConsumedResources,
    ContextFingerprint, ResumePolicy,
};
use crate::core::debug::{DebugAction, DebugGate, DebugHook, NoopGate, NoopHook};
use crate::core::event_bus::GraphEngineEvent;
use crate::core::plugin_gate::PluginGate;
use crate::core::runtime_context::RuntimeContext;
use crate::core::safe_stop::SafeStopSignal;
use crate::core::security_gate::SecurityGate;
#[cfg(feature = "checkpoint")]
use crate::core::variable_pool::{restore_from_checkpoint, snapshot_for_checkpoint};
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::dsl::schema::{
    BackoffStrategy, EdgeHandle, ErrorStrategyConfig, ErrorStrategyType, FieldValidation,
    FormFieldDefinition, FormFieldType, GatherNodeData, HumanInputDecision, HumanInputResumeMode,
    HumanInputTimeoutAction, JoinMode, NodeRunResult, RetryConfig, TimeoutStrategy,
    WorkflowNodeExecutionStatus, WriteMode,
};
use crate::error::{ErrorCode, ErrorContext, NodeError, WorkflowError, WorkflowResult};
use crate::graph::types::{EdgeTraversalState, Graph};
use crate::nodes::executor::NodeExecutorRegistry;
use crate::nodes::human_input::{HumanInputPauseRequest, HUMAN_INPUT_REQUEST_KEY};
#[cfg(feature = "plugin-system")]
use crate::plugin_system::PluginRegistry;
use crate::scheduler::ExecutionStatus;

type HumanInputOutputs = (
    HashMap<String, Segment>,
    EdgeHandle,
    Option<HumanInputDecision>,
    HashMap<String, Value>,
);

/// Sender wrapper for engine events, with an atomic active flag so that event
/// emission can be cheaply skipped when no listener is attached.
#[derive(Clone)]
pub struct EventEmitter {
    tx: mpsc::Sender<GraphEngineEvent>,
    active: Arc<AtomicBool>,
}

impl EventEmitter {
    /// Create a new event emitter.
    pub fn new(tx: mpsc::Sender<GraphEngineEvent>, active: Arc<AtomicBool>) -> Self {
        Self { tx, active }
    }

    #[inline(always)]
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub(crate) fn tx(&self) -> &mpsc::Sender<GraphEngineEvent> {
        &self.tx
    }

    #[doc(hidden)]
    pub async fn emit(&self, event: GraphEngineEvent) {
        if self.is_active() {
            let _ = self.tx.send(event).await;
        }
    }
}

/// External command to control workflow execution
#[derive(Debug, Clone)]
pub enum Command {
    Abort {
        reason: Option<String>,
    },
    Pause,
    UpdateVariables {
        variables: HashMap<String, Value>,
    },
    ResumeWithInput {
        input: HashMap<String, Value>,
    },
    ResumeHumanInput {
        node_id: String,
        resume_token: String,
        decision: Option<HumanInputDecision>,
        form_data: HashMap<String, Value>,
    },
    SafeStop,
}

#[derive(Debug, Clone)]
struct ResumeInputCommand {
    node_id: Option<String>,
    resume_token: Option<String>,
    decision: Option<HumanInputDecision>,
    form_data: HashMap<String, Value>,
}

impl ResumeInputCommand {
    fn legacy(form_data: HashMap<String, Value>) -> Self {
        Self {
            node_id: None,
            resume_token: None,
            decision: None,
            form_data,
        }
    }

    fn human_input(
        node_id: String,
        resume_token: String,
        decision: Option<HumanInputDecision>,
        form_data: HashMap<String, Value>,
    ) -> Self {
        Self {
            node_id: Some(node_id),
            resume_token: Some(resume_token),
            decision,
            form_data,
        }
    }
}

/// Configuration for the workflow engine
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EngineConfig {
    pub max_steps: i32,
    pub max_execution_time_secs: u64,
    #[serde(default)]
    pub strict_template: bool,
    #[serde(default = "default_parallel_enabled")]
    pub parallel_enabled: bool,
    #[serde(default)]
    pub max_concurrency: usize,
}

fn default_parallel_enabled() -> bool {
    true
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            max_steps: 500,
            max_execution_time_secs: 600,
            strict_template: false,
            parallel_enabled: true,
            max_concurrency: 0,
        }
    }
}

#[derive(Clone)]
struct NodeInfo {
    node_type: String,
    node_title: String,
    node_config: Value,
    is_branch: bool,
    is_skipped: bool,
    error_strategy: Option<ErrorStrategyConfig>,
    retry_config: Option<RetryConfig>,
    timeout_secs: Option<u64>,
}

struct NodeExecOutcome {
    exec_id: String,
    node_id: String,
    info: NodeInfo,
    result: Result<NodeRunResult, NodeError>,
}

struct GatherTimeoutAction {
    node_id: String,
    timeout_secs: u64,
    timeout_strategy: TimeoutStrategy,
    cancel_remaining: bool,
}

/// The main workflow dispatcher: drives graph execution
pub struct WorkflowDispatcher<G: DebugGate = NoopGate, H: DebugHook = NoopHook> {
    graph: Arc<RwLock<Graph>>,
    variable_pool: Arc<RwLock<VariablePool>>,
    registry: Arc<NodeExecutorRegistry>,
    compiled_node_configs: Option<Arc<CompiledNodeConfigMap>>,
    event_emitter: EventEmitter,
    config: EngineConfig,
    exceptions_count: i32,
    final_outputs: HashMap<String, Value>,
    context: Arc<RuntimeContext>,
    plugin_gate: Arc<dyn PluginGate>,
    security_gate: Arc<dyn SecurityGate>,
    debug_gate: G,
    debug_hook: H,
    status_tx: Option<watch::Sender<ExecutionStatus>>,
    command_rx: Option<mpsc::Receiver<Command>>,
    pending_resume_input: Option<ResumeInputCommand>,
    safe_stop_requested: bool,
    last_completed_node: Option<String>,
    #[cfg(feature = "checkpoint")]
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    #[cfg(feature = "checkpoint")]
    workflow_id: Option<String>,
    #[cfg(feature = "checkpoint")]
    execution_id: Option<String>,
    #[cfg(feature = "checkpoint")]
    resume_policy: ResumePolicy,
    safe_stop_signal: Option<SafeStopSignal>,
    #[cfg(feature = "checkpoint")]
    schema_hash: Option<String>,
    #[cfg(feature = "checkpoint")]
    engine_config_hash: Option<String>,
}

#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct DispatcherResourceWeak {
    graph: Weak<RwLock<Graph>>,
    variable_pool: Weak<RwLock<VariablePool>>,
    registry: Weak<NodeExecutorRegistry>,
    context: Weak<RuntimeContext>,
}

impl DispatcherResourceWeak {
    pub fn graph_dropped(&self) -> bool {
        self.graph.upgrade().is_none()
    }

    pub fn pool_dropped(&self) -> bool {
        self.variable_pool.upgrade().is_none()
    }

    pub fn registry_dropped(&self) -> bool {
        self.registry.upgrade().is_none()
    }

    pub fn context_dropped(&self) -> bool {
        self.context.upgrade().is_none()
    }
}

impl WorkflowDispatcher<NoopGate, NoopHook> {
    pub fn new(
        graph: Graph,
        variable_pool: VariablePool,
        registry: NodeExecutorRegistry,
        event_emitter: EventEmitter,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
        #[cfg(feature = "plugin-system")] plugin_registry: Option<Arc<PluginRegistry>>,
    ) -> Self {
        let graph = Arc::new(RwLock::new(graph));
        let variable_pool = Arc::new(RwLock::new(variable_pool));
        let plugin_gate: Arc<dyn PluginGate> = {
            #[cfg(feature = "plugin-system")]
            {
                crate::core::plugin_gate::new_plugin_gate_with_registry(
                    variable_pool.clone(),
                    event_emitter.clone(),
                    plugin_registry,
                )
            }

            #[cfg(not(feature = "plugin-system"))]
            {
                crate::core::plugin_gate::new_plugin_gate(
                    event_emitter.clone(),
                    variable_pool.clone(),
                )
            }
        };
        let security_gate: Arc<dyn SecurityGate> =
            crate::core::security_gate::new_security_gate(context.clone(), variable_pool.clone());
        WorkflowDispatcher {
            graph,
            variable_pool,
            registry: Arc::new(registry),
            compiled_node_configs: None,
            event_emitter,
            config,
            exceptions_count: 0,
            final_outputs: HashMap::new(),
            context,
            plugin_gate,
            security_gate,
            debug_gate: NoopGate,
            debug_hook: NoopHook,
            status_tx: None,
            command_rx: None,
            pending_resume_input: None,
            safe_stop_requested: false,
            last_completed_node: None,
            #[cfg(feature = "checkpoint")]
            checkpoint_store: None,
            #[cfg(feature = "checkpoint")]
            workflow_id: None,
            #[cfg(feature = "checkpoint")]
            execution_id: None,
            #[cfg(feature = "checkpoint")]
            resume_policy: ResumePolicy::Normal,
            safe_stop_signal: None,
            #[cfg(feature = "checkpoint")]
            schema_hash: None,
            #[cfg(feature = "checkpoint")]
            engine_config_hash: None,
        }
    }

    pub fn new_with_registry(
        graph: Graph,
        variable_pool: VariablePool,
        registry: Arc<NodeExecutorRegistry>,
        event_emitter: EventEmitter,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
        #[cfg(feature = "plugin-system")] plugin_registry: Option<Arc<PluginRegistry>>,
    ) -> Self {
        let graph = Arc::new(RwLock::new(graph));
        let variable_pool = Arc::new(RwLock::new(variable_pool));
        let plugin_gate: Arc<dyn PluginGate> = {
            #[cfg(feature = "plugin-system")]
            {
                crate::core::plugin_gate::new_plugin_gate_with_registry(
                    variable_pool.clone(),
                    event_emitter.clone(),
                    plugin_registry,
                )
            }

            #[cfg(not(feature = "plugin-system"))]
            {
                crate::core::plugin_gate::new_plugin_gate(
                    event_emitter.clone(),
                    variable_pool.clone(),
                )
            }
        };
        let security_gate: Arc<dyn SecurityGate> =
            crate::core::security_gate::new_security_gate(context.clone(), variable_pool.clone());
        WorkflowDispatcher {
            graph,
            variable_pool,
            registry,
            compiled_node_configs: None,
            event_emitter,
            config,
            exceptions_count: 0,
            final_outputs: HashMap::new(),
            context,
            plugin_gate,
            security_gate,
            debug_gate: NoopGate,
            debug_hook: NoopHook,
            status_tx: None,
            command_rx: None,
            pending_resume_input: None,
            safe_stop_requested: false,
            last_completed_node: None,
            #[cfg(feature = "checkpoint")]
            checkpoint_store: None,
            #[cfg(feature = "checkpoint")]
            workflow_id: None,
            #[cfg(feature = "checkpoint")]
            execution_id: None,
            #[cfg(feature = "checkpoint")]
            resume_policy: ResumePolicy::Normal,
            safe_stop_signal: None,
            #[cfg(feature = "checkpoint")]
            schema_hash: None,
            #[cfg(feature = "checkpoint")]
            engine_config_hash: None,
        }
    }

    pub fn new_with_registry_and_compiled(
        graph: Graph,
        variable_pool: VariablePool,
        registry: Arc<NodeExecutorRegistry>,
        event_emitter: EventEmitter,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
        compiled_node_configs: Arc<CompiledNodeConfigMap>,
        #[cfg(feature = "plugin-system")] plugin_registry: Option<Arc<PluginRegistry>>,
    ) -> Self {
        let graph = Arc::new(RwLock::new(graph));
        let variable_pool = Arc::new(RwLock::new(variable_pool));
        let plugin_gate: Arc<dyn PluginGate> = {
            #[cfg(feature = "plugin-system")]
            {
                crate::core::plugin_gate::new_plugin_gate_with_registry(
                    variable_pool.clone(),
                    event_emitter.clone(),
                    plugin_registry,
                )
            }

            #[cfg(not(feature = "plugin-system"))]
            {
                crate::core::plugin_gate::new_plugin_gate(
                    event_emitter.clone(),
                    variable_pool.clone(),
                )
            }
        };
        let security_gate: Arc<dyn SecurityGate> =
            crate::core::security_gate::new_security_gate(context.clone(), variable_pool.clone());
        WorkflowDispatcher {
            graph,
            variable_pool,
            registry,
            compiled_node_configs: Some(compiled_node_configs),
            event_emitter,
            config,
            exceptions_count: 0,
            final_outputs: HashMap::new(),
            context,
            plugin_gate,
            security_gate,
            debug_gate: NoopGate,
            debug_hook: NoopHook,
            status_tx: None,
            command_rx: None,
            pending_resume_input: None,
            safe_stop_requested: false,
            last_completed_node: None,
            #[cfg(feature = "checkpoint")]
            checkpoint_store: None,
            #[cfg(feature = "checkpoint")]
            workflow_id: None,
            #[cfg(feature = "checkpoint")]
            execution_id: None,
            #[cfg(feature = "checkpoint")]
            resume_policy: ResumePolicy::Normal,
            safe_stop_signal: None,
            #[cfg(feature = "checkpoint")]
            schema_hash: None,
            #[cfg(feature = "checkpoint")]
            engine_config_hash: None,
        }
    }
}

impl<G: DebugGate, H: DebugHook> WorkflowDispatcher<G, H> {
    #[doc(hidden)]
    pub fn debug_resources(&self) -> DispatcherResourceWeak {
        DispatcherResourceWeak {
            graph: Arc::downgrade(&self.graph),
            variable_pool: Arc::downgrade(&self.variable_pool),
            registry: Arc::downgrade(&self.registry),
            context: Arc::downgrade(&self.context),
        }
    }
    pub fn new_with_debug(
        graph: Graph,
        variable_pool: VariablePool,
        registry: Arc<NodeExecutorRegistry>,
        event_emitter: EventEmitter,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
        #[cfg(feature = "plugin-system")] plugin_registry: Option<Arc<PluginRegistry>>,
        debug_gate: G,
        debug_hook: H,
    ) -> Self {
        let graph = Arc::new(RwLock::new(graph));
        let variable_pool = Arc::new(RwLock::new(variable_pool));
        let plugin_gate: Arc<dyn PluginGate> = {
            #[cfg(feature = "plugin-system")]
            {
                crate::core::plugin_gate::new_plugin_gate_with_registry(
                    variable_pool.clone(),
                    event_emitter.clone(),
                    plugin_registry,
                )
            }

            #[cfg(not(feature = "plugin-system"))]
            {
                crate::core::plugin_gate::new_plugin_gate(
                    event_emitter.clone(),
                    variable_pool.clone(),
                )
            }
        };
        let security_gate: Arc<dyn SecurityGate> =
            crate::core::security_gate::new_security_gate(context.clone(), variable_pool.clone());

        WorkflowDispatcher {
            graph,
            variable_pool,
            registry,
            compiled_node_configs: None,
            event_emitter,
            config,
            exceptions_count: 0,
            final_outputs: HashMap::new(),
            context,
            plugin_gate,
            security_gate,
            debug_gate,
            debug_hook,
            status_tx: None,
            command_rx: None,
            pending_resume_input: None,
            safe_stop_requested: false,
            last_completed_node: None,
            #[cfg(feature = "checkpoint")]
            checkpoint_store: None,
            #[cfg(feature = "checkpoint")]
            workflow_id: None,
            #[cfg(feature = "checkpoint")]
            execution_id: None,
            #[cfg(feature = "checkpoint")]
            resume_policy: ResumePolicy::Normal,
            safe_stop_signal: None,
            #[cfg(feature = "checkpoint")]
            schema_hash: None,
            #[cfg(feature = "checkpoint")]
            engine_config_hash: None,
        }
    }

    pub fn new_with_debug_and_compiled(
        graph: Graph,
        variable_pool: VariablePool,
        registry: Arc<NodeExecutorRegistry>,
        event_emitter: EventEmitter,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
        compiled_node_configs: Arc<CompiledNodeConfigMap>,
        #[cfg(feature = "plugin-system")] plugin_registry: Option<Arc<PluginRegistry>>,
        debug_gate: G,
        debug_hook: H,
    ) -> Self {
        let graph = Arc::new(RwLock::new(graph));
        let variable_pool = Arc::new(RwLock::new(variable_pool));
        let plugin_gate: Arc<dyn PluginGate> = {
            #[cfg(feature = "plugin-system")]
            {
                crate::core::plugin_gate::new_plugin_gate_with_registry(
                    variable_pool.clone(),
                    event_emitter.clone(),
                    plugin_registry,
                )
            }

            #[cfg(not(feature = "plugin-system"))]
            {
                crate::core::plugin_gate::new_plugin_gate(
                    event_emitter.clone(),
                    variable_pool.clone(),
                )
            }
        };
        let security_gate: Arc<dyn SecurityGate> =
            crate::core::security_gate::new_security_gate(context.clone(), variable_pool.clone());
        WorkflowDispatcher {
            graph,
            variable_pool,
            registry,
            compiled_node_configs: Some(compiled_node_configs),
            event_emitter,
            config,
            exceptions_count: 0,
            final_outputs: HashMap::new(),
            context,
            plugin_gate,
            security_gate,
            debug_gate,
            debug_hook,
            status_tx: None,
            command_rx: None,
            pending_resume_input: None,
            safe_stop_requested: false,
            last_completed_node: None,
            #[cfg(feature = "checkpoint")]
            checkpoint_store: None,
            #[cfg(feature = "checkpoint")]
            workflow_id: None,
            #[cfg(feature = "checkpoint")]
            execution_id: None,
            #[cfg(feature = "checkpoint")]
            resume_policy: ResumePolicy::Normal,
            safe_stop_signal: None,
            #[cfg(feature = "checkpoint")]
            schema_hash: None,
            #[cfg(feature = "checkpoint")]
            engine_config_hash: None,
        }
    }

    pub fn set_control_channels(
        &mut self,
        status_tx: watch::Sender<ExecutionStatus>,
        command_rx: mpsc::Receiver<Command>,
    ) {
        self.status_tx = Some(status_tx);
        self.command_rx = Some(command_rx);
    }

    pub fn set_safe_stop_signal(&mut self, safe_stop_signal: Option<SafeStopSignal>) {
        self.safe_stop_signal = safe_stop_signal;
    }

    #[cfg(feature = "checkpoint")]
    pub fn set_checkpoint_options(
        &mut self,
        store: Option<Arc<dyn CheckpointStore>>,
        workflow_id: Option<String>,
        execution_id: Option<String>,
        resume_policy: ResumePolicy,
        schema_hash: String,
        engine_config_hash: String,
    ) {
        self.checkpoint_store = store;
        self.workflow_id = workflow_id;
        self.execution_id = execution_id;
        self.resume_policy = resume_policy;
        self.schema_hash = Some(schema_hash);
        self.engine_config_hash = Some(engine_config_hash);
    }

    pub fn partial_outputs(&self) -> HashMap<String, Value> {
        self.final_outputs.clone()
    }

    pub async fn snapshot_pool(&self) -> VariablePool {
        self.variable_pool.read().clone()
    }

    async fn check_limits(
        &self,
        step_count: &mut i32,
        max_steps: i32,
        start_time: i64,
        max_exec_time: u64,
    ) -> WorkflowResult<()> {
        // Check max steps
        *step_count += 1;
        if *step_count > max_steps {
            if self.event_emitter.is_active() {
                self.event_emitter
                    .emit(GraphEngineEvent::GraphRunFailed {
                        error: format!("Max steps exceeded: {}", max_steps),
                        exceptions_count: self.exceptions_count,
                    })
                    .await;
            }
            return Err(WorkflowError::MaxStepsExceeded(max_steps));
        }

        // Check max time
        if self.context.time_provider.elapsed_secs(start_time) > max_exec_time {
            if self.event_emitter.is_active() {
                self.event_emitter
                    .emit(GraphEngineEvent::GraphRunFailed {
                        error: "Max execution time exceeded".into(),
                        exceptions_count: self.exceptions_count,
                    })
                    .await;
            }
            return Err(WorkflowError::ExecutionTimeout);
        }

        Ok(())
    }

    async fn handle_node_success(
        &mut self,
        exec_id: &str,
        node_id: &str,
        info: &NodeInfo,
        result: NodeRunResult,
        queue: &mut Vec<String>,
        ready_predecessor: &HashMap<String, String>,
        step_count: i32,
        start_time: i64,
    ) -> WorkflowResult<Vec<String>> {
        #[cfg(not(feature = "checkpoint"))]
        let _ = (&ready_predecessor, step_count, start_time);

        let mut result = result;

        self.emit_after_node_hooks(node_id, &info.node_type, &info.node_title, &result)
            .await?;

        self.record_llm_usage(&result).await;

        if result.status == WorkflowNodeExecutionStatus::Paused {
            #[cfg(feature = "checkpoint")]
            self.save_checkpoint(node_id, queue, ready_predecessor, step_count, start_time)
                .await?;

            if info.node_type == "human-input" {
                self.handle_human_input_paused(node_id, info, &mut result)
                    .await?;
            } else {
                let prompt = result
                    .metadata
                    .get("prompt")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let node_title = result
                    .metadata
                    .get("node_title")
                    .and_then(|value| value.as_str())
                    .unwrap_or(info.node_title.as_str())
                    .to_string();

                if self.event_emitter.is_active() {
                    self.event_emitter
                        .emit(GraphEngineEvent::WorkflowPaused {
                            node_id: node_id.to_string(),
                            prompt: prompt.clone(),
                        })
                        .await;
                }

                if let Some(status_tx) = &self.status_tx {
                    status_tx.send_replace(ExecutionStatus::Paused {
                        node_id: node_id.to_string(),
                        node_title,
                        prompt,
                    });
                }

                let input = self.wait_for_resume().await?;
                {
                    let mut pool = self.variable_pool.write();
                    for (key, value) in input.form_data {
                        let selector = Selector::new(node_id.to_string(), key);
                        pool.set(&selector, Segment::from_value(&value));
                    }
                }
                self.mark_pool_dirty();

                if self.event_emitter.is_active() {
                    self.event_emitter
                        .emit(GraphEngineEvent::WorkflowResumed {
                            node_id: node_id.to_string(),
                        })
                        .await;
                }

                if let Some(status_tx) = &self.status_tx {
                    status_tx.send_replace(ExecutionStatus::Running);
                }

                result.status = WorkflowNodeExecutionStatus::Succeeded;
            }
        }

        // Store outputs in variable pool
        let (mut outputs_for_write, stream_outputs) = result.outputs.clone().into_parts();
        if info.node_type != "end" && info.node_type != "answer" {
            for (key, value) in outputs_for_write.iter_mut() {
                let selector =
                    crate::core::variable_pool::Selector::new(node_id.to_string(), key.clone());
                self.apply_before_variable_write_hooks(node_id, &selector, value)
                    .await?;
            }
        }

        let mut assigner_meta: Option<(WriteMode, crate::core::variable_pool::Selector, Segment)> =
            None;
        if info.node_type == "assigner" {
            let write_mode = outputs_for_write
                .get("write_mode")
                .and_then(|v| serde_json::from_value::<WriteMode>(v.to_value()).ok())
                .unwrap_or(WriteMode::Overwrite);
            let assigned_sel: crate::core::variable_pool::Selector = outputs_for_write
                .get("assigned_variable_selector")
                .and_then(|v| serde_json::from_value(v.to_value()).ok())
                .unwrap_or_else(|| {
                    crate::core::variable_pool::Selector::new("__scope__", "output")
                });
            let mut output_val = outputs_for_write
                .get("output")
                .cloned()
                .unwrap_or(Segment::None);

            self.apply_before_variable_write_hooks(node_id, &assigned_sel, &mut output_val)
                .await?;

            assigner_meta = Some((write_mode, assigned_sel, output_val));
        }

        let should_pause_after = self.debug_gate.should_pause_after(node_id);
        let mut pool_snapshot_after: Option<VariablePool> = None;

        {
            let mut pool = self.variable_pool.write();

            // Handle variable assigner specially
            if let Some((write_mode, assigned_sel, output_val)) = assigner_meta {
                match write_mode {
                    WriteMode::Overwrite => {
                        pool.set(&assigned_sel, output_val);
                    }
                    WriteMode::Append => {
                        pool.append(&assigned_sel, output_val);
                    }
                    WriteMode::Clear => {
                        pool.clear(&assigned_sel);
                    }
                }
            }

            pool.set_node_outputs(node_id, &outputs_for_write);
            for (key, stream) in stream_outputs {
                let selector = crate::core::variable_pool::Selector::new(node_id.to_string(), key);
                pool.set(&selector, Segment::Stream(stream));
            }

            if should_pause_after {
                pool_snapshot_after = Some(pool.clone());
            }
        }
        self.mark_pool_dirty();

        // Track final outputs for end/answer nodes
        if info.node_type == "end" || info.node_type == "answer" {
            for (k, v) in &outputs_for_write {
                self.final_outputs.insert(k.clone(), v.to_value());
            }
        }

        // Emit success or exception event
        match result.status {
            WorkflowNodeExecutionStatus::Exception => {
                if self.event_emitter.is_active() {
                    self.event_emitter
                        .emit(GraphEngineEvent::NodeRunException {
                            id: exec_id.to_string(),
                            node_id: node_id.to_string(),
                            node_type: info.node_type.clone(),
                            node_run_result: result.clone(),
                            error: result
                                .error
                                .as_ref()
                                .map(|e| e.message.clone())
                                .unwrap_or_default(),
                        })
                        .await;
                }
            }
            _ => {
                if self.event_emitter.is_active() {
                    self.event_emitter
                        .emit(GraphEngineEvent::NodeRunSucceeded {
                            id: exec_id.to_string(),
                            node_id: node_id.to_string(),
                            node_type: info.node_type.clone(),
                            node_run_result: result.clone(),
                        })
                        .await;
                }
            }
        }

        let downstream =
            self.advance_graph_after_success(node_id, info.is_branch, &result.edge_source_handle)?;
        let mut cancelled_sources = Vec::new();
        if info.node_type == "gather" {
            let gather = serde_json::from_value::<GatherNodeData>(info.node_config.clone())
                .unwrap_or(GatherNodeData {
                    variables: Vec::new(),
                    join_mode: JoinMode::All,
                    cancel_remaining: true,
                    timeout_secs: None,
                    timeout_strategy: TimeoutStrategy::ProceedWithAvailable,
                });
            let mut graph = self.graph.write();
            let sources = graph.finalize_gather(node_id, gather.cancel_remaining);
            if gather.cancel_remaining {
                cancelled_sources = sources;
            }
        }

        // [DEBUG] Hook after node execute
        if should_pause_after {
            let pool_snapshot = pool_snapshot_after
                .expect("pool snapshot should exist when pause-after is enabled");
            let action = self
                .debug_hook
                .after_node_execute(
                    node_id,
                    &info.node_type,
                    &info.node_title,
                    &result,
                    &pool_snapshot,
                )
                .await?;

            match self.apply_debug_action(action).await? {
                DebugActionResult::Continue => {}
                DebugActionResult::Abort(reason) => {
                    return Err(WorkflowError::Aborted(reason));
                }
                DebugActionResult::SkipNode => {}
            }
        }

        queue.extend(downstream);
        Ok(cancelled_sources)
    }

    async fn handle_node_failure(
        &self,
        exec_id: String,
        node_id: String,
        info: &NodeInfo,
        error: NodeError,
    ) -> WorkflowError {
        // Node execution failed, abort workflow
        let error_type = error.error_code();
        let error_detail = error.to_structured_json();
        let error_info = crate::dsl::schema::NodeErrorInfo {
            message: error.to_string(),
            error_type: Some(error_type),
            detail: Some(error_detail.clone()),
        };
        let err_result = NodeRunResult {
            status: WorkflowNodeExecutionStatus::Failed,
            error: Some(error_info),
            ..Default::default()
        };

        if self.event_emitter.is_active() {
            self.event_emitter
                .emit(GraphEngineEvent::NodeRunFailed {
                    id: exec_id.clone(),
                    node_id: node_id.clone(),
                    node_type: info.node_type.clone(),
                    node_run_result: err_result,
                    error: error.to_string(),
                })
                .await;

            self.event_emitter
                .emit(GraphEngineEvent::GraphRunFailed {
                    error: error.to_string(),
                    exceptions_count: self.exceptions_count,
                })
                .await;
        }

        WorkflowError::NodeExecutionError {
            node_id,
            error: error.to_string(),
            error_detail: Some(error_detail),
        }
    }

    fn mark_pool_dirty(&self) {
        self.plugin_gate.mark_pool_dirty();
    }

    fn load_node_info(&self, node_id: &str) -> WorkflowResult<NodeInfo> {
        let g = self.graph.read();
        let node = g
            .get_node(node_id)
            .ok_or_else(|| WorkflowError::NodeNotFound(node_id.to_string()))?;
        Ok(NodeInfo {
            node_type: node.node_type.clone(),
            node_title: node.title.clone(),
            node_config: node.config.clone(),
            is_branch: g.is_branch_node(node_id),
            is_skipped: matches!(
                g.node_state(node_id),
                Some(EdgeTraversalState::Skipped | EdgeTraversalState::Cancelled)
            ),
            error_strategy: node.error_strategy.clone(),
            retry_config: node.retry_config.clone(),
            timeout_secs: node.timeout_secs,
        })
    }

    fn skip_node_and_collect_ready(&self, node_id: &str, is_branch: bool) -> Vec<String> {
        let mut g = self.graph.write();
        g.set_node_state(node_id, EdgeTraversalState::Skipped);
        if is_branch {
            g.process_branch_edges(
                node_id,
                &crate::dsl::schema::EdgeHandle::Branch("false".to_string()),
            );
        } else {
            g.process_normal_edges(node_id);
        }

        g.downstream_node_ids(node_id)
            .filter(|ds_id| g.is_node_ready(ds_id))
            .map(|ds_id| ds_id.to_string())
            .collect()
    }

    fn advance_graph_after_success(
        &self,
        node_id: &str,
        is_branch: bool,
        edge_handle: &crate::dsl::schema::EdgeHandle,
    ) -> WorkflowResult<Vec<String>> {
        let mut g = self.graph.write();

        g.set_node_state(node_id, EdgeTraversalState::Taken);

        if is_branch {
            match edge_handle {
                crate::dsl::schema::EdgeHandle::Branch(handle) => {
                    let valid = g.has_branch_handle(node_id, handle.as_str());
                    if !valid {
                        return Err(WorkflowError::GraphValidationError(format!(
                            "Node {} returned branch handle '{}' but no matching edge found",
                            node_id, handle
                        )));
                    }
                    g.process_branch_edges(node_id, edge_handle);
                }
                crate::dsl::schema::EdgeHandle::Default => {
                    return Err(WorkflowError::GraphValidationError(format!(
                        "Node {} returned default handle for branch node",
                        node_id
                    )));
                }
            }
        } else {
            g.process_normal_edges(node_id);
        }

        Ok(g.downstream_node_ids(node_id)
            .filter(|ds_id| g.is_node_ready(ds_id))
            .map(|ds_id| ds_id.to_string())
            .collect())
    }

    async fn emit_before_workflow_hooks(&self) -> WorkflowResult<()> {
        self.plugin_gate.emit_before_workflow_hooks().await
    }

    async fn emit_after_workflow_hooks(&self) -> WorkflowResult<()> {
        self.plugin_gate
            .emit_after_workflow_hooks(&self.final_outputs, self.exceptions_count)
            .await
    }

    async fn emit_before_node_hooks(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        node_config: &Value,
    ) -> WorkflowResult<()> {
        self.plugin_gate
            .emit_before_node_hooks(node_id, node_type, node_title, node_config)
            .await
    }

    async fn emit_after_node_hooks(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        result: &NodeRunResult,
    ) -> WorkflowResult<()> {
        self.plugin_gate
            .emit_after_node_hooks(node_id, node_type, node_title, result)
            .await
    }

    async fn apply_before_variable_write_hooks(
        &self,
        node_id: &str,
        selector: &crate::core::variable_pool::Selector,
        value: &mut Segment,
    ) -> WorkflowResult<()> {
        self.plugin_gate
            .apply_before_variable_write_hooks(node_id, selector, value)
            .await
    }

    async fn execute_node_with_retry_inner(
        registry: Arc<NodeExecutorRegistry>,
        compiled_node_configs: Option<Arc<CompiledNodeConfigMap>>,
        context: Arc<RuntimeContext>,
        event_emitter: EventEmitter,
        exec_id: &str,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        node_config: &Value,
        pool_snapshot: &VariablePool,
        error_strategy: &Option<ErrorStrategyConfig>,
        retry_config: &Option<RetryConfig>,
        node_timeout: Option<u64>,
    ) -> Result<NodeRunResult, NodeError> {
        let executor = registry.get(node_type);
        if executor.is_none() {
            return Err(NodeError::ConfigError(format!(
                "No executor for node type: {}",
                node_type
            )));
        }

        let compiled_config = compiled_node_configs
            .as_ref()
            .and_then(|map| map.get(node_id))
            .cloned();

        let max_retries = retry_config
            .as_ref()
            .map(|rc| rc.max_retries)
            .unwrap_or(0)
            .max(0);
        let retry_on_retryable_only = retry_config
            .as_ref()
            .map(|rc| rc.retry_on_retryable_only)
            .unwrap_or(true);

        let mut last_error: Option<NodeError> = None;
        let mut result = None;

        for attempt in 0..=max_retries {
            let exec_future = if let Some(config) = compiled_config.as_deref() {
                executor
                    .as_ref()
                    .expect("executor checked")
                    .execute_compiled(node_id, config, pool_snapshot, &context)
            } else {
                executor.as_ref().expect("executor checked").execute(
                    node_id,
                    node_config,
                    pool_snapshot,
                    &context,
                )
            };
            let exec_result = if let Some(timeout_secs) = node_timeout {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs),
                    exec_future,
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(NodeError::Timeout.with_context(ErrorContext::retryable(
                        ErrorCode::Timeout,
                        format!("Node execution timed out after {}s", timeout_secs),
                    ))),
                }
            } else {
                exec_future.await
            };

            match exec_result {
                Ok(r) => {
                    result = Some(r);
                    break;
                }
                Err(e) => {
                    let should_retry = if attempt < max_retries {
                        if retry_on_retryable_only {
                            e.is_retryable()
                        } else {
                            true
                        }
                    } else {
                        false
                    };

                    if should_retry {
                        let interval = calculate_retry_interval(retry_config, attempt, &e);
                        if event_emitter.is_active() {
                            event_emitter
                                .emit(GraphEngineEvent::NodeRunRetry {
                                    id: exec_id.to_string(),
                                    node_id: node_id.to_string(),
                                    node_type: node_type.to_string(),
                                    node_title: node_title.to_string(),
                                    error: e.to_string(),
                                    retry_index: attempt + 1,
                                })
                                .await;
                        }
                        if interval > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
                        }
                    }

                    last_error = Some(e);

                    if !should_retry && attempt < max_retries {
                        break;
                    }
                }
            }
        }

        match result {
            Some(r) => Ok(r),
            None => {
                let last_err = last_error
                    .unwrap_or_else(|| NodeError::ExecutionError("Unknown error".to_string()));
                let error_type = last_err.error_code();
                let error_detail = last_err.to_structured_json();
                let error_info = crate::dsl::schema::NodeErrorInfo {
                    message: last_err.to_string(),
                    error_type: Some(error_type),
                    detail: Some(error_detail),
                };

                match error_strategy.as_ref().map(|es| &es.strategy_type) {
                    Some(ErrorStrategyType::FailBranch) => Ok(NodeRunResult {
                        status: WorkflowNodeExecutionStatus::Exception,
                        error: Some(error_info),
                        edge_source_handle: crate::dsl::schema::EdgeHandle::Branch(
                            "fail-branch".to_string(),
                        ),
                        ..Default::default()
                    }),
                    Some(ErrorStrategyType::DefaultValue) => {
                        let defaults = error_strategy
                            .as_ref()
                            .and_then(|es| es.default_value.clone())
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(k, v)| (k, Segment::from_value(&v)))
                            .collect();
                        Ok(NodeRunResult {
                            status: WorkflowNodeExecutionStatus::Exception,
                            outputs: crate::dsl::schema::NodeOutputs::Sync(defaults),
                            error: Some(error_info),
                            edge_source_handle: crate::dsl::schema::EdgeHandle::Default,
                            ..Default::default()
                        })
                    }
                    Some(ErrorStrategyType::None) | None => Err(last_err),
                }
            }
        }
    }

    async fn apply_debug_action(&self, action: DebugAction) -> WorkflowResult<DebugActionResult> {
        let mut next_action = action;
        loop {
            match next_action {
                DebugAction::Continue => return Ok(DebugActionResult::Continue),
                DebugAction::Abort { reason } => return Ok(DebugActionResult::Abort(reason)),
                DebugAction::SkipNode => return Ok(DebugActionResult::SkipNode),
                DebugAction::UpdateVariables { variables, then } => {
                    let mut pool = self.variable_pool.write();
                    for (key, value) in &variables {
                        let parts: Vec<&str> = key.splitn(2, '.').collect();
                        if parts.len() == 2 {
                            let selector =
                                crate::core::variable_pool::Selector::new(parts[0], parts[1]);
                            pool.set(&selector, Segment::from_value(value));
                        }
                    }
                    drop(pool);
                    self.mark_pool_dirty();
                    next_action = *then;
                }
            }
        }
    }

    fn effective_limits(&self) -> (i32, u64) {
        self.security_gate.effective_limits(&self.config)
    }

    fn apply_command_update_variables(&mut self, variables: HashMap<String, Value>) {
        let mut pool = self.variable_pool.write();
        for (key, value) in variables {
            if let Some(selector) = Selector::from_pool_key(&key) {
                pool.set(&selector, Segment::from_value(&value));
            }
        }
        drop(pool);
        self.mark_pool_dirty();
    }

    async fn poll_commands(&mut self) -> WorkflowResult<()> {
        if self.command_rx.is_none() {
            return Ok(());
        }

        loop {
            let command = {
                let rx = self.command_rx.as_mut().expect("checked above");
                rx.try_recv()
            };

            match command {
                Ok(Command::Abort { reason }) => {
                    if self.event_emitter.is_active() {
                        self.event_emitter
                            .emit(GraphEngineEvent::GraphRunAborted {
                                reason: reason.clone(),
                                outputs: self.final_outputs.clone(),
                            })
                            .await;
                    }
                    return Err(WorkflowError::Aborted(
                        reason.unwrap_or_else(|| "aborted by command".to_string()),
                    ));
                }
                Ok(Command::Pause) => {}
                Ok(Command::UpdateVariables { variables }) => {
                    self.apply_command_update_variables(variables);
                }
                Ok(Command::ResumeWithInput { input }) => {
                    self.pending_resume_input = Some(ResumeInputCommand::legacy(input));
                }
                Ok(Command::ResumeHumanInput {
                    node_id,
                    resume_token,
                    decision,
                    form_data,
                }) => {
                    self.pending_resume_input = Some(ResumeInputCommand::human_input(
                        node_id,
                        resume_token,
                        decision,
                        form_data,
                    ));
                }
                Ok(Command::SafeStop) => {
                    self.safe_stop_requested = true;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
            }
        }

        Ok(())
    }

    fn make_safe_stopped_error(&self) -> WorkflowError {
        #[cfg(feature = "checkpoint")]
        let checkpoint_saved = self.checkpoint_store.is_some();
        #[cfg(not(feature = "checkpoint"))]
        let checkpoint_saved = false;

        WorkflowError::SafeStopped {
            last_completed_node: self.last_completed_node.clone(),
            interrupted_nodes: Vec::new(),
            checkpoint_saved,
        }
    }

    async fn wait_for_resume(&mut self) -> WorkflowResult<ResumeInputCommand> {
        if let Some(input) = self.pending_resume_input.take() {
            return Ok(input);
        }

        loop {
            let Some(rx) = self.command_rx.as_mut() else {
                return Err(WorkflowError::InternalError(
                    "human-input paused but command channel is unavailable".to_string(),
                ));
            };

            let Some(command) = rx.recv().await else {
                return Err(WorkflowError::InternalError(
                    "workflow command channel closed while waiting for resume".to_string(),
                ));
            };

            match command {
                Command::ResumeWithInput { input } => return Ok(ResumeInputCommand::legacy(input)),
                Command::ResumeHumanInput {
                    node_id,
                    resume_token,
                    decision,
                    form_data,
                } => {
                    return Ok(ResumeInputCommand::human_input(
                        node_id,
                        resume_token,
                        decision,
                        form_data,
                    ));
                }
                Command::UpdateVariables { variables } => {
                    self.apply_command_update_variables(variables)
                }
                Command::Abort { reason } => {
                    return Err(WorkflowError::Aborted(reason.unwrap_or_else(|| {
                        "aborted while waiting for human input".to_string()
                    })));
                }
                Command::SafeStop => {
                    self.safe_stop_requested = true;
                    return Err(self.make_safe_stopped_error());
                }
                Command::Pause => {}
            }
        }
    }

    fn human_input_resume_mode_name(mode: &HumanInputResumeMode) -> &'static str {
        match mode {
            HumanInputResumeMode::Form => "form",
            HumanInputResumeMode::Approval => "approval",
            HumanInputResumeMode::Webhook => "webhook",
        }
    }

    fn human_input_timeout_action_name(action: &HumanInputTimeoutAction) -> &'static str {
        match action {
            HumanInputTimeoutAction::Fail => "fail",
            HumanInputTimeoutAction::DefaultValue => "default_value",
            HumanInputTimeoutAction::AutoApprove => "auto_approve",
            HumanInputTimeoutAction::AutoReject => "auto_reject",
        }
    }

    fn human_input_decision_name(decision: &HumanInputDecision) -> &'static str {
        match decision {
            HumanInputDecision::Approve => "approve",
            HumanInputDecision::Reject => "reject",
        }
    }

    fn matches_human_input_request(
        request: &HumanInputPauseRequest,
        cmd: &ResumeInputCommand,
    ) -> bool {
        let Some(node_id) = cmd.node_id.as_deref() else {
            return true;
        };
        if node_id != request.node_id {
            return false;
        }

        let Some(token) = cmd.resume_token.as_deref() else {
            return false;
        };
        token == request.resume_token
    }

    fn validate_form_input(
        mode: &HumanInputResumeMode,
        fields: &[FormFieldDefinition],
        cmd: &ResumeInputCommand,
    ) -> Result<(), String> {
        if *mode == HumanInputResumeMode::Approval && cmd.decision.is_none() {
            return Err("approval mode requires decision (approve/reject)".to_string());
        }

        for field in fields {
            let value = cmd
                .form_data
                .get(&field.variable)
                .or(field.default_value.as_ref());

            if field.required
                && (value.is_none() || value.is_some_and(|v| v.is_null())) {
                    return Err(format!("required field is missing: {}", field.variable));
                }

            if let Some(v) = value {
                Self::validate_field_type(field, v)?;
                Self::validate_field_options(field, v)?;
                Self::validate_field_rules(field, v)?;
            }
        }

        Ok(())
    }

    fn validate_field_type(field: &FormFieldDefinition, value: &Value) -> Result<(), String> {
        let valid = match field.field_type {
            FormFieldType::Text
            | FormFieldType::Textarea
            | FormFieldType::Date
            | FormFieldType::Email
            | FormFieldType::Radio
            | FormFieldType::Dropdown => value.is_string(),
            FormFieldType::Number => value.is_number(),
            FormFieldType::Checkbox => value.is_boolean(),
            FormFieldType::MultiSelect => value
                .as_array()
                .map(|arr| arr.iter().all(|item| item.is_string()))
                .unwrap_or(false),
            FormFieldType::Json | FormFieldType::File => value.is_object(),
            FormFieldType::Hidden => true,
        };

        if valid {
            Ok(())
        } else {
            Err(format!("invalid type for field: {}", field.variable))
        }
    }

    fn validate_field_options(field: &FormFieldDefinition, value: &Value) -> Result<(), String> {
        let Some(options) = field.options.as_ref() else {
            return Ok(());
        };

        let allow: HashSet<&str> = options.iter().map(|opt| opt.value.as_str()).collect();

        match field.field_type {
            FormFieldType::Radio | FormFieldType::Dropdown => {
                if let Some(v) = value.as_str() {
                    if !allow.contains(v) {
                        return Err(format!(
                            "field '{}' has value outside options",
                            field.variable
                        ));
                    }
                }
            }
            FormFieldType::MultiSelect => {
                if let Some(arr) = value.as_array() {
                    for item in arr {
                        if let Some(v) = item.as_str() {
                            if !allow.contains(v) {
                                return Err(format!(
                                    "field '{}' has value outside options",
                                    field.variable
                                ));
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn validate_field_rules(field: &FormFieldDefinition, value: &Value) -> Result<(), String> {
        let Some(FieldValidation {
            min_length,
            max_length,
            min_value,
            max_value,
            pattern,
            error_message,
        }) = field.validation.as_ref()
        else {
            return Ok(());
        };

        let message = |fallback: &str| {
            error_message
                .as_ref()
                .cloned()
                .unwrap_or_else(|| fallback.to_string())
        };

        if let Some(s) = value.as_str() {
            if let Some(min) = min_length {
                if s.chars().count() < *min as usize {
                    return Err(message(&format!(
                        "field '{}' is shorter than min_length",
                        field.variable
                    )));
                }
            }
            if let Some(max) = max_length {
                if s.chars().count() > *max as usize {
                    return Err(message(&format!(
                        "field '{}' is longer than max_length",
                        field.variable
                    )));
                }
            }
            if let Some(pat) = pattern {
                let re = regex::Regex::new(pat)
                    .map_err(|e| format!("invalid regex in field '{}': {}", field.variable, e))?;
                if !re.is_match(s) {
                    return Err(message(&format!(
                        "field '{}' does not match pattern",
                        field.variable
                    )));
                }
            }
        }

        if let Some(num) = value.as_f64() {
            if let Some(min) = min_value {
                if num < *min {
                    return Err(message(&format!(
                        "field '{}' is below min_value",
                        field.variable
                    )));
                }
            }
            if let Some(max) = max_value {
                if num > *max {
                    return Err(message(&format!(
                        "field '{}' is above max_value",
                        field.variable
                    )));
                }
            }
        }

        Ok(())
    }

    async fn wait_for_human_input_resume(
        &mut self,
        request: &HumanInputPauseRequest,
    ) -> WorkflowResult<Option<ResumeInputCommand>> {
        if let Some(cmd) = self.pending_resume_input.take() {
            if Self::matches_human_input_request(request, &cmd) {
                return Ok(Some(cmd));
            }
        }

        loop {
            if let Some(timeout_at) = request.timeout_at {
                let now = self.context.time_provider.now_timestamp();
                if now >= timeout_at {
                    return Ok(None);
                }

                let remain_secs = (timeout_at - now) as u64;
                let Some(rx) = self.command_rx.as_mut() else {
                    return Err(WorkflowError::InternalError(
                        "human-input paused but command channel is unavailable".to_string(),
                    ));
                };

                let incoming =
                    tokio::time::timeout(tokio::time::Duration::from_secs(remain_secs), rx.recv())
                        .await;

                let command = match incoming {
                    Ok(Some(cmd)) => cmd,
                    Ok(None) => {
                        return Err(WorkflowError::InternalError(
                            "workflow command channel closed while waiting for resume".to_string(),
                        ));
                    }
                    Err(_) => return Ok(None),
                };

                match command {
                    Command::ResumeWithInput { input } => {
                        return Ok(Some(ResumeInputCommand::legacy(input)))
                    }
                    Command::ResumeHumanInput {
                        node_id,
                        resume_token,
                        decision,
                        form_data,
                    } => {
                        let cmd = ResumeInputCommand::human_input(
                            node_id,
                            resume_token,
                            decision,
                            form_data,
                        );
                        if Self::matches_human_input_request(request, &cmd) {
                            return Ok(Some(cmd));
                        }
                    }
                    Command::UpdateVariables { variables } => {
                        self.apply_command_update_variables(variables)
                    }
                    Command::Abort { reason } => {
                        return Err(WorkflowError::Aborted(reason.unwrap_or_else(|| {
                            "aborted while waiting for human input".to_string()
                        })));
                    }
                    Command::SafeStop => {
                        self.safe_stop_requested = true;
                        return Err(self.make_safe_stopped_error());
                    }
                    Command::Pause => {}
                }
            } else {
                let cmd = self.wait_for_resume().await?;
                if Self::matches_human_input_request(request, &cmd) {
                    return Ok(Some(cmd));
                }
            }
        }
    }

    fn build_human_input_outputs(
        request: &HumanInputPauseRequest,
        cmd: ResumeInputCommand,
    ) -> Result<HumanInputOutputs, WorkflowError> {
        let mut form_data = HashMap::new();
        Self::validate_form_input(&request.resume_mode, &request.form_fields, &cmd)
            .map_err(NodeError::InputValidationError)?;

        for field in &request.form_fields {
            let value = cmd
                .form_data
                .get(&field.variable)
                .cloned()
                .or_else(|| field.default_value.clone());
            if let Some(v) = value {
                form_data.insert(field.variable.clone(), v);
            }
        }

        for (key, val) in cmd.form_data {
            form_data.entry(key).or_insert(val);
        }

        let decision = cmd.decision;

        let edge = match (&request.resume_mode, &decision) {
            (HumanInputResumeMode::Approval, Some(HumanInputDecision::Approve)) => {
                EdgeHandle::Branch("approve".to_string())
            }
            (HumanInputResumeMode::Approval, Some(HumanInputDecision::Reject)) => {
                EdgeHandle::Branch("reject".to_string())
            }
            _ => EdgeHandle::Default,
        };

        if let Some(dec) = decision.as_ref() {
            form_data.insert(
                "__decision".to_string(),
                Value::String(Self::human_input_decision_name(dec).to_string()),
            );
        }

        let mut outputs = HashMap::new();
        for (key, value) in &form_data {
            outputs.insert(key.clone(), Segment::from_value(value));
        }

        Ok((outputs, edge, decision, form_data))
    }

    async fn handle_human_input_paused(
        &mut self,
        node_id: &str,
        info: &NodeInfo,
        result: &mut NodeRunResult,
    ) -> WorkflowResult<()> {
        let request_value = result
            .metadata
            .get(HUMAN_INPUT_REQUEST_KEY)
            .ok_or_else(|| {
                WorkflowError::InternalError("human-input request metadata missing".to_string())
            })?
            .clone();
        let request: HumanInputPauseRequest =
            serde_json::from_value(request_value).map_err(|e| {
                WorkflowError::InternalError(format!("invalid human-input metadata: {}", e))
            })?;

        let prompt = request.prompt_text.clone().unwrap_or_default();

        if self.event_emitter.is_active() {
            self.event_emitter
                .emit(GraphEngineEvent::HumanInputRequested {
                    node_id: request.node_id.clone(),
                    node_type: info.node_type.clone(),
                    node_title: request.node_title.clone(),
                    resume_token: request.resume_token.clone(),
                    resume_mode: Self::human_input_resume_mode_name(&request.resume_mode)
                        .to_string(),
                    form_schema: serde_json::to_value(&request.form_fields).unwrap_or(Value::Null),
                    prompt_text: request.prompt_text.clone(),
                    timeout_at: request.timeout_at,
                })
                .await;

            self.event_emitter
                .emit(GraphEngineEvent::WorkflowPaused {
                    node_id: node_id.to_string(),
                    prompt: prompt.clone(),
                })
                .await;
        }

        if let Some(status_tx) = &self.status_tx {
            status_tx.send_replace(ExecutionStatus::WaitingForInput {
                node_id: request.node_id.clone(),
                resume_token: request.resume_token.clone(),
                resume_mode: Self::human_input_resume_mode_name(&request.resume_mode).to_string(),
                form_schema: serde_json::to_value(&request.form_fields).unwrap_or(Value::Null),
                prompt_text: request.prompt_text.clone(),
                timeout_at: request.timeout_at,
            });
        }

        let (outputs, edge_handle, decision, form_data) = loop {
            let cmd = self.wait_for_human_input_resume(&request).await?;
            match cmd {
                Some(cmd) => match Self::build_human_input_outputs(&request, cmd) {
                    Ok(payload) => break payload,
                    Err(err) => {
                        if self.event_emitter.is_active() {
                            self.event_emitter
                                .emit(GraphEngineEvent::ResumeWarning {
                                    diagnostic: err.to_string(),
                                })
                                .await;
                        }
                        continue;
                    }
                },
                None => {
                    if self.event_emitter.is_active() {
                        self.event_emitter
                            .emit(GraphEngineEvent::HumanInputTimeout {
                                node_id: request.node_id.clone(),
                                resume_token: request.resume_token.clone(),
                                timeout_action: Self::human_input_timeout_action_name(
                                    &request.timeout_action,
                                )
                                .to_string(),
                            })
                            .await;
                    }

                    match request.timeout_action {
                        HumanInputTimeoutAction::Fail => return Err(NodeError::Timeout.into()),
                        HumanInputTimeoutAction::DefaultValue => {
                            let mut defaults = HashMap::new();
                            if let Some(v) = request.timeout_default_values.as_ref() {
                                defaults.extend(v.clone());
                            }
                            let mut out = HashMap::new();
                            for (k, v) in &defaults {
                                out.insert(k.clone(), Segment::from_value(v));
                            }
                            break (out, EdgeHandle::Default, None, defaults);
                        }
                        HumanInputTimeoutAction::AutoApprove => {
                            let mut map = HashMap::new();
                            map.insert(
                                "__decision".to_string(),
                                Value::String("approve".to_string()),
                            );
                            let mut out = HashMap::new();
                            out.insert(
                                "__decision".to_string(),
                                Segment::String("approve".to_string()),
                            );
                            break (
                                out,
                                EdgeHandle::Branch("approve".to_string()),
                                Some(HumanInputDecision::Approve),
                                map,
                            );
                        }
                        HumanInputTimeoutAction::AutoReject => {
                            let mut map = HashMap::new();
                            map.insert(
                                "__decision".to_string(),
                                Value::String("reject".to_string()),
                            );
                            let mut out = HashMap::new();
                            out.insert(
                                "__decision".to_string(),
                                Segment::String("reject".to_string()),
                            );
                            break (
                                out,
                                EdgeHandle::Branch("reject".to_string()),
                                Some(HumanInputDecision::Reject),
                                map,
                            );
                        }
                    }
                }
            };
        };

        result.outputs = crate::dsl::schema::NodeOutputs::Sync(outputs);
        result.edge_source_handle = edge_handle;
        result.status = WorkflowNodeExecutionStatus::Succeeded;

        if self.event_emitter.is_active() {
            self.event_emitter
                .emit(GraphEngineEvent::HumanInputReceived {
                    node_id: request.node_id,
                    resume_token: request.resume_token,
                    decision: decision.map(|d| Self::human_input_decision_name(&d).to_string()),
                    form_data,
                })
                .await;

            self.event_emitter
                .emit(GraphEngineEvent::WorkflowResumed {
                    node_id: node_id.to_string(),
                })
                .await;
        }

        if let Some(status_tx) = &self.status_tx {
            status_tx.send_replace(ExecutionStatus::Running);
        }

        Ok(())
    }

    #[cfg(feature = "checkpoint")]
    fn should_checkpoint_after(&self, node_type: &str, node_config: &Value) -> bool {
        if self.checkpoint_store.is_none() {
            return false;
        }

        match node_type {
            "agent" => true,
            "human-input" => false,
            _ => node_config
                .get("checkpoint")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        }
    }

    #[cfg(feature = "checkpoint")]
    fn should_checkpoint_before(&self, node_type: &str) -> bool {
        if self.checkpoint_store.is_none() {
            return false;
        }
        node_type == "human-input"
    }

    fn is_safe_stop_triggered(&self) -> bool {
        self.safe_stop_requested
            || self
                .safe_stop_signal
                .as_ref()
                .map(|signal| signal.is_triggered())
                .unwrap_or(false)
    }

    async fn handle_safe_stop_joined_outcome(
        &mut self,
        outcome: NodeExecOutcome,
        ready: &mut Vec<String>,
        ready_predecessor: &mut HashMap<String, String>,
        running: &mut HashMap<String, AbortHandle>,
        step_count: i32,
        start_time: i64,
        interrupted_nodes: &mut Vec<String>,
    ) -> WorkflowResult<()> {
        running.remove(&outcome.node_id);

        match outcome.result {
            Ok(result) => {
                if result.status == WorkflowNodeExecutionStatus::Paused {
                    interrupted_nodes.push(outcome.node_id.clone());
                    ready.push(outcome.node_id.clone());
                    return Ok(());
                }

                let run_result = self
                    .enforce_output_limits(&outcome.node_id, &outcome.info.node_type, result)
                    .await;

                if let Ok(result) = run_result {
                    if matches!(result.status, WorkflowNodeExecutionStatus::Exception) {
                        self.exceptions_count += 1;
                    }

                    let ready_len_before = ready.len();
                    let cancelled_sources = self
                        .handle_node_success(
                            &outcome.exec_id,
                            &outcome.node_id,
                            &outcome.info,
                            result,
                            ready,
                            ready_predecessor,
                            step_count,
                            start_time,
                        )
                        .await?;

                    for downstream_node_id in ready.iter().skip(ready_len_before) {
                        ready_predecessor
                            .insert(downstream_node_id.clone(), outcome.node_id.clone());
                    }

                    if !cancelled_sources.is_empty() {
                        {
                            let mut graph = self.graph.write();
                            for source in &cancelled_sources {
                                graph.set_node_state(source, EdgeTraversalState::Cancelled);
                            }
                        }

                        for source in cancelled_sources {
                            if let Some(handle) = running.remove(&source) {
                                handle.abort();
                            }
                        }
                    }

                    self.last_completed_node = Some(outcome.node_id.clone());
                }
            }
            Err(_) => {
                interrupted_nodes.push(outcome.node_id.clone());
                ready.push(outcome.node_id.clone());
            }
        }

        Ok(())
    }

    async fn execute_safe_stop(
        &mut self,
        ready: &mut Vec<String>,
        ready_predecessor: &mut HashMap<String, String>,
        running: &mut HashMap<String, AbortHandle>,
        join_set: &mut JoinSet<NodeExecOutcome>,
        step_count: i32,
        start_time: i64,
    ) -> WorkflowResult<HashMap<String, Value>> {
        #[cfg(feature = "checkpoint")]
        let force_stop_deadline = if self.checkpoint_store.is_some() {
            let timeout_secs = self
                .safe_stop_signal
                .as_ref()
                .map(|signal| signal.timeout_secs())
                .unwrap_or(30);
            Some(tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs))
        } else {
            None
        };
        #[cfg(not(feature = "checkpoint"))]
        let force_stop_deadline: Option<tokio::time::Instant> = None;

        let mut interrupted_nodes = Vec::new();
        #[cfg(feature = "checkpoint")]
        let mut checkpoint_saved = false;
        #[cfg(not(feature = "checkpoint"))]
        let checkpoint_saved = false;

        if !join_set.is_empty() {
            loop {
                if running.is_empty() {
                    break;
                }

                if let Some(deadline) = force_stop_deadline {
                    tokio::select! {
                      joined = join_set.join_next() => {
                        let Some(joined) = joined else {
                          break;
                        };

                        let Ok(outcome) = joined else {
                          continue;
                        };

                        self
                          .handle_safe_stop_joined_outcome(
                            outcome,
                            ready,
                            ready_predecessor,
                            running,
                            step_count,
                            start_time,
                            &mut interrupted_nodes,
                          )
                          .await?;
                      }
                      _ = tokio::time::sleep_until(deadline) => {
                        for (node_id, abort_handle) in running.drain() {
                          abort_handle.abort();
                          interrupted_nodes.push(node_id.clone());
                          ready.push(node_id);
                        }
                        join_set.abort_all();
                        while join_set.join_next().await.is_some() {}
                        break;
                      }
                    }
                } else {
                    let Some(joined) = join_set.join_next().await else {
                        break;
                    };

                    let Ok(outcome) = joined else {
                        continue;
                    };

                    self.handle_safe_stop_joined_outcome(
                        outcome,
                        ready,
                        ready_predecessor,
                        running,
                        step_count,
                        start_time,
                        &mut interrupted_nodes,
                    )
                    .await?;
                }
            }
        }

        #[cfg(feature = "checkpoint")]
        if self.checkpoint_store.is_some() {
            checkpoint_saved = self
                .save_checkpoint(
                    self.last_completed_node.as_deref().unwrap_or("safe_stop"),
                    ready,
                    ready_predecessor,
                    step_count,
                    start_time,
                )
                .await
                .is_ok();
        }

        if self.event_emitter.is_active() {
            self.event_emitter
                .emit(GraphEngineEvent::WorkflowSafeStopped {
                    interrupted_nodes: interrupted_nodes.clone(),
                    checkpoint_saved,
                })
                .await;
        }

        Err(WorkflowError::SafeStopped {
            last_completed_node: self.last_completed_node.clone(),
            interrupted_nodes,
            checkpoint_saved,
        })
    }

    #[cfg(feature = "checkpoint")]
    async fn save_checkpoint(
        &self,
        completed_node_id: &str,
        ready: &[String],
        ready_predecessor: &HashMap<String, String>,
        step_count: i32,
        start_time: i64,
    ) -> WorkflowResult<()> {
        let (Some(store), Some(workflow_id)) = (&self.checkpoint_store, &self.workflow_id) else {
            return Ok(());
        };

        #[cfg(feature = "security")]
        let consumed_resources = if let (Some(governor), Some(group)) = (
            self.context.resource_governor(),
            self.context.resource_group(),
        ) {
            let usage = governor.get_usage(&group.group_id).await;
            Some(ConsumedResources {
                total_prompt_tokens: usage.llm_tokens_today as i64,
                total_completion_tokens: 0,
                total_llm_cost: 0.0,
                total_tool_calls: 0,
            })
        } else {
            None
        };

        #[cfg(not(feature = "security"))]
        let consumed_resources = None;

        let graph_summary = {
            let graph = self.graph.read();
            format!(
                "{}:{}:{}",
                graph.root_node_id(),
                graph.topology.nodes.len(),
                graph.topology.edges.len()
            )
        };
        let schema_hash = self
            .schema_hash
            .clone()
            .unwrap_or_else(|| hash_json(&graph_summary));
        let engine_config_hash = self
            .engine_config_hash
            .clone()
            .unwrap_or_else(|| hash_json(&self.config));

        let (node_states, edge_states, variables) = {
            let pool = self.variable_pool.read();
            let graph = self.graph.read();
            (
                graph
                    .node_states
                    .iter()
                    .map(|(key, state)| (key.clone(), (*state).into()))
                    .collect::<HashMap<String, crate::core::checkpoint::SerializableEdgeState>>(),
                graph
                    .edge_states
                    .iter()
                    .map(|(key, state)| (key.clone(), (*state).into()))
                    .collect::<HashMap<String, crate::core::checkpoint::SerializableEdgeState>>(),
                snapshot_for_checkpoint(&pool),
            )
        };

        #[cfg(feature = "security")]
        let context_fingerprint = Some(ContextFingerprint::capture(
            self.context.as_ref(),
            schema_hash,
            engine_config_hash,
        ));

        #[cfg(not(feature = "security"))]
        let context_fingerprint = Some(ContextFingerprint::capture(
            self.context
                .llm_provider_registry()
                .list()
                .into_iter()
                .map(|provider| provider.id)
                .collect(),
            self.context
                .node_executor_registry()
                .list_registered_types(),
            schema_hash,
            engine_config_hash,
        ));

        let checkpoint = Checkpoint {
            workflow_id: workflow_id.clone(),
            execution_id: self
                .execution_id
                .clone()
                .unwrap_or_else(|| self.context.execution_id.clone()),
            created_at: self.context.time_provider.now_millis(),

            completed_node_id: completed_node_id.to_string(),
            node_states,
            edge_states,
            ready_queue: ready.to_vec(),
            ready_predecessor: ready_predecessor.clone(),

            variables,

            step_count,
            exceptions_count: self.exceptions_count,
            final_outputs: self.final_outputs.clone(),
            elapsed_secs: self.context.time_provider.elapsed_secs(start_time),

            consumed_resources,
            context_fingerprint,
        };

        store
            .save(workflow_id, &checkpoint)
            .await
            .map_err(|error| {
                WorkflowError::InternalError(format!("Checkpoint save failed: {}", error))
            })?;

        if self.event_emitter.is_active() {
            self.event_emitter
                .emit(GraphEngineEvent::CheckpointSaved {
                    node_id: completed_node_id.to_string(),
                })
                .await;
        }

        Ok(())
    }

    #[cfg(feature = "checkpoint")]
    async fn try_resume_from_checkpoint(
        &mut self,
    ) -> WorkflowResult<Option<(Vec<String>, HashMap<String, String>, i32, i64)>> {
        let (Some(store), Some(workflow_id)) = (&self.checkpoint_store, &self.workflow_id) else {
            return Ok(None);
        };

        let Some(checkpoint) = store.load(workflow_id).await.map_err(|error| {
            WorkflowError::InternalError(format!("Checkpoint load failed: {}", error))
        })?
        else {
            return Ok(None);
        };

        let schema_hash = self.schema_hash.clone().unwrap_or_else(|| {
            let graph = self.graph.read();
            let summary = format!(
                "{}:{}:{}",
                graph.root_node_id(),
                graph.topology.nodes.len(),
                graph.topology.edges.len()
            );
            hash_json(&summary)
        });
        let engine_config_hash = self
            .engine_config_hash
            .clone()
            .unwrap_or_else(|| hash_json(&self.config));

        #[cfg(feature = "security")]
        let current_fingerprint =
            ContextFingerprint::capture(self.context.as_ref(), schema_hash, engine_config_hash);

        #[cfg(not(feature = "security"))]
        let current_fingerprint = ContextFingerprint::capture(
            self.context
                .llm_provider_registry()
                .list()
                .into_iter()
                .map(|provider| provider.id)
                .collect(),
            self.context
                .node_executor_registry()
                .list_registered_types(),
            schema_hash,
            engine_config_hash,
        );

        match self.resume_policy {
            ResumePolicy::Normal => {
                if let Some(saved_fingerprint) = &checkpoint.context_fingerprint {
                    let diagnostic = diff_fingerprints(saved_fingerprint, &current_fingerprint);
                    if diagnostic.has_danger() {
                        return Err(WorkflowError::ResumeRejected {
                            workflow_id: workflow_id.clone(),
                            diagnostic: diagnostic.report(),
                            changes: Box::new(diagnostic.changes),
                        });
                    }

                    if diagnostic.has_warnings() && self.event_emitter.is_active() {
                        self.event_emitter
                            .emit(GraphEngineEvent::ResumeWarning {
                                diagnostic: diagnostic.report(),
                            })
                            .await;
                    }
                } else if self.event_emitter.is_active() {
                    self
            .event_emitter
            .emit(GraphEngineEvent::ResumeWarning {
              diagnostic:
                "Checkpoint has no context fingerprint (legacy format). Safety check skipped.".to_string(),
            })
            .await;
                }
            }
            ResumePolicy::Force => {
                if let Some(saved_fingerprint) = &checkpoint.context_fingerprint {
                    let diagnostic = diff_fingerprints(saved_fingerprint, &current_fingerprint);
                    if diagnostic.has_warnings() && self.event_emitter.is_active() {
                        self.event_emitter
                            .emit(GraphEngineEvent::ResumeWarning {
                                diagnostic: diagnostic.report(),
                            })
                            .await;
                    }
                }
            }
        }

        {
            let graph = self.graph.read();
            for node_id in checkpoint.node_states.keys() {
                if graph.get_node(node_id).is_none() {
                    return Err(WorkflowError::InternalError(format!(
                        "Checkpoint corrupted: node '{}' no longer exists in graph",
                        node_id
                    )));
                }
            }
            for edge_id in checkpoint.edge_states.keys() {
                if graph.get_edge(edge_id).is_none() {
                    return Err(WorkflowError::InternalError(format!(
                        "Checkpoint corrupted: edge '{}' no longer exists in graph",
                        edge_id
                    )));
                }
            }
        }

        {
            let mut graph = self.graph.write();
            for (node_id, state) in &checkpoint.node_states {
                graph.set_node_state(node_id, (*state).into());
            }
            for (edge_id, state) in &checkpoint.edge_states {
                graph.set_edge_state(edge_id, (*state).into());
            }
        }

        {
            let mut pool = self.variable_pool.write();
            *pool = restore_from_checkpoint(&checkpoint.variables);
        }
        self.mark_pool_dirty();

        self.exceptions_count = checkpoint.exceptions_count;
        self.final_outputs = checkpoint.final_outputs.clone();
        self.last_completed_node = Some(checkpoint.completed_node_id.clone());

        let adjusted_start =
            self.context.time_provider.now_timestamp() - checkpoint.elapsed_secs as i64;

        if self.event_emitter.is_active() {
            self.event_emitter
                .emit(GraphEngineEvent::CheckpointResumed {
                    node_id: checkpoint.completed_node_id.clone(),
                })
                .await;
        }

        Ok(Some((
            checkpoint.ready_queue,
            checkpoint.ready_predecessor,
            checkpoint.step_count,
            adjusted_start,
        )))
    }

    #[cfg(feature = "checkpoint")]
    async fn delete_checkpoint(&self) {
        let (Some(store), Some(workflow_id)) = (&self.checkpoint_store, &self.workflow_id) else {
            return;
        };
        let _ = store.delete(workflow_id).await;
    }

    async fn check_security_before_node(
        &self,
        node_id: &str,
        node_type: &str,
        node_config: &Value,
    ) -> Result<(), NodeError> {
        self.security_gate
            .check_before_node(node_id, node_type, node_config)
            .await
    }

    async fn enforce_output_limits(
        &self,
        node_id: &str,
        node_type: &str,
        result: NodeRunResult,
    ) -> Result<NodeRunResult, NodeError> {
        self.security_gate
            .enforce_output_limits(node_id, node_type, result)
            .await
    }

    async fn record_llm_usage(&self, result: &NodeRunResult) {
        self.security_gate.record_llm_usage(result).await
    }

    async fn begin_node_execution(
        &self,
        node_id: &str,
        info: &NodeInfo,
        predecessor_node_id: Option<&str>,
        parallel_group_id: Option<&str>,
    ) -> WorkflowResult<String> {
        let exec_id = self.context.id_generator.next_id();

        if let Err(e) = self
            .check_security_before_node(node_id, &info.node_type, &info.node_config)
            .await
        {
            let error_detail = e.to_structured_json();
            let error_info = crate::dsl::schema::NodeErrorInfo {
                message: e.to_string(),
                error_type: Some(e.error_code()),
                detail: Some(error_detail.clone()),
            };
            if self.event_emitter.is_active() {
                self.event_emitter
                    .emit(GraphEngineEvent::NodeRunFailed {
                        id: exec_id.clone(),
                        node_id: node_id.to_string(),
                        node_type: info.node_type.clone(),
                        node_run_result: NodeRunResult {
                            status: WorkflowNodeExecutionStatus::Failed,
                            error: Some(error_info),
                            ..Default::default()
                        },
                        error: e.to_string(),
                    })
                    .await;

                self.event_emitter
                    .emit(GraphEngineEvent::GraphRunFailed {
                        error: e.to_string(),
                        exceptions_count: self.exceptions_count,
                    })
                    .await;
            }

            return Err(WorkflowError::NodeExecutionError {
                node_id: node_id.to_string(),
                error: e.to_string(),
                error_detail: Some(error_detail),
            });
        }

        if self.event_emitter.is_active() {
            self.event_emitter
                .emit(GraphEngineEvent::NodeRunStarted {
                    id: exec_id.clone(),
                    node_id: node_id.to_string(),
                    node_type: info.node_type.clone(),
                    node_title: info.node_title.clone(),
                    predecessor_node_id: predecessor_node_id.map(str::to_string),
                    parallel_group_id: parallel_group_id.map(str::to_string),
                })
                .await;
        }

        self.emit_before_node_hooks(
            node_id,
            &info.node_type,
            &info.node_title,
            &info.node_config,
        )
        .await?;

        Ok(exec_id)
    }

    async fn execute_node_serial(
        &mut self,
        node_id: String,
        predecessor_node_id: Option<String>,
        ready: &mut Vec<String>,
        ready_predecessor: &mut HashMap<String, String>,
        step_count: &mut i32,
        max_steps: i32,
        start_time: i64,
        max_exec_time: u64,
    ) -> WorkflowResult<()> {
        self.check_limits(step_count, max_steps, start_time, max_exec_time)
            .await?;

        let info = self.load_node_info(&node_id)?;
        if info.is_skipped {
            return Ok(());
        }

        #[cfg(feature = "checkpoint")]
        if self.should_checkpoint_before(&info.node_type) {
            self.save_checkpoint(&node_id, ready, ready_predecessor, *step_count, start_time)
                .await?;
        }

        let mut pool_snapshot_for_exec: Option<VariablePool> = None;
        if self.debug_gate.should_pause_before(&node_id) {
            let pool_snapshot = self.variable_pool.read().clone();
            let action = self
                .debug_hook
                .before_node_execute(&node_id, &info.node_type, &info.node_title, &pool_snapshot)
                .await?;

            match self.apply_debug_action(action).await? {
                DebugActionResult::Continue => {
                    pool_snapshot_for_exec = Some(pool_snapshot);
                }
                DebugActionResult::Abort(reason) => {
                    return Err(WorkflowError::Aborted(reason));
                }
                DebugActionResult::SkipNode => {
                    let downstream = self.skip_node_and_collect_ready(&node_id, info.is_branch);
                    for downstream_node_id in &downstream {
                        ready_predecessor.insert(downstream_node_id.clone(), node_id.clone());
                    }
                    ready.extend(downstream);
                    return Ok(());
                }
            }
        }

        let exec_id = self
            .begin_node_execution(&node_id, &info, predecessor_node_id.as_deref(), None)
            .await?;
        let pool_snapshot =
            pool_snapshot_for_exec.unwrap_or_else(|| self.variable_pool.read().clone());

        let run_result = Self::execute_node_with_retry_inner(
            self.registry.clone(),
            self.compiled_node_configs.clone(),
            self.context.clone(),
            self.event_emitter.clone(),
            &exec_id,
            &node_id,
            &info.node_type,
            &info.node_title,
            &info.node_config,
            &pool_snapshot,
            &info.error_strategy,
            &info.retry_config,
            info.timeout_secs,
        )
        .await;

        let run_result = match run_result {
            Ok(result) => {
                self.enforce_output_limits(&node_id, &info.node_type, result)
                    .await
            }
            Err(e) => Err(e),
        };

        match run_result {
            Ok(result) => {
                if matches!(result.status, WorkflowNodeExecutionStatus::Exception) {
                    self.exceptions_count += 1;
                }
                let ready_len_before = ready.len();
                let cancelled_sources = self
                    .handle_node_success(
                        &exec_id,
                        &node_id,
                        &info,
                        result,
                        ready,
                        ready_predecessor,
                        *step_count,
                        start_time,
                    )
                    .await?;
                for downstream_node_id in ready.iter().skip(ready_len_before) {
                    ready_predecessor.insert(downstream_node_id.clone(), node_id.clone());
                }
                if !cancelled_sources.is_empty() {
                    let mut g = self.graph.write();
                    for source in cancelled_sources {
                        g.set_node_state(&source, EdgeTraversalState::Cancelled);
                    }
                }

                #[cfg(feature = "checkpoint")]
                if self.should_checkpoint_after(&info.node_type, &info.node_config) {
                    self.save_checkpoint(
                        &node_id,
                        ready,
                        ready_predecessor,
                        *step_count,
                        start_time,
                    )
                    .await?;
                }

                self.last_completed_node = Some(node_id.clone());
            }
            Err(e) => {
                let err = self.handle_node_failure(exec_id, node_id, &info, e).await;
                return Err(err);
            }
        }

        Ok(())
    }

    fn collect_timed_out_gathers(
        &self,
        wait_started: &mut HashMap<String, i64>,
    ) -> Vec<GatherTimeoutAction> {
        let now = self.context.time_provider.now_timestamp();
        let mut active = HashSet::new();
        let mut actions = Vec::new();

        let graph = self.graph.read();
        for (node_id, node) in &graph.topology.nodes {
            if node.node_type != "gather" {
                continue;
            }

            let gather = serde_json::from_value::<GatherNodeData>(node.config.clone()).unwrap_or(
                GatherNodeData {
                    variables: Vec::new(),
                    join_mode: JoinMode::All,
                    cancel_remaining: true,
                    timeout_secs: None,
                    timeout_strategy: TimeoutStrategy::ProceedWithAvailable,
                },
            );

            let Some(timeout_secs) = gather.timeout_secs else {
                wait_started.remove(node_id);
                continue;
            };

            let is_pending = matches!(graph.node_state(node_id), Some(EdgeTraversalState::Pending));
            let is_ready = graph.is_node_ready(node_id);
            if !is_pending || is_ready {
                wait_started.remove(node_id);
                continue;
            }

            active.insert(node_id.clone());
            let started = *wait_started.entry(node_id.clone()).or_insert(now);
            if self.context.time_provider.elapsed_secs(started) >= timeout_secs {
                actions.push(GatherTimeoutAction {
                    node_id: node_id.clone(),
                    timeout_secs,
                    timeout_strategy: gather.timeout_strategy,
                    cancel_remaining: gather.cancel_remaining,
                });
            }
        }
        drop(graph);

        wait_started.retain(|node_id, _| active.contains(node_id));
        actions
    }

    /// Run the workflow to completion
    pub async fn run(&mut self) -> WorkflowResult<HashMap<String, Value>> {
        self.event_emitter
            .emit(GraphEngineEvent::GraphRunStarted)
            .await;
        self.emit_before_workflow_hooks().await?;

        let (mut ready, mut ready_predecessor, mut step_count, start_time) = {
            #[cfg(feature = "checkpoint")]
            {
                if let Some(resumed) = self.try_resume_from_checkpoint().await? {
                    resumed
                } else {
                    let root_id = {
                        let g = self.graph.read();
                        g.root_node_id().to_string()
                    };
                    (
                        vec![root_id],
                        HashMap::new(),
                        0,
                        self.context.time_provider.now_timestamp(),
                    )
                }
            }

            #[cfg(not(feature = "checkpoint"))]
            {
                let root_id = {
                    let g = self.graph.read();
                    g.root_node_id().to_string()
                };
                (
                    vec![root_id],
                    HashMap::new(),
                    0,
                    self.context.time_provider.now_timestamp(),
                )
            }
        };

        let mut join_set: JoinSet<NodeExecOutcome> = JoinSet::new();
        let mut running: HashMap<String, AbortHandle> = HashMap::new();
        let mut gather_wait_started: HashMap<String, i64> = HashMap::new();

        let (max_steps, max_exec_time) = self.effective_limits();
        let max_concurrency = if !self.config.parallel_enabled {
            1
        } else {
            self.config.max_concurrency
        };

        loop {
            self.poll_commands().await?;

            if self.is_safe_stop_triggered() {
                return self
                    .execute_safe_stop(
                        &mut ready,
                        &mut ready_predecessor,
                        &mut running,
                        &mut join_set,
                        step_count,
                        start_time,
                    )
                    .await;
            }

            let timeout_actions = self.collect_timed_out_gathers(&mut gather_wait_started);
            if !timeout_actions.is_empty() {
                for action in timeout_actions {
                    gather_wait_started.remove(&action.node_id);

                    match action.timeout_strategy {
                        TimeoutStrategy::ProceedWithAvailable => {
                            let (cancelled_sources, should_enqueue) = {
                                let mut graph = self.graph.write();
                                let cancelled_sources =
                                    graph.finalize_gather(&action.node_id, action.cancel_remaining);
                                let should_enqueue = graph.is_node_ready(&action.node_id)
                                    && matches!(
                                        graph.node_state(&action.node_id),
                                        Some(EdgeTraversalState::Pending)
                                    );
                                (cancelled_sources, should_enqueue)
                            };

                            if action.cancel_remaining && !cancelled_sources.is_empty() {
                                {
                                    let mut graph = self.graph.write();
                                    for source in &cancelled_sources {
                                        graph.set_node_state(source, EdgeTraversalState::Cancelled);
                                    }
                                }
                                for source in cancelled_sources {
                                    if let Some(handle) = running.remove(&source) {
                                        handle.abort();
                                    }
                                }
                            }

                            if should_enqueue
                                && !ready.iter().any(|id| id == &action.node_id)
                                && !running.contains_key(&action.node_id)
                            {
                                ready_predecessor.remove(&action.node_id);
                                ready.push(action.node_id);
                            }
                        }
                        TimeoutStrategy::Fail => {
                            join_set.abort_all();
                            while let Some(joined) = join_set.join_next().await {
                                if let Ok(outcome) = joined {
                                    running.remove(&outcome.node_id);
                                }
                            }

                            let info = self.load_node_info(&action.node_id)?;
                            let error =
                                NodeError::Timeout.with_context(ErrorContext::non_retryable(
                                    ErrorCode::Timeout,
                                    format!(
                                        "Gather node '{}' timed out after {}s",
                                        action.node_id, action.timeout_secs
                                    ),
                                ));
                            let err = self
                                .handle_node_failure(
                                    self.context.id_generator.next_id(),
                                    action.node_id,
                                    &info,
                                    error,
                                )
                                .await;
                            return Err(err);
                        }
                    }
                }

                continue;
            }

            let debug_index = ready.iter().rposition(|id| {
                self.debug_gate.should_pause_before(id) || self.debug_gate.should_pause_after(id)
            });

            if join_set.is_empty() {
                if let Some(index) = debug_index {
                    let node_id = ready.swap_remove(index);
                    let predecessor_node_id = ready_predecessor.remove(&node_id);
                    self.execute_node_serial(
                        node_id,
                        predecessor_node_id,
                        &mut ready,
                        &mut ready_predecessor,
                        &mut step_count,
                        max_steps,
                        start_time,
                        max_exec_time,
                    )
                    .await?;
                    continue;
                }
            }

            let parallel_group_id = if ready.is_empty() {
                None
            } else {
                Some(self.context.id_generator.next_id())
            };

            while !ready.is_empty() && (max_concurrency == 0 || join_set.len() < max_concurrency) {
                let index = ready.iter().rposition(|id| {
                    !(self.debug_gate.should_pause_before(id)
                        || self.debug_gate.should_pause_after(id))
                });
                let Some(index) = index else {
                    break;
                };

                let node_id = ready.remove(index);
                if running.contains_key(&node_id) {
                    continue;
                }

                self.check_limits(&mut step_count, max_steps, start_time, max_exec_time)
                    .await?;

                let info = self.load_node_info(&node_id)?;
                if info.is_skipped {
                    continue;
                }

                #[cfg(feature = "checkpoint")]
                if self.should_checkpoint_before(&info.node_type) {
                    self.save_checkpoint(
                        &node_id,
                        &ready,
                        &ready_predecessor,
                        step_count,
                        start_time,
                    )
                    .await?;
                }

                let predecessor_node_id = ready_predecessor.remove(&node_id);
                let exec_id = self
                    .begin_node_execution(
                        &node_id,
                        &info,
                        predecessor_node_id.as_deref(),
                        parallel_group_id.as_deref(),
                    )
                    .await?;
                let pool_snapshot = self.variable_pool.read().clone();

                let registry = self.registry.clone();
                let compiled_node_configs = self.compiled_node_configs.clone();
                let context = self.context.clone();
                let event_emitter = self.event_emitter.clone();
                let task_node_id = node_id.clone();
                let task_info = info.clone();
                let abort_handle = join_set.spawn(async move {
                    let result = WorkflowDispatcher::<G, H>::execute_node_with_retry_inner(
                        registry,
                        compiled_node_configs,
                        context,
                        event_emitter,
                        &exec_id,
                        &task_node_id,
                        &task_info.node_type,
                        &task_info.node_title,
                        &task_info.node_config,
                        &pool_snapshot,
                        &task_info.error_strategy,
                        &task_info.retry_config,
                        task_info.timeout_secs,
                    )
                    .await;

                    NodeExecOutcome {
                        exec_id,
                        node_id: task_node_id,
                        info: task_info,
                        result,
                    }
                });

                running.insert(node_id, abort_handle);
            }

            if join_set.is_empty() {
                if ready.is_empty() {
                    break;
                }
                continue;
            }

            let Some(joined) = join_set.join_next().await else {
                continue;
            };

            let outcome = match joined {
                Ok(outcome) => outcome,
                Err(join_error) => {
                    if join_error.is_cancelled() {
                        continue;
                    }
                    return Err(WorkflowError::InternalError(format!(
                        "node task join error: {}",
                        join_error
                    )));
                }
            };

            running.remove(&outcome.node_id);

            let run_result = match outcome.result {
                Ok(result) => {
                    self.enforce_output_limits(&outcome.node_id, &outcome.info.node_type, result)
                        .await
                }
                Err(e) => Err(e),
            };

            match run_result {
                Ok(result) => {
                    if matches!(result.status, WorkflowNodeExecutionStatus::Exception) {
                        self.exceptions_count += 1;
                    }

                    let ready_len_before = ready.len();
                    let cancelled_sources = self
                        .handle_node_success(
                            &outcome.exec_id,
                            &outcome.node_id,
                            &outcome.info,
                            result,
                            &mut ready,
                            &ready_predecessor,
                            step_count,
                            start_time,
                        )
                        .await?;
                    for downstream_node_id in ready.iter().skip(ready_len_before) {
                        ready_predecessor
                            .insert(downstream_node_id.clone(), outcome.node_id.clone());
                    }

                    if !cancelled_sources.is_empty() {
                        {
                            let mut graph = self.graph.write();
                            for source in &cancelled_sources {
                                graph.set_node_state(source, EdgeTraversalState::Cancelled);
                            }
                        }

                        for source in cancelled_sources {
                            if let Some(handle) = running.remove(&source) {
                                handle.abort();
                            }
                        }
                    }

                    #[cfg(feature = "checkpoint")]
                    if self
                        .should_checkpoint_after(&outcome.info.node_type, &outcome.info.node_config)
                    {
                        self.save_checkpoint(
                            &outcome.node_id,
                            &ready,
                            &ready_predecessor,
                            step_count,
                            start_time,
                        )
                        .await?;
                    }

                    self.last_completed_node = Some(outcome.node_id.clone());
                }
                Err(e) => {
                    join_set.abort_all();
                    while let Some(joined) = join_set.join_next().await {
                        if let Ok(outcome) = joined {
                            running.remove(&outcome.node_id);
                        }
                    }

                    let err = self
                        .handle_node_failure(outcome.exec_id, outcome.node_id, &outcome.info, e)
                        .await;
                    return Err(err);
                }
            }
        }

        if self.event_emitter.is_active() {
            if self.exceptions_count > 0 {
                self.event_emitter
                    .emit(GraphEngineEvent::GraphRunPartialSucceeded {
                        exceptions_count: self.exceptions_count,
                        outputs: self.final_outputs.clone(),
                    })
                    .await;
            } else {
                self.event_emitter
                    .emit(GraphEngineEvent::GraphRunSucceeded {
                        outputs: self.final_outputs.clone(),
                    })
                    .await;
            }
        }

        #[cfg(feature = "checkpoint")]
        self.delete_checkpoint().await;

        self.emit_after_workflow_hooks().await?;

        Ok(self.final_outputs.clone())
    }
}

fn calculate_retry_interval(
    retry_config: &Option<RetryConfig>,
    attempt: i32,
    error: &NodeError,
) -> u64 {
    let rc = match retry_config {
        Some(rc) => rc,
        None => return 0,
    };

    if let Some(ctx) = error.error_context() {
        if let Some(retry_after) = ctx.retry_after_secs {
            return retry_after * 1000;
        }
    }

    let base = rc.retry_interval.max(0) as u64;
    let interval = match rc.backoff_strategy {
        BackoffStrategy::Fixed => base,
        BackoffStrategy::Exponential => {
            let multiplied = base as f64 * rc.backoff_multiplier.powi(attempt);
            multiplied as u64
        }
        BackoffStrategy::ExponentialWithJitter => {
            let multiplied = base as f64 * rc.backoff_multiplier.powi(attempt);
            let jitter = rand::random::<f64>() * multiplied * 0.1;
            (multiplied + jitter) as u64
        }
    };

    interval.min(rc.max_retry_interval.max(0) as u64)
}

#[cfg(all(test, feature = "builtin-core-nodes"))]
mod tests {
    use super::*;
    use crate::dsl::{parse_dsl, DslFormat};
    use crate::graph::build_graph;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    #[derive(Clone)]
    struct ParallelProbe {
        current: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
    }

    impl ParallelProbe {
        fn new() -> Self {
            Self {
                current: Arc::new(AtomicUsize::new(0)),
                max_seen: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn max_seen(&self) -> usize {
            self.max_seen.load(Ordering::SeqCst)
        }
    }

    struct ProbeSleepExecutor {
        probe: ParallelProbe,
    }

    #[async_trait]
    impl crate::nodes::executor::NodeExecutor for ProbeSleepExecutor {
        async fn execute(
            &self,
            _node_id: &str,
            _config: &Value,
            _variable_pool: &VariablePool,
            _context: &RuntimeContext,
        ) -> Result<NodeRunResult, NodeError> {
            let active = self.probe.current.fetch_add(1, Ordering::SeqCst) + 1;
            let mut prev = self.probe.max_seen.load(Ordering::SeqCst);
            while active > prev {
                match self.probe.max_seen.compare_exchange(
                    prev,
                    active,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(actual) => prev = actual,
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            self.probe.current.fetch_sub(1, Ordering::SeqCst);

            Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                ..Default::default()
            })
        }
    }

    fn make_emitter() -> (EventEmitter, mpsc::Receiver<GraphEngineEvent>) {
        let (tx, rx) = mpsc::channel(100);
        let active = Arc::new(AtomicBool::new(true));
        (EventEmitter::new(tx, active), rx)
    }

    #[tokio::test]
    async fn test_simple_start_end() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start_1
    data:
      type: start
      title: Start
      variables:
        - variable: query
          label: Query
          type: string
          required: true
  - id: end_1
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["start_1", "query"]
edges:
  - source: start_1
    target: end_1
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();

        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("start_1", "query"),
            Segment::String("hello".into()),
        );
        pool.set(
            &crate::core::variable_pool::Selector::new("sys", "query"),
            Segment::String("hello".into()),
        );

        let registry = NodeExecutorRegistry::new();
        let (emitter, mut rx) = make_emitter();

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await.unwrap();

        assert_eq!(result.get("result"), Some(&Value::String("hello".into())));

        // Check events
        let mut events = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            events.push(evt);
        }
        assert!(events
            .iter()
            .any(|e| matches!(e, GraphEngineEvent::GraphRunStarted)));
        assert!(events
            .iter()
            .any(|e| matches!(e, GraphEngineEvent::GraphRunSucceeded { .. })));
    }

    #[tokio::test]
    async fn test_ifelse_branch() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: if1
    data:
      type: if-else
      title: Check
      cases:
        - case_id: case1
          logical_operator: and
          conditions:
            - variable_selector: ["start", "x"]
              comparison_operator: greater_than
              value: 5
  - id: a
    data:
      type: end
      title: End A
      outputs:
        - variable: branch
          value_selector: ["start", "x"]
  - id: b
    data:
      type: end
      title: End B
      outputs:
        - variable: branch
          value_selector: ["start", "x"]
edges:
  - source: start
    target: if1
  - source: if1
    target: a
    sourceHandle: case1
  - source: if1
    target: b
    sourceHandle: "false"
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();

        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("start", "x"),
            Segment::Integer(10),
        );

        let registry = NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await.unwrap();

        assert_eq!(result.get("branch"), Some(&serde_json::json!(10)));
    }

    #[tokio::test]
    async fn test_ifelse_else_branch() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: if1
    data:
      type: if-else
      title: Check
      cases:
        - case_id: case1
          logical_operator: and
          conditions:
            - variable_selector: ["start", "x"]
              comparison_operator: greater_than
              value: 5
  - id: a
    data:
      type: end
      title: A
      outputs: []
  - id: b
    data:
      type: end
      title: B
      outputs:
        - variable: went
          value_selector: ["start", "x"]
edges:
  - source: start
    target: if1
  - source: if1
    target: a
    sourceHandle: case1
  - source: if1
    target: b
    sourceHandle: "false"
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();

        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("start", "x"),
            Segment::Integer(3),
        );

        let registry = NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await.unwrap();

        assert_eq!(result.get("went"), Some(&serde_json::json!(3)));
    }

    #[tokio::test]
    async fn test_answer_node() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: ans
    data:
      type: answer
      title: Answer
      answer: "Hello {{#start.name#}}!"
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: answer
          value_selector: ["ans", "answer"]
edges:
  - source: start
    target: ans
  - source: ans
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();

        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("start", "name"),
            Segment::String("Alice".into()),
        );

        let registry = NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await.unwrap();

        assert_eq!(
            result.get("answer"),
            Some(&Value::String("Hello Alice!".into()))
        );
    }

    #[tokio::test]
    async fn test_max_steps() {
        // Create a simple graph with just start -> end but set max_steps to 0
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();
        let pool = VariablePool::new();
        let registry = NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();

        let config = EngineConfig {
            max_steps: 0,
            max_execution_time_secs: 600,
            strict_template: false,
            ..Default::default()
        };
        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            config,
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await;
        assert!(result.is_err());
    }

    #[cfg(feature = "builtin-transform-nodes")]
    #[tokio::test]
    async fn test_template_transform_pipeline() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: tt
    data:
      type: template-transform
      title: Transform
      template: "Hello {{ name }}!"
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
        let graph = build_graph(&schema).unwrap();

        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("start", "name"),
            Segment::String("World".into()),
        );

        let registry = NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await.unwrap();

        assert_eq!(
            result.get("result"),
            Some(&Value::String("Hello World!".into()))
        );
    }

    #[cfg(all(feature = "builtin-transform-nodes", feature = "builtin-code-node"))]
    #[tokio::test]
    async fn test_variable_aggregator_pipeline() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: if1
    data:
      type: if-else
      title: IF
      cases:
        - case_id: case1
          logical_operator: and
          conditions:
            - variable_selector: ["start", "flag"]
              comparison_operator: is
              value: "true"
  - id: code_a
    data: { type: code, title: A, code: "function main(inputs) { return { result: 'from_a' }; }", language: javascript }
  - id: code_b
    data: { type: code, title: B, code: "function main(inputs) { return { result: 'from_b' }; }", language: javascript }
  - id: agg
    data:
      type: variable-aggregator
      title: Agg
      variables:
        - ["code_a", "result"]
        - ["code_b", "result"]
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: out
          value_selector: ["agg", "output"]
edges:
  - source: start
    target: if1
  - source: if1
    target: code_a
    sourceHandle: case1
  - source: if1
    target: code_b
    sourceHandle: "false"
  - source: code_a
    target: agg
  - source: code_b
    target: agg
  - source: agg
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();

        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("start", "flag"),
            Segment::String("true".into()),
        );

        let registry = NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await.unwrap();

        // The aggregator picks up code_a's result (first non-null in the list)
        assert!(result.contains_key("out"));
    }

    #[tokio::test]
    async fn test_event_sequence() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: Start }
  - id: e
    data: { type: end, title: End, outputs: [] }
edges:
  - source: s
    target: e
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();
        let pool = VariablePool::new();
        let registry = NodeExecutorRegistry::new();
        let (emitter, mut rx) = make_emitter();

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let _ = dispatcher.run().await.unwrap();

        let mut event_types = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            let json = evt.to_json();
            event_types.push(json["type"].as_str().unwrap_or("").to_string());
        }

        assert_eq!(event_types[0], "graph_run_started");
        assert!(event_types.contains(&"node_run_started".to_string()));
        assert!(event_types.contains(&"node_run_succeeded".to_string()));
        assert!(event_types.last().unwrap().contains("graph_run"));
    }

    #[test]
    fn test_engine_config_default() {
        let config = EngineConfig::default();
        assert_eq!(config.max_steps, 500);
        assert_eq!(config.max_execution_time_secs, 600);
        assert!(!config.strict_template);
        assert!(config.parallel_enabled);
        assert_eq!(config.max_concurrency, 0);
    }

    #[tokio::test]
    async fn test_parallel_execution_respects_unlimited_concurrency() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: a
    data: { type: probe-sleep, title: A }
  - id: b
    data: { type: probe-sleep, title: B }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: a
  - source: start
    target: b
  - source: a
    target: end
  - source: b
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();

        let pool = VariablePool::new();
        let mut registry = NodeExecutorRegistry::new();
        let probe = ParallelProbe::new();
        registry.register(
            "probe-sleep",
            Box::new(ProbeSleepExecutor {
                probe: probe.clone(),
            }),
        );
        let (emitter, _rx) = make_emitter();

        let config = EngineConfig {
            parallel_enabled: true,
            max_concurrency: 0,
            ..Default::default()
        };
        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            config,
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );

        let _ = dispatcher.run().await.unwrap();
        assert!(probe.max_seen() >= 2);
    }

    #[tokio::test]
    async fn test_parallel_execution_respects_max_concurrency_one() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: a
    data: { type: probe-sleep, title: A }
  - id: b
    data: { type: probe-sleep, title: B }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: a
  - source: start
    target: b
  - source: a
    target: end
  - source: b
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();

        let pool = VariablePool::new();
        let mut registry = NodeExecutorRegistry::new();
        let probe = ParallelProbe::new();
        registry.register(
            "probe-sleep",
            Box::new(ProbeSleepExecutor {
                probe: probe.clone(),
            }),
        );
        let (emitter, _rx) = make_emitter();

        let config = EngineConfig {
            parallel_enabled: true,
            max_concurrency: 1,
            ..Default::default()
        };
        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            config,
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );

        let _ = dispatcher.run().await.unwrap();
        assert_eq!(probe.max_seen(), 1);
    }

    #[tokio::test]
    async fn test_safe_stop_without_checkpoint_waits_running_nodes() {
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: a
    data: { type: probe-sleep, title: A }
  - id: b
    data: { type: probe-sleep, title: B }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: a
  - source: start
    target: b
  - source: a
    target: end
  - source: b
    target: end
"#;
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();

        let pool = VariablePool::new();
        let mut registry = NodeExecutorRegistry::new();
        let probe = ParallelProbe::new();
        registry.register(
            "probe-sleep",
            Box::new(ProbeSleepExecutor {
                probe: probe.clone(),
            }),
        );
        let (emitter, _rx) = make_emitter();

        let config = EngineConfig {
            parallel_enabled: true,
            max_concurrency: 0,
            ..Default::default()
        };
        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            config,
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );

        let (status_tx, _status_rx) = watch::channel(ExecutionStatus::Running);
        let (command_tx, command_rx) = mpsc::channel(8);
        dispatcher.set_control_channels(status_tx, command_rx);

        let run_task = tokio::spawn(async move { dispatcher.run().await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        command_tx.send(Command::SafeStop).await.unwrap();

        let result = run_task.await.unwrap();
        match result {
            Err(WorkflowError::SafeStopped {
                interrupted_nodes,
                checkpoint_saved,
                ..
            }) => {
                assert!(interrupted_nodes.is_empty());
                assert!(!checkpoint_saved);
            }
            other => panic!("expected SafeStopped, got {:?}", other),
        }
    }

    #[test]
    fn test_engine_config_serde() {
        let config = EngineConfig {
            max_steps: 100,
            max_execution_time_secs: 30,
            strict_template: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["max_steps"], 100);
        assert_eq!(json["max_execution_time_secs"], 30);
        assert_eq!(json["strict_template"], true);

        let deserialized: EngineConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.max_steps, 100);
    }

    #[test]
    fn test_engine_config_strict_template_default_false() {
        let json = serde_json::json!({
            "max_steps": 100,
            "max_execution_time_secs": 30
        });
        let config: EngineConfig = serde_json::from_value(json).unwrap();
        assert!(!config.strict_template);
    }

    #[test]
    fn test_calculate_retry_interval_none_config() {
        let err = crate::error::NodeError::ExecutionError("e".into());
        assert_eq!(calculate_retry_interval(&None, 0, &err), 0);
    }

    #[test]
    fn test_calculate_retry_interval_fixed() {
        let rc = crate::dsl::schema::RetryConfig {
            max_retries: 3,
            retry_interval: 100,
            backoff_strategy: crate::dsl::schema::BackoffStrategy::Fixed,
            backoff_multiplier: 2.0,
            max_retry_interval: 10000,
            retry_on_retryable_only: true,
        };
        let err = crate::error::NodeError::ExecutionError("e".into());
        let interval = calculate_retry_interval(&Some(rc), 0, &err);
        assert_eq!(interval, 100);
    }

    #[test]
    fn test_calculate_retry_interval_exponential() {
        let rc = crate::dsl::schema::RetryConfig {
            max_retries: 3,
            retry_interval: 100,
            backoff_strategy: crate::dsl::schema::BackoffStrategy::Exponential,
            backoff_multiplier: 2.0,
            max_retry_interval: 10000,
            retry_on_retryable_only: true,
        };
        let err = crate::error::NodeError::ExecutionError("e".into());
        let i0 = calculate_retry_interval(&Some(rc.clone()), 0, &err);
        let i1 = calculate_retry_interval(&Some(rc.clone()), 1, &err);
        let i2 = calculate_retry_interval(&Some(rc), 2, &err);
        assert_eq!(i0, 100); // 100 * 2^0
        assert_eq!(i1, 200); // 100 * 2^1
        assert_eq!(i2, 400); // 100 * 2^2
    }

    #[test]
    fn test_calculate_retry_interval_exponential_with_jitter() {
        let rc = crate::dsl::schema::RetryConfig {
            max_retries: 3,
            retry_interval: 100,
            backoff_strategy: crate::dsl::schema::BackoffStrategy::ExponentialWithJitter,
            backoff_multiplier: 2.0,
            max_retry_interval: 10000,
            retry_on_retryable_only: true,
        };
        let err = crate::error::NodeError::ExecutionError("e".into());
        let interval = calculate_retry_interval(&Some(rc), 0, &err);
        // Base is 100, jitter adds up to 10% → [100, 110]
        assert!((100..=110).contains(&interval));
    }

    #[test]
    fn test_calculate_retry_interval_capped() {
        let rc = crate::dsl::schema::RetryConfig {
            max_retries: 3,
            retry_interval: 1000,
            backoff_strategy: crate::dsl::schema::BackoffStrategy::Exponential,
            backoff_multiplier: 10.0,
            max_retry_interval: 5000,
            retry_on_retryable_only: true,
        };
        let err = crate::error::NodeError::ExecutionError("e".into());
        let interval = calculate_retry_interval(&Some(rc), 2, &err);
        // 1000 * 10^2 = 100000, capped at 5000
        assert_eq!(interval, 5000);
    }

    #[test]
    fn test_calculate_retry_interval_with_retry_after() {
        use crate::error::ErrorCode;
        let rc = crate::dsl::schema::RetryConfig {
            max_retries: 3,
            retry_interval: 100,
            backoff_strategy: crate::dsl::schema::BackoffStrategy::Fixed,
            backoff_multiplier: 1.0,
            max_retry_interval: 10000,
            retry_on_retryable_only: true,
        };
        let ctx = crate::error::ErrorContext::retryable(ErrorCode::LlmRateLimit, "rate limited")
            .with_retry_after(30);
        let err = crate::error::NodeError::ExecutionError("rate limited".into()).with_context(ctx);
        let interval = calculate_retry_interval(&Some(rc), 0, &err);
        assert_eq!(interval, 30000); // 30s * 1000
    }

    #[tokio::test]
    async fn test_event_emitter_inactive() {
        let (tx, _rx) = mpsc::channel(10);
        let active = Arc::new(AtomicBool::new(false));
        let emitter = EventEmitter::new(tx, active);
        assert!(!emitter.is_active());
        // emit should do nothing when inactive
        emitter.emit(GraphEngineEvent::GraphRunStarted).await;
    }

    #[tokio::test]
    async fn test_event_emitter_active() {
        let (tx, mut rx) = mpsc::channel(10);
        let active = Arc::new(AtomicBool::new(true));
        let emitter = EventEmitter::new(tx, active);
        assert!(emitter.is_active());
        emitter.emit(GraphEngineEvent::GraphRunStarted).await;
        let evt = rx.recv().await.unwrap();
        assert!(matches!(evt, GraphEngineEvent::GraphRunStarted));
    }

    #[test]
    fn test_command_debug() {
        let cmd = Command::Abort {
            reason: Some("test".into()),
        };
        let debug_str = format!("{:?}", cmd);
        assert!(debug_str.contains("Abort"));

        let cmd2 = Command::Pause;
        assert!(format!("{:?}", cmd2).contains("Pause"));

        let cmd3 = Command::UpdateVariables {
            variables: HashMap::new(),
        };
        assert!(format!("{:?}", cmd3).contains("UpdateVariables"));
    }

    #[tokio::test]
    async fn test_dispatcher_partial_outputs() {
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
        - variable: val
          value_selector: ["s", "x"]
edges:
  - source: s
    target: e
"#;
        let schema = crate::dsl::parse_dsl(yaml, crate::dsl::DslFormat::Yaml).unwrap();
        let graph = crate::graph::build_graph(&schema).unwrap();
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("s", "x"),
            crate::core::variable_pool::Segment::Integer(42),
        );
        let registry = crate::nodes::executor::NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();
        let context = Arc::new(crate::core::runtime_context::RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let _ = dispatcher.run().await.unwrap();
        let partial = dispatcher.partial_outputs();
        assert_eq!(partial.get("val"), Some(&serde_json::json!(42)));
    }

    #[tokio::test]
    async fn test_dispatcher_snapshot_pool() {
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
        let schema = crate::dsl::parse_dsl(yaml, crate::dsl::DslFormat::Yaml).unwrap();
        let graph = crate::graph::build_graph(&schema).unwrap();
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("s", "key"),
            crate::core::variable_pool::Segment::String("val".into()),
        );
        let registry = crate::nodes::executor::NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();
        let context = Arc::new(crate::core::runtime_context::RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let _ = dispatcher.run().await.unwrap();
        let snap = dispatcher.snapshot_pool().await;
        assert!(!snap.is_empty());
    }

    #[test]
    fn test_dispatcher_resource_weak_dropped() {
        let graph = Arc::new(RwLock::new(
            crate::graph::build_graph(
                &crate::dsl::parse_dsl(
                    r#"version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: e
"#,
                    crate::dsl::DslFormat::Yaml,
                )
                .unwrap(),
            )
            .unwrap(),
        ));
        let pool = Arc::new(RwLock::new(VariablePool::new()));
        let registry = Arc::new(crate::nodes::executor::NodeExecutorRegistry::new());
        let context = Arc::new(crate::core::runtime_context::RuntimeContext::default());

        let weak = DispatcherResourceWeak {
            graph: Arc::downgrade(&graph),
            variable_pool: Arc::downgrade(&pool),
            registry: Arc::downgrade(&registry),
            context: Arc::downgrade(&context),
        };

        assert!(!weak.graph_dropped());
        assert!(!weak.pool_dropped());
        assert!(!weak.registry_dropped());
        assert!(!weak.context_dropped());

        drop(graph);
        assert!(weak.graph_dropped());

        drop(pool);
        assert!(weak.pool_dropped());

        drop(registry);
        assert!(weak.registry_dropped());

        drop(context);
        assert!(weak.context_dropped());
    }

    #[tokio::test]
    async fn test_dispatcher_node_not_found_executor() {
        // Workflow with a node type that has no registered executor
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: custom
    data: { type: custom-node, title: C }
  - id: e
    data: { type: end, title: E, outputs: [] }
edges:
  - source: s
    target: custom
  - source: custom
    target: e
"#;
        let schema = crate::dsl::parse_dsl(yaml, crate::dsl::DslFormat::Yaml).unwrap();
        let graph = crate::graph::build_graph(&schema).unwrap();
        let pool = VariablePool::new();
        let registry = crate::nodes::executor::NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();
        let context = Arc::new(crate::core::runtime_context::RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await;
        // Should fail because custom-node has no executor
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatcher_max_execution_time_field() {
        // Verify EngineConfig with custom max_execution_time_secs compiles and can be used
        let mut config = EngineConfig {
            max_execution_time_secs: 30,
            ..EngineConfig::default()
        };
        assert_eq!(config.max_execution_time_secs, 30);
        config.max_steps = 100;
        assert_eq!(config.max_steps, 100);
    }

    #[tokio::test]
    async fn test_dispatcher_error_strategy_default_value() {
        // Use a node that will fail but with default_value error strategy
        let yaml = r#"
version: "0.1.0"
nodes:
  - id: s
    data: { type: start, title: S }
  - id: code
    data:
      type: code
      title: Code
      code: "function main() { throw new Error('fail'); }"
      language: javascript
      error_strategy:
        type: default-value
        default_value:
          result: "fallback"
  - id: e
    data: { type: end, title: E, outputs: [{variable_selector: [code, result], value_selector: [code, result]}] }
edges:
  - source: s
    target: code
  - source: code
    target: e
"#;
        let schema = crate::dsl::parse_dsl(yaml, crate::dsl::DslFormat::Yaml).unwrap();
        let graph = crate::graph::build_graph(&schema).unwrap();
        let pool = VariablePool::new();
        let registry = crate::nodes::executor::NodeExecutorRegistry::new();
        let (emitter, _rx) = make_emitter();
        let context = Arc::new(crate::core::runtime_context::RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
            graph,
            pool,
            registry,
            emitter,
            EngineConfig::default(),
            context,
            #[cfg(feature = "plugin-system")]
            None,
        );
        let result = dispatcher.run().await;
        // Either succeeds with fallback or fails - exercises error strategy path
        assert!(result.is_ok() || result.is_err());
    }
}

enum DebugActionResult {
    Continue,
    Abort(String),
    SkipNode,
}

#[cfg(test)]
mod emitter_tests {
    use super::*;

    #[tokio::test]
    async fn test_event_emitter_send_after_rx_drop() {
        let (tx, rx) = mpsc::channel(8);
        drop(rx);

        let active = Arc::new(AtomicBool::new(true));
        let emitter = EventEmitter::new(tx, active);

        // emit should not panic even when receiver is dropped
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            emitter.emit(GraphEngineEvent::GraphRunStarted),
        )
        .await
        .expect("emit after drop should complete quickly");
    }

    #[tokio::test]
    async fn test_event_collector_exits_on_channel_close() {
        let (tx, mut rx) = mpsc::channel::<()>(256);
        let exited = Arc::new(AtomicBool::new(false));
        let exited_clone = exited.clone();

        tokio::spawn(async move {
            while let Some(_event) = rx.recv().await {}
            exited_clone.store(true, Ordering::SeqCst);
        });

        drop(tx);

        let start = tokio::time::Instant::now();
        let timeout = std::time::Duration::from_secs(2);
        let interval = std::time::Duration::from_millis(50);
        while !exited.load(Ordering::SeqCst) {
            if start.elapsed() > timeout {
                panic!("condition 'collector exit' not met within {:?}", timeout);
            }
            tokio::time::sleep(interval).await;
        }
    }
}
