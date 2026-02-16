use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::Value;
use xworkflow_sandbox_wasm::wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};

use xworkflow::sandbox::{
    CodeLanguage, CodeSandbox, ExecutionConfig, SandboxRequest, WasmSandbox, WasmSandboxConfig,
};

mod helpers;
use helpers::bench_runtime;

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
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .unwrap();
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

    c.bench_function("wasm_compile_and_execute", |b| {
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

    c.bench_function("wasm_instantiate_and_execute", |b| {
        let engine = Engine::default();
        let bytes = xworkflow_sandbox_wasm::parse_wat(BASIC_WAT).unwrap();
        let module = Module::new(&engine, bytes).unwrap();
        let input = serde_json::json!({"value": 1});
        b.iter(|| {
            let output = execute_precompiled(&engine, &module, &input);
            black_box(output);
        });
    });

    c.bench_function("wasm_validation_only", |b| {
        b.to_async(&rt).iter(|| async {
            sandbox
                .validate(BASIC_WAT, CodeLanguage::Wasm)
                .await
                .unwrap();
        });
    });

    let mut group = c.benchmark_group("wasm_fuel_overhead");
    let fuel_config = WasmSandboxConfig {
        enable_fuel: true,
        ..WasmSandboxConfig::default()
    };
    let fuel_sandbox = WasmSandbox::new(fuel_config);
    let no_fuel_config = WasmSandboxConfig {
        enable_fuel: false,
        ..WasmSandboxConfig::default()
    };
    let no_fuel_sandbox = WasmSandbox::new(no_fuel_config);

    for (label, sb) in [("enabled", &fuel_sandbox), ("disabled", &no_fuel_sandbox)] {
        group.bench_with_input(BenchmarkId::new("fuel", label), &label, |b, _| {
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

criterion_group!(benches, bench_wasm_sandbox);
criterion_main!(benches);
