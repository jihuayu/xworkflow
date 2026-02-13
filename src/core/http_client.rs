#![expect(
    clippy::result_large_err,
    reason = "HTTP client helpers return NodeError to align with node execution API contracts"
)]

use std::time::Duration;

#[cfg(feature = "security")]
use parking_lot::Mutex;

#[cfg(feature = "security")]
use std::collections::HashMap;

#[cfg(feature = "security")]
use std::time::Instant;

use crate::core::runtime_context::RuntimeContext;
use crate::error::NodeError;

#[cfg(feature = "security")]
use crate::security::{SecureHttpClientFactory, SecurityPolicy};

#[derive(Debug, Clone)]
pub struct HttpPoolConfig {
    pub pool_max_idle_per_host: usize,
    pub pool_idle_timeout: Duration,
    pub default_timeout: Duration,
    pub tcp_keepalive: Option<Duration>,
    pub http2_enabled: bool,
    #[cfg(feature = "security")]
    pub max_group_clients: usize,
    #[cfg(feature = "security")]
    pub group_client_idle_timeout: Duration,
}

impl Default for HttpPoolConfig {
    fn default() -> Self {
        Self {
            pool_max_idle_per_host: 10,
            pool_idle_timeout: Duration::from_secs(90),
            default_timeout: Duration::from_secs(30),
            tcp_keepalive: Some(Duration::from_secs(60)),
            http2_enabled: true,
            #[cfg(feature = "security")]
            max_group_clients: 32,
            #[cfg(feature = "security")]
            group_client_idle_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Debug)]
pub struct HttpClientProvider {
    #[cfg(not(feature = "security"))]
    standard_client: reqwest::Client,
    #[cfg(feature = "security")]
    group_clients: Mutex<HashMap<GroupClientKey, GroupClientEntry>>,
    config: HttpPoolConfig,
}

#[cfg(feature = "security")]
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct GroupClientKey {
    group_id: String,
    policy_hash: u64,
}

#[cfg(feature = "security")]
#[derive(Debug, Clone)]
struct GroupClientEntry {
    client: reqwest::Client,
    created_at: Instant,
    last_used: Instant,
    request_count: u64,
}

#[cfg(feature = "security")]
#[derive(Debug, Clone)]
pub struct GroupClientStats {
    pub group_id: String,
    pub request_count: u64,
    pub idle_secs: u64,
    pub age_secs: u64,
}

#[cfg(feature = "security")]
#[derive(Debug, Clone)]
pub struct HttpPoolStats {
    pub active_groups: usize,
    pub entries: Vec<GroupClientStats>,
}

impl Default for HttpClientProvider {
    fn default() -> Self {
        Self::new(HttpPoolConfig::default())
    }
}

impl HttpClientProvider {
    pub fn new(config: HttpPoolConfig) -> Self {
        Self {
            #[cfg(not(feature = "security"))]
            standard_client: Self::build_default_client(&config)
                .expect("failed to create default HTTP client"),
            #[cfg(feature = "security")]
            group_clients: Mutex::new(HashMap::new()),
            config,
        }
    }

    fn apply_pool_options(
        mut builder: reqwest::ClientBuilder,
        config: &HttpPoolConfig,
    ) -> reqwest::ClientBuilder {
        builder = builder
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .pool_idle_timeout(config.pool_idle_timeout)
            .tcp_keepalive(config.tcp_keepalive)
            .timeout(config.default_timeout);

        if !config.http2_enabled {
            builder = builder.http1_only();
        }

        builder
    }

    fn build_default_client(config: &HttpPoolConfig) -> Result<reqwest::Client, NodeError> {
        Self::apply_pool_options(reqwest::Client::builder(), config)
            .build()
            .map_err(|e| NodeError::HttpError(e.to_string()))
    }

    #[cfg(not(feature = "security"))]
    pub fn client_for_context(
        &self,
        _context: &RuntimeContext,
    ) -> Result<reqwest::Client, NodeError> {
        Ok(self.standard_client.clone())
    }

