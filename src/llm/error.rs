//! Error types specific to LLM provider operations.

use thiserror::Error;

use crate::error::{ErrorCode, ErrorContext, NodeError};

/// Errors that can occur when interacting with an LLM provider.
#[derive(Debug, Error)]
pub enum LlmError {
    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    #[error("Model not supported: {0}")]
    ModelNotSupported(String),

    #[error("Authentication error: {0}")]
    AuthenticationError(String),

    #[error("Rate limit exceeded: retry after {retry_after:?}s")]
    RateLimitExceeded { retry_after: Option<u64> },

    #[error("API error ({status}): {message}")]
    ApiError { status: u16, message: String },

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Timeout")]
    Timeout,

    #[error("Stream error: {0}")]
    StreamError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("WASM provider error: {0}")]
    WasmError(String),
}

impl From<LlmError> for NodeError {
    fn from(e: LlmError) -> Self {
        let context = match &e {
            LlmError::AuthenticationError(_) => {
                ErrorContext::non_retryable(ErrorCode::LlmAuthError, e.to_string())
            }
            LlmError::RateLimitExceeded { retry_after } => {
                let mut ctx = ErrorContext::retryable(ErrorCode::LlmRateLimit, e.to_string());
                if let Some(secs) = retry_after {
                    ctx = ctx.with_retry_after(*secs);
                }
                ctx
            }
            LlmError::ApiError { status, .. } => {
                let retryable = *status >= 500 || *status == 429;
                let ctx = if retryable {
                    ErrorContext::retryable(ErrorCode::LlmApiError, e.to_string())
                } else {
                    ErrorContext::non_retryable(ErrorCode::LlmApiError, e.to_string())
                };
                ctx.with_http_status(*status)
            }
            LlmError::NetworkError(_) => {
                ErrorContext::retryable(ErrorCode::NetworkError, e.to_string())
            }
            LlmError::Timeout => ErrorContext::retryable(ErrorCode::Timeout, e.to_string()),
            LlmError::InvalidRequest(_) => {
                ErrorContext::non_retryable(ErrorCode::ConfigError, e.to_string())
            }
            LlmError::ProviderNotFound(_) | LlmError::ModelNotSupported(_) => {
                ErrorContext::non_retryable(ErrorCode::LlmModelNotFound, e.to_string())
            }
            _ => ErrorContext::non_retryable(ErrorCode::InternalError, e.to_string()),
        };

        NodeError::ExecutionError(e.to_string()).with_context(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_error_display() {
        assert_eq!(LlmError::ProviderNotFound("p".into()).to_string(), "Provider not found: p");
        assert_eq!(LlmError::ModelNotSupported("m".into()).to_string(), "Model not supported: m");
        assert_eq!(LlmError::AuthenticationError("a".into()).to_string(), "Authentication error: a");
        assert!(LlmError::RateLimitExceeded { retry_after: Some(60) }.to_string().contains("60"));
        assert!(LlmError::RateLimitExceeded { retry_after: None }.to_string().contains("None"));
        assert!(LlmError::ApiError { status: 500, message: "err".into() }.to_string().contains("500"));
        assert_eq!(LlmError::NetworkError("n".into()).to_string(), "Network error: n");
        assert_eq!(LlmError::Timeout.to_string(), "Timeout");
        assert_eq!(LlmError::StreamError("s".into()).to_string(), "Stream error: s");
        assert_eq!(LlmError::SerializationError("se".into()).to_string(), "Serialization error: se");
        assert_eq!(LlmError::InvalidRequest("ir".into()).to_string(), "Invalid request: ir");
        assert_eq!(LlmError::WasmError("w".into()).to_string(), "WASM provider error: w");
    }

    #[test]
    fn test_from_llm_error_auth() {
        let err: NodeError = LlmError::AuthenticationError("bad key".into()).into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::LlmAuthError);
        assert_eq!(ctx.retryability, crate::error::ErrorRetryability::NonRetryable);
    }

    #[test]
    fn test_from_llm_error_rate_limit() {
        let err: NodeError = LlmError::RateLimitExceeded { retry_after: Some(30) }.into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::LlmRateLimit);
        assert_eq!(ctx.retry_after_secs, Some(30));
        assert_eq!(ctx.retryability, crate::error::ErrorRetryability::Retryable);
    }

    #[test]
    fn test_from_llm_error_rate_limit_no_retry_after() {
        let err: NodeError = LlmError::RateLimitExceeded { retry_after: None }.into();
        let ctx = err.error_context().unwrap();
        assert!(ctx.retry_after_secs.is_none());
    }

    #[test]
    fn test_from_llm_error_api_error_retryable() {
        let err: NodeError = LlmError::ApiError { status: 500, message: "server err".into() }.into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::LlmApiError);
        assert_eq!(ctx.http_status, Some(500));
        assert_eq!(ctx.retryability, crate::error::ErrorRetryability::Retryable);
    }

    #[test]
    fn test_from_llm_error_api_error_429() {
        let err: NodeError = LlmError::ApiError { status: 429, message: "rate limit".into() }.into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.retryability, crate::error::ErrorRetryability::Retryable);
        assert_eq!(ctx.http_status, Some(429));
    }

    #[test]
    fn test_from_llm_error_api_error_non_retryable() {
        let err: NodeError = LlmError::ApiError { status: 400, message: "bad request".into() }.into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.retryability, crate::error::ErrorRetryability::NonRetryable);
    }

    #[test]
    fn test_from_llm_error_network() {
        let err: NodeError = LlmError::NetworkError("timeout".into()).into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::NetworkError);
        assert_eq!(ctx.retryability, crate::error::ErrorRetryability::Retryable);
    }

    #[test]
    fn test_from_llm_error_timeout() {
        let err: NodeError = LlmError::Timeout.into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::Timeout);
        assert_eq!(ctx.retryability, crate::error::ErrorRetryability::Retryable);
    }

    #[test]
    fn test_from_llm_error_invalid_request() {
        let err: NodeError = LlmError::InvalidRequest("bad".into()).into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::ConfigError);
        assert_eq!(ctx.retryability, crate::error::ErrorRetryability::NonRetryable);
    }

    #[test]
    fn test_from_llm_error_provider_not_found() {
        let err: NodeError = LlmError::ProviderNotFound("gpt".into()).into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::LlmModelNotFound);
    }

    #[test]
    fn test_from_llm_error_model_not_supported() {
        let err: NodeError = LlmError::ModelNotSupported("m".into()).into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::LlmModelNotFound);
    }

    #[test]
    fn test_from_llm_error_stream_error() {
        let err: NodeError = LlmError::StreamError("s".into()).into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::InternalError);
    }

    #[test]
    fn test_from_llm_error_serialization_error() {
        let err: NodeError = LlmError::SerializationError("s".into()).into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::InternalError);
    }

    #[test]
    fn test_from_llm_error_wasm_error() {
        let err: NodeError = LlmError::WasmError("w".into()).into();
        let ctx = err.error_context().unwrap();
        assert_eq!(ctx.code, ErrorCode::InternalError);
    }
}
