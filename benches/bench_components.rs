use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::Value;
use wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};

use xworkflow::core::runtime_context::RuntimeContext;
use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};
use xworkflow::nodes::control_flow::{
    AnswerNodeExecutor, EndNodeExecutor, IfElseNodeExecutor, StartNodeExecutor,
};
use xworkflow::nodes::data_transform::{
    CodeNodeExecutor, TemplateTransformExecutor, VariableAggregatorExecutor,
};
use xworkflow::nodes::executor::NodeExecutor;
use xworkflow::sandbox::{
    CodeLanguage, CodeSandbox, ExecutionConfig, SandboxManager, SandboxManagerConfig,
    SandboxRequest, WasmSandbox, WasmSandboxConfig,
};

mod helpers;

use helpers::bench_runtime;

fn bench_node_executors(c: &mut Criterion) {
    let rt = bench_runtime();

    let mut group = c.benchmark_group("node/start");
    let mut pool = VariablePool::new();
    for i in 0..5 {
        pool.set(
            &Selector::new("start", format!("v{}", i)),
            Segment::String(format!("val{}", i)),
        );
    }
    let config = serde_json::json!({
        "variables": (0..5)
            .map(|i| serde_json::json!({"variable": format!("v{}", i), "label": "v", "type": "string", "required": false}))
            .collect::<Vec<_>>()
    });
    let executor = StartNodeExecutor;
    let context = RuntimeContext::default();
    group.bench_function("5_vars", |b| {
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("start", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("node/end");
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
    group.bench_function("5_outputs", |b| {
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("end", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("node/answer");
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
    group.bench_function("5_vars", |b| {
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("ans", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("node/ifelse");
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
    group.bench_function("5x5_conditions", |b| {
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("if1", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("node/template");
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
    group.bench_function("5_vars", |b| {
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("tpl", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("node/aggregator");
    let mut pool = VariablePool::new();
    pool.set(&Selector::new("n3", "out"), Segment::String("hit".into()));
    let variables = (0..5)
        .map(|i| serde_json::json!([format!("n{}", i), "out"]))
        .collect::<Vec<_>>();
    let config = serde_json::json!({"variables": variables});
    let executor = VariableAggregatorExecutor;
    let context = RuntimeContext::default();
    group.bench_function("5_selectors", |b| {
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("agg", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });
    group.finish();

    let mut group = c.benchmark_group("node/code");
    let pool = VariablePool::new();
    let config = serde_json::json!({
        "code": "function main(inputs) { return { result: 1 + 1 }; }",
        "language": "javascript"
    });
    let executor = CodeNodeExecutor::new();
    let context = RuntimeContext::default();
    group.bench_function("js_simple", |b| {
        b.to_async(&rt).iter(|| async {
            let result = executor.execute("code", &config, &pool, &context).await.unwrap();
            black_box(result.outputs.clone());
        });
    });
    group.finish();
}

fn bench_js_sandbox(c: &mut Criterion) {
    let rt = bench_runtime();
    let manager = SandboxManager::new(SandboxManagerConfig::default());

    let noop = "function main(inputs) { return {}; }".to_string();
    let arithmetic = "function main(inputs) { return { result: inputs.a + inputs.b }; }".to_string();
    let string_ops = "function main(inputs) { return { out: (inputs.s + inputs.s).split('').join('') }; }".to_string();
    let json_ops = "function main(inputs) { return { out: JSON.stringify(JSON.parse(inputs.s)) }; }".to_string();
    let array_100 = "function main(inputs) { return { sum: inputs.arr.reduce((a,b)=>a+b,0) }; }".to_string();
    let array_1000 = array_100.clone();
    let sha256 = "function main(inputs) { return { hash: crypto.sha256(inputs.s) }; }".to_string();
    let uuid = "function main(inputs) { return { id: uuidv4() }; }".to_string();
    let base64 = "function main(inputs) { return { out: btoa(inputs.s) }; }".to_string();

    let input_small = serde_json::json!({"a": 1, "b": 2, "s": "hello", "arr": (0..100).collect::<Vec<i32>>()});
    let input_large = serde_json::json!({"arr": (0..1000).collect::<Vec<i32>>(), "s": "x".repeat(1024)});

    let cases = vec![
        ("noop", noop, input_small.clone()),
        ("arithmetic", arithmetic, input_small.clone()),
        ("string", string_ops, input_small.clone()),
        ("json", json_ops, serde_json::json!({"s": "{\"a\":1}"})),
        ("array_100", array_100, input_small.clone()),
        ("array_1000", array_1000, input_large.clone()),
        ("sha256", sha256, serde_json::json!({"s": "hello"})),
        ("uuid", uuid, serde_json::json!({})),
        ("base64", base64, serde_json::json!({"s": "hello"})),
    ];

    let mut group = c.benchmark_group("sandbox/js");
    for (name, code, input) in cases {
        group.bench_function(name, |b| {
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

    group.bench_function("input_10k", |b| {
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

    group.bench_function("input_100k", |b| {
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

    group.finish();
}

const BASIC_WAT: &str = r#"(module
    (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
    (func (export "alloc") (param $size i32) (result i32)
    (local $addr i32)
    global.get $heap
    local.set $addr
    global.get $heap
    local.get $size
    i32.add
    global.set $heap
    local.get $addr)
    (func (export "dealloc") (param i32 i32))
    (data (i32.const 0) "{\"result\":42}")
    (data (i32.const 32) "\00\00\00\00\0d\00\00\00")
    (func (export "main") (param i32 i32) (result i32)
    (i32.const 32))
)"#;

fn execute_precompiled(engine: &Engine, module: &Module, inputs: &Value) -> Value {
    let mut linker = Linker::new(engine);
    linker
        .func_wrap("env", "abort", |code: i32| -> anyhow::Result<()> {
            Err(anyhow::anyhow!("abort({})", code))
        })
        .unwrap();

    let limits = StoreLimitsBuilder::new()
        .memory_size(64 * 1024 * 256)
        .build();
    let mut store = Store::new(engine, limits);
    store.limiter(|state| state);

    let instance = linker.instantiate(&mut store, module).unwrap();
    let memory = instance.get_memory(&mut store, "memory").unwrap();
    let alloc = instance.get_typed_func::<i32, i32>(&mut store, "alloc").unwrap();
    let dealloc = instance
        .get_typed_func::<(i32, i32), ()>(&mut store, "dealloc")
        .unwrap();
    let main = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "main")
        .unwrap();

    let input_bytes = serde_json::to_vec(inputs).unwrap();
    let input_len = input_bytes.len() as i32;
    let input_ptr = alloc.call(&mut store, input_len).unwrap();
    memory
        .write(&mut store, input_ptr as usize, &input_bytes)
        .unwrap();

    let result_ptr = main.call(&mut store, (input_ptr, input_len)).unwrap();

    let mut header = [0u8; 8];
    memory
        .read(&mut store, result_ptr as usize, &mut header)
        .unwrap();
    let out_ptr = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    let out_len = i32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let mut out_bytes = vec![0u8; out_len];
    memory.read(&mut store, out_ptr, &mut out_bytes).unwrap();

    let output: Value = serde_json::from_slice(&out_bytes).unwrap();

    let _ = dealloc.call(&mut store, (input_ptr, input_len));
    let _ = dealloc.call(&mut store, (out_ptr as i32, out_len as i32));
    let _ = dealloc.call(&mut store, (result_ptr, 8));

    output
}

fn bench_wasm_sandbox(c: &mut Criterion) {
    let rt = bench_runtime();
    let sandbox = WasmSandbox::new(WasmSandboxConfig::default());

    let mut group = c.benchmark_group("sandbox/wasm");
    group.bench_function("compile_execute", |b| {
        let request = SandboxRequest {
            code: BASIC_WAT.to_string(),
            language: CodeLanguage::Wasm,
            inputs: serde_json::json!({"value": 1}),
            config: ExecutionConfig::default(),
        };
        b.to_async(&rt).iter(|| async {
            let result = sandbox.execute(request.clone()).await.unwrap();
            black_box(result.output);
        });
    });

    group.bench_function("precompiled", |b| {
        let engine = Engine::default();
        let bytes = wat::parse_str(BASIC_WAT).unwrap();
        let module = Module::new(&engine, bytes).unwrap();
        let input = serde_json::json!({"value": 1});
        b.iter(|| {
            let output = execute_precompiled(&engine, &module, &input);
            black_box(output);
        });
    });

    group.bench_function("validation", |b| {
        b.to_async(&rt).iter(|| async {
            let _ = sandbox.validate(BASIC_WAT, CodeLanguage::Wasm).await.unwrap();
        });
    });

    let mut fuel_config = WasmSandboxConfig::default();
    fuel_config.enable_fuel = true;
    let fuel_sandbox = WasmSandbox::new(fuel_config);
    let mut no_fuel_config = WasmSandboxConfig::default();
    no_fuel_config.enable_fuel = false;
    let no_fuel_sandbox = WasmSandbox::new(no_fuel_config);

    for (label, sb) in [("enabled", &fuel_sandbox), ("disabled", &no_fuel_sandbox)] {
        group.bench_with_input(BenchmarkId::new("fuel_overhead", label), &label, |b, _| {
            let request = SandboxRequest {
                code: BASIC_WAT.to_string(),
                language: CodeLanguage::Wasm,
                inputs: serde_json::json!({"value": 1}),
                config: ExecutionConfig::default(),
            };
            b.to_async(&rt).iter(|| async {
                let result = sb.execute(request.clone()).await.unwrap();
                black_box(result.output);
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
    targets = bench_node_executors, bench_js_sandbox, bench_wasm_sandbox
}
criterion_main!(benches);
