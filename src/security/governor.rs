//! Resource governance: rate limiting, quota enforcement, and usage tracking.

use async_trait::async_trait;
use chrono::{Datelike, Utc};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::dsl::schema::LlmUsage;

use super::resource_group::ResourceQuota;

/// Errors returned when a resource quota is exceeded.
#[derive(Debug, Clone, thiserror::Error)]
pub enum QuotaError {
    #[error("Concurrent workflow limit exceeded: {current}/{max}")]
    ConcurrentWorkflowLimit { max: usize, current: usize },
    #[error("HTTP rate limit exceeded: {max_per_minute} per minute")]
    HttpRateLimit { max_per_minute: usize },
    #[error("LLM rate limit exceeded: {max_per_minute} per minute")]
    LlmRateLimit { max_per_minute: usize },
    #[error("LLM token budget exhausted: {used}/{budget}")]
    LlmTokenBudgetExhausted { budget: u64, used: u64 },
    #[error("LLM request tokens too large: {requested}/{max_tokens}")]
    LlmRequestTooLarge { max_tokens: u32, requested: u32 },
    #[error("Variable pool entries exceeded: {current}/{max_entries}")]
    VariablePoolTooLarge { max_entries: usize, current: usize },
    #[error("Variable pool memory exceeded: {current}/{max_bytes}")]
    VariablePoolMemoryExceeded { max_bytes: usize, current: usize },
}

/// Point-in-time usage snapshot for a resource group.
#[derive(Debug, Clone, Default)]
pub struct GroupUsage {
    pub active_workflows: usize,
    pub http_requests_this_minute: usize,
    pub llm_requests_this_minute: usize,
    pub llm_tokens_today: u64,
}

/// Trait for enforcing resource quotas across workflow executions.
#[async_trait]
pub trait ResourceGovernor: Send + Sync {
    /// Check whether the group can start a new workflow.
    async fn check_workflow_start(&self, group_id: &str) -> Result<(), QuotaError>;
    /// Record that a workflow has started.
    async fn record_workflow_start(&self, group_id: &str, workflow_id: &str);
    /// Record that a workflow has ended.
    async fn record_workflow_end(&self, group_id: &str, workflow_id: &str);
    /// Check HTTP rate-limit for the group.
    async fn check_http_rate(&self, group_id: &str) -> Result<(), QuotaError>;
    /// Check whether the group can make an LLM request with the estimated token count.
    async fn check_llm_request(
        &self,
        group_id: &str,
        estimated_tokens: u32,
    ) -> Result<(), QuotaError>;
    /// Record actual LLM token usage after a completion.
    async fn record_llm_usage(&self, group_id: &str, usage: &LlmUsage);
    /// Check variable pool size limits.
    async fn check_variable_pool_size(
        &self,
        group_id: &str,
        current_entries: usize,
        current_bytes_estimate: usize,
    ) -> Result<(), QuotaError>;
    /// Return the current usage snapshot for a group.
    async fn get_usage(&self, group_id: &str) -> GroupUsage;
}

#[derive(Debug, Default)]
struct GroupUsageState {
    active_workflows: usize,
    http_requests: u64,
    http_window: i64,
    llm_requests: u64,
    llm_window: i64,
    llm_tokens: u64,
    llm_day: i32,
}

#[derive(Clone, Default)]
pub struct InMemoryResourceGovernor {
    quotas: Arc<HashMap<String, ResourceQuota>>,
    usage: Arc<DashMap<String, Arc<Mutex<GroupUsageState>>>>,
}

impl InMemoryResourceGovernor {
    pub fn new(quotas: HashMap<String, ResourceQuota>) -> Self {
        Self {
            quotas: Arc::new(quotas),
            usage: Arc::new(DashMap::new()),
        }
    }

    fn quota_for(&self, group_id: &str) -> ResourceQuota {
        self.quotas
            .get(group_id)
            .cloned()
            .unwrap_or_default()
    }

