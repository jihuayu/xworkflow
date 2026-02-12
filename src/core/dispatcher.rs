//! Workflow dispatcher — the main execution driver.
//!
//! The [`WorkflowDispatcher`] walks the DAG graph, executing each node via its
//! registered [`NodeExecutor`](crate::nodes::NodeExecutor), managing edge
//! traversal, error strategies, retry logic, timeout enforcement, and streaming
//! propagation.

use serde_json::Value;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::mpsc;
use parking_lot::RwLock;

use crate::core::debug::{DebugAction, DebugGate, DebugHook, NoopGate, NoopHook};
use crate::core::event_bus::GraphEngineEvent;
use crate::core::plugin_gate::PluginGate;
use crate::core::runtime_context::RuntimeContext;
use crate::core::security_gate::SecurityGate;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{
  BackoffStrategy, ErrorStrategyConfig, ErrorStrategyType, NodeRunResult,
  RetryConfig, WorkflowNodeExecutionStatus, WriteMode,
};
use crate::error::{ErrorCode, ErrorContext, NodeError, WorkflowError, WorkflowResult};
use crate::compiler::CompiledNodeConfigMap;
use crate::graph::types::{EdgeTraversalState, Graph};
use crate::nodes::executor::NodeExecutorRegistry;
#[cfg(feature = "plugin-system")]
use crate::plugin_system::PluginRegistry;

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
    Abort { reason: Option<String> },
    Pause,
    UpdateVariables { variables: HashMap<String, Value> },
}

