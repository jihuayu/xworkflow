//! Node-level error types.

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
    #[error("Output too large for node {node_id} (max {max} bytes, got {actual} bytes)")]
    OutputTooLarge { node_id: String, max: usize, actual: usize },

    #[error("{}", .context.message)]
    WithContext {
        #[source]
        source: Box<NodeError>,
        context: ErrorContext,
    },
}

impl NodeError {
    /// Wrap this error with structured [`ErrorContext`] metadata.
    pub fn with_context(self, context: ErrorContext) -> Self {
        NodeError::WithContext {
            source: Box::new(self),
            context,
        }
    }

    /// Return the attached [`ErrorContext`], if any.
    pub fn error_context(&self) -> Option<&ErrorContext> {
        match self {
            NodeError::WithContext { context, .. } => Some(context),
            _ => None,
        }
    }

    /// Return `true` if this error is retryable.
    pub fn is_retryable(&self) -> bool {
        match self.error_context() {
            Some(ctx) => ctx.retryability == ErrorRetryability::Retryable,
            None => self.default_retryability(),
        }
    }

    fn default_retryability(&self) -> bool {
        matches!(self, NodeError::Timeout | NodeError::HttpError(_))
    }

    /// Return a machine-readable error code string.
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
            NodeError::OutputTooLarge { .. } => "output_too_large".to_string(),
        }
    }

    /// Serialize this error into a structured JSON value.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::error_context::{ErrorCode, ErrorRetryability, ErrorSeverity};

    #[test]
    fn test_node_error_display() {
        assert_eq!(NodeError::ConfigError("x".into()).to_string(), "Configuration error: x");
        assert_eq!(NodeError::VariableNotFound("v".into()).to_string(), "Variable not found: v");
        assert_eq!(NodeError::ExecutionError("e".into()).to_string(), "Execution error: e");
        assert_eq!(NodeError::TypeError("t".into()).to_string(), "Type error: t");
        assert_eq!(NodeError::TemplateError("te".into()).to_string(), "Template error: te");
        assert_eq!(NodeError::InputValidationError("iv".into()).to_string(), "Input validation error: iv");
        assert_eq!(NodeError::Timeout.to_string(), "Timeout: node execution exceeded time limit");
        assert_eq!(NodeError::SerializationError("s".into()).to_string(), "Serialization error: s");
        assert_eq!(NodeError::HttpError("h".into()).to_string(), "HTTP error: h");
        assert_eq!(NodeError::SandboxError("sb".into()).to_string(), "Sandbox error: sb");
        assert_eq!(NodeError::EventSendError("es".into()).to_string(), "Event send error: es");
    }

    #[test]
    fn test_node_error_output_too_large_display() {
        let err = NodeError::OutputTooLarge {
            node_id: "n1".into(),
            max: 100,
            actual: 200,
        };
        let msg = err.to_string();
        assert!(msg.contains("n1"));
        assert!(msg.contains("100"));
        assert!(msg.contains("200"));
    }

    #[test]
    fn test_with_context() {
        let err = NodeError::HttpError("timeout".into());
        let ctx = ErrorContext::retryable(ErrorCode::HttpTimeout, "http timeout");
        let with_ctx = err.with_context(ctx.clone());
        assert!(matches!(with_ctx, NodeError::WithContext { .. }));
        assert_eq!(with_ctx.to_string(), "http timeout");
    }

    #[test]
    fn test_error_context_accessor() {
        let err = NodeError::ConfigError("bad".into());
        assert!(err.error_context().is_none());

        let ctx = ErrorContext::non_retryable(ErrorCode::ConfigError, "config err");
        let with_ctx = err.with_context(ctx);
        let retrieved = with_ctx.error_context().unwrap();
        assert_eq!(retrieved.code, ErrorCode::ConfigError);
    }

    #[test]
    fn test_is_retryable_with_context() {
        let err = NodeError::HttpError("err".into())
            .with_context(ErrorContext::retryable(ErrorCode::HttpTimeout, "timeout"));
        assert!(err.is_retryable());

        let err2 = NodeError::HttpError("err".into())
            .with_context(ErrorContext::non_retryable(ErrorCode::HttpClientError, "bad request"));
        assert!(!err2.is_retryable());
    }

    #[test]
    fn test_is_retryable_default() {
        assert!(NodeError::Timeout.is_retryable());
        assert!(NodeError::HttpError("e".into()).is_retryable());
        assert!(!NodeError::ConfigError("e".into()).is_retryable());
        assert!(!NodeError::VariableNotFound("v".into()).is_retryable());
        assert!(!NodeError::ExecutionError("e".into()).is_retryable());
        assert!(!NodeError::TypeError("t".into()).is_retryable());
        assert!(!NodeError::TemplateError("te".into()).is_retryable());
        assert!(!NodeError::InputValidationError("iv".into()).is_retryable());
        assert!(!NodeError::SerializationError("s".into()).is_retryable());
        assert!(!NodeError::SandboxError("sb".into()).is_retryable());
        assert!(!NodeError::EventSendError("es".into()).is_retryable());
        assert!(!NodeError::OutputTooLarge { node_id: "n".into(), max: 1, actual: 2 }.is_retryable());
    }

    #[test]
    fn test_error_code() {
        assert_eq!(NodeError::ConfigError("e".into()).error_code(), "config_error");
        assert_eq!(NodeError::VariableNotFound("v".into()).error_code(), "variable_not_found");
        assert_eq!(NodeError::ExecutionError("e".into()).error_code(), "execution_error");
        assert_eq!(NodeError::TypeError("t".into()).error_code(), "type_error");
        assert_eq!(NodeError::TemplateError("te".into()).error_code(), "template_error");
        assert_eq!(NodeError::InputValidationError("iv".into()).error_code(), "input_validation_error");
        assert_eq!(NodeError::Timeout.error_code(), "timeout");
        assert_eq!(NodeError::SerializationError("s".into()).error_code(), "serialization_error");
        assert_eq!(NodeError::HttpError("h".into()).error_code(), "http_error");
        assert_eq!(NodeError::SandboxError("sb".into()).error_code(), "sandbox_error");
        assert_eq!(NodeError::EventSendError("es".into()).error_code(), "event_send_error");
        assert_eq!(NodeError::OutputTooLarge { node_id: "n".into(), max: 1, actual: 2 }.error_code(), "output_too_large");
    }

    #[test]
    fn test_error_code_with_context() {
        let err = NodeError::HttpError("e".into())
            .with_context(ErrorContext::retryable(ErrorCode::HttpTimeout, "timeout"));
        assert_eq!(err.error_code(), "http_timeout");
    }

    #[test]
    fn test_to_structured_json_without_context() {
        let err = NodeError::ConfigError("bad config".into());
        let json = err.to_structured_json();
        assert_eq!(json["code"], "config_error");
        assert_eq!(json["message"], "Configuration error: bad config");
        assert_eq!(json["retryability"], "unknown");
        assert_eq!(json["severity"], "error");
    }

    #[test]
    fn test_to_structured_json_with_context() {
        let ctx = ErrorContext::retryable(ErrorCode::LlmRateLimit, "rate limited")
            .with_retry_after(60)
            .with_http_status(429);
        let err = NodeError::HttpError("e".into()).with_context(ctx);
        let json = err.to_structured_json();
        assert_eq!(json["code"], "llm_rate_limit");
        assert_eq!(json["message"], "rate limited");
        assert_eq!(json["retry_after_secs"], 60);
        assert_eq!(json["http_status"], 429);
    }

    #[test]
    fn test_from_serde_json_error() {
        let err: Result<serde_json::Value, _> = serde_json::from_str("not json");
        let node_err: NodeError = err.unwrap_err().into();
        assert!(matches!(node_err, NodeError::SerializationError(_)));
        assert!(node_err.to_string().contains("Serialization error"));
    }
}
