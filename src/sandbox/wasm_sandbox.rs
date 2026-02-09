//! WASM sandbox implementation using wasmtime.
//!
//! Implements a custom ABI for code nodes with JSON I/O via linear memory.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use base64::Engine as _;
use serde_json::Value;
use wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};

use super::error::SandboxError;
use super::types::*;

/// WASM sandbox configuration
#[derive(Clone, Debug)]
pub struct WasmSandboxConfig {
    /// Max wasm binary size (bytes)
    pub max_wasm_size: usize,
    /// Default execution timeout
    pub default_timeout: Duration,
    /// Max linear memory pages (64KB per page)
    pub max_memory_pages: u32,
    /// Max fuel for instruction counting
    pub max_fuel: u64,
    /// Enable fuel metering
    pub enable_fuel: bool,
}

impl Default for WasmSandboxConfig {
    fn default() -> Self {
        Self {
            max_wasm_size: 5 * 1024 * 1024,
            default_timeout: Duration::from_secs(30),
            max_memory_pages: 256,
            max_fuel: 1_000_000_000,
            enable_fuel: true,
        }
    }
}

struct WasmStoreState {
    limits: wasmtime::StoreLimits,
}

/// WASM sandbox implementation
pub struct WasmSandbox {
    engine: Engine,
    config: WasmSandboxConfig,
    stats: Arc<RwLock<SandboxStats>>,
}

impl WasmSandbox {
    pub fn new(config: WasmSandboxConfig) -> Self {
        let engine = build_engine(&config)
            .unwrap_or_else(|_| Engine::default());
        Self {
            engine,
            config,
            stats: Arc::new(RwLock::new(SandboxStats::default())),
        }
    }

    fn decode_wasm(&self, code: &str) -> Result<Vec<u8>, SandboxError> {
        let trimmed = code.trim();
        if trimmed.is_empty() {
            return Err(SandboxError::CompilationError("Empty wasm code".into()));
        }

        // Try base64 first
        let base64_result = base64::engine::general_purpose::STANDARD
            .decode(trimmed.as_bytes());
        if let Ok(bytes) = base64_result {
            return Ok(bytes);
        }

        // Fallback to WAT (dev/test convenience)
        wat::parse_str(trimmed).map_err(|e| SandboxError::CompilationError(e.to_string()))
    }

    fn validate_wasm_bytes(&self, bytes: &[u8]) -> Result<(), SandboxError> {
        if bytes.len() > self.config.max_wasm_size {
            return Err(SandboxError::CodeTooLarge {
                max: self.config.max_wasm_size,
                actual: bytes.len(),
            });
        }
        if bytes.len() < 4 || !bytes.starts_with(b"\0asm") {
            return Err(SandboxError::CompilationError(
                "Invalid WASM magic header".into(),
            ));
        }
        Ok(())
    }

    async fn update_stats(
        &self,
        result: &Result<SandboxResult, SandboxError>,
        execution_time: Duration,
    ) {
        let mut stats = self.stats.write().await;
        stats.total_executions += 1;
        match result {
            Ok(_) => stats.successful_executions += 1,
            Err(_) => stats.failed_executions += 1,
        }
        if stats.total_executions == 1 {
            stats.avg_execution_time = execution_time;
        } else {
            let total_ns = stats.avg_execution_time.as_nanos() as u64
                * (stats.total_executions - 1)
                + execution_time.as_nanos() as u64;
            stats.avg_execution_time = Duration::from_nanos(total_ns / stats.total_executions);
        }
    }
}

#[async_trait::async_trait]
impl CodeSandbox for WasmSandbox {
    fn sandbox_type(&self) -> SandboxType {
        SandboxType::Wasm
    }

    fn supported_languages(&self) -> Vec<CodeLanguage> {
        vec![CodeLanguage::Wasm]
    }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        if request.language != CodeLanguage::Wasm {
            return Err(SandboxError::UnsupportedLanguage(request.language));
        }

        let timeout = if request.config.timeout.as_secs() == 0 {
            self.config.default_timeout
        } else {
            request.config.timeout
        };

        let code = request.code.clone();
        let inputs = request.inputs.clone();
        let config = self.config.clone();
        let engine = self.engine.clone();

        let start_time = Instant::now();

