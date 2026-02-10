use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::policy::SecurityLevel;

#[derive(Debug, Clone)]
pub struct ResourceGroup {
    pub group_id: String,
    pub group_name: Option<String>,
    pub security_level: SecurityLevel,
    pub quota: ResourceQuota,
    pub credential_refs: HashMap<String, String>,
}

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