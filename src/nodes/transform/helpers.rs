//! Shared helper functions for data-transform node executors.

use futures::StreamExt;

use crate::core::runtime_context::RuntimeContext;
use crate::error::{ErrorCode, ErrorContext, NodeError};

/// Read HTTP response body with optional size limit.
pub(crate) async fn read_response_with_limit(
    resp: reqwest::Response,
    max_bytes: Option<usize>,
) -> Result<String, NodeError> {
    if let Some(limit) = max_bytes {
        if let Some(len) = resp.content_length() {
            if len as usize > limit {
                return Err(NodeError::HttpError(format!(
                    "HTTP response too large (max {} bytes, got {})",
                    limit, len
                )));
            }
        }
    }

    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| NodeError::HttpError(e.to_string()))?;
        if let Some(limit) = max_bytes {
            if buf.len() + chunk.len() > limit {
                return Err(NodeError::HttpError(format!(
                    "HTTP response too large (max {} bytes)",
                    limit
                )));
            }
        }
        buf.extend_from_slice(&chunk);
    }

    Ok(String::from_utf8_lossy(&buf).to_string())
}

#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEvent, SecurityEventType};

/// Log a security audit event.
#[cfg(feature = "security")]
pub(crate) async fn audit_security_event(
    context: &RuntimeContext,
    event_type: SecurityEventType,
    severity: EventSeverity,
    node_id: Option<String>,
) {
    let logger = match context.audit_logger() {
        Some(l) => l,
        None => return,
    };
    let group_id = match context.resource_group() {
        Some(g) => g.group_id.clone(),
        None => return,
    };
    let event = SecurityEvent {
        timestamp: context.time_provider.now_timestamp(),
        group_id,
        workflow_id: None,
        node_id,
        event_type,
        details: serde_json::Value::Null,
        severity,
    };
    logger.log_event(event).await;
}

/// Check if a template error message indicates an anomaly (output too large,
/// infinite loop, recursion, timeout, etc.).
#[cfg(feature = "security")]
pub(crate) fn is_template_anomaly_error(message: &str) -> bool {
    let msg = message.to_lowercase();
    msg.contains("output too large")
        || msg.contains("template output too large")
        || msg.contains("fuel")
        || msg.contains("loop")
        || msg.contains("recursion")
        || msg.contains("timeout")
}

/// Execute code in a sandbox with security audit logging.
pub(crate) async fn execute_sandbox_with_audit(
    manager: &crate::sandbox::SandboxManager,
    request: crate::sandbox::SandboxRequest,
    context: &RuntimeContext,
    node_id: &str,
    language: crate::sandbox::CodeLanguage,
) -> Result<crate::sandbox::SandboxResult, NodeError> {
    let result = manager.execute(request).await;

    let result = match result {
        Ok(result) => result,
        Err(e) => {
            #[cfg(feature = "security")]
            audit_security_event(
                context,
                SecurityEventType::SandboxViolation {
                    sandbox_type: format!("{:?}", language),
                    violation: e.to_string(),
                },
                EventSeverity::Warning,
                Some(node_id.to_string()),
            )
            .await;

            let error_context = match &e {
                crate::sandbox::SandboxError::ExecutionTimeout => {
                    ErrorContext::retryable(ErrorCode::SandboxTimeout, e.to_string())
                }
                crate::sandbox::SandboxError::InputTooLarge { .. }
                | crate::sandbox::SandboxError::OutputTooLarge { .. } => {
                    ErrorContext::non_retryable(ErrorCode::SandboxMemoryLimit, e.to_string())
                }
                crate::sandbox::SandboxError::MemoryLimitExceeded => {
                    ErrorContext::non_retryable(ErrorCode::SandboxMemoryLimit, e.to_string())
                }
                crate::sandbox::SandboxError::CompilationError(_) => {
                    ErrorContext::non_retryable(ErrorCode::SandboxCompilationError, e.to_string())
                }
                crate::sandbox::SandboxError::DangerousCode(_) => {
                    ErrorContext::non_retryable(ErrorCode::SandboxDangerousCode, e.to_string())
                }
                _ => ErrorContext::non_retryable(ErrorCode::SandboxExecutionError, e.to_string()),
            };

            return Err(NodeError::SandboxError(e.to_string()).with_context(error_context));
        }
    };

    if !result.success {
        let message = result
            .error
            .unwrap_or_else(|| "Unknown sandbox error".to_string());
        #[cfg(feature = "security")]
        audit_security_event(
            context,
            SecurityEventType::SandboxViolation {
                sandbox_type: format!("{:?}", language),
                violation: message.clone(),
            },
            EventSeverity::Warning,
            Some(node_id.to_string()),
        )
        .await;

        let error_context = ErrorContext::non_retryable(ErrorCode::SandboxExecutionError, &message);
        return Err(NodeError::ExecutionError(message).with_context(error_context));
    }

    Ok(result)
}