        let handle = tokio::task::spawn_blocking(move || {
            let wasm_bytes = {
                let sandbox = WasmSandbox::new(config.clone());
                let bytes = sandbox.decode_wasm(&code)?;
                sandbox.validate_wasm_bytes(&bytes)?;
                bytes
            };

            let module = Module::new(&engine, &wasm_bytes)
                .map_err(|e| SandboxError::CompilationError(e.to_string()))?;

            let mut linker = Linker::new(&engine);
            linker
                .func_wrap("env", "abort", |code: i32| -> anyhow::Result<()> {
                    Err(anyhow::anyhow!("abort({})", code))
                })
                .map_err(|e| SandboxError::InternalError(e.to_string()))?;

            let limits = StoreLimitsBuilder::new()
                .memory_size(config.max_memory_pages as usize * 64 * 1024)
                .build();

            let mut store = Store::new(&engine, WasmStoreState { limits });
            store.limiter(|state| &mut state.limits);

            if config.enable_fuel {
                store
                    .set_fuel(config.max_fuel)
                    .map_err(|e| SandboxError::InternalError(e.to_string()))?;
            }

            let instance = linker
                .instantiate(&mut store, &module)
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| SandboxError::ExecutionError("Missing export: memory".into()))?;

            let alloc = instance
                .get_typed_func::<i32, i32>(&mut store, "alloc")
                .map_err(|_| SandboxError::ExecutionError("Missing export: alloc".into()))?;
            let dealloc = instance
                .get_typed_func::<(i32, i32), ()>(&mut store, "dealloc")
                .map_err(|_| SandboxError::ExecutionError("Missing export: dealloc".into()))?;
            let main = instance
                .get_typed_func::<(i32, i32), i32>(&mut store, "main")
                .map_err(|_| SandboxError::ExecutionError("Missing export: main".into()))?;

            let input_bytes = serde_json::to_vec(&inputs)
                .map_err(|e| SandboxError::SerializationError(e.to_string()))?;
            let input_len = input_bytes.len() as i32;
            let input_ptr = alloc
                .call(&mut store, input_len)
                .map_err(|e| map_wasm_error(&e))?;
            memory
                .write(&mut store, input_ptr as usize, &input_bytes)
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let result_ptr = main
                .call(&mut store, (input_ptr, input_len))
                .map_err(|e| map_wasm_error(&e))?;

            let mut header = [0u8; 8];
            memory
                .read(&mut store, result_ptr as usize, &mut header)
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let out_ptr = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
            let out_len = i32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;

            let mut out_bytes = vec![0u8; out_len];
            memory
                .read(&mut store, out_ptr, &mut out_bytes)
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let output: Value = serde_json::from_slice(&out_bytes)
                .map_err(|e| SandboxError::SerializationError(e.to_string()))?;

            let _ = dealloc.call(&mut store, (input_ptr, input_len));
            let _ = dealloc.call(&mut store, (out_ptr as i32, out_len as i32));
            let _ = dealloc.call(&mut store, (result_ptr, 8));

            Ok(SandboxResult {
                success: true,
                output,
                stdout: String::new(),
                stderr: String::new(),
                execution_time: start_time.elapsed(),
                memory_used: input_bytes.len() + out_len,
                error: None,
            })
        });

        let result = match tokio::time::timeout(timeout, handle).await {
            Ok(join_result) => match join_result {
                Ok(res) => res,
                Err(e) => Err(SandboxError::ExecutionError(e.to_string())),
            },
            Err(_) => Err(SandboxError::ExecutionTimeout),
        };

        self.update_stats(&result, start_time.elapsed()).await;
        result
    }

    async fn validate(&self, code: &str, _language: CodeLanguage) -> Result<(), SandboxError> {
        let wasm_bytes = self.decode_wasm(code)?;
        self.validate_wasm_bytes(&wasm_bytes)?;
        Module::new(&self.engine, &wasm_bytes)
            .map_err(|e| SandboxError::CompilationError(e.to_string()))?;
        Ok(())
    }
}

fn build_engine(config: &WasmSandboxConfig) -> Result<Engine, SandboxError> {
    let mut cfg = wasmtime::Config::new();
    if config.enable_fuel {
        cfg.consume_fuel(true);
    }
    Engine::new(&cfg).map_err(|e| SandboxError::InternalError(e.to_string()))
}

