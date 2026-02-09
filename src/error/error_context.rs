use serde::{Deserialize, Serialize};

/// Error retryability marker
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorRetryability {
    Retryable,
    NonRetryable,
    Unknown,
}

/// Error severity marker
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSeverity {
    Warning,
    Error,
    Fatal,
}

/// Error classification code
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // Common
    ConfigError,
    TypeError,
    Timeout,
    SerializationError,
    InternalError,

    // Network/HTTP
    NetworkError,
    HttpClientError,
    HttpServerError,
    HttpTimeout,

    // LLM
    LlmAuthError,
    LlmRateLimit,
    LlmApiError,
    LlmModelNotFound,

    // Sandbox/Code
    SandboxCompilationError,
    SandboxExecutionError,
    SandboxTimeout,
    SandboxMemoryLimit,
    SandboxDangerousCode,

    // Plugin
    PluginNotFound,
    PluginExecutionError,
    PluginCapabilityDenied,
    PluginResourceLimit,

    // Variable/Template
    VariableNotFound,
    TemplateError,
    InputValidationError,
}

/// Structured error context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorContext {
    pub code: ErrorCode,
    pub retryability: ErrorRetryability,
    pub severity: ErrorSeverity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ErrorContext {
    pub fn non_retryable(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            retryability: ErrorRetryability::NonRetryable,
            severity: ErrorSeverity::Error,
            message: message.into(),
            retry_after_secs: None,
            http_status: None,
            metadata: None,
        }
    }

    pub fn retryable(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            retryability: ErrorRetryability::Retryable,
            severity: ErrorSeverity::Error,
            message: message.into(),
            retry_after_secs: None,
            http_status: None,
            metadata: None,
        }
    }

    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }
}
