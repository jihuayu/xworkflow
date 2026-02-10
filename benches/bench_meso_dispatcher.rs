use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use criterion::{criterion_group, criterion_main, Criterion};
use tokio::sync::mpsc;

use xworkflow::core::dispatcher::{EngineConfig, EventEmitter, WorkflowDispatcher};
use xworkflow::core::variable_pool::{Segment, VariablePool};
use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::graph::build_graph;
use xworkflow::nodes::NodeExecutorRegistry;

mod helpers;

use helpers::workflow_builders::{
    build_branch_workflow, build_deep_branch_chain, build_diamond_workflow, build_fanout_workflow,
    build_linear_workflow,
};
use helpers::{bench_context, bench_runtime};

async fn run_dispatcher(yaml: &str, pool: VariablePool) {
    let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();
    let graph = build_graph(&schema).unwrap();
    let registry = NodeExecutorRegistry::new();
    let (tx, mut rx) = mpsc::channel(256);
    tokio::spawn(async move {
        while rx.recv().await.is_some() {}
    });
    let active = Arc::new(AtomicBool::new(true));
    let emitter = EventEmitter::new(tx, active);
    let context = Arc::new(bench_context());

    let mut dispatcher = WorkflowDispatcher::new(
        graph,
        pool,
        registry,
        emitter,
        EngineConfig::default(),
        context,
        None,
    );

    let _ = dispatcher.run().await.unwrap();
}

fn bench_dispatcher(c: &mut Criterion) {
    let rt = bench_runtime();

    c.bench_function("dispatch_start_end", |b| {
        let yaml = build_linear_workflow(1, "template-transform");
        b.to_async(&rt).iter(|| async {
            run_dispatcher(&yaml, VariablePool::new()).await;
        });
    });

    for size in [5usize, 10, 50] {
        let name = format!("dispatch_linear_{}", size);
        c.bench_function(&name, |b| {
            let yaml = build_linear_workflow(size, "template-transform");
            b.to_async(&rt).iter(|| async {
                run_dispatcher(&yaml, VariablePool::new()).await;
            });
        });
    }

    c.bench_function("dispatch_branch_2_way", |b| {
        let yaml = build_branch_workflow(2);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&["start".to_string(), "flag0".to_string()], Segment::Boolean(true));
            run_dispatcher(&yaml, pool).await;
        });
    });

    c.bench_function("dispatch_branch_10_way", |b| {
        let yaml = build_fanout_workflow(10);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&["start".to_string(), "flag0".to_string()], Segment::Boolean(true));
            run_dispatcher(&yaml, pool).await;
        });
    });

    c.bench_function("dispatch_diamond_10", |b| {
        let yaml = build_diamond_workflow(10);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(&["start".to_string(), "flag0".to_string()], Segment::Boolean(true));
            pool.set(&["start".to_string(), "query".to_string()], Segment::String("bench".into()));
            run_dispatcher(&yaml, pool).await;
        });
    });

    c.bench_function("dispatch_deep_branch_10", |b| {
        let yaml = build_deep_branch_chain(10);
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            for i in 1..=10 {
                pool.set(
                    &["start".to_string(), format!("flag{}", i)],
                    Segment::Boolean(true),
                );
            }
            run_dispatcher(&yaml, pool).await;
        });
    });
}

criterion_group!(benches, bench_dispatcher);
criterion_main!(benches);
