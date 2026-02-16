use criterion::{black_box, criterion_group, criterion_main, Criterion};

use xworkflow::core::runtime_context::RuntimeContext;
use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};
use xworkflow::nodes::control_flow::{
    AnswerNodeExecutor, EndNodeExecutor, IfElseNodeExecutor, StartNodeExecutor,
};
use xworkflow::nodes::executor::NodeExecutor;
use xworkflow::nodes::transform::{
    CodeNodeExecutor, TemplateTransformExecutor, VariableAggregatorExecutor,
};

mod helpers;
use helpers::bench_runtime;

fn bench_node_executors(c: &mut Criterion) {
    let rt = bench_runtime();

    c.bench_function("start_node_5_vars", |b| {
        let mut pool = VariablePool::new();
        for i in 0..5 {
            pool.set(
                &Selector::new("start", format!("v{}", i)),
                Segment::String(format!("val{}", i)),
            );
        }
        let config = serde_json::json!({
            "variables": (0..5).map(|i| serde_json::json!({"variable": format!("v{}", i), "label": "v", "type": "string", "required": false})).collect::<Vec<_>>()
        });
        let executor = StartNodeExecutor;
        let context = RuntimeContext::default();
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("start", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });

    c.bench_function("end_node_5_outputs", |b| {
        let mut pool = VariablePool::new();
        for i in 0..5 {
            pool.set(
                &Selector::new(format!("n{}", i), "out"),
                Segment::String(format!("v{}", i)),
            );
        }
        let config = serde_json::json!({
            "outputs": (0..5)
                .map(|i| serde_json::json!({"variable": format!("o{}", i), "value_selector": [format!("n{}", i), "out"]}))
                .collect::<Vec<_>>()
        });
        let executor = EndNodeExecutor;
        let context = RuntimeContext::default();
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("end", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });

    c.bench_function("answer_node_5_vars", |b| {
        let mut pool = VariablePool::new();
        for i in 0..5 {
            pool.set(
                &Selector::new("n", format!("v{}", i)),
                Segment::String(format!("val{}", i)),
            );
        }
        let template = (0..5)
            .map(|i| format!("{{{{#n.v{}#}}}}", i))
            .collect::<Vec<_>>()
            .join(" ");
        let config = serde_json::json!({"answer": template});
        let executor = AnswerNodeExecutor;
        let context = RuntimeContext::default();
        b.to_async(&rt).iter(|| async {
            let result = executor
                .execute("ans", &config, &pool, &context)
                .await
                .unwrap();
            black_box(result.outputs.clone());
        });
    });

    c.bench_function("ifelse_5_cases_5_conditions", |b| {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n", "x"), Segment::Integer(10));
        let cases = (0..5)
            .map(|i| {
                serde_json::json!({
                    "case_id": format!("c{}", i),
                    "logical_operator": "and",
                    "conditions": (0..5)
                        .map(|_| serde_json::json!({
                            "variable_selector": ["n", "x"],
                            "comparison_operator": "greater_than",
                            "value": 5
                        }))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let config = serde_json::json!({"cases": cases});
        let executor = IfElseNodeExecutor;
        let context = RuntimeContext::default();
        b.to_async(&rt).iter(|| async {
            let result = executor
                .execute("if1", &config, &pool, &context)
                .await
                .unwrap();
            black_box(result.outputs.clone());
        });
    });

    c.bench_function("template_transform_5_vars", |b| {
        let mut pool = VariablePool::new();
        for i in 0..5 {
            pool.set(
                &Selector::new("n", format!("v{}", i)),
                Segment::String(format!("val{}", i)),
            );
        }
        let template = (0..5)
            .map(|i| format!("{{{{ v{} }}}}", i))
            .collect::<Vec<_>>()
            .join(" ");
        let variables = (0..5)
            .map(|i| serde_json::json!({"variable": format!("v{}", i), "value_selector": ["n", format!("v{}", i)]}))
            .collect::<Vec<_>>();
        let config = serde_json::json!({"template": template, "variables": variables});
        let executor = TemplateTransformExecutor::new();
        let context = RuntimeContext::default();
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("tpl", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });

    c.bench_function("variable_aggregator_5_selectors", |b| {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n3", "out"), Segment::String("hit".into()));
        let variables = (0..5)
            .map(|i| serde_json::json!([format!("n{}", i), "out"]))
            .collect::<Vec<_>>();
        let config = serde_json::json!({"variables": variables});
        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        b.to_async(&rt).iter(|| async {
            let result = executor
                .execute("agg", &config, &pool, &context)
                .await
                .unwrap();
            black_box(result.outputs.clone());
        });
    });

    c.bench_function("code_node_js_simple", |b| {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return { result: 1 + 1 }; }",
            "language": "javascript"
        });
        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        b.to_async(&rt).iter(|| async {
            let result = executor
                .execute("code", &config, &pool, &context)
                .await
                .unwrap();
            black_box(result.outputs.clone());
        });
    });
}

criterion_group!(benches, bench_node_executors);
criterion_main!(benches);
