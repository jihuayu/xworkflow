use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::core::debug::{DebugConfig, DebugHandle, InteractiveDebugGate, InteractiveDebugHook, StepMode};
use crate::core::dispatcher::{EngineConfig, WorkflowDispatcher};
use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{ErrorHandlingMode, WorkflowSchema};
use crate::dsl::validation::{validate_schema, ValidationReport};
use crate::error::WorkflowError;
use crate::graph::build_graph;
use crate::nodes::executor::NodeExecutorRegistry;
use crate::nodes::subgraph::SubGraphExecutor;
use crate::plugin::PluginManager;
use crate::llm::LlmProviderRegistry;

/// Execution status of a workflow
#[derive(Debug, Clone)]
pub enum ExecutionStatus {
    Running,
    Completed(HashMap<String, Value>),
    Failed(String),
  FailedWithRecovery {
    original_error: String,
    recovered_outputs: HashMap<String, Value>,
  },
}

/// Handle to a running or completed workflow
pub struct WorkflowHandle {
    status: Arc<Mutex<ExecutionStatus>>,
    events: Arc<Mutex<Vec<GraphEngineEvent>>>,
}

impl WorkflowHandle {
    pub async fn status(&self) -> ExecutionStatus {
        self.status.lock().await.clone()
    }

    pub async fn events(&self) -> Vec<GraphEngineEvent> {
        self.events.lock().await.clone()
    }

    pub async fn wait(&self) -> ExecutionStatus {
        loop {
            let status = self.status.lock().await.clone();
            match &status {
                ExecutionStatus::Running => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                _ => return status,
            }
        }
    }
}

/// Workflow runner with builder-based configuration
#[allow(dead_code)]
pub struct WorkflowRunner {
  schema: WorkflowSchema,
  user_inputs: HashMap<String, Value>,
  system_vars: HashMap<String, Value>,
  environment_vars: HashMap<String, Value>,
  conversation_vars: HashMap<String, Value>,
  config: EngineConfig,
  context: RuntimeContext,
  plugin_manager: Option<Arc<PluginManager>>,
  llm_provider_registry: Option<Arc<LlmProviderRegistry>>,
}

impl WorkflowRunner {
  pub fn builder(schema: WorkflowSchema) -> WorkflowRunnerBuilder {
    WorkflowRunnerBuilder {
      schema,
      user_inputs: HashMap::new(),
      system_vars: HashMap::new(),
      environment_vars: HashMap::new(),
      conversation_vars: HashMap::new(),
      config: EngineConfig::default(),
      context: RuntimeContext::default(),
      plugin_manager: None,
      llm_provider_registry: None,
      debug_config: None,
    }
  }
}

fn build_error_context(
  error: &WorkflowError,
  schema: &WorkflowSchema,
  partial_outputs: &HashMap<String, Value>,
) -> HashMap<String, Value> {
  let (node_id, node_type) = extract_error_node_info(error, schema);
  let mut ctx = HashMap::new();
  ctx.insert(
    "sys.error_message".to_string(),
    Value::String(error.to_string()),
  );
  ctx.insert(
    "sys.error_node_id".to_string(),
    Value::String(node_id.unwrap_or_default()),
  );
  ctx.insert(
    "sys.error_node_type".to_string(),
    Value::String(node_type.unwrap_or_default()),
  );
  ctx.insert(
    "sys.error_type".to_string(),
    Value::String(error_type_name(error).to_string()),
  );
  ctx.insert(
    "sys.workflow_outputs".to_string(),
    Value::Object(partial_outputs.clone().into_iter().collect()),
  );
  ctx
}

fn error_type_name(error: &WorkflowError) -> &'static str {
  match error {
    WorkflowError::NodeExecutionError { .. } => "NodeExecutionError",
    WorkflowError::Timeout | WorkflowError::ExecutionTimeout => "Timeout",
    WorkflowError::MaxStepsExceeded(_) => "MaxStepsExceeded",
    WorkflowError::Aborted(_) => "Aborted",
    _ => "InternalError",
  }
}

fn extract_error_node_info(
  error: &WorkflowError,
  schema: &WorkflowSchema,
) -> (Option<String>, Option<String>) {
  match error {
    WorkflowError::NodeExecutionError { node_id, .. } => {
      let node_type = schema
        .nodes
        .iter()
        .find(|n| n.id == *node_id)
        .map(|n| n.data.node_type.clone());
      (Some(node_id.clone()), node_type)
    }
    _ => (None, None),
  }
}

