use thiserror::Error;

use crate::error::{ErrorCode, ErrorContext, NodeError};

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
