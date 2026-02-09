use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{
  BackoffStrategy, ErrorStrategyConfig, ErrorStrategyType, NodeRunResult,
  RetryConfig, WorkflowNodeExecutionStatus, WriteMode,
};
use crate::error::{ErrorCode, ErrorContext, NodeError, WorkflowError, WorkflowResult};
use crate::graph::types::{Graph, NodeState};
use crate::nodes::executor::NodeExecutorRegistry;
use crate::plugin::{PluginHookType, PluginManager};

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
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            max_steps: 500,
            max_execution_time_secs: 600,
        }
    }
}

/// The main workflow dispatcher: drives graph execution
pub struct WorkflowDispatcher {
    graph: Arc<RwLock<Graph>>,
    variable_pool: Arc<RwLock<VariablePool>>,
    registry: Arc<NodeExecutorRegistry>,
    event_tx: mpsc::Sender<GraphEngineEvent>,
    config: EngineConfig,
    exceptions_count: i32,
    final_outputs: HashMap<String, Value>,
    context: Arc<RuntimeContext>,
  plugin_manager: Option<Arc<PluginManager>>,
}

impl WorkflowDispatcher {
    pub fn new(
        graph: Graph,
        variable_pool: VariablePool,
        registry: NodeExecutorRegistry,
        event_tx: mpsc::Sender<GraphEngineEvent>,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
      plugin_manager: Option<Arc<PluginManager>>,
    ) -> Self {
        WorkflowDispatcher {
            graph: Arc::new(RwLock::new(graph)),
            variable_pool: Arc::new(RwLock::new(variable_pool)),
            registry: Arc::new(registry),
            event_tx,
            config,
            exceptions_count: 0,
            final_outputs: HashMap::new(),
            context,
        plugin_manager,
        }
    }

        pub fn partial_outputs(&self) -> HashMap<String, Value> {
          self.final_outputs.clone()
        }

        pub async fn snapshot_pool(&self) -> VariablePool {
          self.variable_pool.read().await.clone()
        }

