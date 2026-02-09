use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use xworkflow::core::variable_pool::{Segment, VariablePool};
use xworkflow::dsl::schema::{Case, ComparisonOperator, Condition, LogicalOperator};
use xworkflow::evaluator::{evaluate_case, evaluate_cases, evaluate_condition};

fn make_pool_with_value(value: Segment) -> VariablePool {
    let mut pool = VariablePool::new();
    pool.set(&["n".to_string(), "x".to_string()], value);
    pool
}

fn make_condition(op: ComparisonOperator, val: serde_json::Value) -> Condition {
    Condition {
        variable_selector: vec!["n".to_string(), "x".to_string()],
        comparison_operator: op,
        value: val,
    }
}

fn bench_condition(c: &mut Criterion) {
    c.bench_function("eval_string_is", |b| {
        let pool = make_pool_with_value(Segment::String("hello".into()));
        let cond = make_condition(ComparisonOperator::Is, serde_json::json!("hello"));
        b.iter(|| {
            let _ = black_box(evaluate_condition(&cond, &pool));
        });
    });

    c.bench_function("eval_string_contains", |b| {
        let pool = make_pool_with_value(Segment::String("x".repeat(1024)));
        let cond = make_condition(ComparisonOperator::Contains, serde_json::json!("abc"));
        b.iter(|| {
            let _ = black_box(evaluate_condition(&cond, &pool));
        });
    });

    c.bench_function("eval_numeric_gt", |b| {
        let pool = make_pool_with_value(Segment::Integer(42));
        let cond = make_condition(ComparisonOperator::GreaterThan, serde_json::json!(10));
        b.iter(|| {
            let _ = black_box(evaluate_condition(&cond, &pool));
        });
    });

    for size in [10usize, 100] {
        c.bench_with_input(BenchmarkId::new("eval_in_items", size), &size, |b, size| {
            let pool = make_pool_with_value(Segment::String("item42".into()));
            let items: Vec<String> = (0..*size).map(|i| format!("item{}", i)).collect();
            let cond = make_condition(ComparisonOperator::In, serde_json::json!(items));
            b.iter(|| {
                let _ = black_box(evaluate_condition(&cond, &pool));
            });
        });
    }

    c.bench_function("eval_case_and_5_conditions", |b| {
        let pool = make_pool_with_value(Segment::Integer(10));
        let conditions = (0..5)
            .map(|_| make_condition(ComparisonOperator::GreaterThan, serde_json::json!(5)))
            .collect();
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::And,
            conditions,
        };
        b.iter(|| {
            let _ = black_box(evaluate_case(&case, &pool));
        });
    });

    c.bench_function("eval_case_or_5_conditions", |b| {
        let pool = make_pool_with_value(Segment::Integer(10));
        let conditions = (0..5)
            .map(|_| make_condition(ComparisonOperator::GreaterThan, serde_json::json!(5)))
            .collect();
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::Or,
            conditions,
        };
        b.iter(|| {
            let _ = black_box(evaluate_case(&case, &pool));
        });
    });

    for (label, last_match) in [
        ("eval_cases_10_first_match", false),
        ("eval_cases_10_last_match", true),
    ] {
        c.bench_function(label, |b| {
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
            b.iter(|| {
                let _ = black_box(evaluate_cases(&cases, &pool));
            });
        });
    }
}

criterion_group!(benches, bench_condition);
criterion_main!(benches);
