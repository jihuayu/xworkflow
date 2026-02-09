use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use xworkflow::dsl::{parse_dsl, DslFormat, WorkflowSchema};
use xworkflow::scheduler::WorkflowRunner;

mod helpers;

use helpers::workflow_builders::{build_fanout_workflow, build_linear_workflow};
use helpers::bench_runtime;

async fn run_workflow(
    schema: WorkflowSchema,
    inputs: HashMap<String, serde_json::Value>,
    system_vars: HashMap<String, serde_json::Value>,
) {
    let runner = WorkflowRunner::builder(schema)
        .user_inputs(inputs)
        .system_vars(system_vars);
    let handle = runner.run().await.unwrap();
    let status = handle.wait().await;
    black_box(status);
}

fn build_condition_count_workflow(count: usize) -> String {
    let mut yaml = String::new();
    yaml.push_str("version: \"0.1.0\"\n");
    yaml.push_str("nodes:\n");
    yaml.push_str("  - id: start\n    data: { type: start, title: Start }\n");
    yaml.push_str("  - id: if1\n    data:\n      type: if-else\n      title: Cond\n      cases:\n");
    yaml.push_str("        - case_id: c1\n          logical_operator: and\n          conditions:\n");
    for i in 0..count {
        yaml.push_str(&format!(
            "            - variable_selector: [\"start\", \"c{}\"]\n              comparison_operator: is\n              value: true\n",
            i
        ));
    }
    yaml.push_str("  - id: end\n    data: { type: end, title: End, outputs: [] }\n");
    yaml.push_str("edges:\n  - source: start\n    target: if1\n  - source: if1\n    target: end\n    sourceHandle: c1\n  - source: if1\n    target: end\n    sourceHandle: \"false\"\n");
    yaml
}

fn build_template_var_workflow(var_count: usize) -> String {
    let mut yaml = String::new();
    yaml.push_str("version: \"0.1.0\"\n");
    yaml.push_str("nodes:\n");
    yaml.push_str("  - id: start\n    data: { type: start, title: Start }\n");
    yaml.push_str("  - id: tpl\n    data:\n      type: template-transform\n      title: Template\n      template: \"");
    for i in 0..var_count {
        yaml.push_str(&format!("{{{{ v{} }}}} ", i));
    }
    yaml.push_str("\"\n      variables:\n");
    for i in 0..var_count {
        yaml.push_str(&format!(
            "        - variable: v{}\n          value_selector: [\"start\", \"v{}\"]\n",
            i, i
        ));
    }
    yaml.push_str("  - id: end\n    data: { type: end, title: End, outputs: [] }\n");
    yaml.push_str("edges:\n  - source: start\n    target: tpl\n  - source: tpl\n    target: end\n");
    yaml
}

fn bench_macro_scalability(c: &mut Criterion) {
    let rt = bench_runtime();

    let mut group = c.benchmark_group("scale_node_count");
    for size in [5usize, 10, 25, 50, 100, 200] {
        let yaml = build_linear_workflow(size, "template-transform");
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.to_async(&rt).iter(|| async {
                run_workflow(schema.clone(), HashMap::new(), HashMap::new()).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale_branch_count");
    for size in [2usize, 5, 10, 20, 50] {
        let yaml = build_fanout_workflow(size);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.to_async(&rt).iter(|| async {
                run_workflow(schema.clone(), HashMap::new(), HashMap::new()).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale_pool_size");
    for size in [10usize, 50, 100, 500] {
        let yaml = build_linear_workflow(5, "template-transform");
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, size| {
            b.to_async(&rt).iter(|| async {
                let mut sys_vars = HashMap::new();
                for i in 0..*size {
                    sys_vars.insert(format!("k{}", i), serde_json::json!(i));
                }
                run_workflow(schema.clone(), HashMap::new(), sys_vars).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale_data_size");
    for size in [100usize, 1024, 10 * 1024, 100 * 1024] {
        let yaml = build_linear_workflow(5, "template-transform");
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, size| {
            b.to_async(&rt).iter(|| async {
                let mut sys_vars = HashMap::new();
                sys_vars.insert("blob".to_string(), serde_json::json!("x".repeat(*size)));
                run_workflow(schema.clone(), HashMap::new(), sys_vars).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale_condition_count");
    for count in [1usize, 5, 10, 20, 50] {
        let yaml = build_condition_count_workflow(count);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, count| {
            b.to_async(&rt).iter(|| async {
                let mut inputs = HashMap::new();
                for i in 0..*count {
                    inputs.insert(format!("c{}", i), serde_json::json!(true));
                }
                run_workflow(schema.clone(), inputs, HashMap::new()).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale_template_vars");
    for count in [1usize, 5, 10, 25, 50] {
        let yaml = build_template_var_workflow(count);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, count| {
            b.to_async(&rt).iter(|| async {
                let mut inputs = HashMap::new();
                for i in 0..*count {
                    inputs.insert(format!("v{}", i), serde_json::json!("x"));
                }
                run_workflow(schema.clone(), inputs, HashMap::new()).await;
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_macro_scalability);
criterion_main!(benches);