    /// Run the workflow to completion
    pub async fn run(&mut self) -> WorkflowResult<HashMap<String, Value>> {
        // Emit graph started
        let _ = self.event_tx.send(GraphEngineEvent::GraphRunStarted).await;

      if let Some(pm) = &self.plugin_manager {
        let pool_snapshot = self.variable_pool.read().await.clone();
        let payload = serde_json::json!({
          "event": "before_workflow_run",
        });
        pm.execute_hooks(
          PluginHookType::BeforeWorkflowRun,
          &payload,
          Arc::new(std::sync::RwLock::new(pool_snapshot)),
          Some(self.event_tx.clone()),
        )
        .await
        .map_err(|e| WorkflowError::InternalError(e.to_string()))?;
      }

        let root_id = {
            let g = self.graph.read().await;
            g.root_node_id.clone()
        };

        let mut queue: Vec<String> = vec![root_id];
        let mut step_count: i32 = 0;
        let start_time = self.context.time_provider.now_timestamp();

        while let Some(node_id) = queue.pop() {
            // Check max steps
            step_count += 1;
            if step_count > self.config.max_steps {
                let _ = self.event_tx.send(GraphEngineEvent::GraphRunFailed {
                    error: format!("Max steps exceeded: {}", self.config.max_steps),
                    exceptions_count: self.exceptions_count,
                }).await;
                return Err(WorkflowError::MaxStepsExceeded(self.config.max_steps));
            }

            // Check max time
            if self.context.time_provider.elapsed_secs(start_time) > self.config.max_execution_time_secs {
                let _ = self.event_tx.send(GraphEngineEvent::GraphRunFailed {
                    error: "Max execution time exceeded".into(),
                    exceptions_count: self.exceptions_count,
                }).await;
                return Err(WorkflowError::ExecutionTimeout);
            }

            // Get node info
            let (node_type, node_title, node_config, is_branch) = {
                let g = self.graph.read().await;
                let node = g.get_node(&node_id)
                    .ok_or_else(|| WorkflowError::NodeNotFound(node_id.clone()))?;
                (
                    node.node_type.clone(),
                    node.title.clone(),
                    node.config.clone(),
                    g.is_branch_node(&node_id),
                )
            };

            // Check if node is skipped
            {
                let g = self.graph.read().await;
                if let Some(node) = g.get_node(&node_id) {
                    if node.state == NodeState::Skipped {
                        continue;
                    }
                }
            }

            let exec_id = self.context.id_generator.next_id();

            // Emit NodeRunStarted
            let _ = self.event_tx.send(GraphEngineEvent::NodeRunStarted {
                id: exec_id.clone(),
                node_id: node_id.clone(),
                node_type: node_type.clone(),
                node_title: node_title.clone(),
                predecessor_node_id: None,
            }).await;

            if let Some(pm) = &self.plugin_manager {
              let pool_snapshot = self.variable_pool.read().await.clone();
              let payload = serde_json::json!({
                "event": "before_node_execute",
                "node_id": node_id.clone(),
                "node_type": node_type.clone(),
                "node_title": node_title.clone(),
                "config": node_config.clone(),
              });
              pm.execute_hooks(
                PluginHookType::BeforeNodeExecute,
                &payload,
                Arc::new(std::sync::RwLock::new(pool_snapshot)),
                Some(self.event_tx.clone()),
              )
              .await
              .map_err(|e| WorkflowError::InternalError(e.to_string()))?;
            }

            // Execute the node
            let pool_snapshot = {
                self.variable_pool.read().await.clone()
            };

            let executor = self.registry.get(&node_type);
            let run_result = if let Some(exec) = executor {
                // Check error strategy & retry
                let error_strategy: Option<ErrorStrategyConfig> = node_config
                  .get("error_strategy")
                  .and_then(|v| serde_json::from_value(v.clone()).ok());
                let retry_config: Option<RetryConfig> = node_config
                  .get("retry_config")
                  .and_then(|v| serde_json::from_value(v.clone()).ok());
                let node_timeout = node_config.get("timeout_secs").and_then(|v| v.as_u64());

                let max_retries = retry_config.as_ref().map(|rc| rc.max_retries).unwrap_or(0).max(0);
                let retry_on_retryable_only = retry_config
                  .as_ref()
                  .map(|rc| rc.retry_on_retryable_only)
                  .unwrap_or(true);

                let mut last_error: Option<NodeError> = None;
                let mut result = None;

                for attempt in 0..=max_retries {
                  let exec_future = exec.execute(&node_id, &node_config, &pool_snapshot, &self.context);
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
                        let interval = calculate_retry_interval(&retry_config, attempt, &e);
                        let _ = self.event_tx.send(GraphEngineEvent::NodeRunRetry {
                          id: exec_id.clone(),
                          node_id: node_id.clone(),
                          node_type: node_type.clone(),
                          node_title: node_title.clone(),
                          error: e.to_string(),
                          retry_index: attempt + 1,
                        }).await;
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

                    match error_strategy.as_ref().map(|es| &es.strategy_type) {
                      Some(ErrorStrategyType::FailBranch) => {
                        self.exceptions_count += 1;
                        Ok(NodeRunResult {
                          status: WorkflowNodeExecutionStatus::Exception,
                          error: Some(last_err.to_string()),
                          error_type: Some(error_type),
                          error_detail: Some(error_detail),
                          edge_source_handle: "fail-branch".to_string(),
                          ..Default::default()
                        })
                      }
                      Some(ErrorStrategyType::DefaultValue) => {
                        self.exceptions_count += 1;
                        let defaults = error_strategy
                          .as_ref()
                          .and_then(|es| es.default_value.clone())
                          .unwrap_or_default();
                        Ok(NodeRunResult {
                          status: WorkflowNodeExecutionStatus::Exception,
                          outputs: defaults,
                          error: Some(last_err.to_string()),
                          error_type: Some(error_type),
                          error_detail: Some(error_detail),
                          edge_source_handle: "source".to_string(),
                          ..Default::default()
                        })
                      }
                      Some(ErrorStrategyType::None) | None => Err(last_err),
                    }
                  }
                }
            } else {
                Err(NodeError::ConfigError(format!(
                    "No executor for node type: {}",
                    node_type
                )))
            };

            match run_result {
                Ok(result) => {
                if let Some(pm) = &self.plugin_manager {
                  let pool_snapshot = self.variable_pool.read().await.clone();
                  let payload = serde_json::json!({
                    "event": "after_node_execute",
                    "node_id": node_id.clone(),
                    "node_type": node_type.clone(),
                    "node_title": node_title.clone(),
                    "status": format!("{:?}", result.status),
                    "outputs": result.outputs.clone(),
                    "error": result.error.clone(),
                  });
                  pm.execute_hooks(
                    PluginHookType::AfterNodeExecute,
                    &payload,
                    Arc::new(std::sync::RwLock::new(pool_snapshot)),
                    Some(self.event_tx.clone()),
                  )
                  .await
                  .map_err(|e| WorkflowError::InternalError(e.to_string()))?;
                }

                    // Store outputs in variable pool
                    let mut outputs_for_write = result.outputs.clone();
                    let stream_outputs = result.stream_outputs.clone();
                    if let Some(pm) = &self.plugin_manager {
                      let pool_snapshot = self.variable_pool.read().await.clone();
                      for (key, value) in outputs_for_write.iter_mut() {
                        let payload = serde_json::json!({
                          "event": "before_variable_write",
                          "node_id": node_id.clone(),
                          "selector": [node_id.clone(), key.clone()],
                          "value": value.clone(),
                        });
                        let results = pm
                          .execute_hooks(
                            PluginHookType::BeforeVariableWrite,
                            &payload,
                            Arc::new(std::sync::RwLock::new(pool_snapshot.clone())),
                            Some(self.event_tx.clone()),
                          )
                          .await
                          .map_err(|e| WorkflowError::InternalError(e.to_string()))?;
                        if let Some(last) = results.into_iter().rev().find(|v| !v.is_null()) {
                          *value = last;
                        }
                      }
                    }

                    let mut assigner_meta: Option<(WriteMode, Vec<String>, Value)> = None;
                    if node_type == "assigner" {
                      let write_mode = result.outputs.get("write_mode")
                        .and_then(|v| serde_json::from_value::<WriteMode>(v.clone()).ok())
                        .unwrap_or(WriteMode::Overwrite);
                      let assigned_sel: Vec<String> = result.outputs.get("assigned_variable_selector")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default();
                      let mut output_val = outputs_for_write
                        .get("output")
                        .cloned()
                        .unwrap_or(Value::Null);

                      if let Some(pm) = &self.plugin_manager {
                        let pool_snapshot = self.variable_pool.read().await.clone();
                        let payload = serde_json::json!({
                          "event": "before_variable_write",
                          "node_id": node_id.clone(),
                          "selector": assigned_sel.clone(),
                          "value": output_val.clone(),
                        });
                        let results = pm
                          .execute_hooks(
                            PluginHookType::BeforeVariableWrite,
                            &payload,
                            Arc::new(std::sync::RwLock::new(pool_snapshot)),
                            Some(self.event_tx.clone()),
                          )
                          .await
                          .map_err(|e| WorkflowError::InternalError(e.to_string()))?;
                        if let Some(last) = results.into_iter().rev().find(|v| !v.is_null()) {
                          output_val = last;
                        }
                      }

                      assigner_meta = Some((write_mode, assigned_sel, output_val));
                    }

                    {
                      let mut pool = self.variable_pool.write().await;

                      // Handle variable assigner specially
                      if let Some((write_mode, assigned_sel, output_val)) = assigner_meta {
                        match write_mode {
                          WriteMode::Overwrite => {
                            pool.set(&assigned_sel, Segment::from_value(&output_val));
                          }
                          WriteMode::Append => {
                            pool.append(&assigned_sel, Segment::from_value(&output_val));
                          }
                          WriteMode::Clear => {
                            pool.clear(&assigned_sel);
                          }
                        }
                      }

                      pool.set_node_outputs(&node_id, &outputs_for_write);
                      for (key, stream) in stream_outputs {
                        pool.set(&[node_id.clone(), key], Segment::Stream(stream));
                      }
                    }

                    // Track final outputs for end/answer nodes
                    if node_type == "end" || node_type == "answer" {
                      for (k, v) in &result.outputs {
                        self.final_outputs.insert(k.clone(), v.clone());
                      }
                    }

                    // Emit success or exception event
                    match result.status {
                        WorkflowNodeExecutionStatus::Exception => {
                            let _ = self.event_tx.send(GraphEngineEvent::NodeRunException {
                                id: exec_id.clone(),
                                node_id: node_id.clone(),
                                node_type: node_type.clone(),
                                node_run_result: result.clone(),
                                error: result.error.clone().unwrap_or_default(),
                            }).await;
                        }
                        _ => {
                            let _ = self.event_tx.send(GraphEngineEvent::NodeRunSucceeded {
                                id: exec_id.clone(),
                                node_id: node_id.clone(),
                                node_type: node_type.clone(),
                                node_run_result: result.clone(),
                            }).await;
                        }
                    }

                    // Mark node as Taken
                    {
                        let mut g = self.graph.write().await;
                        if let Some(node) = g.nodes.get_mut(&node_id) {
                            node.state = NodeState::Taken;
                        }
                    }

                    // Process edges
                    {
                        let mut g = self.graph.write().await;
                        if is_branch {
                            g.process_branch_edges(&node_id, &result.edge_source_handle);
                        } else {
                            g.process_normal_edges(&node_id);
                        }
                    }

                    // Find ready downstream nodes
                    {
                        let g = self.graph.read().await;
                        let downstream = g.get_downstream_node_ids(&node_id);
                        for ds_id in downstream {
                            if g.is_node_ready(&ds_id) {
                                queue.push(ds_id);
                            }
                        }
                    }
                }
                Err(e) => {
                    // Node execution failed, abort workflow
                  let error_type = e.error_code();
                  let error_detail = e.to_structured_json();
                    let err_result = NodeRunResult {
                        status: WorkflowNodeExecutionStatus::Failed,
                    error: Some(e.to_string()),
                    error_type: Some(error_type),
                    error_detail: Some(error_detail.clone()),
                        ..Default::default()
                    };

                    let _ = self.event_tx.send(GraphEngineEvent::NodeRunFailed {
                        id: exec_id,
                        node_id: node_id.clone(),
                        node_type: node_type.clone(),
                        node_run_result: err_result,
                        error: e.to_string(),
                    }).await;

                    let _ = self.event_tx.send(GraphEngineEvent::GraphRunFailed {
                        error: e.to_string(),
                        exceptions_count: self.exceptions_count,
                    }).await;

                    return Err(WorkflowError::NodeExecutionError {
                        node_id,
                        error: e.to_string(),
                      error_detail: Some(error_detail),
                    });
                }
            }
        }