fn map_wasm_error(err: &anyhow::Error) -> SandboxError {
    let msg = err.to_string();
    if msg.to_lowercase().contains("fuel") {
        SandboxError::ExecutionError("Fuel exhausted".into())
    } else if msg.to_lowercase().contains("memory") {
        SandboxError::MemoryLimitExceeded
    } else {
        SandboxError::ExecutionError(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

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
        let bytes = wat::parse_str(wat).unwrap();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

        fn fuel_loop_wasm_base64() -> String {
                let wat = r#"(module
    (memory (export "memory") 1)
    (func (export "alloc") (param i32) (result i32) (i32.const 0))
    (func (export "dealloc") (param i32 i32))
    (func (export "main") (param i32 i32) (result i32)
        (local $i i32)
        (local.set $i (i32.const 0))
        (loop $loop
            local.get $i
            i32.const 1
            i32.add
            local.tee $i
            i32.const 1000000
            i32.lt_s
            br_if $loop
        )
        (i32.const 0))
)"#;
        let bytes = wat::parse_str(wat).unwrap();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

        fn long_loop_wasm_base64() -> String {
                let wat = r#"(module
    (memory (export "memory") 1)
    (func (export "alloc") (param i32) (result i32) (i32.const 0))
    (func (export "dealloc") (param i32 i32))
    (func (export "main") (param i32 i32) (result i32)
        (local $i i32)
        (local.set $i (i32.const 0))
        (loop $loop
            local.get $i
            i32.const 1
            i32.add
            local.tee $i
            i32.const 100000000
            i32.lt_s
            br_if $loop
        )
        (i32.const 0))
)"#;
                let bytes = wat::parse_str(wat).unwrap();
                base64::engine::general_purpose::STANDARD.encode(bytes)
        }

    fn memory_grow_wasm_base64() -> String {
          let wat = r#"(module
      (memory (export "memory") 1)
      (func (export "alloc") (param i32) (result i32) (i32.const 0))
      (func (export "dealloc") (param i32 i32))
      (func (export "main") (param i32 i32) (result i32)
    (drop (memory.grow (i32.const 1000)))
    (i32.const 0))
)"#;
        let bytes = wat::parse_str(wat).unwrap();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[tokio::test]
    async fn test_wasm_sandbox_basic() {
        let sandbox = WasmSandbox::new(WasmSandboxConfig::default());
        let request = SandboxRequest {
            code: basic_wasm_base64(),
            language: CodeLanguage::Wasm,
            inputs: serde_json::json!({"value": 1}),
            config: ExecutionConfig::default(),
        };
        let result = sandbox.execute(request).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output["result"], serde_json::json!(42));
    }

    #[cfg_attr(target_os = "windows", ignore)]
    #[tokio::test]
    async fn test_wasm_sandbox_fuel_limit() {
        let mut config = WasmSandboxConfig::default();
        config.enable_fuel = true;
        config.max_fuel = 10_000;
        let sandbox = WasmSandbox::new(config);
        let request = SandboxRequest {
            code: long_loop_wasm_base64(),
            language: CodeLanguage::Wasm,
            inputs: serde_json::json!({}),
            config: ExecutionConfig::default(),
        };
        let result = sandbox.execute(request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_wasm_sandbox_memory_limit() {
        let mut config = WasmSandboxConfig::default();
        config.max_memory_pages = 1;
        let sandbox = WasmSandbox::new(config);
        let request = SandboxRequest {
            code: memory_grow_wasm_base64(),
            language: CodeLanguage::Wasm,
            inputs: serde_json::json!({}),
            config: ExecutionConfig::default(),
        };
        let result = sandbox.execute(request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_wasm_sandbox_timeout() {
        let mut config = WasmSandboxConfig::default();
        config.enable_fuel = false;
        config.default_timeout = Duration::from_millis(1);
        let sandbox = WasmSandbox::new(config);
        let request = SandboxRequest {
            code: fuel_loop_wasm_base64(),
            language: CodeLanguage::Wasm,
            inputs: serde_json::json!({}),
            config: ExecutionConfig {
                timeout: Duration::from_millis(1),
                ..ExecutionConfig::default()
            },
        };
        let result = sandbox.execute(request).await;
        assert!(matches!(result, Err(SandboxError::ExecutionTimeout)));
    }

    #[tokio::test]
    async fn test_wasm_sandbox_invalid_wasm() {
        let sandbox = WasmSandbox::new(WasmSandboxConfig::default());
        let request = SandboxRequest {
            code: "AAAA".to_string(),
            language: CodeLanguage::Wasm,
            inputs: serde_json::json!({}),
            config: ExecutionConfig::default(),
        };
        let result = sandbox.execute(request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_wasm_sandbox_missing_export() {
                let wat = r#"(module
    (memory (export "memory") 1)
)"#;
        let bytes = wat::parse_str(wat).unwrap();
        let code = base64::engine::general_purpose::STANDARD.encode(bytes);
        let sandbox = WasmSandbox::new(WasmSandboxConfig::default());
        let request = SandboxRequest {
            code,
            language: CodeLanguage::Wasm,
            inputs: serde_json::json!({}),
            config: ExecutionConfig::default(),
        };
        let result = sandbox.execute(request).await;
        assert!(result.is_err());
    }
}
