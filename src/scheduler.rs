use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use crate::core::dispatcher::{EngineConfig, WorkflowDispatcher};
use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::WorkflowSchema;
use crate::dsl::validator::validate_workflow_schema;
use crate::error::WorkflowError;
use crate::graph::build_graph;
use crate::nodes::executor::NodeExecutorRegistry;
use crate::plugin::PluginManager;

/// Execution status of a workflow
#[derive(Debug, Clone)]
pub enum ExecutionStatus {
    Running,
    Completed(HashMap<String, Value>),
    Failed(String),
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
    }
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

  pub async fn run(self) -> Result<WorkflowHandle, WorkflowError> {
    validate_workflow_schema(&self.schema)?;
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

    let registry = if let Some(pm) = &self.plugin_manager {
      NodeExecutorRegistry::new_with_plugins(pm.clone())
    } else {
      NodeExecutorRegistry::new()
    };
    let (tx, mut rx) = mpsc::channel(256);
    let config = self.config;
    let context = Arc::new(self.context);

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
    tokio::spawn(async move {
      let mut dispatcher = WorkflowDispatcher::new(
        graph,
        pool,
        registry,
        tx,
        config,
        context,
        plugin_manager,
      );
      match dispatcher.run().await {
        Ok(outputs) => {
          *status_exec.lock().await = ExecutionStatus::Completed(outputs);
        }
        Err(e) => {
          *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
        }
      }
    });

    Ok(WorkflowHandle { status, events })
  }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
