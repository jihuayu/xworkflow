use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::Value;

use xworkflow::core::variable_pool::{Segment, VariablePool};

mod helpers;

use helpers::pool_factories::{make_pool_with_objects, make_pool_with_strings, make_realistic_pool};

fn bench_variable_pool(c: &mut Criterion) {
    c.bench_function("pool_set_string", |b| {
        let mut pool = VariablePool::new();
        let selector = vec!["n".to_string(), "k".to_string()];
        b.iter(|| {
            pool.set(black_box(&selector), Segment::String("value".into()));
        });
    });

    c.bench_function("pool_get_hit", |b| {
        let mut pool = VariablePool::new();
        let selector = vec!["n".to_string(), "k".to_string()];
        pool.set(&selector, Segment::String("value".into()));
        b.iter(|| {
            let _ = black_box(pool.get(&selector));
        });
    });

    c.bench_function("pool_get_miss", |b| {
        let pool = VariablePool::new();
        let selector = vec!["n".to_string(), "missing".to_string()];
        b.iter(|| {
            let _ = black_box(pool.get(&selector));
        });
    });

    c.bench_function("pool_set_node_outputs_5", |b| {
        let mut pool = VariablePool::new();
        let mut outputs = HashMap::new();
        for i in 0..5 {
            outputs.insert(format!("k{}", i), Value::String(format!("v{}", i)));
        }
        b.iter(|| {
            pool.set_node_outputs("node", black_box(&outputs));
        });
    });

    let mut group = c.benchmark_group("pool_clone");
    for size in [10usize, 50, 100, 500] {
        let pool = make_pool_with_strings(size, 16);
        group.bench_with_input(BenchmarkId::from_parameter(size), &pool, |b, pool| {
            b.iter(|| {
                let _ = black_box(pool.clone());
            });
        });
    }
    group.finish();

    c.bench_function("pool_clone_large_objects", |b| {
        let pool = make_pool_with_objects(50, 3);
        b.iter(|| {
            let _ = black_box(pool.clone());
        });
    });

    c.bench_function("pool_get_node_variables", |b| {
        let pool = make_realistic_pool(100);
        b.iter(|| {
            let _ = black_box(pool.get_node_variables("node42"));
        });
    });

    c.bench_function("pool_append_array", |b| {
        let mut pool = VariablePool::new();
        let selector = vec!["n".to_string(), "arr".to_string()];
        pool.set(&selector, Segment::ArrayAny(vec![]));
        b.iter(|| {
            pool.append(&selector, Segment::Integer(1));
        });
    });

    c.bench_function("pool_has_check", |b| {
        let mut pool = VariablePool::new();
        let selector = vec!["n".to_string(), "k".to_string()];
        pool.set(&selector, Segment::String("value".into()));
        b.iter(|| {
            let _ = black_box(pool.has(&selector));
        });
    });
}

criterion_group!(benches, bench_variable_pool);
criterion_main!(benches);
