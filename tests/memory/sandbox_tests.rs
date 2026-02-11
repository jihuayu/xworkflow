#[cfg(feature = "builtin-sandbox-js")]
use xworkflow::sandbox::{BuiltinSandbox, BuiltinSandboxConfig};
#[cfg(feature = "builtin-sandbox-wasm")]
use xworkflow::sandbox::{WasmSandbox, WasmSandboxConfig};
#[cfg(any(feature = "builtin-sandbox-js", feature = "builtin-sandbox-wasm"))]
use xworkflow::sandbox::CodeSandbox;
#[cfg(any(feature = "builtin-sandbox-js", feature = "builtin-sandbox-wasm"))]
use xworkflow::sandbox::{CodeLanguage, ExecutionConfig, SandboxRequest};
#[cfg(any(feature = "builtin-sandbox-js", feature = "builtin-sandbox-wasm"))]
use serde_json::json;

use super::helpers::dhat_guard;

#[cfg(feature = "builtin-sandbox-wasm")]
use base64::Engine as _;
#[cfg(feature = "builtin-sandbox-wasm")]
use xworkflow_sandbox_wasm::parse_wat;

#[cfg(feature = "builtin-sandbox-wasm")]
fn basic_wasm_base64() -> String {
    let wat = r#"(module
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
    let bytes = parse_wat(wat).expect("parse wat");
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[tokio::test]
#[cfg(feature = "builtin-sandbox-wasm")]
async fn test_wasm_store_cleanup_after_execution() {
    let _guard = dhat_guard();
    let sandbox = WasmSandbox::new(WasmSandboxConfig::default());
    let request = SandboxRequest {
        code: basic_wasm_base64(),
        language: CodeLanguage::Wasm,
        inputs: json!({"value": 1}),
        config: ExecutionConfig::default(),
    };

    let before = dhat::HeapStats::get();
    let result = sandbox.execute(request).await.expect("wasm execute");
    let after = dhat::HeapStats::get();

    assert!(result.success);
    assert!(after.curr_bytes <= before.curr_bytes + 50_000_000);
}

#[tokio::test]
#[cfg(feature = "builtin-sandbox-wasm")]
async fn test_wasm_repeated_execution_memory() {
    let _guard = dhat_guard();
    let sandbox = WasmSandbox::new(WasmSandboxConfig::default());

    for _ in 0..5 {
        let request = SandboxRequest {
            code: basic_wasm_base64(),
            language: CodeLanguage::Wasm,
            inputs: json!({}),
            config: ExecutionConfig::default(),
        };
        let _ = sandbox.execute(request).await.expect("warmup");
    }

    let baseline = dhat::HeapStats::get().curr_bytes.max(1);

    for _ in 0..25 {
        let request = SandboxRequest {
            code: basic_wasm_base64(),
            language: CodeLanguage::Wasm,
            inputs: json!({}),
            config: ExecutionConfig::default(),
        };
        let _ = sandbox.execute(request).await.expect("run");
    }

    let final_stats = dhat::HeapStats::get();
    let growth = final_stats.curr_bytes as f64 / baseline as f64;
    assert!(growth < 2.0, "wasm memory growth too high: {:.2}x", growth);
}

#[tokio::test]
#[cfg(feature = "builtin-sandbox-js")]
async fn test_js_context_cleanup_after_execution() {
    let _guard = dhat_guard();
    let sandbox = BuiltinSandbox::new(BuiltinSandboxConfig::default());

    let request = SandboxRequest {
        code: "function main(inputs) { return { result: inputs.value + 1 }; }".to_string(),
        language: CodeLanguage::JavaScript,
        inputs: json!({"value": 1}),
        config: ExecutionConfig::default(),
    };

    let before = dhat::HeapStats::get();
    let result = sandbox.execute(request).await.expect("js execute");
    let after = dhat::HeapStats::get();

    assert!(result.success);
    assert!(after.curr_bytes <= before.curr_bytes + 50_000_000);
}

#[tokio::test]
#[cfg(feature = "builtin-sandbox-js")]
async fn test_js_repeated_execution_memory() {
    let _guard = dhat_guard();
    let sandbox = BuiltinSandbox::new(BuiltinSandboxConfig::default());

    for _ in 0..5 {
        let request = SandboxRequest {
            code: "function main(inputs) { return { result: inputs.value + 1 }; }".to_string(),
            language: CodeLanguage::JavaScript,
            inputs: json!({"value": 1}),
            config: ExecutionConfig::default(),
        };
        let _ = sandbox.execute(request).await.expect("warmup");
    }

    let baseline = dhat::HeapStats::get().curr_bytes.max(1);

    for _ in 0..25 {
        let request = SandboxRequest {
            code: "function main(inputs) { return { result: inputs.value + 1 }; }".to_string(),
            language: CodeLanguage::JavaScript,
            inputs: json!({"value": 1}),
            config: ExecutionConfig::default(),
        };
        let _ = sandbox.execute(request).await.expect("run");
    }

    let final_stats = dhat::HeapStats::get();
    let growth = final_stats.curr_bytes as f64 / baseline as f64;
    assert!(growth < 2.0, "js memory growth too high: {:.2}x", growth);
}
