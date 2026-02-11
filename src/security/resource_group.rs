//! Resource group definitions and quota configuration.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::policy::SecurityLevel;

/// A logical tenant grouping that shares quotas and credentials.
#[derive(Debug, Clone)]
pub struct ResourceGroup {
    pub group_id: String,
    pub group_name: Option<String>,
    pub security_level: SecurityLevel,
    pub quota: ResourceQuota,
    pub credential_refs: HashMap<String, String>,
}

/// Numeric limits for a [`ResourceGroup`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceQuota {
    pub max_concurrent_workflows: usize,
    pub max_variable_pool_entries: usize,
    pub max_variable_pool_bytes: usize,
    pub max_steps: i32,
    pub max_execution_time_secs: u64,
    pub http_rate_limit_per_minute: usize,
    pub llm_rate_limit_per_minute: usize,
    pub llm_daily_token_budget: u64,
    pub llm_max_tokens_per_request: u32,
    pub http_max_response_bytes: usize,
}

impl Default for ResourceQuota {
    fn default() -> Self {
        Self {
            max_concurrent_workflows: 10,
            max_variable_pool_entries: 10_000,
            max_variable_pool_bytes: 50 * 1024 * 1024,
            max_steps: 500,
            max_execution_time_secs: 600,
            http_rate_limit_per_minute: 100,
            llm_rate_limit_per_minute: 60,
            llm_daily_token_budget: 1_000_000,
            llm_max_tokens_per_request: 4096,
            http_max_response_bytes: 10 * 1024 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_quota_default() {
        let q = ResourceQuota::default();
        assert_eq!(q.max_concurrent_workflows, 10);
        assert_eq!(q.max_variable_pool_entries, 10_000);
        assert_eq!(q.max_variable_pool_bytes, 50 * 1024 * 1024);
        assert_eq!(q.max_steps, 500);
        assert_eq!(q.max_execution_time_secs, 600);
        assert_eq!(q.http_rate_limit_per_minute, 100);
        assert_eq!(q.llm_rate_limit_per_minute, 60);
        assert_eq!(q.llm_daily_token_budget, 1_000_000);
        assert_eq!(q.llm_max_tokens_per_request, 4096);
        assert_eq!(q.http_max_response_bytes, 10 * 1024 * 1024);
    }

    #[test]
    fn test_resource_quota_serde_roundtrip() {
        let q = ResourceQuota::default();
        let json = serde_json::to_string(&q).unwrap();
        let deserialized: ResourceQuota = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_concurrent_workflows, q.max_concurrent_workflows);
        assert_eq!(deserialized.max_steps, q.max_steps);
    }

    #[test]
    fn test_resource_group_creation() {
        let group = ResourceGroup {
            group_id: "g1".into(),
            group_name: Some("Test Group".into()),
            security_level: SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };
        assert_eq!(group.group_id, "g1");
        assert_eq!(group.group_name, Some("Test Group".to_string()));
        assert_eq!(group.security_level, SecurityLevel::Standard);
    }

    #[test]
    fn test_resource_group_with_credentials() {
        let mut creds = HashMap::new();
        creds.insert("openai".into(), "ref-123".into());
        let group = ResourceGroup {
            group_id: "g2".into(),
            group_name: None,
            security_level: SecurityLevel::Permissive,
            quota: ResourceQuota::default(),
            credential_refs: creds,
        };
        assert_eq!(group.credential_refs.get("openai"), Some(&"ref-123".to_string()));
    }
}