    async fn state_for(&self, group_id: &str) -> Arc<Mutex<GroupUsageState>> {
        if let Some(entry) = self.usage.get(group_id) {
            return entry.value().clone();
        }
        let state = Arc::new(Mutex::new(GroupUsageState::default()));
        self.usage.insert(group_id.to_string(), state.clone());
        state
    }

    fn current_minute() -> i64 {
        let now = Utc::now();
        now.timestamp() / 60
    }

    fn current_day_key() -> i32 {
        let now = Utc::now();
        now.year() * 10_000 + now.month() as i32 * 100 + now.day() as i32
    }
}

#[async_trait]
impl ResourceGovernor for InMemoryResourceGovernor {
    async fn check_workflow_start(&self, group_id: &str) -> Result<(), QuotaError> {
        let quota = self.quota_for(group_id);
        let state = self.state_for(group_id).await;
        let state_guard = state.lock().await;
        if state_guard.active_workflows >= quota.max_concurrent_workflows {
            return Err(QuotaError::ConcurrentWorkflowLimit {
                max: quota.max_concurrent_workflows,
                current: state_guard.active_workflows,
            });
        }
        Ok(())
    }

    async fn record_workflow_start(&self, group_id: &str, _workflow_id: &str) {
        let state = self.state_for(group_id).await;
        let mut guard = state.lock().await;
        guard.active_workflows += 1;
    }

    async fn record_workflow_end(&self, group_id: &str, _workflow_id: &str) {
        let state = self.state_for(group_id).await;
        let mut guard = state.lock().await;
        if guard.active_workflows > 0 {
            guard.active_workflows -= 1;
        }
    }

    async fn check_http_rate(&self, group_id: &str) -> Result<(), QuotaError> {
        let quota = self.quota_for(group_id);
        let state = self.state_for(group_id).await;
        let mut guard = state.lock().await;
        let now_minute = Self::current_minute();
        if guard.http_window != now_minute {
            guard.http_window = now_minute;
            guard.http_requests = 0;
        }
        if guard.http_requests as usize >= quota.http_rate_limit_per_minute {
            return Err(QuotaError::HttpRateLimit {
                max_per_minute: quota.http_rate_limit_per_minute,
            });
        }
        guard.http_requests += 1;
        Ok(())
    }

    async fn check_llm_request(
        &self,
        group_id: &str,
        estimated_tokens: u32,
    ) -> Result<(), QuotaError> {
        let quota = self.quota_for(group_id);
        if estimated_tokens > 0 && estimated_tokens > quota.llm_max_tokens_per_request {
            return Err(QuotaError::LlmRequestTooLarge {
                max_tokens: quota.llm_max_tokens_per_request,
                requested: estimated_tokens,
            });
        }

        let state = self.state_for(group_id).await;
        let mut guard = state.lock().await;
        let now_minute = Self::current_minute();
        if guard.llm_window != now_minute {
            guard.llm_window = now_minute;
            guard.llm_requests = 0;
        }
        if guard.llm_requests as usize >= quota.llm_rate_limit_per_minute {
            return Err(QuotaError::LlmRateLimit {
                max_per_minute: quota.llm_rate_limit_per_minute,
            });
        }

        let day_key = Self::current_day_key();
        if guard.llm_day != day_key {
            guard.llm_day = day_key;
            guard.llm_tokens = 0;
        }

        if estimated_tokens > 0 {
            let projected = guard.llm_tokens + estimated_tokens as u64;
            if projected > quota.llm_daily_token_budget {
                return Err(QuotaError::LlmTokenBudgetExhausted {
                    budget: quota.llm_daily_token_budget,
                    used: guard.llm_tokens,
                });
            }
        }

        guard.llm_requests += 1;
        Ok(())
    }