    #[cfg(feature = "security")]
    pub fn client_for_context(
        &self,
        context: &RuntimeContext,
    ) -> Result<reqwest::Client, NodeError> {
        let group = context.resource_group();
        let policy = context.security_policy();

        let Some(group) = group else {
            return Self::build_client_with_policy(&self.config, policy);
        };

        let key = GroupClientKey {
            group_id: group.group_id.clone(),
            policy_hash: policy.map(SecurityPolicy::network_hash).unwrap_or(0),
        };

        let now = Instant::now();
        let mut cache = self.group_clients.lock();
        Self::cleanup_expired_locked(&mut cache, self.config.group_client_idle_timeout, now);

        if let Some(entry) = cache.get_mut(&key) {
            entry.last_used = now;
            entry.request_count = entry.request_count.saturating_add(1);
            return Ok(entry.client.clone());
        }

        let client = Self::build_client_with_policy(&self.config, policy)?;

        if cache.len() >= self.config.max_group_clients {
            let oldest_key = cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());
            if let Some(k) = oldest_key {
                cache.remove(&k);
            }
        }

        cache.insert(
            key,
            GroupClientEntry {
                client: client.clone(),
                created_at: now,
                last_used: now,
                request_count: 1,
            },
        );

        Ok(client)
    }

    #[cfg(feature = "security")]
    fn build_client_with_policy(
        config: &HttpPoolConfig,
        policy: Option<&SecurityPolicy>,
    ) -> Result<reqwest::Client, NodeError> {
        if let Some(network_policy) = policy.and_then(|p| p.network.as_ref()) {
            let builder = SecureHttpClientFactory::builder(network_policy, config.default_timeout);
            return Self::apply_pool_options(builder, config)
                .build()
                .map_err(|e| NodeError::HttpError(e.to_string()));
        }

        Self::build_default_client(config)
    }

    #[cfg(feature = "security")]
    fn cleanup_expired_locked(
        cache: &mut HashMap<GroupClientKey, GroupClientEntry>,
        timeout: Duration,
        now: Instant,
    ) {
        cache.retain(|_, entry| now.saturating_duration_since(entry.last_used) < timeout);
    }

