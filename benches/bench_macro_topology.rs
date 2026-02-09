use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use xworkflow::dsl::{parse_dsl, DslFormat, WorkflowSchema};
use xworkflow::scheduler::WorkflowRunner;

mod helpers;

use helpers::workflow_builders::{
    build_deep_branch_chain, build_diamond_workflow, build_fanout_workflow,
    build_linear_workflow, build_realistic_mixed_workflow,
};
use helpers::bench_runtime;

async fn run_workflow(schema: WorkflowSchema, inputs: HashMap<String, serde_json::Value>) {
    let runner = WorkflowRunner::builder(schema).user_inputs(inputs);
    let handle = runner.run().await.unwrap();
    let status = handle.wait().await;
    black_box(status);
}

fn bench_macro_topology(c: &mut Criterion) {
    let rt = bench_runtime();

    c.bench_function("topo_minimal", |b| {
        let yaml = build_linear_workflow(1, "template-transform");
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        b.to_async(&rt).iter(|| async {
            run_workflow(schema.clone(), HashMap::new()).await;
        });
    });

    for size in [5usize, 10] {
        let name = format!("topo_linear_{}", size);
        let yaml = build_linear_workflow(size, "template-transform");
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        c.bench_function(&name, |b| {
            b.to_async(&rt).iter(|| async {
                run_workflow(schema.clone(), HashMap::new()).await;
            });
        });
    }

    c.bench_function("topo_branch_2_way", |b| {
        let yaml = build_fanout_workflow(2);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        b.to_async(&rt).iter(|| async {
            run_workflow(schema.clone(), HashMap::new()).await;
        });
    });

    c.bench_function("topo_branch_5_way", |b| {
        let yaml = build_fanout_workflow(5);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        b.to_async(&rt).iter(|| async {
            run_workflow(schema.clone(), HashMap::new()).await;
        });
    });

    c.bench_function("topo_diamond_5", |b| {
        let yaml = build_diamond_workflow(5);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        b.to_async(&rt).iter(|| async {
            let mut inputs = HashMap::new();
            inputs.insert("query".into(), serde_json::json!("bench"));
            inputs.insert("flag0".into(), serde_json::json!(true));
            run_workflow(schema.clone(), inputs).await;
        });
    });

    c.bench_function("topo_diamond_10", |b| {
        let yaml = build_diamond_workflow(10);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        b.to_async(&rt).iter(|| async {
            let mut inputs = HashMap::new();
            inputs.insert("query".into(), serde_json::json!("bench"));
            inputs.insert("flag0".into(), serde_json::json!(true));
            run_workflow(schema.clone(), inputs).await;
        });
    });

    c.bench_function("topo_deep_branch_5", |b| {
        let yaml = build_deep_branch_chain(5);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        b.to_async(&rt).iter(|| async {
            let mut inputs = HashMap::new();
            for i in 1..=5 {
                inputs.insert(format!("flag{}", i), serde_json::json!(true));
            }
            run_workflow(schema.clone(), inputs).await;
        });
    });

    c.bench_function("topo_mixed_realistic", |b| {
        let yaml = build_realistic_mixed_workflow();
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        b.to_async(&rt).iter(|| async {
            let mut inputs = HashMap::new();
            inputs.insert("query".into(), serde_json::json!("world"));
            inputs.insert("flag".into(), serde_json::json!(true));
            run_workflow(schema.clone(), inputs).await;
        });
    });
}

criterion_group!(benches, bench_macro_topology);
criterion_main!(benches);
