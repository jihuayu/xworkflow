//! Audit logging for security-relevant events.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

/// A security event recorded by the audit system.
#[derive(Debug, Clone, Serialize)]
pub struct SecurityEvent {
    pub timestamp: i64,
    pub group_id: String,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub event_type: SecurityEventType,
    pub details: Value,
    pub severity: EventSeverity,
}

/// Categorisation of a security event.
#[derive(Debug, Clone, Serialize)]
pub enum SecurityEventType {
    SsrfBlocked {
        url: String,
        reason: String,
    },
    SandboxViolation {
        sandbox_type: String,
        violation: String,
    },
    QuotaExceeded {
        quota_type: String,
        limit: u64,
        current: u64,
    },
    CredentialAccess {
        provider: String,
        success: bool,
    },
    ToolInvocation {
        tool_name: String,
    },
    CodeAnalysisBlocked {
        violations: Vec<String>,
    },
    TemplateRenderingAnomaly {
        template_length: usize,
    },
    DslValidationFailed {
        errors: Vec<String>,
    },
    OutputSizeExceeded {
        node_id: String,
        max: usize,
        actual: usize,
    },
    #[cfg(feature = "memory")]
    MemoryAccess {
        namespace: String,
        operation: String,
        node_id: String,
        success: bool,
    },
}

/// Severity level attached to a [`SecurityEvent`].
#[derive(Debug, Clone, Serialize)]
pub enum EventSeverity {
    Info,
    Warning,
    Critical,
}

/// Trait for persisting security audit events.
#[async_trait]
pub trait AuditLogger: Send + Sync {
    /// Record a single security event.
    async fn log_event(&self, event: SecurityEvent);

    /// Record multiple events (default: log each individually).
    async fn log_events(&self, events: Vec<SecurityEvent>) {
        for event in events {
            self.log_event(event).await;
        }
    }
}

/// Audit logger that writes events via the `tracing` crate.
pub struct TracingAuditLogger;

#[async_trait]
impl AuditLogger for TracingAuditLogger {
    async fn log_event(&self, event: SecurityEvent) {
        match event.severity {
            EventSeverity::Critical => {
                tracing::error!(
                    group_id = %event.group_id,
                    event_type = ?event.event_type,
                    "SECURITY: {:?}",
                    event
                );
            }
            EventSeverity::Warning => {
                tracing::warn!(
                    group_id = %event.group_id,
                    event_type = ?event.event_type,
                    "SECURITY: {:?}",
                    event
                );
            }
            EventSeverity::Info => {
                tracing::info!(
                    group_id = %event.group_id,
                    event_type = ?event.event_type,
                    "SECURITY: {:?}",
                    event
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(severity: EventSeverity) -> SecurityEvent {
        SecurityEvent {
            timestamp: 1234567890,
            group_id: "test-group".to_string(),
            workflow_id: Some("wf-1".to_string()),
            node_id: Some("node-1".to_string()),
            event_type: SecurityEventType::SsrfBlocked {
                url: "http://evil.com".to_string(),
                reason: "blocked".to_string(),
            },
            details: serde_json::json!({"key": "value"}),
            severity,
        }
    }

    #[test]
    fn test_security_event_creation() {
        let event = make_event(EventSeverity::Info);
        assert_eq!(event.timestamp, 1234567890);
        assert_eq!(event.group_id, "test-group");
        assert_eq!(event.workflow_id.as_deref(), Some("wf-1"));
        assert_eq!(event.node_id.as_deref(), Some("node-1"));
    }

    #[test]
    fn test_security_event_serialize() {
        let event = make_event(EventSeverity::Warning);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["group_id"], "test-group");
        assert_eq!(json["timestamp"], 1234567890);
    }

    #[test]
    fn test_security_event_types() {
        let sandbox = SecurityEventType::SandboxViolation {
            sandbox_type: "js".into(),
            violation: "eval".into(),
        };
        let json = serde_json::to_value(&sandbox).unwrap();
        assert!(json.to_string().contains("SandboxViolation"));

        let quota = SecurityEventType::QuotaExceeded {
            quota_type: "memory".into(),
            limit: 100,
            current: 200,
        };
        let json = serde_json::to_value(&quota).unwrap();
        assert!(json.to_string().contains("QuotaExceeded"));

        let cred = SecurityEventType::CredentialAccess {
            provider: "openai".into(),
            success: true,
        };
        let json = serde_json::to_value(&cred).unwrap();
        assert!(json.to_string().contains("CredentialAccess"));

        let tool = SecurityEventType::ToolInvocation {
            tool_name: "search".into(),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert!(json.to_string().contains("ToolInvocation"));

        let code = SecurityEventType::CodeAnalysisBlocked {
            violations: vec!["eval".into()],
        };
        let json = serde_json::to_value(&code).unwrap();
        assert!(json.to_string().contains("CodeAnalysisBlocked"));

        let tmpl = SecurityEventType::TemplateRenderingAnomaly {
            template_length: 99999,
        };
        let json = serde_json::to_value(&tmpl).unwrap();
        assert!(json.to_string().contains("99999"));

        let dsl = SecurityEventType::DslValidationFailed {
            errors: vec!["err1".into()],
        };
        let json = serde_json::to_value(&dsl).unwrap();
        assert!(json.to_string().contains("DslValidationFailed"));

        let output = SecurityEventType::OutputSizeExceeded {
            node_id: "n1".into(),
            max: 100,
            actual: 200,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.to_string().contains("OutputSizeExceeded"));
    }

    #[test]
    fn test_event_severity_variants() {
        let info = EventSeverity::Info;
        let warning = EventSeverity::Warning;
        let critical = EventSeverity::Critical;
        // Just ensure they serialize distinctly
        let i = serde_json::to_string(&info).unwrap();
        let w = serde_json::to_string(&warning).unwrap();
        let c = serde_json::to_string(&critical).unwrap();
        assert_ne!(i, w);
        assert_ne!(w, c);
        assert_ne!(i, c);
    }

    #[tokio::test]
    async fn test_tracing_audit_logger_info() {
        let logger = TracingAuditLogger;
        let event = make_event(EventSeverity::Info);
        logger.log_event(event).await;
    }

    #[tokio::test]
    async fn test_tracing_audit_logger_warning() {
        let logger = TracingAuditLogger;
        let event = make_event(EventSeverity::Warning);
        logger.log_event(event).await;
    }

    #[tokio::test]
    async fn test_tracing_audit_logger_critical() {
        let logger = TracingAuditLogger;
        let event = make_event(EventSeverity::Critical);
        logger.log_event(event).await;
    }

    #[tokio::test]
    async fn test_tracing_audit_logger_log_events() {
        let logger = TracingAuditLogger;
        let events = vec![
            make_event(EventSeverity::Info),
            make_event(EventSeverity::Warning),
            make_event(EventSeverity::Critical),
        ];
        logger.log_events(events).await;
    }

    #[test]
    fn test_security_event_without_optional_fields() {
        let event = SecurityEvent {
            timestamp: 0,
            group_id: "g".to_string(),
            workflow_id: None,
            node_id: None,
            event_type: SecurityEventType::SsrfBlocked {
                url: "http://x".into(),
                reason: "r".into(),
            },
            details: serde_json::Value::Null,
            severity: EventSeverity::Info,
        };
        assert!(event.workflow_id.is_none());
        assert!(event.node_id.is_none());
    }
}
