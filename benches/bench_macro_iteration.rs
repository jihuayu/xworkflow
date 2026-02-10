use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};

mod helpers;

use helpers::workflow_builders::build_iteration_workflow;
use helpers::{bench_runtime, DispatcherSetup};

fn bench_macro_iteration(c: &mut Criterion) {
    let rt = bench_runtime();

    let cases = vec![
        ("iter_seq_10", 10, false, 1),
        ("iter_seq_50", 50, false, 1),
        ("iter_seq_100", 100, false, 1),
        ("iter_par_10_p5", 10, true, 5),
        ("iter_par_10_p10", 10, true, 10),
        ("iter_par_50_p10", 50, true, 10),
        ("iter_par_100_p10", 100, true, 10),
        ("iter_par_100_p50", 100, true, 50),
    ];

    for (name, items, parallel, parallelism) in cases {
        let yaml = build_iteration_workflow(items, parallel, parallelism);
        let setup = DispatcherSetup::from_yaml(&yaml);
        let items_vec: Vec<String> = (0..items).map(|i| format!("item{}", i)).collect();
        let mut base_pool = VariablePool::new();
        base_pool.set(
            &Selector::new("start", "items"),
            Segment::from_value(&serde_json::json!(items_vec)),
        );
        c.bench_function(name, |b| {
            let base_pool = base_pool.clone();
            b.to_async(&rt)
                .iter(|| async { setup.run_hot(base_pool.clone()).await });
        });
    }

    let mut group = c.benchmark_group("iter_seq_vs_par_50");
    for (label, parallel, parallelism) in [("seq", false, 1), ("par", true, 10)] {
        let yaml = build_iteration_workflow(50, parallel, parallelism);
        let setup = DispatcherSetup::from_yaml(&yaml);
        let items_vec: Vec<String> = (0..50).map(|i| format!("item{}", i)).collect();
        let mut base_pool = VariablePool::new();
        base_pool.set(
            &Selector::new("start", "items"),
            Segment::from_value(&serde_json::json!(items_vec)),
        );
        group.bench_with_input(BenchmarkId::new("mode", label), &parallel, |b, _| {
            let base_pool = base_pool.clone();
            b.to_async(&rt)
                .iter(|| async { setup.run_hot(base_pool.clone()).await });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_macro_iteration);
criterion_main!(benches);
