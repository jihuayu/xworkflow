use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};

mod helpers;

use helpers::{bench_runtime, DispatcherSetup};
use helpers::workflow_builders::{
    build_branch_workflow, build_deep_branch_chain, build_diamond_workflow, build_fanout_workflow,
    build_iteration_workflow, build_linear_workflow, build_realistic_mixed_workflow,
};

fn build_start_end_workflow() -> String {
    r#"
version: "0.1.0"
nodes:
  - id: start
    data: { type: start, title: Start }
  - id: end
    data: { type: end, title: End, outputs: [] }
edges:
  - source: start
    target: end
"#
    .to_string()
}

fn build_items_pool(items: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    let values = (0..items)
        .map(|i| Segment::String(format!("item{}", i)))
        .collect();
    pool.set(
        &Selector::new("start", "items"),
        Segment::Array(values),
    );
    pool
}

fn bench_dispatch(c: &mut Criterion) {
    let rt = bench_runtime();

    let mut group = c.benchmark_group("dispatch/linear");
    let yaml = build_start_end_workflow();
    let setup = DispatcherSetup::from_yaml(&yaml);
    group.bench_function("2_nodes", |b| {
        b.to_async(&rt).iter(|| async { setup.run_hot(VariablePool::new()).await });
    });

    for total_nodes in [5usize, 10, 50] {
        let internal = total_nodes.saturating_sub(2).max(1);
        let yaml = build_linear_workflow(internal, "template-transform");
        let setup = DispatcherSetup::from_yaml(&yaml);
        let name = format!("{}_nodes", total_nodes);
        group.bench_function(&name, |b| {
            b.to_async(&rt).iter(|| async { setup.run_hot(VariablePool::new()).await });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("dispatch/branch");
    let yaml = build_branch_workflow(2);
    let setup = DispatcherSetup::from_yaml(&yaml);
    group.bench_function("2_way", |b| {
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            for i in 0..2 {
                pool.set(
                    &Selector::new("start", format!("flag{}", i)),
                    Segment::Boolean(true),
                );
            }
            setup.run_hot(pool).await;
        });
    });

    let yaml = build_fanout_workflow(10);
    let setup = DispatcherSetup::from_yaml(&yaml);
    group.bench_function("10_way", |b| {
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            for i in 0..10 {
                pool.set(
                    &Selector::new("start", format!("flag{}", i)),
                    Segment::Boolean(true),
                );
            }
            setup.run_hot(pool).await;
        });
    });
    group.finish();

    let mut group = c.benchmark_group("dispatch/diamond");
    let yaml = build_diamond_workflow(10);
    let setup = DispatcherSetup::from_yaml(&yaml);
    group.bench_function("10_wide", |b| {
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            for i in 0..10 {
                pool.set(
                    &Selector::new("start", format!("flag{}", i)),
                    Segment::Boolean(true),
                );
            }
            pool.set(
                &Selector::new("start", "query"),
                Segment::String("bench".into()),
            );
            setup.run_hot(pool).await;
        });
    });

    let yaml = build_deep_branch_chain(10);
    let setup = DispatcherSetup::from_yaml(&yaml);
    group.bench_function("deep_10", |b| {
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            for i in 1..=10 {
                pool.set(
                    &Selector::new("start", format!("flag{}", i)),
                    Segment::Boolean(true),
                );
            }
            setup.run_hot(pool).await;
        });
    });
    group.finish();

    let mut group = c.benchmark_group("dispatch/iteration");
    for (name, items, parallel, parallelism) in [
        ("seq_10", 10usize, false, 1usize),
        ("seq_100", 100, false, 1),
        ("par_10_p10", 10, true, 10),
        ("par_100_p10", 100, true, 10),
    ] {
        let yaml = build_iteration_workflow(items, parallel, parallelism);
        let setup = DispatcherSetup::from_yaml(&yaml);
        group.bench_function(name, |b| {
            b.to_async(&rt)
                .iter(|| async { setup.run_hot(build_items_pool(items)).await });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("dispatch/realistic");
    let yaml = build_realistic_mixed_workflow();
    let setup = DispatcherSetup::from_yaml(&yaml);
    group.bench_function("mixed", |b| {
        b.to_async(&rt).iter(|| async {
            let mut pool = VariablePool::new();
            pool.set(
                &Selector::new("start", "query"),
                Segment::String("world".into()),
            );
            pool.set(
                &Selector::new("start", "flag"),
                Segment::Boolean(true),
            );
            setup.run_hot(pool).await;
        });
    });
    group.finish();
}

fn criterion_config() -> Criterion {
    Criterion::default()
        .sample_size(100)
        .measurement_time(Duration::from_secs(5))
        .warm_up_time(Duration::from_secs(2))
        .significance_level(0.05)
        .noise_threshold(0.02)
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_dispatch
}
criterion_main!(benches);
