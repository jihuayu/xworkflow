use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use xworkflow::dsl::{parse_dsl, DslFormat, WorkflowSchema};
use xworkflow::scheduler::WorkflowRunner;

mod helpers;

use helpers::workflow_builders::build_iteration_workflow;
use helpers::bench_runtime;

async fn run_iteration(schema: WorkflowSchema, items: usize) {
    let mut inputs = HashMap::new();
    let items_vec: Vec<String> = (0..items).map(|i| format!("item{}", i)).collect();
    inputs.insert("items".to_string(), serde_json::json!(items_vec));

    let runner = WorkflowRunner::builder(schema).user_inputs(inputs);
    let handle = runner.run().await.unwrap();
    let status = handle.wait().await;
    black_box(status);
}

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
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        c.bench_function(name, |b| {
            b.to_async(&rt).iter(|| async {
                run_iteration(schema.clone(), items).await;
            });
        });
    }

    let mut group = c.benchmark_group("iter_seq_vs_par_50");
    for (label, parallel, parallelism) in [("seq", false, 1), ("par", true, 10)] {
        let yaml = build_iteration_workflow(50, parallel, parallelism);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        group.bench_with_input(BenchmarkId::new("mode", label), &parallel, |b, _| {
            b.to_async(&rt).iter(|| async {
                run_iteration(schema.clone(), 50).await;
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_macro_iteration);
criterion_main!(benches);
