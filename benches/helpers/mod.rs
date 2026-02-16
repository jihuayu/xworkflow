#![allow(dead_code)]

pub mod pool_factories;
pub mod workflow_builders;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde_json::Value;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use xworkflow::core::dispatcher::{EngineConfig, EventEmitter, WorkflowDispatcher};
use xworkflow::core::variable_pool::VariablePool;
use xworkflow::dsl::{parse_dsl, DslFormat, WorkflowSchema};
use xworkflow::graph::build_graph;
use xworkflow::graph::types::Graph;
use xworkflow::nodes::NodeExecutorRegistry;
use xworkflow::{FakeIdGenerator, FakeTimeProvider, RuntimeContext};

pub fn bench_context() -> RuntimeContext {
    RuntimeContext {
        time_provider: std::sync::Arc::new(FakeTimeProvider::new(1_700_000_000)),
        id_generator: std::sync::Arc::new(FakeIdGenerator::new("bench".into())),
        ..RuntimeContext::default()
    }
}

pub fn bench_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to build runtime")
}

pub struct DispatcherSetup {
    graph: Graph,
    registry: Arc<NodeExecutorRegistry>,
    context: Arc<RuntimeContext>,
    config: EngineConfig,
}

impl DispatcherSetup {
    pub fn from_yaml(yaml: &str) -> Self {
        let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();
        let registry = Arc::new(NodeExecutorRegistry::new());
        let context = Arc::new(bench_context().with_node_executor_registry(Arc::clone(&registry)));
        let config = EngineConfig::default();
        Self {
            graph,
            registry,
            context,
            config,
        }
    }

    pub fn from_schema(schema: &WorkflowSchema) -> Self {
        let graph = build_graph(schema).unwrap();
        let registry = Arc::new(NodeExecutorRegistry::new());
        let context = Arc::new(bench_context().with_node_executor_registry(Arc::clone(&registry)));
        let config = EngineConfig::default();
        Self {
            graph,
            registry,
            context,
            config,
        }
    }

    pub async fn run_hot(&self, pool: VariablePool) {
        let graph = self.graph.clone();
        let emitter = Self::make_emitter();

        #[cfg(not(feature = "plugin-system"))]
        let mut dispatcher = WorkflowDispatcher::new_with_registry(
            graph,
            pool,
            Arc::clone(&self.registry),
            emitter,
            self.config.clone(),
            Arc::clone(&self.context),
            None,
        );

        #[cfg(feature = "plugin-system")]
        let mut dispatcher = WorkflowDispatcher::new_with_registry(
            graph,
            pool,
            Arc::clone(&self.registry),
            emitter,
            self.config.clone(),
            Arc::clone(&self.context),
            None,
        );

        let _ = dispatcher.run().await.unwrap();
    }

    pub async fn run_hot_with_outputs(&self, pool: VariablePool) -> HashMap<String, Value> {
        let graph = self.graph.clone();
        let emitter = Self::make_emitter();

        #[cfg(not(feature = "plugin-system"))]
        let mut dispatcher = WorkflowDispatcher::new_with_registry(
            graph,
            pool,
            Arc::clone(&self.registry),
            emitter,
            self.config.clone(),
            Arc::clone(&self.context),
            None,
        );

        #[cfg(feature = "plugin-system")]
        let mut dispatcher = WorkflowDispatcher::new_with_registry(
            graph,
            pool,
            Arc::clone(&self.registry),
            emitter,
            self.config.clone(),
            Arc::clone(&self.context),
            None,
        );

        dispatcher.run().await.unwrap()
    }

    fn make_emitter() -> EventEmitter {
        let (tx, _rx) = mpsc::channel(8);
        let active = Arc::new(AtomicBool::new(false));
        EventEmitter::new(tx, active)
    }
}
