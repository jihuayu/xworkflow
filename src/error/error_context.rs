//! Structured error context metadata.

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
    OutputTooLarge,
    ResourceLimitExceeded,
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
    /// Create a non-retryable error context.
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

    /// Create a retryable error context.
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

    /// Set the recommended retry-after duration in seconds.
    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    /// Attach the HTTP status code that caused this error.
    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_retryability_variants() {
        assert_eq!(ErrorRetryability::Retryable, ErrorRetryability::Retryable);
        assert_eq!(ErrorRetryability::NonRetryable, ErrorRetryability::NonRetryable);
        assert_eq!(ErrorRetryability::Unknown, ErrorRetryability::Unknown);
        assert_ne!(ErrorRetryability::Retryable, ErrorRetryability::NonRetryable);
    }

    #[test]
    fn test_error_severity_variants() {
        assert_ne!(ErrorSeverity::Warning, ErrorSeverity::Error);
        assert_ne!(ErrorSeverity::Error, ErrorSeverity::Fatal);
        assert_ne!(ErrorSeverity::Warning, ErrorSeverity::Fatal);
    }

    #[test]
    fn test_error_code_serde_roundtrip() {
        let code = ErrorCode::ConfigError;
        let json = serde_json::to_string(&code).unwrap();
        let deserialized: ErrorCode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, code);
    }

    #[test]
    fn test_error_code_all_variants() {
        let codes = vec![
            ErrorCode::ConfigError,
            ErrorCode::TypeError,
            ErrorCode::Timeout,
            ErrorCode::SerializationError,
            ErrorCode::InternalError,
            ErrorCode::NetworkError,
            ErrorCode::HttpClientError,
            ErrorCode::HttpServerError,
            ErrorCode::HttpTimeout,
            ErrorCode::LlmAuthError,
            ErrorCode::LlmRateLimit,
            ErrorCode::LlmApiError,
            ErrorCode::LlmModelNotFound,
            ErrorCode::SandboxCompilationError,
            ErrorCode::SandboxExecutionError,
            ErrorCode::SandboxTimeout,
            ErrorCode::SandboxMemoryLimit,
            ErrorCode::SandboxDangerousCode,
            ErrorCode::PluginNotFound,
            ErrorCode::PluginExecutionError,
            ErrorCode::PluginCapabilityDenied,
            ErrorCode::PluginResourceLimit,
            ErrorCode::VariableNotFound,
            ErrorCode::TemplateError,
            ErrorCode::InputValidationError,
            ErrorCode::OutputTooLarge,
            ErrorCode::ResourceLimitExceeded,
        ];
        for code in codes {
            let json = serde_json::to_string(&code).unwrap();
            let deserialized: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, code);
        }
    }

    #[test]
    fn test_non_retryable() {
        let ctx = ErrorContext::non_retryable(ErrorCode::ConfigError, "bad config");
        assert_eq!(ctx.code, ErrorCode::ConfigError);
        assert_eq!(ctx.retryability, ErrorRetryability::NonRetryable);
        assert_eq!(ctx.severity, ErrorSeverity::Error);
        assert_eq!(ctx.message, "bad config");
        assert!(ctx.retry_after_secs.is_none());
        assert!(ctx.http_status.is_none());
        assert!(ctx.metadata.is_none());
    }

    #[test]
    fn test_retryable() {
        let ctx = ErrorContext::retryable(ErrorCode::HttpTimeout, "timed out");
        assert_eq!(ctx.code, ErrorCode::HttpTimeout);
        assert_eq!(ctx.retryability, ErrorRetryability::Retryable);
        assert_eq!(ctx.severity, ErrorSeverity::Error);
        assert_eq!(ctx.message, "timed out");
    }

    #[test]
    fn test_with_retry_after() {
        let ctx = ErrorContext::retryable(ErrorCode::LlmRateLimit, "rate limited")
            .with_retry_after(60);
        assert_eq!(ctx.retry_after_secs, Some(60));
    }

    #[test]
    fn test_with_http_status() {
        let ctx = ErrorContext::non_retryable(ErrorCode::HttpClientError, "not found")
            .with_http_status(404);
        assert_eq!(ctx.http_status, Some(404));
    }

    #[test]
    fn test_chained_builders() {
        let ctx = ErrorContext::retryable(ErrorCode::HttpServerError, "server error")
            .with_retry_after(30)
            .with_http_status(503);
        assert_eq!(ctx.retry_after_secs, Some(30));
        assert_eq!(ctx.http_status, Some(503));
        assert_eq!(ctx.retryability, ErrorRetryability::Retryable);
    }

    #[test]
    fn test_error_context_serde_roundtrip() {
        let ctx = ErrorContext::retryable(ErrorCode::LlmRateLimit, "slow down")
            .with_retry_after(60)
            .with_http_status(429);
        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: ErrorContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.code, ErrorCode::LlmRateLimit);
        assert_eq!(deserialized.retry_after_secs, Some(60));
        assert_eq!(deserialized.http_status, Some(429));
    }

    #[test]
    fn test_error_context_skip_serializing_none() {
        let ctx = ErrorContext::non_retryable(ErrorCode::ConfigError, "err");
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("retry_after_secs"));
        assert!(!json.contains("http_status"));
        assert!(!json.contains("metadata"));
    }

    #[test]
    fn test_error_retryability_serde() {
        let json = serde_json::to_string(&ErrorRetryability::Retryable).unwrap();
        assert!(json.contains("retryable"));
    }

    #[test]
    fn test_error_severity_serde() {
        let json = serde_json::to_string(&ErrorSeverity::Fatal).unwrap();
        assert!(json.contains("fatal"));
    }
}
