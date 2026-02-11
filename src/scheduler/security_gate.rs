use async_trait::async_trait;
use std::sync::Arc;

use crate::core::dispatcher::EngineConfig;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::WorkflowSchema;
use crate::dsl::validation::{validate_schema, ValidationReport};
use crate::error::WorkflowError;

#[async_trait]
pub trait SchedulerSecurityGate: Send + Sync {
  fn validate_schema(&self, schema: &WorkflowSchema, context: &RuntimeContext) -> ValidationReport;

  async fn audit_validation_failed(&self, context: &RuntimeContext, report: &ValidationReport);

  fn configure_variable_pool(&self, context: &RuntimeContext, pool: &mut VariablePool);

  fn effective_engine_config(&self, context: &RuntimeContext, config: EngineConfig) -> EngineConfig;

  async fn on_workflow_start(&self, context: &RuntimeContext) -> Result<Option<String>, WorkflowError>;

  async fn record_workflow_end(&self, context: &RuntimeContext, workflow_id: Option<&str>);
}

#[cfg(not(feature = "security"))]
#[derive(Debug, Default)]
struct NoopSchedulerSecurityGate;

#[cfg(not(feature = "security"))]
#[async_trait]
impl SchedulerSecurityGate for NoopSchedulerSecurityGate {
  fn validate_schema(&self, schema: &WorkflowSchema, _context: &RuntimeContext) -> ValidationReport {
    validate_schema(schema)
  }

  async fn audit_validation_failed(&self, _context: &RuntimeContext, _report: &ValidationReport) {}

  fn configure_variable_pool(&self, _context: &RuntimeContext, _pool: &mut VariablePool) {}

  fn effective_engine_config(&self, _context: &RuntimeContext, config: EngineConfig) -> EngineConfig {
    config
  }

  async fn on_workflow_start(&self, _context: &RuntimeContext) -> Result<Option<String>, WorkflowError> {
    Ok(None)
  }

  async fn record_workflow_end(&self, _context: &RuntimeContext, _workflow_id: Option<&str>) {}
}

#[cfg(feature = "security")]
mod real {
  use super::*;

  use crate::security::audit::{EventSeverity, SecurityEvent, SecurityEventType};

  #[derive(Debug, Default)]
  pub struct RealSchedulerSecurityGate;

  #[async_trait]
  impl SchedulerSecurityGate for RealSchedulerSecurityGate {
    fn validate_schema(&self, schema: &WorkflowSchema, context: &RuntimeContext) -> ValidationReport {
      if let Some(policy) = context
        .security_policy()
        .and_then(|p| p.dsl_validation.as_ref())
      {
        crate::dsl::validation::validate_schema_with_config(schema, policy)
      } else {
        validate_schema(schema)
      }
    }

    async fn audit_validation_failed(&self, context: &RuntimeContext, report: &ValidationReport) {
      let logger = match context.audit_logger() {
        Some(l) => l,
        None => return,
      };
      let group_id = match context.resource_group() {
        Some(g) => g.group_id.clone(),
        None => return,
      };

      let errors = report
        .diagnostics
        .iter()
        .filter(|d| d.level == crate::dsl::validation::DiagnosticLevel::Error)
        .map(|d| d.message.clone())
        .collect::<Vec<_>>();

      if errors.is_empty() {
        return;
      }

      let event = SecurityEvent {
        timestamp: context.time_provider.now_timestamp(),
        group_id,
        workflow_id: None,
        node_id: None,
        event_type: SecurityEventType::DslValidationFailed { errors },
        details: serde_json::Value::Null,
        severity: EventSeverity::Warning,
      };

      logger.log_event(event).await;
    }

    fn configure_variable_pool(&self, context: &RuntimeContext, pool: &mut VariablePool) {
      if let Some(selector_cfg) = context
        .security_policy()
        .and_then(|p| p.dsl_validation.as_ref())
        .and_then(|d| d.selector_validation.clone())
      {
        pool.set_selector_validation(Some(selector_cfg));
      }
    }

    fn effective_engine_config(&self, context: &RuntimeContext, config: EngineConfig) -> EngineConfig {
      if let Some(group) = context.resource_group() {
        EngineConfig {
          max_steps: config.max_steps.min(group.quota.max_steps),
          max_execution_time_secs: config
            .max_execution_time_secs
            .min(group.quota.max_execution_time_secs),
          strict_template: config.strict_template,
        }
      } else {
        config
      }
    }

    async fn on_workflow_start(&self, context: &RuntimeContext) -> Result<Option<String>, WorkflowError> {
      let workflow_id = context.id_generator.next_id();

      if let (Some(governor), Some(group)) = (context.resource_governor(), context.resource_group()) {
        governor
          .check_workflow_start(&group.group_id)
          .await
          .map_err(|e| WorkflowError::InternalError(e.to_string()))?;
        governor
          .record_workflow_start(&group.group_id, &workflow_id)
          .await;
      }

      Ok(Some(workflow_id))
    }

    async fn record_workflow_end(&self, context: &RuntimeContext, workflow_id: Option<&str>) {
      let Some(workflow_id) = workflow_id else {
        return;
      };

      if let (Some(governor), Some(group)) = (context.resource_governor(), context.resource_group()) {
        governor
          .record_workflow_end(&group.group_id, workflow_id)
          .await;
      }
    }
  }

  pub fn new_gate() -> Arc<dyn SchedulerSecurityGate> {
    Arc::new(RealSchedulerSecurityGate)
  }
}

#[cfg(feature = "security")]
pub use real::new_gate as new_scheduler_security_gate;

#[cfg(not(feature = "security"))]
pub fn new_scheduler_security_gate() -> Arc<dyn SchedulerSecurityGate> {
  Arc::new(NoopSchedulerSecurityGate)
}
