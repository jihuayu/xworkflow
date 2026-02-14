use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use xworkflow::core::dispatcher::{EngineConfig, EventEmitter, WorkflowDispatcher};
use xworkflow::core::variable_pool::VariablePool;
use xworkflow::dsl::schema::NodeRunResult;
use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::error::{NodeError, WorkflowError};
use xworkflow::graph::build_graph;
use xworkflow::nodes::executor::NodeExecutor;
use xworkflow::nodes::NodeExecutorRegistry;
use xworkflow::{RuntimeContext, TimeProvider};

const SIMPLE_WORKFLOW_YAML: &str = r#"
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

const ERROR_WORKFLOW_YAML: &str = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: err
    data:
      type: error
      title: Error
  - id: end
    data:
      type: end
      title: End
      outputs: []
edges:
  - source: start
    target: err
  - source: err
    target: end
"#;

struct ErrorNodeExecutor;

#[async_trait]
impl NodeExecutor for ErrorNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        _config: &Value,
        _variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        Err(NodeError::ExecutionError("forced error".into()))
    }
}

struct FastForwardTimeProvider;

impl TimeProvider for FastForwardTimeProvider {
    fn now_timestamp(&self) -> i64 {
        0
    }

    fn now_millis(&self) -> i64 {
        0
    }

    fn elapsed_secs(&self, _since: i64) -> u64 {
        9999
    }
}

fn make_emitter() -> EventEmitter {
    let (tx, _rx) = mpsc::channel(8);
    let active = Arc::new(std::sync::atomic::AtomicBool::new(false));
    EventEmitter::new(tx, active)
}

#[tokio::test]
async fn test_dispatcher_arc_release_after_run() {
    let schema = parse_dsl(SIMPLE_WORKFLOW_YAML, DslFormat::Yaml).expect("parse schema");
    let graph = build_graph(&schema).expect("build graph");
    let registry = Arc::new(NodeExecutorRegistry::new());
    let context = Arc::new(RuntimeContext::default());
    let pool = VariablePool::new();
    let emitter = make_emitter();

    let mut dispatcher = WorkflowDispatcher::new_with_registry(
        graph,
        pool,
        Arc::clone(&registry),
        emitter,
        EngineConfig::default(),
        Arc::clone(&context),
        #[cfg(feature = "plugin-system")]
        None,
    );

    let weak = dispatcher.debug_resources();
    let result = dispatcher.run().await;
    assert!(result.is_ok());

    drop(dispatcher);
    drop(registry);
    drop(context);

    assert!(weak.graph_dropped());
    assert!(weak.pool_dropped());
    assert!(weak.registry_dropped());
    assert!(weak.context_dropped());
}

#[tokio::test]
async fn test_dispatcher_cleanup_on_error() {
    let schema = parse_dsl(ERROR_WORKFLOW_YAML, DslFormat::Yaml).expect("parse schema");
    let graph = build_graph(&schema).expect("build graph");
    let mut registry = NodeExecutorRegistry::new();
    registry.register("error", Box::new(ErrorNodeExecutor));
    let registry = Arc::new(registry);
    let context = Arc::new(RuntimeContext::default());
    let pool = VariablePool::new();
    let emitter = make_emitter();

    let mut dispatcher = WorkflowDispatcher::new_with_registry(
        graph,
        pool,
        Arc::clone(&registry),
        emitter,
        EngineConfig::default(),
        Arc::clone(&context),
        #[cfg(feature = "plugin-system")]
        None,
    );

    let weak = dispatcher.debug_resources();
    let result = dispatcher.run().await;
    assert!(matches!(
        result,
        Err(WorkflowError::NodeExecutionError { .. })
    ));

    drop(dispatcher);
    drop(registry);
    drop(context);

    assert!(weak.graph_dropped());
    assert!(weak.pool_dropped());
    assert!(weak.registry_dropped());
    assert!(weak.context_dropped());
}

#[tokio::test]
async fn test_dispatcher_cleanup_on_timeout() {
    let schema = parse_dsl(SIMPLE_WORKFLOW_YAML, DslFormat::Yaml).expect("parse schema");
    let graph = build_graph(&schema).expect("build graph");
    let registry = Arc::new(NodeExecutorRegistry::new());
    let context = Arc::new(RuntimeContext {
        time_provider: Arc::new(FastForwardTimeProvider),
        ..RuntimeContext::default()
    });
    let pool = VariablePool::new();
    let emitter = make_emitter();

    let config = EngineConfig {
        max_execution_time_secs: 0,
        ..EngineConfig::default()
    };

    let mut dispatcher = WorkflowDispatcher::new_with_registry(
        graph,
        pool,
        Arc::clone(&registry),
        emitter,
        config,
        Arc::clone(&context),
        #[cfg(feature = "plugin-system")]
        None,
    );

    let weak = dispatcher.debug_resources();
    let result = dispatcher.run().await;
    assert!(matches!(result, Err(WorkflowError::ExecutionTimeout)));

    drop(dispatcher);
    drop(registry);
    drop(context);

    assert!(weak.graph_dropped());
    assert!(weak.pool_dropped());
    assert!(weak.registry_dropped());
    assert!(weak.context_dropped());
}