#[cfg(all(test, feature = "builtin-sandbox-js"))]
pub(crate) fn escape_js_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(all(test, feature = "builtin-sandbox-js"))]
pub(crate) fn parse_json_result(result_str: &str) -> Result<Option<serde_json::Value>, String> {
    if result_str == "__undefined__" {
        return Ok(None);
    }
    let val: serde_json::Value =
        serde_json::from_str(result_str).map_err(|e| format!("Failed to parse JSON: {}", e))?;
    if val.is_null() {
        return Ok(None);
    }
    Ok(Some(val))
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "security")]
    use super::is_template_anomaly_error;

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_output_too_large() {
        assert!(is_template_anomaly_error("Output too large for template"));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_fuel() {
        assert!(is_template_anomaly_error("run out of FUEL"));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_loop() {
        assert!(is_template_anomaly_error("infinite loop detected"));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_recursion() {
        assert!(is_template_anomaly_error(
            "maximum recursion depth exceeded"
        ));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_timeout() {
        assert!(is_template_anomaly_error("execution timeout"));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_normal() {
        assert!(!is_template_anomaly_error("variable not found"));
        assert!(!is_template_anomaly_error(""));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_large_output() {
        assert!(is_template_anomaly_error("output too large for template"));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_recursion_depth() {
        assert!(is_template_anomaly_error("recursion depth exceeded"));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_is_template_anomaly_error_not_anomaly() {
        assert!(!is_template_anomaly_error("variable not found"));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_escape_js_string() {
        use super::escape_js_string;
        assert_eq!(escape_js_string("hello"), "hello");
        assert_eq!(escape_js_string("it's"), "it\\'s");
        assert_eq!(escape_js_string("back\\slash"), "back\\\\slash");
        assert_eq!(escape_js_string("it\\'s"), "it\\\\\\'s");
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_escape_js_string_backslash_and_quote() {
        use super::escape_js_string;
        let result = escape_js_string("hello\\world");
        assert!(result.contains("\\\\"));
        let result2 = escape_js_string("it's");
        assert!(result2.contains("\\'"));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_undefined() {
        use super::parse_json_result;
        let result = parse_json_result("__undefined__").unwrap();
        assert!(result.is_none());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_null() {
        use super::parse_json_result;
        let result = parse_json_result("null").unwrap();
        assert!(result.is_none());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_valid() {
        use super::parse_json_result;
        let result = parse_json_result(r#"{"key": "value"}"#).unwrap();
        assert_eq!(result, Some(serde_json::json!({"key": "value"})));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_invalid() {
        use super::parse_json_result;
        let result = parse_json_result("not valid json {{{");
        assert!(result.is_err());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_number() {
        use super::parse_json_result;
        let result = parse_json_result("42").unwrap();
        assert_eq!(result, Some(serde_json::json!(42)));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_empty_string() {
        use super::parse_json_result;
        let result = parse_json_result("");
        assert!(result.is_err() || result.unwrap().is_none());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_boolean_true() {
        use super::parse_json_result;
        let result = parse_json_result("true").unwrap().unwrap();
        assert_eq!(result, serde_json::Value::Bool(true));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_boolean_false() {
        use super::parse_json_result;
        let result = parse_json_result("false").unwrap().unwrap();
        assert_eq!(result, serde_json::Value::Bool(false));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_array() {
        use super::parse_json_result;
        let result = parse_json_result("[1,2,3]").unwrap().unwrap();
        assert_eq!(result, serde_json::json!([1, 2, 3]));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_object() {
        use super::parse_json_result;
        let result = parse_json_result("{\"key\":\"val\"}").unwrap().unwrap();
        assert_eq!(result, serde_json::json!({"key": "val"}));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_undefined_sentinel() {
        use super::parse_json_result;
        let result = parse_json_result("__undefined__").unwrap();
        assert!(result.is_none());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_null_returns_none() {
        use super::parse_json_result;
        let result = parse_json_result("null").unwrap();
        assert!(result.is_none());
    }
}
