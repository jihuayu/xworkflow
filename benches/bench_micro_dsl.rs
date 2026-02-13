use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use xworkflow::dsl::{parse_dsl, validate_schema, DslFormat};
use xworkflow::graph::build_graph;

mod helpers;

use helpers::workflow_builders::{build_branch_workflow, build_linear_workflow};

fn build_json_workflow(node_count: usize) -> String {
    let nodes: Vec<serde_json::Value> = (0..node_count)
        .map(|i| {
            serde_json::json!({
                "id": format!("n{}", i),
                "data": {"type": "template-transform", "title": format!("N{}", i)}
            })
        })
        .collect();

    let mut edges = Vec::new();
    if node_count > 1 {
        for i in 0..(node_count - 1) {
            edges.push(
                serde_json::json!({"source": format!("n{}", i), "target": format!("n{}", i + 1)}),
            );
        }
    }

    serde_json::json!({
        "version": "0.1.0",
        "nodes": nodes,
        "edges": edges,
    })
    .to_string()
}

fn bench_dsl(c: &mut Criterion) {
    for size in [2usize, 10, 50, 200] {
        c.bench_with_input(
            BenchmarkId::new("parse_yaml_nodes", size),
            &size,
            |b, size| {
                let yaml = build_linear_workflow(*size, "template-transform");
                b.iter(|| {
                    let _ = black_box(parse_dsl(&yaml, DslFormat::Yaml).unwrap());
                });
            },
        );
    }

    for size in [2usize, 50, 200] {
        c.bench_with_input(
            BenchmarkId::new("parse_json_nodes", size),
            &size,
            |b, size| {
                let json = build_json_workflow(*size);
                b.iter(|| {
                    let _ = black_box(parse_dsl(&json, DslFormat::Json).unwrap());
                });
            },
        );
    }

    c.bench_function("validate_schema_50_nodes", |b| {
        let yaml = build_linear_workflow(50, "template-transform");
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        b.iter(|| {
            let _ = black_box(validate_schema(&schema));
        });
    });

    for size in [2usize, 50, 200] {
        c.bench_with_input(
            BenchmarkId::new("build_graph_nodes", size),
            &size,
            |b, size| {
                let yaml = build_linear_workflow(*size, "template-transform");
                let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
                b.iter(|| {
                    let _ = black_box(build_graph(&schema).unwrap());
                });
            },
        );
    }

    c.bench_function("is_node_ready_check", |b| {
        let yaml = build_linear_workflow(50, "template-transform");
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        let graph = build_graph(&schema).unwrap();
        b.iter(|| {
            let _ = black_box(graph.is_node_ready("n10"));
        });
    });

    c.bench_function("process_normal_edges", |b| {
        let yaml = build_linear_workflow(10, "template-transform");
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        let mut graph = build_graph(&schema).unwrap();
        b.iter(|| {
            graph.process_normal_edges("n5");
        });
    });

    c.bench_function("process_branch_edges_5", |b| {
        let yaml = build_branch_workflow(5);
        let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
        let mut graph = build_graph(&schema).unwrap();
        b.iter(|| {
            graph.process_branch_edges(
                "if1",
                &xworkflow::dsl::schema::EdgeHandle::Branch("c0".to_string()),
            );
        });
    });
}

criterion_group!(benches, bench_dsl);
criterion_main!(benches);
