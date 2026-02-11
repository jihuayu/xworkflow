use std::collections::HashMap;
use std::sync::Arc;

use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use xworkflow::core::variable_pool::{Segment, SegmentArray, SegmentObject, Selector, VariablePool};
use xworkflow::dsl::{parse_dsl, DslFormat};
use xworkflow::evaluator::{evaluate_case, evaluate_cases, evaluate_condition};
use xworkflow::graph::build_graph;
use xworkflow::template::{render_jinja2, render_template};
use xworkflow::dsl::schema::{Case, ComparisonOperator, Condition, LogicalOperator};

mod helpers;

use helpers::bench_runtime;
use helpers::pool_factories::{make_pool_with_objects, make_pool_with_strings, make_realistic_pool};
use helpers::workflow_builders::build_linear_workflow;

fn build_pool_with_vars(count: usize) -> VariablePool {
    let mut pool = VariablePool::new();
    for i in 0..count {
        pool.set(
            &Selector::new("node", format!("v{}", i)),
            Segment::String(format!("val{}", i)),
        );
    }
    pool
}

fn build_dify_template(var_count: usize) -> String {
    let mut tpl = String::new();
    for i in 0..var_count {
        tpl.push_str(&format!("{{{{#node.v{}#}}}} ", i));
    }
    tpl
}

fn build_jinja_vars(count: usize) -> HashMap<String, serde_json::Value> {
    let mut vars = HashMap::new();
    for i in 0..count {
        vars.insert(format!("v{}", i), serde_json::json!(format!("val{}", i)));
    }
    vars
}

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
            edges.push(serde_json::json!({"source": format!("n{}", i), "target": format!("n{}", i + 1)}));
        }
    }

    serde_json::json!({
        "version": "0.1.0",
        "nodes": nodes,
        "edges": edges,
    })
    .to_string()
}

fn make_pool_with_value(value: Segment) -> VariablePool {
    let mut pool = VariablePool::new();
    pool.set(&Selector::new("n", "x"), value);
    pool
}

fn make_condition(op: ComparisonOperator, val: serde_json::Value) -> Condition {
    Condition {
        variable_selector: Selector::new("n", "x"),
        comparison_operator: op,
        value: val,
    }
}

