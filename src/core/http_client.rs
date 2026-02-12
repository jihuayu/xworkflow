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
    pub fn client_for_context(&self, _context: &RuntimeContext) -> Result<reqwest::Client, NodeError> {
        Ok(self.standard_client.clone())
    }

    #[cfg(feature = "security")]
    pub fn client_for_context(&self, context: &RuntimeContext) -> Result<reqwest::Client, NodeError> {
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