pub struct WorkflowRunnerBuilder {
  schema: WorkflowSchema,
  user_inputs: HashMap<String, Value>,
  system_vars: HashMap<String, Value>,
  environment_vars: HashMap<String, Value>,
  conversation_vars: HashMap<String, Value>,
  config: EngineConfig,
  context: RuntimeContext,
  plugin_manager: Option<Arc<PluginManager>>,
  llm_provider_registry: Option<Arc<LlmProviderRegistry>>,
  debug_config: Option<DebugConfig>,
}

impl WorkflowRunnerBuilder {
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

  pub fn config(mut self, config: EngineConfig) -> Self {
    self.config = config;
    self
  }

  pub fn context(mut self, context: RuntimeContext) -> Self {
    self.context = context;
    self
  }

  pub fn plugin_manager(mut self, plugin_manager: Arc<PluginManager>) -> Self {
    self.plugin_manager = Some(plugin_manager);
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
    validate_schema(&self.schema)
  }

  pub async fn run(self) -> Result<WorkflowHandle, WorkflowError> {
    let report = validate_schema(&self.schema);
    if !report.is_valid {
      return Err(WorkflowError::ValidationFailed(report));
    }
    let graph = build_graph(&self.schema)?;

    // Build variable pool
    let mut pool = VariablePool::new();

    // Set system variables
    for (k, v) in &self.system_vars {
      pool.set(&["sys".to_string(), k.clone()], Segment::from_value(v));
    }

    // Set environment variables
    for (k, v) in &self.environment_vars {
      pool.set(&["env".to_string(), k.clone()], Segment::from_value(v));
    }

    // Set conversation variables
    for (k, v) in &self.conversation_vars {
      pool.set(
        &["conversation".to_string(), k.clone()],
        Segment::from_value(v),
      );
    }

    // Set user inputs mapped to start node
    let start_node_id = graph.root_node_id.clone();
    for (k, v) in &self.user_inputs {
      pool.set(
        &[start_node_id.clone(), k.clone()],
        Segment::from_value(v),
      );
    }

    let mut registry = if let Some(pm) = &self.plugin_manager {
      NodeExecutorRegistry::new_with_plugins(pm.clone())
    } else {
      NodeExecutorRegistry::new()
    };
    if let Some(llm_reg) = &self.llm_provider_registry {
      registry.set_llm_provider_registry(llm_reg.clone());
    }
    let (tx, mut rx) = mpsc::channel(256);
    let config = self.config;
    let context = Arc::new(self.context.with_event_tx(tx.clone()));

    let status = Arc::new(Mutex::new(ExecutionStatus::Running));
    let events = Arc::new(Mutex::new(Vec::new()));

    let events_clone = events.clone();

    // Spawn event collector
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        events_clone.lock().await.push(event);
      }
    });

    // Spawn workflow execution
    let status_exec = status.clone();
    let plugin_manager = self.plugin_manager.clone();
    let schema = self.schema.clone();
    tokio::spawn(async move {
      let mut dispatcher = WorkflowDispatcher::new(
        graph,
        pool,
        registry,
        tx.clone(),
        config,
        context.clone(),
        plugin_manager,
      );
      match dispatcher.run().await {
        Ok(outputs) => {
          *status_exec.lock().await = ExecutionStatus::Completed(outputs);
        }
        Err(e) => {
          if let Some(error_handler) = &schema.error_handler {
            let partial_outputs = dispatcher.partial_outputs();
            let pool_snapshot = dispatcher.snapshot_pool().await;
            let error_context = build_error_context(&e, &schema, &partial_outputs);

            let _ = tx
              .send(GraphEngineEvent::ErrorHandlerStarted {
                error: e.to_string(),
              })
              .await;

            let executor = SubGraphExecutor::new();
            match executor
              .execute(
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
                  .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                  .unwrap_or_default();

                let _ = tx
                  .send(GraphEngineEvent::ErrorHandlerSucceeded {
                    outputs: recovered_outputs.clone(),
                  })
                  .await;

                match error_handler.mode {
                  ErrorHandlingMode::Recover => {
                    *status_exec.lock().await = ExecutionStatus::FailedWithRecovery {
                      original_error: e.to_string(),
                      recovered_outputs,
                    };
                  }
                  ErrorHandlingMode::Notify => {
                    *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
                  }
                }
              }
              Err(handler_err) => {
                let _ = tx
                  .send(GraphEngineEvent::ErrorHandlerFailed {
                    error: handler_err.to_string(),
                  })
                  .await;
                *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
              }
            }
          } else {
            *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
          }
        }
      }
    });

    Ok(WorkflowHandle { status, events })
  }

  pub async fn run_debug(self) -> Result<(WorkflowHandle, DebugHandle), WorkflowError> {
    let debug_config = self.debug_config.clone().unwrap_or_default();
    let report = validate_schema(&self.schema);
    if !report.is_valid {
      return Err(WorkflowError::ValidationFailed(report));
    }
    let graph = build_graph(&self.schema)?;

    let mut pool = VariablePool::new();

    for (k, v) in &self.system_vars {
      pool.set(&["sys".to_string(), k.clone()], Segment::from_value(v));
    }

    for (k, v) in &self.environment_vars {
      pool.set(&["env".to_string(), k.clone()], Segment::from_value(v));
    }

    for (k, v) in &self.conversation_vars {
      pool.set(
        &["conversation".to_string(), k.clone()],
        Segment::from_value(v),
      );
    }

    let start_node_id = graph.root_node_id.clone();
    for (k, v) in &self.user_inputs {
      pool.set(
        &[start_node_id.clone(), k.clone()],
        Segment::from_value(v),
      );
    }

    let mut registry = if let Some(pm) = &self.plugin_manager {
      NodeExecutorRegistry::new_with_plugins(pm.clone())
    } else {
      NodeExecutorRegistry::new()
    };
    if let Some(llm_reg) = &self.llm_provider_registry {
      registry.set_llm_provider_registry(llm_reg.clone());
    }

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
    let config = self.config;
    let context = Arc::new(self.context.with_event_tx(tx.clone()));

    let status = Arc::new(Mutex::new(ExecutionStatus::Running));
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        events_clone.lock().await.push(event);
      }
    });

    let status_exec = status.clone();
    let plugin_manager = self.plugin_manager.clone();
    let schema = self.schema.clone();
    let mut hook = hook;
    hook.graph_event_tx = Some(tx.clone());

    tokio::spawn(async move {
      let mut dispatcher = WorkflowDispatcher::new_with_debug(
        graph,
        pool,
        registry,
        tx.clone(),
        config,
        context.clone(),
        plugin_manager,
        gate,
        hook,
      );
      match dispatcher.run().await {
        Ok(outputs) => {
          *status_exec.lock().await = ExecutionStatus::Completed(outputs);
        }
        Err(e) => {
          if let Some(error_handler) = &schema.error_handler {
            let partial_outputs = dispatcher.partial_outputs();
            let pool_snapshot = dispatcher.snapshot_pool().await;
            let error_context = build_error_context(&e, &schema, &partial_outputs);

            let _ = tx
              .send(GraphEngineEvent::ErrorHandlerStarted {
                error: e.to_string(),
              })
              .await;

            let executor = SubGraphExecutor::new();
            match executor
              .execute(
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
                  .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                  .unwrap_or_default();

                let _ = tx
                  .send(GraphEngineEvent::ErrorHandlerSucceeded {
                    outputs: recovered_outputs.clone(),
                  })
                  .await;

                match error_handler.mode {
                  ErrorHandlingMode::Recover => {
                    *status_exec.lock().await = ExecutionStatus::FailedWithRecovery {
                      original_error: e.to_string(),
                      recovered_outputs,
                    };
                  }
                  ErrorHandlingMode::Notify => {
                    *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
                  }
                }
              }
              Err(handler_err) => {
                let _ = tx
                  .send(GraphEngineEvent::ErrorHandlerFailed {
                    error: handler_err.to_string(),
                  })
                  .await;
                *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
              }
            }
          } else {
            *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
          }
        }
      }
    });

    let workflow_handle = WorkflowHandle { status, events };
    let debug_handle = DebugHandle::new(cmd_tx, debug_evt_rx);

    Ok((workflow_handle, debug_handle))
  }
}

#[cfg(test)]
mod tests {
    use super::*;
  use crate::core::debug::{DebugConfig, DebugEvent, PauseLocation, PauseReason as DebugPauseReason};
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
        - case_id: yes
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
    sourceHandle: yes
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

        let mut dbg = DebugConfig::default();
        dbg.break_on_start = true;
        dbg.auto_snapshot = true;

        let (handle, debug) = WorkflowRunner::builder(schema)
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
        assert!(snapshot.contains_key(&("start".to_string(), "query".to_string())));

        debug.continue_run().await.unwrap();

        let status = handle.wait().await;
        match status {
            ExecutionStatus::Completed(outputs) => {
                assert_eq!(outputs.get("result"), Some(&Value::String("test".into())));
            }
            other => panic!("Expected Completed, got {:?}", other),
        }
    }
}
