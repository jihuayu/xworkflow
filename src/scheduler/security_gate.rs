use async_trait::async_trait;
use std::sync::Arc;

use crate::core::dispatcher::EngineConfig;
use crate::core::workflow_context::WorkflowContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::WorkflowSchema;
use crate::dsl::validation::{validate_schema, ValidationReport};
use crate::error::WorkflowError;

#[async_trait]
pub trait SchedulerSecurityGate: Send + Sync {
  fn validate_schema(&self, schema: &WorkflowSchema, context: &WorkflowContext) -> ValidationReport;

  async fn audit_validation_failed(&self, context: &WorkflowContext, report: &ValidationReport);

  fn configure_variable_pool(&self, context: &WorkflowContext, pool: &mut VariablePool);

  fn effective_engine_config(&self, context: &WorkflowContext, config: EngineConfig) -> EngineConfig;

  async fn on_workflow_start(&self, context: &WorkflowContext) -> Result<Option<String>, WorkflowError>;

  async fn record_workflow_end(&self, context: &WorkflowContext, workflow_id: Option<&str>);
}

#[cfg(not(feature = "security"))]
#[derive(Debug, Default)]
struct NoopSchedulerSecurityGate;

#[cfg(not(feature = "security"))]
#[async_trait]
impl SchedulerSecurityGate for NoopSchedulerSecurityGate {
  fn validate_schema(&self, schema: &WorkflowSchema, _context: &WorkflowContext) -> ValidationReport {
    validate_schema(schema)
  }

  async fn audit_validation_failed(&self, _context: &WorkflowContext, _report: &ValidationReport) {}

  fn configure_variable_pool(&self, _context: &WorkflowContext, _pool: &mut VariablePool) {}

  fn effective_engine_config(&self, _context: &WorkflowContext, config: EngineConfig) -> EngineConfig {
    config
  }

  async fn on_workflow_start(&self, _context: &WorkflowContext) -> Result<Option<String>, WorkflowError> {
    Ok(None)
  }

  async fn record_workflow_end(&self, _context: &WorkflowContext, _workflow_id: Option<&str>) {}
}

#[cfg(feature = "security")]
mod real {
  use super::*;

  use crate::security::audit::{EventSeverity, SecurityEvent, SecurityEventType};

  #[derive(Debug, Default)]
  pub struct RealSchedulerSecurityGate;

  #[async_trait]
  impl SchedulerSecurityGate for RealSchedulerSecurityGate {
    fn validate_schema(&self, schema: &WorkflowSchema, context: &WorkflowContext) -> ValidationReport {
      if let Some(policy) = context
        .security_policy()
        .and_then(|p| p.dsl_validation.as_ref())
      {
        crate::dsl::validation::validate_schema_with_config(schema, policy)
      } else {
        validate_schema(schema)
      }
    }