        // Emit final event
        if self.exceptions_count > 0 {
          let _ = self.event_tx.send(GraphEngineEvent::GraphRunPartialSucceeded {
            exceptions_count: self.exceptions_count,
            outputs: self.final_outputs.clone(),
          }).await;
        } else {
          let _ = self.event_tx.send(GraphEngineEvent::GraphRunSucceeded {
            outputs: self.final_outputs.clone(),
          }).await;
        }

        if let Some(pm) = &self.plugin_manager {
          let pool_snapshot = self.variable_pool.read().await.clone();
          let payload = serde_json::json!({
            "event": "after_workflow_run",
            "outputs": self.final_outputs.clone(),
            "exceptions_count": self.exceptions_count,
          });
          pm.execute_hooks(
            PluginHookType::AfterWorkflowRun,
            &payload,
            Arc::new(std::sync::RwLock::new(pool_snapshot)),
            Some(self.event_tx.clone()),
          )
          .await
          .map_err(|e| WorkflowError::InternalError(e.to_string()))?;
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{parse_dsl, DslFormat};
    use crate::graph::build_graph;
  use std::sync::Arc;

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
            &["start_1".to_string(), "query".to_string()],
            Segment::String("hello".into()),
        );
        pool.set(
            &["sys".to_string(), "query".to_string()],
            Segment::String("hello".into()),
        );

