#![allow(unused)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::sync::mpsc;

use xworkflow::core::dispatcher::{EngineConfig, EventEmitter, WorkflowDispatcher};
use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};
use xworkflow::dsl::{parse_dsl, DslFormat, WorkflowSchema};
use xworkflow::graph::build_graph;
use xworkflow::nodes::NodeExecutorRegistry;
use xworkflow::RuntimeContext;

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

#[allow(dead_code)]
pub struct DropCounter {
    counter: Arc<AtomicUsize>,
}

impl DropCounter {
    pub fn new() -> (Self, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        (Self { counter: counter.clone() }, counter)
    }
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

pub async fn with_timeout<F, T>(label: &str, duration: Duration, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(duration, f)
        .await
        .unwrap_or_else(|_| panic!("'{}' timed out after {:?}", label, duration))
}

#[allow(dead_code)]
pub fn assert_arc_dropped<T>(weak: &Weak<T>, label: &str) {
    assert!(
        weak.upgrade().is_none(),
        "{} still alive (strong_count > 0)",
        label
    );
}

#[test]
fn test_helper_smoke() {
    let (counter, count) = DropCounter::new();
    drop(counter);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let arc = Arc::new(());
    let weak = Arc::downgrade(&arc);
    drop(arc);
    assert_arc_dropped(&weak, "arc");
}

pub async fn wait_for_condition(
    label: &str,
    timeout: Duration,
    interval: Duration,
    condition: impl Fn() -> bool,
) {
    let start = tokio::time::Instant::now();
    while !condition() {
        if start.elapsed() > timeout {
            panic!("condition '{}' not met within {:?}", label, timeout);
        }
        tokio::time::sleep(interval).await;
    }
}

pub fn simple_workflow_schema() -> WorkflowSchema {
    parse_dsl(SIMPLE_WORKFLOW_YAML, DslFormat::Yaml).expect("parse simple workflow")
}

pub fn make_realistic_pool(node_count: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    for n in 0..node_count {
        pool.set(
            &Selector::new(format!("node{}", n), "status"),
            Segment::String("ok".into()),
        );
        pool.set(
            &Selector::new(format!("node{}", n), "value"),
            Segment::Integer(n as i64),
        );
    }
    pool
}

pub struct DispatcherSetup {
    graph: xworkflow::Graph,
    registry: Arc<NodeExecutorRegistry>,
    context: Arc<RuntimeContext>,
    config: EngineConfig,
}

impl DispatcherSetup {
    pub fn from_schema(schema: &WorkflowSchema) -> Self {
        let graph = build_graph(schema).expect("build graph");
        let registry = Arc::new(NodeExecutorRegistry::new());
        let context = Arc::new(RuntimeContext::default().with_node_executor_registry(Arc::clone(&registry)));
        let config = EngineConfig::default();
        Self {
            graph,
            registry,
            context,
            config,
        }
    }

    pub async fn run_hot(&self, pool: VariablePool) {
        let emitter = make_emitter();

        #[cfg(not(feature = "plugin-system"))]
        let mut dispatcher = WorkflowDispatcher::new_with_registry(
            self.graph.clone(),
            pool,
            Arc::clone(&self.registry),
            emitter,
            self.config.clone(),
            Arc::clone(&self.context),
            None,
        );

        #[cfg(feature = "plugin-system")]
        let mut dispatcher = WorkflowDispatcher::new_with_registry(
            self.graph.clone(),
            pool,
            Arc::clone(&self.registry),
            emitter,
            self.config.clone(),
            Arc::clone(&self.context),
            None,
        );

        let _ = dispatcher.run().await.expect("dispatcher run");
    }
}

fn make_emitter() -> EventEmitter {
    let (tx, _rx) = mpsc::channel(8);
    let active = Arc::new(AtomicBool::new(false));
    EventEmitter::new(tx, active)
}