fn bench_data_ops(c: &mut Criterion) {
    let rt = bench_runtime();

    let mut group = c.benchmark_group("pool/write");
    group.bench_function("set_string", |b| {
        let mut pool = VariablePool::new();
        let selector = Selector::new("n", "k");
        b.iter(|| {
            pool.set(black_box(&selector), Segment::String("value".into()));
        });
    });

    group.bench_function("set_node_outputs_5", |b| {
        let mut pool = VariablePool::new();
        let mut outputs = HashMap::new();
        for i in 0..5 {
            outputs.insert(format!("k{}", i), serde_json::Value::String(format!("v{}", i)));
        }
        b.iter(|| {
            pool.set_node_outputs("node", black_box(&outputs));
        });
    });
    group.finish();

    let mut group = c.benchmark_group("pool/read");
    group.bench_function("get_hit", |b| {
        let mut pool = VariablePool::new();
        let selector = Selector::new("n", "k");
        pool.set(&selector, Segment::String("value".into()));
        b.iter(|| {
            let _ = black_box(pool.get(&selector));
        });
    });

    group.bench_function("get_miss", |b| {
        let pool = VariablePool::new();
        let selector = Selector::new("n", "missing");
        b.iter(|| {
            let _ = black_box(pool.get(&selector));
        });
    });

    group.bench_function("has_check", |b| {
        let mut pool = VariablePool::new();
        let selector = Selector::new("n", "k");
        pool.set(&selector, Segment::String("value".into()));
        b.iter(|| {
            let _ = black_box(pool.has(&selector));
        });
    });

    group.bench_function("get_node_variables", |b| {
        let pool = make_realistic_pool(100);
        b.iter(|| {
            let _ = black_box(pool.get_node_variables("node42"));
        });
    });
    group.finish();

    let mut group = c.benchmark_group("pool/clone");
    for size in [10usize, 50, 100, 500] {
        let pool = make_pool_with_strings(size, 16);
        group.bench_with_input(BenchmarkId::from_parameter(size), &pool, |b, pool| {
            b.iter(|| {
                let _ = black_box(pool.clone());
            });
        });
    }
    group.bench_function("large_objects", |b| {
        let pool = make_pool_with_objects(50, 3);
        b.iter(|| {
            let _ = black_box(pool.clone());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("pool/mutate");
    group.bench_function("append_array", |b| {
        let mut pool = VariablePool::new();
        let selector = Selector::new("n", "arr");
        pool.set(
            &selector,
            Segment::Array(Arc::new(SegmentArray::new(Vec::new()))),
        );
        b.iter(|| {
            pool.append(&selector, Segment::Integer(1));
        });
    });
    group.finish();

    let mut group = c.benchmark_group("template/dify");
    group.bench_function("no_vars", |b| {
        let pool = VariablePool::new();
        b.iter(|| {
            let _ = black_box(render_template("static text", &pool));
        });
    });

    group.bench_function("1_var", |b| {
        let pool = build_pool_with_vars(1);
        let tpl = build_dify_template(1);
        b.iter(|| {
            let _ = black_box(render_template(&tpl, &pool));
        });
    });

    for vars in [10usize, 50] {
        group.bench_with_input(BenchmarkId::from_parameter(vars), &vars, |b, vars| {
            let pool = build_pool_with_vars(*vars);
            let tpl = build_dify_template(*vars);
            b.iter(|| {
                let _ = black_box(render_template(&tpl, &pool));
            });
        });
    }

    group.bench_function("large_1kb", |b| {
        let pool = build_pool_with_vars(5);
        let mut tpl = "x".repeat(1024);
        tpl.push_str(" {{#node.v0#}} ");
        b.iter(|| {
            let _ = black_box(render_template(&tpl, &pool));
        });
    });
    group.finish();

    let mut group = c.benchmark_group("template/jinja2");
    group.bench_function("simple", |b| {
        let vars = build_jinja_vars(1);
        b.iter(|| {
            let _ = black_box(render_jinja2("Hello {{ v0 }}", &vars));
        });
    });

    group.bench_function("loop_100", |b| {
        let mut vars = HashMap::new();
        let items: Vec<String> = (0..100).map(|i| format!("i{}", i)).collect();
        vars.insert("items".to_string(), serde_json::json!(items));
        b.iter(|| {
            let _ = black_box(render_jinja2("{% for i in items %}{{ i }} {% endfor %}", &vars));
        });
    });

    group.bench_function("conditional", |b| {
        let vars = build_jinja_vars(1);
        b.iter(|| {
            let _ = black_box(render_jinja2("{% if v0 %}yes{% else %}no{% endif %}", &vars));
        });
    });

    group.bench_function("complex", |b| {
        let mut vars = HashMap::new();
        vars.insert("items".to_string(), serde_json::json!([1, 2, 3]));
        vars.insert("flag".to_string(), serde_json::json!(true));
        let tpl = r#"{% for i in items %}{% if flag %}{{ i }}{% endif %}{% endfor %}"#;
        b.iter(|| {
            let _ = black_box(render_jinja2(tpl, &vars));
        });
    });
    group.finish();

    let mut group = c.benchmark_group("condition/eval");
    group.bench_function("string_is", |b| {
        let pool = make_pool_with_value(Segment::String("hello".into()));
        let cond = make_condition(ComparisonOperator::Is, serde_json::json!("hello"));
        b.to_async(&rt).iter(|| async {
            let _ = black_box(evaluate_condition(&cond, &pool).await);
        });
    });

    group.bench_function("string_contains", |b| {
        let pool = make_pool_with_value(Segment::String("x".repeat(1024)));
        let cond = make_condition(ComparisonOperator::Contains, serde_json::json!("abc"));
        b.to_async(&rt).iter(|| async {
            let _ = black_box(evaluate_condition(&cond, &pool).await);
        });
    });

    group.bench_function("numeric_gt", |b| {
        let pool = make_pool_with_value(Segment::Integer(42));
        let cond = make_condition(ComparisonOperator::GreaterThan, serde_json::json!(10));
        b.to_async(&rt).iter(|| async {
            let _ = black_box(evaluate_condition(&cond, &pool).await);
        });
    });

    for size in [10usize, 100] {
        group.bench_with_input(BenchmarkId::new("in", size), &size, |b, size| {
            let pool = make_pool_with_value(Segment::String("item42".into()));
            let items: Vec<String> = (0..*size).map(|i| format!("item{}", i)).collect();
            let cond = make_condition(ComparisonOperator::In, serde_json::json!(items));
            b.to_async(&rt).iter(|| async {
                let _ = black_box(evaluate_condition(&cond, &pool).await);
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("condition/case");
    group.bench_function("and_5", |b| {
        let pool = make_pool_with_value(Segment::Integer(10));
        let conditions = (0..5)
            .map(|_| make_condition(ComparisonOperator::GreaterThan, serde_json::json!(5)))
            .collect();
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::And,
            conditions,
        };
        b.to_async(&rt).iter(|| async {
            let _ = black_box(evaluate_case(&case, &pool).await);
        });
    });

    group.bench_function("or_5", |b| {
        let pool = make_pool_with_value(Segment::Integer(10));
        let conditions = (0..5)
            .map(|_| make_condition(ComparisonOperator::GreaterThan, serde_json::json!(5)))
            .collect();
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::Or,
            conditions,
        };
        b.to_async(&rt).iter(|| async {
            let _ = black_box(evaluate_case(&case, &pool).await);
        });
    });

    for (label, last_match) in [("10_first_match", false), ("10_last_match", true)] {
        group.bench_function(label, |b| {
            let pool = make_pool_with_value(Segment::Integer(10));
            let mut cases = Vec::new();
            for i in 0..10 {
                let value = if last_match {
                    if i == 9 { 20 } else { 5 }
                } else if i == 0 {
                    20
                } else {
                    5
                };
                cases.push(Case {
                    case_id: format!("c{}", i),
                    logical_operator: LogicalOperator::And,
                    conditions: vec![make_condition(
                        ComparisonOperator::LessThan,
                        serde_json::json!(value),
                    )],
                });
            }
            b.to_async(&rt).iter(|| async {
                let _ = black_box(evaluate_cases(&cases, &pool).await);
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("segment/to_value");
    group.bench_function("string", |b| {
        let seg = Segment::String("hello".into());
        b.iter(|| {
            let _ = black_box(seg.to_value());
        });
    });

    group.bench_function("nested_object", |b| {
        let seg = Segment::Object(Arc::new(SegmentObject::new({
            let mut map = std::collections::HashMap::new();
            map.insert(
                "level1".into(),
                Segment::Object(Arc::new(SegmentObject::new({
                    let mut inner = std::collections::HashMap::new();
                    inner.insert("level2".into(), Segment::String("value".into()));
                    inner
                }))),
            );
            map
        })));
        b.iter(|| {
            let _ = black_box(seg.to_value());
        });
    });

    group.bench_function("array_1000", |b| {
        let seg = Segment::Array(Arc::new(SegmentArray::new(
            (0..1000).map(|i| Segment::Integer(i)).collect(),
        )));
        b.iter(|| {
            let _ = black_box(seg.to_value());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("segment/from_value");
    group.bench_function("string", |b| {
        let val = serde_json::json!("hello");
        b.iter(|| {
            let _ = black_box(Segment::from_value(&val));
        });
    });

    group.bench_function("nested_object", |b| {
        let val = serde_json::json!({"a": {"b": {"c": 1}}});
        b.iter(|| {
            let _ = black_box(Segment::from_value(&val));
        });
    });

    group.bench_function("array_1000", |b| {
        let val = serde_json::json!((0..1000).collect::<Vec<i32>>());
        b.iter(|| {
            let _ = black_box(Segment::from_value(&val));
        });
    });
    group.finish();

    let mut group = c.benchmark_group("dsl/parse");
    for size in [2usize, 10, 50, 200] {
        group.bench_with_input(BenchmarkId::new("yaml", size), &size, |b, size| {
            let yaml = build_linear_workflow(*size, "template-transform");
            b.iter(|| {
                let _ = black_box(parse_dsl(&yaml, DslFormat::Yaml).unwrap());
            });
        });
    }

    group.bench_function("json_50", |b| {
        let json = build_json_workflow(50);
        b.iter(|| {
            let _ = black_box(parse_dsl(&json, DslFormat::Json).unwrap());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("dsl/graph");
    for size in [2usize, 50, 200] {
        group.bench_with_input(BenchmarkId::new("build", size), &size, |b, size| {
            let yaml = build_linear_workflow(*size, "template-transform");
            let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
            b.iter(|| {
                let _ = black_box(build_graph(&schema).unwrap());
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
    targets = bench_data_ops
}
criterion_main!(benches);