        let registry = NodeExecutorRegistry::new();
        let (tx, mut rx) = mpsc::channel(100);

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
          graph,
          pool,
          registry,
          tx,
          EngineConfig::default(),
          context,
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
        pool.set(&["start".to_string(), "x".to_string()], Segment::Integer(10));

        let registry = NodeExecutorRegistry::new();
        let (tx, _rx) = mpsc::channel(100);

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
          graph,
          pool,
          registry,
          tx,
          EngineConfig::default(),
          context,
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
        pool.set(&["start".to_string(), "x".to_string()], Segment::Integer(3));

        let registry = NodeExecutorRegistry::new();
        let (tx, _rx) = mpsc::channel(100);

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
          graph,
          pool,
          registry,
          tx,
          EngineConfig::default(),
          context,
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
        pool.set(&["start".to_string(), "name".to_string()], Segment::String("Alice".into()));

        let registry = NodeExecutorRegistry::new();
        let (tx, _rx) = mpsc::channel(100);

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
          graph,
          pool,
          registry,
          tx,
          EngineConfig::default(),
          context,
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
        let (tx, _rx) = mpsc::channel(100);

        let config = EngineConfig { max_steps: 0, max_execution_time_secs: 600 };
        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
          graph,
          pool,
          registry,
          tx,
          config,
          context,
          None,
        );
        let result = dispatcher.run().await;
        assert!(result.is_err());
    }

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
        pool.set(&["start".to_string(), "name".to_string()], Segment::String("World".into()));

        let registry = NodeExecutorRegistry::new();
        let (tx, _rx) = mpsc::channel(100);

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
          graph,
          pool,
          registry,
          tx,
          EngineConfig::default(),
          context,
          None,
        );
        let result = dispatcher.run().await.unwrap();

        assert_eq!(result.get("result"), Some(&Value::String("Hello World!".into())));
    }

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
        pool.set(&["start".to_string(), "flag".to_string()], Segment::String("true".into()));

        let registry = NodeExecutorRegistry::new();
        let (tx, _rx) = mpsc::channel(100);

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
          graph,
          pool,
          registry,
          tx,
          EngineConfig::default(),
          context,
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
        let (tx, mut rx) = mpsc::channel(100);

        let context = Arc::new(RuntimeContext::default());
        let mut dispatcher = WorkflowDispatcher::new(
          graph,
          pool,
          registry,
          tx,
          EngineConfig::default(),
          context,
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
}
