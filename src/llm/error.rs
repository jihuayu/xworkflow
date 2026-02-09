use thiserror::Error;

use crate::error::NodeError;

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
        NodeError::ExecutionError(e.to_string())
    }
}
