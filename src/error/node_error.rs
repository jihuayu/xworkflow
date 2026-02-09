use thiserror::Error;
use serde_json;

use super::error_context::{ErrorContext, ErrorRetryability};

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

    #[error("{}", .context.message)]
    WithContext {
        #[source]
        source: Box<NodeError>,
        context: ErrorContext,
    },
}

impl NodeError {
    pub fn with_context(self, context: ErrorContext) -> Self {
        NodeError::WithContext {
            source: Box::new(self),
            context,
        }
    }

    pub fn error_context(&self) -> Option<&ErrorContext> {
        match self {
            NodeError::WithContext { context, .. } => Some(context),
            _ => None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self.error_context() {
            Some(ctx) => ctx.retryability == ErrorRetryability::Retryable,
            None => self.default_retryability(),
        }
    }

    fn default_retryability(&self) -> bool {
        matches!(self, NodeError::Timeout | NodeError::HttpError(_))
    }

    pub fn error_code(&self) -> String {
        match self {
            NodeError::WithContext { context, .. } => serde_json::to_value(&context.code)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown".to_string()),
            NodeError::ConfigError(_) => "config_error".to_string(),
            NodeError::VariableNotFound(_) => "variable_not_found".to_string(),
            NodeError::ExecutionError(_) => "execution_error".to_string(),
            NodeError::TypeError(_) => "type_error".to_string(),
            NodeError::TemplateError(_) => "template_error".to_string(),
            NodeError::InputValidationError(_) => "input_validation_error".to_string(),
            NodeError::Timeout => "timeout".to_string(),
            NodeError::SerializationError(_) => "serialization_error".to_string(),
            NodeError::HttpError(_) => "http_error".to_string(),
            NodeError::SandboxError(_) => "sandbox_error".to_string(),
            NodeError::EventSendError(_) => "event_send_error".to_string(),
        }
    }

    pub fn to_structured_json(&self) -> serde_json::Value {
        match self.error_context() {
            Some(ctx) => serde_json::to_value(ctx).unwrap_or_else(|_| serde_json::json!({
                "code": self.error_code(),
                "message": self.to_string(),
            })),
            None => serde_json::json!({
                "code": self.error_code(),
                "message": self.to_string(),
                "retryability": "unknown",
                "severity": "error",
            }),
        }
    }
}

impl From<serde_json::Error> for NodeError {
    fn from(e: serde_json::Error) -> Self {
        NodeError::SerializationError(e.to_string())
    }
}
