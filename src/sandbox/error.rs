use super::types::CodeLanguage;

/// Sandbox errors
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("Unsupported language: {0:?}")]
    UnsupportedLanguage(CodeLanguage),

    #[error("Code too large (max {max} bytes, got {actual} bytes)")]
    CodeTooLarge { max: usize, actual: usize },

    #[error("Dangerous code detected: {0}")]
    DangerousCode(String),

    #[error("Input too large (max {max} bytes, got {actual} bytes)")]
    InputTooLarge { max: usize, actual: usize },

    #[error("Compilation error: {0}")]
    CompilationError(String),

    #[error("Execution error: {0}")]
    ExecutionError(String),

    #[error("Type error: {0}")]
    TypeError(String),

    #[error("Memory limit exceeded")]
    MemoryLimitExceeded,

    #[error("Output too large (max {max} bytes, got {actual} bytes)")]
    OutputTooLarge { max: usize, actual: usize },

    #[error("Execution timeout")]
    ExecutionTimeout,

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Sandbox unavailable: {0}")]
    SandboxUnavailable(String),

    #[error("Internal error: {0}")]
    InternalError(String),
}
