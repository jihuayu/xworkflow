use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};
use xworkflow::template::{render_jinja2, render_template};

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

fn bench_template(c: &mut Criterion) {
    c.bench_function("dify_no_vars", |b| {
        let pool = VariablePool::new();
        b.iter(|| {
            let _ = black_box(render_template("static text", &pool));
        });
    });

    c.bench_function("dify_1_var", |b| {
        let pool = build_pool_with_vars(1);
        let tpl = build_dify_template(1);
        b.iter(|| {
            let _ = black_box(render_template(&tpl, &pool));
        });
    });

    for vars in [10usize, 50] {
        c.bench_with_input(BenchmarkId::new("dify_vars", vars), &vars, |b, vars| {
            let pool = build_pool_with_vars(*vars);
            let tpl = build_dify_template(*vars);
            b.iter(|| {
                let _ = black_box(render_template(&tpl, &pool));
            });
        });
    }

    c.bench_function("dify_large_1kb", |b| {
        let pool = build_pool_with_vars(5);
        let mut tpl = "x".repeat(1024);
        tpl.push_str(" {{#node.v0#}} ");
        b.iter(|| {
            let _ = black_box(render_template(&tpl, &pool));
        });
    });

    c.bench_function("jinja2_simple", |b| {
        let vars = build_jinja_vars(1);
        b.iter(|| {
            let _ = black_box(render_jinja2("Hello {{ v0 }}", &vars));
        });
    });

    c.bench_function("jinja2_loop_100", |b| {
        let mut vars = HashMap::new();
        let items: Vec<String> = (0..100).map(|i| format!("i{}", i)).collect();
        vars.insert("items".to_string(), serde_json::json!(items));
        b.iter(|| {
            let _ = black_box(render_jinja2("{% for i in items %}{{ i }} {% endfor %}", &vars));
        });
    });

    c.bench_function("jinja2_conditional", |b| {
        let vars = build_jinja_vars(1);
        b.iter(|| {
            let _ = black_box(render_jinja2("{% if v0 %}yes{% else %}no{% endif %}", &vars));
        });
    });

    c.bench_function("jinja2_complex", |b| {
        let mut vars = HashMap::new();
        vars.insert("items".to_string(), serde_json::json!([1, 2, 3]));
        vars.insert("flag".to_string(), serde_json::json!(true));
        let tpl = r#"{% for i in items %}{% if flag %}{{ i }}{% endif %}{% endfor %}"#;
        b.iter(|| {
            let _ = black_box(render_jinja2(tpl, &vars));
        });
    });
}

criterion_group!(benches, bench_template);
criterion_main!(benches);
