use thiserror::Error;

/// Node-level errors
#[derive(Debug, Error)]
pub enum NodeError {
    #[error("Configuration error: {0}")]
    ConfigError(String),
    #[error("Variable not found: {0}")]
    VariableNotFound(String),
    #[error("Execution error: {0}")]
    ExecutionError(String),
    #[error("Type error: {0}")]
    TypeError(String),
    #[error("Template error: {0}")]
    TemplateError(String),
    #[error("Input validation error: {0}")]
    InputValidationError(String),
    #[error("Timeout: node execution exceeded time limit")]
    Timeout,
    #[error("Serialization error: {0}")]
    SerializationError(String),
    #[error("HTTP error: {0}")]
    HttpError(String),
    #[error("Sandbox error: {0}")]
    SandboxError(String),
    #[error("Event send error: {0}")]
    EventSendError(String),
}

impl From<serde_json::Error> for NodeError {
    fn from(e: serde_json::Error) -> Self {
        NodeError::SerializationError(e.to_string())
    }
}
