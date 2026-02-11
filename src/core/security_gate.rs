use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use crate::core::dispatcher::EngineConfig;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::NodeRunResult;
use crate::error::NodeError;
use parking_lot::RwLock;

#[async_trait]
pub trait SecurityGate: Send + Sync {
  async fn check_before_node(
    &self,
    node_id: &str,
    node_type: &str,
    node_config: &Value,
  ) -> Result<(), NodeError>;

  async fn enforce_output_limits(
    &self,
    node_id: &str,
    node_type: &str,
    result: NodeRunResult,
  ) -> Result<NodeRunResult, NodeError>;

  async fn record_llm_usage(&self, result: &NodeRunResult);

  fn effective_limits(&self, config: &EngineConfig) -> (i32, u64);
}

#[derive(Debug, Default)]
pub struct NoopSecurityGate;

#[async_trait]
impl SecurityGate for NoopSecurityGate {
  async fn check_before_node(
    &self,
    _node_id: &str,
    _node_type: &str,
    _node_config: &Value,
  ) -> Result<(), NodeError> {
    Ok(())
  }

  async fn enforce_output_limits(
    &self,
    _node_id: &str,
    _node_type: &str,
    result: NodeRunResult,
  ) -> Result<NodeRunResult, NodeError> {
    Ok(result)
  }

  async fn record_llm_usage(&self, _result: &NodeRunResult) {}

  fn effective_limits(&self, config: &EngineConfig) -> (i32, u64) {
    (config.max_steps, config.max_execution_time_secs)
  }
}

#[cfg(feature = "security")]
mod real {
  use super::*;

  use crate::dsl::schema::NodeOutputs;
  use crate::error::{ErrorCode, ErrorContext};

  use crate::security::audit::{EventSeverity, SecurityEvent, SecurityEventType};
  use crate::security::governor::QuotaError;

  pub struct RealSecurityGate {
    context: Arc<RuntimeContext>,
    variable_pool: Arc<RwLock<VariablePool>>,
  }

  impl RealSecurityGate {
    pub fn new(context: Arc<RuntimeContext>, variable_pool: Arc<RwLock<VariablePool>>) -> Self {
      Self {
        context,
        variable_pool,
      }
    }

    async fn audit_security_event(
      &self,
      event_type: SecurityEventType,
      severity: EventSeverity,
      node_id: Option<String>,
    ) {
      let logger = match self.context.audit_logger() {
        Some(l) => l,
        None => return,
      };
      let group_id = match self.context.resource_group() {
        Some(g) => g.group_id.clone(),
        None => return,
      };
      let event = SecurityEvent {
        timestamp: self.context.time_provider.now_timestamp(),
        group_id,
        workflow_id: None,
        node_id,
        event_type,
        details: Value::Null,
        severity,
      };
      logger.log_event(event).await;
    }

    async fn handle_quota_error(&self, node_id: &str, err: QuotaError) -> NodeError {
      let (quota_type, limit, current, message) = match &err {
        QuotaError::ConcurrentWorkflowLimit { max, current } => (
          "concurrent_workflows".to_string(),
          *max as u64,
          *current as u64,
          format!("Concurrent workflow limit exceeded: {}/{}", current, max),
        ),
        QuotaError::HttpRateLimit { max_per_minute } => (
          "http_rate".to_string(),
          *max_per_minute as u64,
          *max_per_minute as u64,
          format!("HTTP rate limit exceeded: {} per minute", max_per_minute),
        ),
        QuotaError::LlmRateLimit { max_per_minute } => (
          "llm_rate".to_string(),
          *max_per_minute as u64,
          *max_per_minute as u64,
          format!("LLM rate limit exceeded: {} per minute", max_per_minute),
        ),
        QuotaError::LlmTokenBudgetExhausted { budget, used } => (
          "llm_token_budget".to_string(),
          *budget,
          *used,
          format!("LLM token budget exhausted: {}/{}", used, budget),
        ),
        QuotaError::LlmRequestTooLarge { max_tokens, requested } => (
          "llm_request_tokens".to_string(),
          *max_tokens as u64,
          *requested as u64,
          format!("LLM request tokens too large: {}/{}", requested, max_tokens),
        ),
        QuotaError::VariablePoolTooLarge { max_entries, current } => (
          "variable_pool_entries".to_string(),
          *max_entries as u64,
          *current as u64,
          format!("Variable pool entries exceeded: {}/{}", current, max_entries),
        ),
        QuotaError::VariablePoolMemoryExceeded { max_bytes, current } => (
          "variable_pool_bytes".to_string(),
          *max_bytes as u64,
          *current as u64,
          format!("Variable pool memory exceeded: {}/{}", current, max_bytes),
        ),
      };

      self
        .audit_security_event(
          SecurityEventType::QuotaExceeded {
            quota_type,
            limit,
            current,
          },
          EventSeverity::Warning,
          Some(node_id.to_string()),
        )
        .await;

      NodeError::InputValidationError(message).with_context(ErrorContext::non_retryable(
        ErrorCode::ResourceLimitExceeded,
        "resource limit exceeded",
      ))
    }