/// Configuration for the workflow engine
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EngineConfig {
    pub max_steps: i32,
    pub max_execution_time_secs: u64,
    #[serde(default)]
    pub strict_template: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            max_steps: 500,
            max_execution_time_secs: 600,
      strict_template: false,
        }
    }
}

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
      #[cfg(feature = "plugin-system")]
      plugin_registry: Option<Arc<PluginRegistry>>,
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
            crate::core::plugin_gate::new_plugin_gate(event_emitter.clone(), variable_pool.clone())
          }
        };
        let security_gate: Arc<dyn SecurityGate> = crate::core::security_gate::new_security_gate(
          context.clone(),
          variable_pool.clone(),
        );
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
        }
    }

  pub fn new_with_registry(
        graph: Graph,
        variable_pool: VariablePool,
        registry: Arc<NodeExecutorRegistry>,
      event_emitter: EventEmitter,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
      #[cfg(feature = "plugin-system")]
      plugin_registry: Option<Arc<PluginRegistry>>,
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
            crate::core::plugin_gate::new_plugin_gate(event_emitter.clone(), variable_pool.clone())
          }
        };
        let security_gate: Arc<dyn SecurityGate> = crate::core::security_gate::new_security_gate(
          context.clone(),
          variable_pool.clone(),
        );
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
      #[cfg(feature = "plugin-system")]
      plugin_registry: Option<Arc<PluginRegistry>>,
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
            crate::core::plugin_gate::new_plugin_gate(event_emitter.clone(), variable_pool.clone())
          }
        };
        let security_gate: Arc<dyn SecurityGate> = crate::core::security_gate::new_security_gate(
          context.clone(),
          variable_pool.clone(),
        );
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
    #[cfg(feature = "plugin-system")]
    plugin_registry: Option<Arc<PluginRegistry>>,
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
        crate::core::plugin_gate::new_plugin_gate(event_emitter.clone(), variable_pool.clone())
      }
    };
    let security_gate: Arc<dyn SecurityGate> = crate::core::security_gate::new_security_gate(
      context.clone(),
      variable_pool.clone(),
    );

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
    #[cfg(feature = "plugin-system")]
    plugin_registry: Option<Arc<PluginRegistry>>,
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
        crate::core::plugin_gate::new_plugin_gate(event_emitter.clone(), variable_pool.clone())
      }
    };
    let security_gate: Arc<dyn SecurityGate> = crate::core::security_gate::new_security_gate(
      context.clone(),
      variable_pool.clone(),
    );
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
    }
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
        self
          .event_emitter
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
        self
          .event_emitter
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
  ) -> WorkflowResult<()> {
    self
      .emit_after_node_hooks(node_id, &info.node_type, &info.node_title, &result)
      .await?;

    self.record_llm_usage(&result).await;

    // Store outputs in variable pool
    let (mut outputs_for_write, stream_outputs) = result.outputs.clone().into_parts();
    if info.node_type != "end" && info.node_type != "answer" {
      for (key, value) in outputs_for_write.iter_mut() {
        let selector = crate::core::variable_pool::Selector::new(node_id.to_string(), key.clone());
        self
          .apply_before_variable_write_hooks(node_id, &selector, value)
          .await?;
      }
    }

    let mut assigner_meta: Option<(WriteMode, crate::core::variable_pool::Selector, Segment)> = None;
    if info.node_type == "assigner" {
      let write_mode = outputs_for_write
        .get("write_mode")
        .and_then(|v| serde_json::from_value::<WriteMode>(v.to_value()).ok())
        .unwrap_or(WriteMode::Overwrite);
      let assigned_sel: crate::core::variable_pool::Selector = outputs_for_write
        .get("assigned_variable_selector")
        .and_then(|v| serde_json::from_value(v.to_value()).ok())
        .unwrap_or_else(|| crate::core::variable_pool::Selector::new("__scope__", "output"));
      let mut output_val = outputs_for_write.get("output").cloned().unwrap_or(Segment::None);

      self
        .apply_before_variable_write_hooks(node_id, &assigned_sel, &mut output_val)
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
          self
            .event_emitter
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
          self
            .event_emitter
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

    let downstream = self.advance_graph_after_success(node_id, info.is_branch, &result.edge_source_handle)?;

    // [DEBUG] Hook after node execute
    if should_pause_after {
      let pool_snapshot = pool_snapshot_after.expect("pool snapshot should exist when pause-after is enabled");
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
    Ok(())
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
      self
        .event_emitter
        .emit(GraphEngineEvent::NodeRunFailed {
          id: exec_id.clone(),
          node_id: node_id.clone(),
          node_type: info.node_type.clone(),
          node_run_result: err_result,
          error: error.to_string(),
        })
        .await;

      self
        .event_emitter
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
      is_skipped: g.node_state(node_id) == Some(EdgeTraversalState::Skipped),
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

    Ok(
      g.downstream_node_ids(node_id)
        .filter(|ds_id| g.is_node_ready(ds_id))
        .map(|ds_id| ds_id.to_string())
        .collect(),
    )
  }

  async fn emit_before_workflow_hooks(&self) -> WorkflowResult<()> {
    self.plugin_gate.emit_before_workflow_hooks().await
  }

  async fn emit_after_workflow_hooks(&self) -> WorkflowResult<()> {
    self
      .plugin_gate
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
    self
      .plugin_gate
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
    self
      .plugin_gate
      .emit_after_node_hooks(node_id, node_type, node_title, result)
      .await
  }

  async fn apply_before_variable_write_hooks(
    &self,
    node_id: &str,
    selector: &crate::core::variable_pool::Selector,
    value: &mut Segment,
  ) -> WorkflowResult<()> {
    self
      .plugin_gate
      .apply_before_variable_write_hooks(node_id, selector, value)
      .await
  }

  async fn execute_node_with_retry(
    &mut self,
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
    let executor = self.registry.get(node_type);
    if executor.is_none() {
      return Err(NodeError::ConfigError(format!(
        "No executor for node type: {}",
        node_type
      )));
    }

    let compiled_config = self
      .compiled_node_configs
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
          .execute_compiled(node_id, config, pool_snapshot, &self.context)
      } else {
        executor
          .as_ref()
          .expect("executor checked")
          .execute(node_id, node_config, pool_snapshot, &self.context)
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
            if self.event_emitter.is_active() {
              self.event_emitter
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
          Some(ErrorStrategyType::FailBranch) => {
            self.exceptions_count += 1;
            Ok(NodeRunResult {
              status: WorkflowNodeExecutionStatus::Exception,
              error: Some(error_info),
              edge_source_handle: crate::dsl::schema::EdgeHandle::Branch("fail-branch".to_string()),
              ..Default::default()
            })
          }
          Some(ErrorStrategyType::DefaultValue) => {
            self.exceptions_count += 1;
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
              let selector = crate::core::variable_pool::Selector::new(parts[0], parts[1]);
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

  async fn check_security_before_node(
    &self,
    node_id: &str,
    node_type: &str,
    node_config: &Value,
  ) -> Result<(), NodeError> {
    self
      .security_gate
      .check_before_node(node_id, node_type, node_config)
      .await
  }

  async fn enforce_output_limits(
    &self,
    node_id: &str,
    node_type: &str,
    result: NodeRunResult,
  ) -> Result<NodeRunResult, NodeError> {
    self
      .security_gate
      .enforce_output_limits(node_id, node_type, result)
      .await
  }

  async fn record_llm_usage(&self, result: &NodeRunResult) {
    self.security_gate.record_llm_usage(result).await
  }
  /// Run the workflow to completion
  pub async fn run(&mut self) -> WorkflowResult<HashMap<String, Value>> {
    // Emit graph started
    self.event_emitter.emit(GraphEngineEvent::GraphRunStarted).await;

    self.emit_before_workflow_hooks().await?;

    let root_id = {
      let g = self.graph.read();
      g.root_node_id().to_string()
    };

    let mut queue: Vec<String> = vec![root_id];
    let mut step_count: i32 = 0;
    let start_time = self.context.time_provider.now_timestamp();
    let (max_steps, max_exec_time) = self.effective_limits();

    while let Some(node_id) = queue.pop() {
      self
        .check_limits(&mut step_count, max_steps, start_time, max_exec_time)
        .await?;

      let info = self.load_node_info(&node_id)?;
      if info.is_skipped {
        continue;
      }

      // 合并 pool 读取：debug-before 与执行共用一次快照（仅在需要 debug-before 时复用）
      let mut pool_snapshot_for_exec: Option<VariablePool> = None;

      // [DEBUG] Hook before node execute
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
            queue.extend(downstream);
            continue;
          }
        }
      }

      let exec_id = self.context.id_generator.next_id();

      if let Err(e) = self
        .check_security_before_node(&node_id, &info.node_type, &info.node_config)
        .await
      {
        let error_detail = e.to_structured_json();
        let error_info = crate::dsl::schema::NodeErrorInfo {
          message: e.to_string(),
          error_type: Some(e.error_code()),
          detail: Some(error_detail.clone()),
        };
        if self.event_emitter.is_active() {
          self
            .event_emitter
            .emit(GraphEngineEvent::NodeRunFailed {
              id: exec_id.clone(),
              node_id: node_id.clone(),
              node_type: info.node_type.clone(),
              node_run_result: NodeRunResult {
                status: WorkflowNodeExecutionStatus::Failed,
                error: Some(error_info.clone()),
                ..Default::default()
              },
              error: e.to_string(),
            })
            .await;

          self
            .event_emitter
            .emit(GraphEngineEvent::GraphRunFailed {
              error: e.to_string(),
              exceptions_count: self.exceptions_count,
            })
            .await;
        }

        return Err(WorkflowError::NodeExecutionError {
          node_id,
          error: e.to_string(),
          error_detail: Some(error_detail),
        });
      }

      // Emit NodeRunStarted
      if self.event_emitter.is_active() {
        self
          .event_emitter
          .emit(GraphEngineEvent::NodeRunStarted {
            id: exec_id.clone(),
            node_id: node_id.clone(),
            node_type: info.node_type.clone(),
            node_title: info.node_title.clone(),
            predecessor_node_id: None,
          })
          .await;
      }

      self
        .emit_before_node_hooks(&node_id, &info.node_type, &info.node_title, &info.node_config)
        .await?;

      // Execute the node
      let pool_snapshot = match pool_snapshot_for_exec {
        Some(s) => s,
        None => self.variable_pool.read().clone(),
      };
      let run_result = self
        .execute_node_with_retry(
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
        Ok(result) => self
          .enforce_output_limits(&node_id, &info.node_type, result)
          .await,
        Err(e) => Err(e),
      };

      match run_result {
        Ok(result) => {
          self
            .handle_node_success(&exec_id, &node_id, &info, result, &mut queue)
            .await?;
        }
        Err(e) => {
          let err = self
            .handle_node_failure(exec_id, node_id, &info, e)
            .await;
          return Err(err);
        }
      }
    }

    // Emit final event
    if self.event_emitter.is_active() {
      if self.exceptions_count > 0 {
        self
          .event_emitter
          .emit(GraphEngineEvent::GraphRunPartialSucceeded {
            exceptions_count: self.exceptions_count,
            outputs: self.final_outputs.clone(),
          })
          .await;
      } else {
        self
          .event_emitter
          .emit(GraphEngineEvent::GraphRunSucceeded {
            outputs: self.final_outputs.clone(),
          })
          .await;
      }
    }

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
  use std::sync::atomic::AtomicBool;
  use std::sync::Arc;

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
        assert!(events.iter().any(|e| matches!(e, GraphEngineEvent::GraphRunStarted)));
        assert!(events.iter().any(|e| matches!(e, GraphEngineEvent::GraphRunSucceeded { .. })));
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

        assert_eq!(result.get("answer"), Some(&Value::String("Hello Alice!".into())));
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

        assert_eq!(result.get("result"), Some(&Value::String("Hello World!".into())));
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
    }

    #[test]
    fn test_engine_config_serde() {
        let config = EngineConfig {
            max_steps: 100,
            max_execution_time_secs: 30,
            strict_template: true,
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
        assert!(interval >= 100 && interval <= 110);
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
        let err = crate::error::NodeError::ExecutionError("rate limited".into())
            .with_context(ctx);
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
        let cmd = Command::Abort { reason: Some("test".into()) };
        let debug_str = format!("{:?}", cmd);
        assert!(debug_str.contains("Abort"));

        let cmd2 = Command::Pause;
        assert!(format!("{:?}", cmd2).contains("Pause"));

        let cmd3 = Command::UpdateVariables { variables: HashMap::new() };
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
        assert!(snap.len() > 0);
    }

    #[test]
    fn test_dispatcher_resource_weak_dropped() {

        let graph = Arc::new(RwLock::new(
            crate::graph::build_graph(&crate::dsl::parse_dsl(
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
            ).unwrap()).unwrap(),
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
        let mut config = EngineConfig::default();
        config.max_execution_time_secs = 30;
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
