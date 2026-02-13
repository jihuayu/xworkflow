use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};

mod helpers;

use helpers::workflow_builders::{build_fanout_workflow, build_linear_workflow};
use helpers::{bench_runtime, DispatcherSetup};

fn build_condition_count_workflow(count: usize) -> String {
    let mut yaml = String::new();
    yaml.push_str("version: \"0.1.0\"\n");
    yaml.push_str("nodes:\n");
    yaml.push_str("  - id: start\n    data: { type: start, title: Start }\n");
    yaml.push_str("  - id: if1\n    data:\n      type: if-else\n      title: Cond\n      cases:\n");
    yaml.push_str(
        "        - case_id: c1\n          logical_operator: and\n          conditions:\n",
    );
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

fn build_flags_pool(count: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    for i in 0..count {
        pool.set(
            &Selector::new("start", format!("flag{}", i)),
            Segment::Boolean(true),
        );
    }
    pool
}

fn bench_scalability(c: &mut Criterion) {
    let rt = bench_runtime();

    let mut group = c.benchmark_group("scale/nodes");
    for total_nodes in [5usize, 10, 25, 50, 100, 200] {
        let internal = total_nodes.saturating_sub(2).max(1);
        let yaml = build_linear_workflow(internal, "template-transform");
        let setup = DispatcherSetup::from_yaml(&yaml);
        group.bench_with_input(
            BenchmarkId::from_parameter(total_nodes),
            &total_nodes,
            |b, _| {
                b.to_async(&rt)
                    .iter(|| async { setup.run_hot(VariablePool::new()).await });
            },
        );
    }
    group.finish();

    let mut group = c.benchmark_group("scale/branches");
    for size in [2usize, 5, 10, 20, 50] {
        let yaml = build_fanout_workflow(size);
        let setup = DispatcherSetup::from_yaml(&yaml);
        let base_pool = build_flags_pool(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            let base_pool = base_pool.clone();
            b.to_async(&rt).iter(|| async {
                setup.run_hot(base_pool.clone()).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale/pool_size");
    let yaml = build_linear_workflow(3, "template-transform");
    let setup = DispatcherSetup::from_yaml(&yaml);
    for size in [10usize, 50, 100, 500] {
        let mut base_pool = VariablePool::new();
        for i in 0..size {
            base_pool.set(
                &Selector::new("start", format!("k{}", i)),
                Segment::Integer(i as i64),
            );
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            let base_pool = base_pool.clone();
            b.to_async(&rt).iter(|| async {
                setup.run_hot(base_pool.clone()).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale/data_size");
    let yaml = build_linear_workflow(3, "template-transform");
    let setup = DispatcherSetup::from_yaml(&yaml);
    for size in [100usize, 1024, 10 * 1024, 100 * 1024] {
        let mut base_pool = VariablePool::new();
        base_pool.set(
            &Selector::new("start", "blob"),
            Segment::String("x".repeat(size)),
        );
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            let base_pool = base_pool.clone();
            b.to_async(&rt).iter(|| async {
                setup.run_hot(base_pool.clone()).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale/conditions");
    for count in [1usize, 5, 10, 20, 50] {
        let yaml = build_condition_count_workflow(count);
        let setup = DispatcherSetup::from_yaml(&yaml);
        let mut base_pool = VariablePool::new();
        for i in 0..count {
            base_pool.set(
                &Selector::new("start", format!("c{}", i)),
                Segment::Boolean(true),
            );
        }
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            let base_pool = base_pool.clone();
            b.to_async(&rt).iter(|| async {
                setup.run_hot(base_pool.clone()).await;
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("scale/template_vars");
    for count in [1usize, 5, 10, 25, 50] {
        let yaml = build_template_var_workflow(count);
        let setup = DispatcherSetup::from_yaml(&yaml);
        let mut base_pool = VariablePool::new();
        for i in 0..count {
            base_pool.set(
                &Selector::new("start", format!("v{}", i)),
                Segment::String("x".into()),
            );
        }
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            let base_pool = base_pool.clone();
            b.to_async(&rt).iter(|| async {
                setup.run_hot(base_pool.clone()).await;
            });
        });
    }
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
    targets = bench_scalability
}
criterion_main!(benches);
