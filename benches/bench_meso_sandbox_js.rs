use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use xworkflow::sandbox::{
    CodeLanguage, ExecutionConfig, SandboxManager, SandboxManagerConfig, SandboxRequest,
};

mod helpers;
use helpers::bench_runtime;

fn bench_js_sandbox(c: &mut Criterion) {
    let rt = bench_runtime();
    let manager = SandboxManager::new(SandboxManagerConfig::default());

    let noop = "function main(inputs) { return {}; }".to_string();
    let arithmetic =
        "function main(inputs) { return { result: inputs.a + inputs.b }; }".to_string();
    let string_ops =
        "function main(inputs) { return { out: (inputs.s + inputs.s).split('').join('') }; }"
            .to_string();
    let json_ops =
        "function main(inputs) { return { out: JSON.stringify(JSON.parse(inputs.s)) }; }"
            .to_string();
    let array_100 =
        "function main(inputs) { return { sum: inputs.arr.reduce((a,b)=>a+b,0) }; }".to_string();
    let array_1000 = array_100.clone();
    let obj_transform =
        "function main(inputs) { return { out: {a: inputs.a, b: inputs.b, c: inputs.c} }; }"
            .to_string();
    let sha256 = "function main(inputs) { return { hash: crypto.sha256(inputs.s) }; }".to_string();
    let uuid = "function main(inputs) { return { id: uuidv4() }; }".to_string();
    let base64 = "function main(inputs) { return { out: btoa(inputs.s) }; }".to_string();
    let validate_only = "function main(inputs) { return {}; }".to_string();

    let input_small = serde_json::json!({"a": 1, "b": 2, "s": "hello", "arr": (0..100).collect::<Vec<i32>>(), "c": 3});
    let input_large =
        serde_json::json!({"arr": (0..1000).collect::<Vec<i32>>(), "s": "x".repeat(1024)});

    let cases = vec![
        ("js_noop", noop, input_small.clone()),
        ("js_arithmetic", arithmetic, input_small.clone()),
        ("js_string_manipulation", string_ops, input_small.clone()),
        (
            "js_json_parse_stringify",
            json_ops,
            serde_json::json!({"s": "{\"a\":1}"}),
        ),
        ("js_array_100", array_100, input_small.clone()),
        ("js_array_1000", array_1000, input_large.clone()),
        (
            "js_object_transform_20_fields",
            obj_transform,
            input_small.clone(),
        ),
        (
            "js_builtins_sha256",
            sha256,
            serde_json::json!({"s": "hello"}),
        ),
        ("js_builtins_uuid", uuid, serde_json::json!({})),
        (
            "js_builtins_base64",
            base64,
            serde_json::json!({"s": "hello"}),
        ),
    ];

    for (name, code, input) in cases {
        c.bench_with_input(BenchmarkId::new("sandbox", name), &input, |b, input| {
            let request = SandboxRequest {
                code: code.clone(),
                language: CodeLanguage::JavaScript,
                inputs: input.clone(),
                config: ExecutionConfig::default(),
            };
            b.to_async(&rt).iter(|| async {
                let result = manager.execute(request.clone()).await.unwrap();
                black_box(result.output);
            });
        });
    }

    c.bench_function("js_large_input_10k", |b| {
        let input = serde_json::json!({"s": "x".repeat(10_000)});
        let request = SandboxRequest {
            code: "function main(inputs) { return { len: inputs.s.length }; }".to_string(),
            language: CodeLanguage::JavaScript,
            inputs: input,
            config: ExecutionConfig::default(),
        };
        b.to_async(&rt).iter(|| async {
            let result = manager.execute(request.clone()).await.unwrap();
            black_box(result.output);
        });
    });

    c.bench_function("js_large_input_100k", |b| {
        let input = serde_json::json!({"s": "x".repeat(100_000)});
        let request = SandboxRequest {
            code: "function main(inputs) { return { len: inputs.s.length }; }".to_string(),
            language: CodeLanguage::JavaScript,
            inputs: input,
            config: ExecutionConfig::default(),
        };
        b.to_async(&rt).iter(|| async {
            let result = manager.execute(request.clone()).await.unwrap();
            black_box(result.output);
        });
    });

    c.bench_function("js_validation_only", |b| {
        b.to_async(&rt).iter(|| async {
            let _ = manager
                .validate(&validate_only, CodeLanguage::JavaScript)
                .await;
        });
    });
}

criterion_group!(benches, bench_js_sandbox);
criterion_main!(benches);