    fn check_output_size(
      &self,
      node_id: &str,
      node_type: &str,
      outputs: &NodeOutputs,
    ) -> Result<(), NodeError> {
      let Some(policy) = self.context.security_policy() else {
        return Ok(());
      };
      let Some(limits) = policy.node_limits.get(node_type) else {
        return Ok(());
      };

      let output_size: usize = outputs
        .ready()
        .values()
        .map(|v| serde_json::to_vec(v).map(|b| b.len()).unwrap_or(0))
        .sum();

      if output_size > limits.max_output_bytes {
        return Err(NodeError::OutputTooLarge {
          node_id: node_id.to_string(),
          max: limits.max_output_bytes,
          actual: output_size,
        }
        .with_context(ErrorContext::non_retryable(
          ErrorCode::OutputTooLarge,
          "output too large",
        )));
      }

      Ok(())
    }
  }

  #[async_trait]
  impl SecurityGate for RealSecurityGate {
    async fn check_before_node(
      &self,
      node_id: &str,
      node_type: &str,
      node_config: &Value,
    ) -> Result<(), NodeError> {
      let Some(governor) = self.context.resource_governor() else {
        return Ok(());
      };
      let Some(group) = self.context.resource_group() else {
        return Ok(());
      };

      let (pool_len, pool_bytes) = {
        let pool_snapshot = self.variable_pool.read();
        (pool_snapshot.len(), pool_snapshot.estimate_total_bytes())
      };
      if let Err(err) = governor
        .check_variable_pool_size(&group.group_id, pool_len, pool_bytes)
        .await
      {
        return Err(self.handle_quota_error(node_id, err).await);
      }

      match node_type {
        "http-request" => {
          if let Err(err) = governor.check_http_rate(&group.group_id).await {
            return Err(self.handle_quota_error(node_id, err).await);
          }
        }
        "llm" => {
          let estimated_tokens = node_config
            .get("model")
            .and_then(|m| m.get("completion_params"))
            .and_then(|p| p.get("max_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
          if let Err(err) = governor
            .check_llm_request(&group.group_id, estimated_tokens)
            .await
          {
            return Err(self.handle_quota_error(node_id, err).await);
          }
        }
        _ => {}
      }

      Ok(())
    }

    async fn enforce_output_limits(
      &self,
      node_id: &str,
      node_type: &str,
      result: NodeRunResult,
    ) -> Result<NodeRunResult, NodeError> {
      if let Err(err) = self.check_output_size(node_id, node_type, &result.outputs) {
        if let NodeError::OutputTooLarge { max, actual, .. } = &err {
          self
            .audit_security_event(
              SecurityEventType::OutputSizeExceeded {
                node_id: node_id.to_string(),
                max: *max,
                actual: *actual,
              },
              EventSeverity::Warning,
              Some(node_id.to_string()),
            )
            .await;
        }
        Err(err)
      } else {
        Ok(result)
      }
    }

    async fn record_llm_usage(&self, result: &NodeRunResult) {
      if let (Some(governor), Some(group), Some(usage)) = (
        self.context.resource_governor(),
        self.context.resource_group(),
        result.llm_usage.as_ref(),
      ) {
        governor.record_llm_usage(&group.group_id, usage).await;
      }
    }

    fn effective_limits(&self, config: &EngineConfig) -> (i32, u64) {
      if let Some(group) = self.context.resource_group() {
        let max_steps = config.max_steps.min(group.quota.max_steps);
        let max_time = config
          .max_execution_time_secs
          .min(group.quota.max_execution_time_secs);
        (max_steps, max_time)
      } else {
        (config.max_steps, config.max_execution_time_secs)
      }
    }
  }

  pub fn new_gate(
    context: Arc<RuntimeContext>,
    variable_pool: Arc<RwLock<VariablePool>>,
  ) -> Arc<dyn SecurityGate> {
    Arc::new(RealSecurityGate::new(context, variable_pool))
  }
}

#[cfg(feature = "security")]
pub use real::new_gate as new_security_gate;

#[cfg(not(feature = "security"))]
pub fn new_security_gate(
  _context: Arc<RuntimeContext>,
  _variable_pool: Arc<RwLock<VariablePool>>,
) -> Arc<dyn SecurityGate> {
  Arc::new(NoopSecurityGate)
}