    async fn audit_validation_failed(&self, context: &WorkflowContext, report: &ValidationReport) {
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

    fn configure_variable_pool(&self, context: &WorkflowContext, pool: &mut VariablePool) {
      if let Some(selector_cfg) = context
        .security_policy()
        .and_then(|p| p.dsl_validation.as_ref())
        .and_then(|d| d.selector_validation.clone())
      {
        pool.set_selector_validation(Some(selector_cfg));
      }
    }

    fn effective_engine_config(&self, context: &WorkflowContext, config: EngineConfig) -> EngineConfig {
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

    async fn on_workflow_start(&self, context: &WorkflowContext) -> Result<Option<String>, WorkflowError> {
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

    async fn record_workflow_end(&self, context: &WorkflowContext, workflow_id: Option<&str>) {
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_scheduler_security_gate_validate_schema() {
    let gate = new_scheduler_security_gate();
    let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
    let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
    let context = WorkflowContext::default();
    let report = gate.validate_schema(&schema, &context);
    assert!(report.is_valid, "diagnostics: {:?}", report.diagnostics);
  }

  #[tokio::test]
  async fn test_scheduler_security_gate_audit_validation_noop() {
    let gate = new_scheduler_security_gate();
    let context = WorkflowContext::default();
    let report = crate::dsl::validation::ValidationReport {
      is_valid: true,
      diagnostics: vec![],
    };
    gate.audit_validation_failed(&context, &report).await;
  }

  #[test]
  fn test_scheduler_security_gate_configure_variable_pool() {
    let gate = new_scheduler_security_gate();
    let context = WorkflowContext::default();
    let mut pool = VariablePool::new();
    gate.configure_variable_pool(&context, &mut pool);
  }

  #[test]
  fn test_scheduler_security_gate_effective_engine_config() {
    let gate = new_scheduler_security_gate();
    let context = WorkflowContext::default();
    let config = EngineConfig {
      max_steps: 100,
      max_execution_time_secs: 300,
      strict_template: false,
    };
    let effective = gate.effective_engine_config(&context, config.clone());
    assert_eq!(effective.max_steps, 100);
    assert_eq!(effective.max_execution_time_secs, 300);
  }

  #[tokio::test]
  async fn test_scheduler_security_gate_on_workflow_start() {
    let gate = new_scheduler_security_gate();
    let context = WorkflowContext::default();
    let result = gate.on_workflow_start(&context).await;
    assert!(result.is_ok());
  }

  #[tokio::test]
  async fn test_scheduler_security_gate_record_workflow_end() {
    let gate = new_scheduler_security_gate();
    let context = WorkflowContext::default();
    gate.record_workflow_end(&context, Some("wf1")).await;
    gate.record_workflow_end(&context, None).await;
  }

  #[cfg(feature = "security")]
  #[test]
  fn test_real_scheduler_security_gate_effective_config_with_group() {
    use crate::security::resource_group::{ResourceGroup, ResourceQuota};
    use crate::security::policy::SecurityLevel;

    let gate = new_scheduler_security_gate();
    let mut context = WorkflowContext::default();
    context.set_resource_group(ResourceGroup {
      group_id: "grp".into(),
      group_name: None,
      security_level: SecurityLevel::Standard,
      quota: ResourceQuota {
        max_steps: 50,
        max_execution_time_secs: 120,
        ..Default::default()
      },
      credential_refs: std::collections::HashMap::new(),
    });
    let config = EngineConfig {
      max_steps: 200,
      max_execution_time_secs: 600,
      strict_template: true,
    };
    let effective = gate.effective_engine_config(&context, config);
    assert_eq!(effective.max_steps, 50);
    assert_eq!(effective.max_execution_time_secs, 120);
    assert!(effective.strict_template);
  }

  #[cfg(feature = "security")]
  #[tokio::test]
  async fn test_real_scheduler_security_gate_on_workflow_start_with_governor() {
    use crate::security::resource_group::{ResourceGroup, ResourceQuota};
    use crate::security::policy::SecurityLevel;
    use crate::security::governor::InMemoryResourceGovernor;

    let quota = ResourceQuota::default();
    let mut quotas = std::collections::HashMap::new();
    quotas.insert("grp1".to_string(), quota.clone());
    let governor = Arc::new(InMemoryResourceGovernor::new(quotas));

    let mut context = WorkflowContext::default();
    context.set_resource_group(ResourceGroup {
      group_id: "grp1".into(),
      group_name: None,
      security_level: SecurityLevel::Standard,
      quota,
      credential_refs: std::collections::HashMap::new(),
    });
    context.set_resource_governor(governor);
    let gate = new_scheduler_security_gate();
    let result: Result<Option<String>, WorkflowError> = gate.on_workflow_start(&context).await;
    assert!(result.is_ok());
    let workflow_id = result.unwrap();
    assert!(workflow_id.is_some());

    gate.record_workflow_end(&context, workflow_id.as_deref()).await;
  }

  #[cfg(feature = "security")]
  #[test]
  fn test_real_scheduler_security_gate_validate_with_policy() {
    use crate::security::policy::{SecurityLevel, SecurityPolicy};
    use crate::security::DslValidationConfig;

    let gate = new_scheduler_security_gate();
    let policy = SecurityPolicy {
      level: SecurityLevel::Strict,
      network: None,
      template: None,
      dsl_validation: Some(DslValidationConfig {
        max_nodes: 2,
        ..Default::default()
      }),
      node_limits: std::collections::HashMap::new(),
      audit_logger: None,
    };
    let mut context = WorkflowContext::default();
    context.set_security_policy(policy);
    let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"a","data":{"type":"code","title":"A","code":"x","language":"javascript"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"a"},{"source":"a","target":"end"}]}"#;
    let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
    let report = gate.validate_schema(&schema, &context);
    // max_nodes=2 but we have 3 nodes -> should fail
    assert!(report.diagnostics.iter().any(|d| d.code == "E020"), "got: {:?}", report.diagnostics);
  }

  #[cfg(feature = "security")]
  #[test]
  fn test_real_scheduler_security_gate_configure_variable_pool_with_selector_validation() {
    use crate::security::policy::{SecurityLevel, SecurityPolicy};
    use crate::security::{DslValidationConfig, SelectorValidation};

    let gate = new_scheduler_security_gate();
    let policy = SecurityPolicy {
      level: SecurityLevel::Standard,
      network: None,
      template: None,
      dsl_validation: Some(DslValidationConfig {
        selector_validation: Some(SelectorValidation {
          max_depth: 5,
          max_length: 100,
          allowed_prefixes: std::collections::HashSet::new(),
        }),
        ..Default::default()
      }),
      node_limits: std::collections::HashMap::new(),
      audit_logger: None,
    };
    let mut context = WorkflowContext::default();
    context.set_security_policy(policy);
    let mut pool = VariablePool::new();
    gate.configure_variable_pool(&context, &mut pool);
    // Pool should now have selector validation configured
  }
}