    #[cfg(feature = "security")]
    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        let mut cache = self.group_clients.lock();
        Self::cleanup_expired_locked(&mut cache, self.config.group_client_idle_timeout, now);
    }

    #[cfg(feature = "security")]
    pub fn stats(&self) -> HttpPoolStats {
        let now = Instant::now();
        let cache = self.group_clients.lock();
        HttpPoolStats {
            active_groups: cache.len(),
            entries: cache
                .iter()
                .map(|(k, v)| GroupClientStats {
                    group_id: k.group_id.clone(),
                    request_count: v.request_count,
                    idle_secs: now.saturating_duration_since(v.last_used).as_secs(),
                    age_secs: now.saturating_duration_since(v.created_at).as_secs(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn test_http_pool_config_default() {
        let config = HttpPoolConfig::default();
        assert_eq!(config.pool_max_idle_per_host, 10);
        assert_eq!(config.pool_idle_timeout, Duration::from_secs(90));
        assert_eq!(config.default_timeout, Duration::from_secs(30));
        assert_eq!(config.tcp_keepalive, Some(Duration::from_secs(60)));
        assert!(config.http2_enabled);
        #[cfg(feature = "security")]
        {
            assert_eq!(config.max_group_clients, 32);
            assert_eq!(config.group_client_idle_timeout, Duration::from_secs(300));
        }
    }

    #[test]
    fn test_http_pool_config_custom() {
        let config = HttpPoolConfig {
            pool_max_idle_per_host: 5,
            pool_idle_timeout: Duration::from_secs(120),
            default_timeout: Duration::from_secs(60),
            tcp_keepalive: None,
            http2_enabled: false,
            #[cfg(feature = "security")]
            max_group_clients: 16,
            #[cfg(feature = "security")]
            group_client_idle_timeout: Duration::from_secs(600),
        };
        assert_eq!(config.pool_max_idle_per_host, 5);
        assert_eq!(config.pool_idle_timeout, Duration::from_secs(120));
        assert_eq!(config.default_timeout, Duration::from_secs(60));
        assert_eq!(config.tcp_keepalive, None);
        assert!(!config.http2_enabled);
    }

    #[test]
    fn test_http_client_provider_default() {
        let provider = HttpClientProvider::default();
        assert_eq!(provider.config.pool_max_idle_per_host, 10);
        assert_eq!(provider.config.default_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_http_client_provider_custom_config() {
        let config = HttpPoolConfig {
            pool_max_idle_per_host: 15,
            pool_idle_timeout: Duration::from_secs(100),
            default_timeout: Duration::from_secs(45),
            tcp_keepalive: Some(Duration::from_secs(30)),
            http2_enabled: true,
            #[cfg(feature = "security")]
            max_group_clients: 64,
            #[cfg(feature = "security")]
            group_client_idle_timeout: Duration::from_secs(400),
        };
        let provider = HttpClientProvider::new(config.clone());
        assert_eq!(provider.config.pool_max_idle_per_host, 15);
        assert_eq!(provider.config.default_timeout, Duration::from_secs(45));
    }

    #[cfg(not(feature = "security"))]
    #[tokio::test]
    async fn test_client_for_context_no_security() {
        let provider = HttpClientProvider::default();
        let context = RuntimeContext::default();
        let client = provider.client_for_context(&context).unwrap();
        assert!(client.get("https://example.com").build().is_ok());
    }

    #[cfg(not(feature = "security"))]
    #[tokio::test]
    async fn test_client_reuse_no_security() {
        let provider = HttpClientProvider::default();
        let context = RuntimeContext::default();
        let client1 = provider.client_for_context(&context).unwrap();
        let client2 = provider.client_for_context(&context).unwrap();
        // Both clients should come from the same standard_client
        let req1 = client1.get("https://example.com").build().unwrap();
        let req2 = client2.get("https://example.com").build().unwrap();
        assert_eq!(req1.url(), req2.url());
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_client_for_context_with_security() {
        use crate::core::runtime_group::RuntimeGroup;
        use crate::core::workflow_context::WorkflowContext;
        use crate::security::{ResourceGroup, ResourceQuota, SecurityLevel};
        use std::collections::HashMap;

        let provider = HttpClientProvider::default();
        let group = ResourceGroup {
            group_id: "test_group".to_string(),
            group_name: Some("Test Group".to_string()),
            security_level: SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };

        let runtime_group = RuntimeGroup::builder().resource_group(group).build();
        let context = WorkflowContext::new(Arc::new(runtime_group));

        let client = provider.client_for_context(&context).unwrap();
        assert!(client.get("https://example.com").build().is_ok());
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_client_caching_same_group() {
        use crate::core::runtime_group::RuntimeGroup;
        use crate::core::workflow_context::WorkflowContext;
        use crate::security::{ResourceGroup, ResourceQuota, SecurityLevel};
        use std::collections::HashMap;

        let provider = HttpClientProvider::default();
        let group = ResourceGroup {
            group_id: "test_group".to_string(),
            group_name: Some("Test Group".to_string()),
            security_level: SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };

        let runtime_group = RuntimeGroup::builder().resource_group(group).build();
        let context = WorkflowContext::new(Arc::new(runtime_group));

        // First request should create a new client
        let _client1 = provider.client_for_context(&context).unwrap();

        // Second request should reuse cached client
        let _client2 = provider.client_for_context(&context).unwrap();

        let stats = provider.stats();
        assert_eq!(stats.active_groups, 1);
        assert_eq!(stats.entries.len(), 1);
        assert_eq!(stats.entries[0].group_id, "test_group");
        assert_eq!(stats.entries[0].request_count, 2);
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_client_caching_different_groups() {
        use crate::core::runtime_group::RuntimeGroup;
        use crate::core::workflow_context::WorkflowContext;
        use crate::security::{ResourceGroup, ResourceQuota, SecurityLevel};
        use std::collections::HashMap;

        let provider = HttpClientProvider::default();

        let group1 = ResourceGroup {
            group_id: "group1".to_string(),
            group_name: Some("Group 1".to_string()),
            security_level: SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };
        let runtime_group1 = RuntimeGroup::builder().resource_group(group1).build();
        let context1 = WorkflowContext::new(Arc::new(runtime_group1));

        let group2 = ResourceGroup {
            group_id: "group2".to_string(),
            group_name: Some("Group 2".to_string()),
            security_level: SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };
        let runtime_group2 = RuntimeGroup::builder().resource_group(group2).build();
        let context2 = WorkflowContext::new(Arc::new(runtime_group2));

        let _client1 = provider.client_for_context(&context1).unwrap();
        let _client2 = provider.client_for_context(&context2).unwrap();

        let stats = provider.stats();
        assert_eq!(stats.active_groups, 2);
        assert_eq!(stats.entries.len(), 2);
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_client_cache_eviction() {
        use crate::core::runtime_group::RuntimeGroup;
        use crate::core::workflow_context::WorkflowContext;
        use crate::security::{ResourceGroup, ResourceQuota, SecurityLevel};
        use std::collections::HashMap;

        let config = HttpPoolConfig {
            max_group_clients: 2,
            ..Default::default()
        };
        let provider = HttpClientProvider::new(config);

        // Add 3 groups (should evict the oldest)
        for i in 0..3 {
            let group = ResourceGroup {
                group_id: format!("group{}", i),
                group_name: Some(format!("Group {}", i)),
                security_level: SecurityLevel::Standard,
                quota: ResourceQuota::default(),
                credential_refs: HashMap::new(),
            };
            let runtime_group = RuntimeGroup::builder().resource_group(group).build();
            let context = WorkflowContext::new(Arc::new(runtime_group));
            let _client = provider.client_for_context(&context).unwrap();

            // Add small delay to ensure different timestamps
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let stats = provider.stats();
        // Should only have 2 groups (max_group_clients)
        assert_eq!(stats.active_groups, 2);
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_cleanup_expired() {
        use crate::core::runtime_group::RuntimeGroup;
        use crate::core::workflow_context::WorkflowContext;
        use crate::security::{ResourceGroup, ResourceQuota, SecurityLevel};
        use std::collections::HashMap;

        let config = HttpPoolConfig {
            group_client_idle_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let provider = HttpClientProvider::new(config);

        let group = ResourceGroup {
            group_id: "test_group".to_string(),
            group_name: Some("Test Group".to_string()),
            security_level: SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };
        let runtime_group = RuntimeGroup::builder().resource_group(group).build();
        let context = WorkflowContext::new(Arc::new(runtime_group));

        let _client = provider.client_for_context(&context).unwrap();

        let stats_before = provider.stats();
        assert_eq!(stats_before.active_groups, 1);

        // Wait for timeout
        tokio::time::sleep(Duration::from_millis(150)).await;

        provider.cleanup_expired();

        let stats_after = provider.stats();
        assert_eq!(stats_after.active_groups, 0);
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_stats_accuracy() {
        use crate::core::runtime_group::RuntimeGroup;
        use crate::core::workflow_context::WorkflowContext;
        use crate::security::{ResourceGroup, ResourceQuota, SecurityLevel};
        use std::collections::HashMap;

        let provider = HttpClientProvider::default();
        let group = ResourceGroup {
            group_id: "test_group".to_string(),
            group_name: Some("Test Group".to_string()),
            security_level: SecurityLevel::Standard,
            quota: ResourceQuota::default(),
            credential_refs: HashMap::new(),
        };
        let runtime_group = RuntimeGroup::builder().resource_group(group).build();
        let context = WorkflowContext::new(Arc::new(runtime_group));

        // Make multiple requests
        for _ in 0..5 {
            let _client = provider.client_for_context(&context).unwrap();
        }

        let stats = provider.stats();
        assert_eq!(stats.active_groups, 1);
        assert_eq!(stats.entries[0].request_count, 5);
        assert_eq!(stats.entries[0].group_id, "test_group");
        assert!(stats.entries[0].idle_secs < 1); // Just created
        assert!(stats.entries[0].age_secs < 1); // Just created
    }

    #[cfg(feature = "security")]
    #[tokio::test]
    async fn test_no_group_context() {
        let provider = HttpClientProvider::default();
        let context = RuntimeContext::default();

        // Should build a client without caching
        let client = provider.client_for_context(&context).unwrap();
        assert!(client.get("https://example.com").build().is_ok());

        // Stats should show no cached groups
        let stats = provider.stats();
        assert_eq!(stats.active_groups, 0);
    }

    #[test]
    fn test_build_default_client() {
        let config = HttpPoolConfig::default();
        let result = HttpClientProvider::build_default_client(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_client_http1_only() {
        let config = HttpPoolConfig {
            http2_enabled: false,
            ..Default::default()
        };
        let result = HttpClientProvider::build_default_client(&config);
        assert!(result.is_ok());
    }
}
