//! WASM sandbox implementation using wasmtime.
//!
//! Implements a custom ABI for code nodes with JSON I/O via linear memory.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use base64::Engine as _;
use serde_json::Value;
use wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};

use xworkflow_types::sandbox::*;

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

    /// Max input JSON size (bytes)
    pub max_input_bytes: usize,
    /// Max output JSON size (bytes)
    pub max_output_bytes: usize,
    /// Allow WASI (always false)
    pub allow_wasi: bool,
    /// Fuel warning threshold
    pub fuel_warning_threshold: u64,
}

impl Default for WasmSandboxConfig {
    fn default() -> Self {
        Self {
            max_wasm_size: 5 * 1024 * 1024,
            default_timeout: Duration::from_secs(30),
            max_memory_pages: 256,
            max_fuel: 1_000_000_000,
            enable_fuel: true,
            max_input_bytes: 1 * 1024 * 1024,
            max_output_bytes: 1 * 1024 * 1024,
            allow_wasi: false,
            fuel_warning_threshold: 800_000_000,
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
        let engine = build_engine(&config).unwrap_or_else(|_| Engine::default());
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
        let base64_result = base64::engine::general_purpose::STANDARD.decode(trimmed.as_bytes());
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
            return Err(SandboxError::CompilationError("Invalid WASM magic header".into()));
        }
        Ok(())
    }

    async fn update_stats(&self, result: &Result<SandboxResult, SandboxError>, execution_time: Duration) {
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
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let dealloc = instance
                .get_typed_func::<(i32, i32), ()>(&mut store, "dealloc")
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let main = instance
                .get_typed_func::<(i32, i32), i32>(&mut store, "main")
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let input_json = serde_json::to_string(&inputs)
                .map_err(|e| SandboxError::SerializationError(e.to_string()))?;
            if input_json.len() > config.max_input_bytes {
                return Err(SandboxError::InputTooLarge {
                    max: config.max_input_bytes,
                    actual: input_json.len(),
                });
            }

            let input_bytes = input_json.as_bytes();
            let input_ptr = alloc
                .call(&mut store, input_bytes.len() as i32)
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            memory
                .write(&mut store, input_ptr as usize, input_bytes)
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let output_ptr = main
                .call(&mut store, (input_ptr, input_bytes.len() as i32))
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            // Read result header: [out_ptr, out_len]
            let mut header = [0u8; 8];
            memory
                .read(&mut store, output_ptr as usize, &mut header)
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;
            let out_ptr = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
            let out_len = i32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;

            if out_len > config.max_output_bytes {
                return Err(SandboxError::OutputTooLarge {
                    max: config.max_output_bytes,
                    actual: out_len,
                });
            }

            let mut output_buf = vec![0u8; out_len];
            memory
                .read(&mut store, out_ptr, &mut output_buf)
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            // Clean up
            dealloc
                .call(&mut store, (input_ptr, input_bytes.len() as i32))
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;
            dealloc
                .call(&mut store, (out_ptr as i32, out_len as i32))
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;
            dealloc
                .call(&mut store, (output_ptr, 8))
                .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

            let output_json = String::from_utf8(output_buf)
                .map_err(|e| SandboxError::SerializationError(e.to_string()))?;
            let output: Value = serde_json::from_str(&output_json)
                .map_err(|e| SandboxError::SerializationError(e.to_string()))?;

            Ok(SandboxResult {
                success: true,
                output,
                stdout: String::new(),
                stderr: String::new(),
                execution_time: Duration::from_secs(0),
                memory_used: 0,
                error: None,
            })
        });

        let result = tokio::time::timeout(timeout, handle)
            .await
            .map_err(|_| SandboxError::ExecutionTimeout)?
            .map_err(|e| SandboxError::ExecutionError(e.to_string()))?;

        let execution_time = start_time.elapsed();

        self.update_stats(&result, execution_time).await;

        result.map(|mut r| {
            r.execution_time = execution_time;
            r
        })
    }

    async fn validate(&self, code: &str, language: CodeLanguage) -> Result<(), SandboxError> {
        if language != CodeLanguage::Wasm {
            return Err(SandboxError::UnsupportedLanguage(language));
        }
        let bytes = self.decode_wasm(code)?;
        self.validate_wasm_bytes(&bytes)
    }

    async fn health_check(&self) -> Result<HealthStatus, SandboxError> {
        let wasm = r#"(module
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
                local.get $addr
            )
            (func (export "dealloc") (param i32) (param i32))
            (data (i32.const 0) "{\"ok\":true}")
            (data (i32.const 32) "\00\00\00\00\0b\00\00\00")
            (func (export "main") (param i32 i32) (result i32)
                (i32.const 32)
            )
        )"#;

        let request = SandboxRequest {
            code: wasm.to_string(),
            language: CodeLanguage::Wasm,
            inputs: serde_json::json!({}),
            config: ExecutionConfig {
                timeout: Duration::from_secs(2),
                ..ExecutionConfig::default()
            },
        };

        match self.execute(request).await {
            Ok(r) if r.success => Ok(HealthStatus::Healthy),
            Ok(_) => Ok(HealthStatus::Degraded),
            Err(_) => Ok(HealthStatus::Unhealthy),
        }
    }

    async fn get_stats(&self) -> Result<SandboxStats, SandboxError> {
        let stats = self.stats.read().await;
        Ok(stats.clone())
    }
}

fn build_engine(config: &WasmSandboxConfig) -> Result<Engine, SandboxError> {
    let mut cfg = wasmtime::Config::new();
    cfg.consume_fuel(config.enable_fuel);
    cfg.max_wasm_stack(4 * 1024 * 1024);
    cfg.wasm_simd(true);
    cfg.wasm_bulk_memory(true);
    cfg.wasm_multi_value(true);
    cfg.wasm_reference_types(false);

    Engine::new(&cfg).map_err(|e| SandboxError::InternalError(e.to_string()))
}

// ================================
// Tests
// ================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_wasm_execute() {
        let sandbox = WasmSandbox::new(WasmSandboxConfig::default());
        // Simple test module that returns a hardcoded JSON result
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
                local.get $addr
            )
            (func (export "dealloc") (param i32) (param i32))
            (data (i32.const 0) "{\"test\":true}")
            (data (i32.const 32) "\00\00\00\00\0d\00\00\00")
            (func (export "main") (param i32 i32) (result i32)
                (i32.const 32)
            )
        )"#;

        let result = sandbox
            .execute(SandboxRequest {
                code: wat.to_string(),
                language: CodeLanguage::Wasm,
                inputs: json!({}),
                config: ExecutionConfig::default(),
            })
            .await
            .unwrap();

        assert!(result.success);
    }
}