    async fn record_llm_usage(&self, group_id: &str, usage: &LlmUsage) {
        let state = self.state_for(group_id).await;
        let mut guard = state.lock().await;
        let day_key = Self::current_day_key();
        if guard.llm_day != day_key {
            guard.llm_day = day_key;
            guard.llm_tokens = 0;
        }
        let total = usage.total_tokens.max(0) as u64;
        guard.llm_tokens = guard.llm_tokens.saturating_add(total);
    }

    async fn check_variable_pool_size(
        &self,
        group_id: &str,
        current_entries: usize,
        current_bytes_estimate: usize,
    ) -> Result<(), QuotaError> {
        let quota = self.quota_for(group_id);
        if current_entries > quota.max_variable_pool_entries {
            return Err(QuotaError::VariablePoolTooLarge {
                max_entries: quota.max_variable_pool_entries,
                current: current_entries,
            });
        }
        if current_bytes_estimate > quota.max_variable_pool_bytes {
            return Err(QuotaError::VariablePoolMemoryExceeded {
                max_bytes: quota.max_variable_pool_bytes,
                current: current_bytes_estimate,
            });
        }
        Ok(())
    }

    async fn get_usage(&self, group_id: &str) -> GroupUsage {
        let state = self.state_for(group_id).await;
        let guard = state.lock().await;
        GroupUsage {
            active_workflows: guard.active_workflows,
            http_requests_this_minute: guard.http_requests as usize,
            llm_requests_this_minute: guard.llm_requests as usize,
            llm_tokens_today: guard.llm_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_governor_concurrent_limit() {
        let mut quotas = HashMap::new();
        let mut quota = ResourceQuota::default();
        quota.max_concurrent_workflows = 1;
        quotas.insert("g1".to_string(), quota);
        let governor = InMemoryResourceGovernor::new(quotas);

        assert!(governor.check_workflow_start("g1").await.is_ok());
        governor.record_workflow_start("g1", "w1").await;
        let err = governor.check_workflow_start("g1").await.unwrap_err();
        match err {
            QuotaError::ConcurrentWorkflowLimit { max, current } => {
                assert_eq!(max, 1);
                assert_eq!(current, 1);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_governor_http_rate_limit() {
        let mut quotas = HashMap::new();
        let mut quota = ResourceQuota::default();
        quota.http_rate_limit_per_minute = 1;
        quotas.insert("g1".to_string(), quota);
        let governor = InMemoryResourceGovernor::new(quotas);

        assert!(governor.check_http_rate("g1").await.is_ok());
        let err = governor.check_http_rate("g1").await.unwrap_err();
        match err {
            QuotaError::HttpRateLimit { max_per_minute } => {
                assert_eq!(max_per_minute, 1);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_governor_llm_rate_limit() {
        let mut quotas = HashMap::new();
        let mut quota = ResourceQuota::default();
        quota.llm_rate_limit_per_minute = 1;
        quotas.insert("g1".to_string(), quota);
        let governor = InMemoryResourceGovernor::new(quotas);

        assert!(governor.check_llm_request("g1", 0).await.is_ok());
        let err = governor.check_llm_request("g1", 0).await.unwrap_err();
        match err {
            QuotaError::LlmRateLimit { max_per_minute } => assert_eq!(max_per_minute, 1),
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_governor_llm_request_too_large() {
        let mut quotas = HashMap::new();
        let mut quota = ResourceQuota::default();
        quota.llm_max_tokens_per_request = 100;
        quotas.insert("g1".to_string(), quota);
        let governor = InMemoryResourceGovernor::new(quotas);

        let err = governor.check_llm_request("g1", 200).await.unwrap_err();
        match err {
            QuotaError::LlmRequestTooLarge { max_tokens, requested } => {
                assert_eq!(max_tokens, 100);
                assert_eq!(requested, 200);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_governor_llm_token_budget() {
        let mut quotas = HashMap::new();
        let mut quota = ResourceQuota::default();
        quota.llm_daily_token_budget = 50;
        quota.llm_rate_limit_per_minute = 100;
        quotas.insert("g1".to_string(), quota);
        let governor = InMemoryResourceGovernor::new(quotas);

        assert!(governor.check_llm_request("g1", 30).await.is_ok());
        // Record usage to increment the token counter
        let usage = LlmUsage { total_tokens: 30, ..Default::default() };
        governor.record_llm_usage("g1", &usage).await;
        let err = governor.check_llm_request("g1", 30).await.unwrap_err();
        match err {
            QuotaError::LlmTokenBudgetExhausted { budget, .. } => assert_eq!(budget, 50),
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_governor_record_workflow_end() {
        let quotas = HashMap::new();
        let governor = InMemoryResourceGovernor::new(quotas);

        governor.record_workflow_start("g1", "w1").await;
        let usage = governor.get_usage("g1").await;
        assert_eq!(usage.active_workflows, 1);

        governor.record_workflow_end("g1", "w1").await;
        let usage = governor.get_usage("g1").await;
        assert_eq!(usage.active_workflows, 0);

        // Should not underflow
        governor.record_workflow_end("g1", "w1").await;
        let usage = governor.get_usage("g1").await;
        assert_eq!(usage.active_workflows, 0);
    }

    #[tokio::test]
    async fn test_governor_record_llm_usage() {
        let quotas = HashMap::new();
        let governor = InMemoryResourceGovernor::new(quotas);

        let usage_data = LlmUsage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            ..Default::default()
        };
        governor.record_llm_usage("g1", &usage_data).await;
        let usage = governor.get_usage("g1").await;
        assert_eq!(usage.llm_tokens_today, 30);
    }

    #[tokio::test]
    async fn test_governor_variable_pool_size() {
        let mut quotas = HashMap::new();
        let mut quota = ResourceQuota::default();
        quota.max_variable_pool_entries = 10;
        quota.max_variable_pool_bytes = 1000;
        quotas.insert("g1".to_string(), quota);
        let governor = InMemoryResourceGovernor::new(quotas);

        assert!(governor.check_variable_pool_size("g1", 5, 500).await.is_ok());
        let err = governor.check_variable_pool_size("g1", 20, 500).await.unwrap_err();
        match err {
            QuotaError::VariablePoolTooLarge { max_entries, current } => {
                assert_eq!(max_entries, 10);
                assert_eq!(current, 20);
            }
            other => panic!("unexpected error: {:?}", other),
        }
        let err = governor.check_variable_pool_size("g1", 5, 2000).await.unwrap_err();
        match err {
            QuotaError::VariablePoolMemoryExceeded { max_bytes, current } => {
                assert_eq!(max_bytes, 1000);
                assert_eq!(current, 2000);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_governor_get_usage_unknown_group() {
        let quotas = HashMap::new();
        let governor = InMemoryResourceGovernor::new(quotas);
        let usage = governor.get_usage("unknown").await;
        assert_eq!(usage.active_workflows, 0);
        assert_eq!(usage.http_requests_this_minute, 0);
    }

    #[test]
    fn test_quota_error_display() {
        let e = QuotaError::ConcurrentWorkflowLimit { max: 5, current: 5 };
        assert!(e.to_string().contains("5"));
        let e = QuotaError::LlmRateLimit { max_per_minute: 10 };
        assert!(e.to_string().contains("10"));
        let e = QuotaError::LlmTokenBudgetExhausted { budget: 1000, used: 500 };
        assert!(e.to_string().contains("1000"));
        let e = QuotaError::LlmRequestTooLarge { max_tokens: 100, requested: 200 };
        assert!(e.to_string().contains("200"));
        let e = QuotaError::VariablePoolTooLarge { max_entries: 10, current: 20 };
        assert!(e.to_string().contains("20"));
        let e = QuotaError::VariablePoolMemoryExceeded { max_bytes: 1000, current: 2000 };
        assert!(e.to_string().contains("2000"));
    }
}