use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

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

#[derive(Debug, Clone, Serialize)]
pub enum SecurityEventType {
    SsrfBlocked { url: String, reason: String },
    SandboxViolation { sandbox_type: String, violation: String },
    QuotaExceeded { quota_type: String, limit: u64, current: u64 },
    CredentialAccess { provider: String, success: bool },
    CodeAnalysisBlocked { violations: Vec<String> },
    TemplateRenderingAnomaly { template_length: usize },
    DslValidationFailed { errors: Vec<String> },
    OutputSizeExceeded { node_id: String, max: usize, actual: usize },
}

#[derive(Debug, Clone, Serialize)]
pub enum EventSeverity {
    Info,
    Warning,
    Critical,
}

#[async_trait]
pub trait AuditLogger: Send + Sync {
    async fn log_event(&self, event: SecurityEvent);

    async fn log_events(&self, events: Vec<SecurityEvent>) {
        for event in events {
            self.log_event(event).await;
        }
    }
}

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