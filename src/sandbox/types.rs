use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

use super::error::SandboxError;

// ================================
// CodeSandbox Trait
// ================================

/// Code sandbox execution interface
///
/// All sandbox implementations (V8, WASM, remote calls, etc.) must implement this trait
#[async_trait::async_trait]
pub trait CodeSandbox: Send + Sync {
    /// Sandbox type identifier
    fn sandbox_type(&self) -> SandboxType;

    /// Supported language list
    fn supported_languages(&self) -> Vec<CodeLanguage>;

    /// Execute code
    ///
    /// # Parameters
    /// - `request`: execution request including code, inputs, config, etc.
    ///
    /// # Returns
    /// - `Ok(SandboxResult)`: execution succeeded, returns result
    /// - `Err(SandboxError)`: execution failed
    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError>;

    /// Validate code (optional, for pre-check)
    async fn validate(&self, code: &str, language: CodeLanguage) -> Result<(), SandboxError> {
        let _ = code;
        let _ = language;
        Ok(())
    }

    /// Health check
    async fn health_check(&self) -> Result<HealthStatus, SandboxError> {
        Ok(HealthStatus::Healthy)
    }

    /// Get resource usage stats
    async fn get_stats(&self) -> Result<SandboxStats, SandboxError> {
        Ok(SandboxStats::default())
    }
}

// ================================
// Enums
// ================================

/// Sandbox type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SandboxType {
    /// Built-in JavaScript interpreter
    Builtin,
    /// V8 JavaScript engine
    V8,
    /// WebAssembly
    Wasm,
    /// Remote sandbox service
    Remote,
    /// Python interpreter
    Python,
    /// Lua interpreter
    Lua,
}

/// Supported programming languages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CodeLanguage {
    JavaScript,
    TypeScript,
    Python,
    Lua,
    Wasm,
}

/// Health status
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

// ================================
// Request / Response / Config
// ================================

/// Sandbox execution request
#[derive(Debug, Clone)]
pub struct SandboxRequest {
    /// Code
    pub code: String,

    /// Programming language
    pub language: CodeLanguage,

    /// Input data
    pub inputs: Value,

    /// Execution config
    pub config: ExecutionConfig,
}

/// Execution config
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Execution timeout
    pub timeout: Duration,

    /// Max memory (bytes)
    pub max_memory: usize,

    /// Max CPU time (seconds)
    pub max_cpu_time: Option<Duration>,

    /// Environment variables
    pub env_vars: HashMap<String, String>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_memory: 128 * 1024 * 1024, // 128MB
            max_cpu_time: None,
            env_vars: HashMap::new(),
        }
    }
}

/// Sandbox execution result
#[derive(Debug, Clone)]
pub struct SandboxResult {
    /// Whether execution succeeded
    pub success: bool,

    /// Output data
    pub output: Value,

    /// Stdout
    pub stdout: String,

    /// Stderr
    pub stderr: String,

    /// Execution time
    pub execution_time: Duration,

    /// Memory used (bytes)
    pub memory_used: usize,

    /// Error message (if failed)
    pub error: Option<String>,
}

/// Sandbox statistics
#[derive(Debug, Clone)]
pub struct SandboxStats {
    /// Total executions
    pub total_executions: u64,

    /// Successful executions
    pub successful_executions: u64,

    /// Failed executions
    pub failed_executions: u64,

    /// Average execution time
    pub avg_execution_time: Duration,

    /// Average memory usage
    pub avg_memory_used: usize,
}

impl Default for SandboxStats {
    fn default() -> Self {
        Self {
            total_executions: 0,
            successful_executions: 0,
            failed_executions: 0,
            avg_execution_time: Duration::from_secs(0),
            avg_memory_used: 0,
        }
    }
}
