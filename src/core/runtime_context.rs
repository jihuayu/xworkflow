use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::core::event_bus::GraphEngineEvent;
use crate::nodes::executor::NodeExecutorRegistry;
use crate::core::sub_graph_runner::SubGraphRunner;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Runtime context providing time and ID generation
#[derive(Clone)]
pub struct RuntimeContext {
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub extensions: RuntimeExtensions,
}

#[derive(Clone, Default)]
pub struct RuntimeExtensions {
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    pub sub_graph_runner: Option<Arc<dyn SubGraphRunner>>,
    pub node_executor_registry: Option<Arc<NodeExecutorRegistry>>,
    pub strict_template: bool,
    #[cfg(feature = "plugin-system")]
    pub template_functions: Option<Arc<std::collections::HashMap<String, Arc<dyn crate::plugin_system::TemplateFunction>>>>,
    #[cfg(feature = "security")]
    pub security: Option<SecurityContext>,
}

#[cfg(feature = "security")]
#[derive(Clone)]
pub struct SecurityContext {
    pub resource_group: Option<crate::security::ResourceGroup>,
    pub security_policy: Option<crate::security::SecurityPolicy>,
    pub resource_governor: Option<Arc<dyn crate::security::ResourceGovernor>>,
    pub credential_provider: Option<Arc<dyn crate::security::CredentialProvider>>,
    pub audit_logger: Option<Arc<dyn crate::security::AuditLogger>>,
}

impl Default for RuntimeContext {
    fn default() -> Self {
        Self {
            time_provider: Arc::new(RealTimeProvider::default()),
            id_generator: Arc::new(RealIdGenerator::default()),
            extensions: RuntimeExtensions::default(),
        }
    }
}

impl RuntimeContext {
    pub fn with_event_tx(mut self, event_tx: mpsc::Sender<GraphEngineEvent>) -> Self {
        self.extensions.event_tx = Some(event_tx);
        self
    }

    pub fn with_node_executor_registry(
        mut self,
        registry: Arc<NodeExecutorRegistry>,
    ) -> Self {
        self.extensions.node_executor_registry = Some(registry);
        self
    }

    pub fn event_tx(&self) -> Option<&mpsc::Sender<GraphEngineEvent>> {
        self.extensions.event_tx.as_ref()
    }

    pub fn sub_graph_runner(&self) -> Option<&Arc<dyn SubGraphRunner>> {
        self.extensions.sub_graph_runner.as_ref()
    }

    pub fn node_executor_registry(&self) -> Option<&Arc<NodeExecutorRegistry>> {
        self.extensions.node_executor_registry.as_ref()
    }

    pub fn strict_template(&self) -> bool {
        self.extensions.strict_template
    }

    #[cfg(feature = "plugin-system")]
    pub fn template_functions(
        &self,
    ) -> Option<&Arc<std::collections::HashMap<String, Arc<dyn crate::plugin_system::TemplateFunction>>>> {
        self.extensions.template_functions.as_ref()
    }

    #[cfg(feature = "security")]
    pub fn security(&self) -> Option<&SecurityContext> {
        self.extensions.security.as_ref()
    }

    #[cfg(feature = "security")]
    pub fn security_mut(&mut self) -> Option<&mut SecurityContext> {
        self.extensions.security.as_mut()
    }

    #[cfg(feature = "security")]
    pub fn resource_group(&self) -> Option<&crate::security::ResourceGroup> {
        self.extensions
            .security
            .as_ref()
            .and_then(|s| s.resource_group.as_ref())
    }

    #[cfg(feature = "security")]
    pub fn security_policy(&self) -> Option<&crate::security::SecurityPolicy> {
        self.extensions
            .security
            .as_ref()
            .and_then(|s| s.security_policy.as_ref())
    }

    #[cfg(feature = "security")]
    pub fn resource_governor(&self) -> Option<&Arc<dyn crate::security::ResourceGovernor>> {
        self.extensions
            .security
            .as_ref()
            .and_then(|s| s.resource_governor.as_ref())
    }

    #[cfg(feature = "security")]
    pub fn credential_provider(&self) -> Option<&Arc<dyn crate::security::CredentialProvider>> {
        self.extensions
            .security
            .as_ref()
            .and_then(|s| s.credential_provider.as_ref())
    }

    #[cfg(feature = "security")]
    pub fn audit_logger(&self) -> Option<&Arc<dyn crate::security::AuditLogger>> {
        self.extensions
            .security
            .as_ref()
            .and_then(|s| s.audit_logger.as_ref())
    }
}

pub trait TimeProvider: Send + Sync {
    fn now_timestamp(&self) -> i64;
    fn now_millis(&self) -> i64;
    fn elapsed_secs(&self, since: i64) -> u64;
}

pub trait IdGenerator: Send + Sync {
    fn next_id(&self) -> String;
}

// --- Real implementations ---

pub struct RealTimeProvider {
    #[allow(dead_code)]
    start: Instant,
}

impl RealTimeProvider {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for RealTimeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeProvider for RealTimeProvider {
    fn now_timestamp(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn now_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    fn elapsed_secs(&self, since: i64) -> u64 {
        let now = self.now_timestamp();
        if now >= since {
            (now - since) as u64
        } else {
            0
        }
    }
}

pub struct RealIdGenerator;

impl Default for RealIdGenerator {
    fn default() -> Self {
        Self
    }
}

impl IdGenerator for RealIdGenerator {
    fn next_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

// --- Fake implementations ---

pub struct FakeTimeProvider {
    pub fixed_timestamp: i64,
}

impl FakeTimeProvider {
    pub fn new(fixed_timestamp: i64) -> Self {
        Self { fixed_timestamp }
    }
}

impl TimeProvider for FakeTimeProvider {
    fn now_timestamp(&self) -> i64 {
        self.fixed_timestamp
    }

    fn now_millis(&self) -> i64 {
        self.fixed_timestamp.saturating_mul(1000)
    }

    fn elapsed_secs(&self, since: i64) -> u64 {
        if self.fixed_timestamp >= since {
            (self.fixed_timestamp - since) as u64
        } else {
            0
        }
    }
}

pub struct FakeIdGenerator {
    pub prefix: String,
    pub counter: AtomicU64,
}

impl FakeIdGenerator {
    pub fn new(prefix: String) -> Self {
        Self {
            prefix,
            counter: AtomicU64::new(0),
        }
    }
}

impl IdGenerator for FakeIdGenerator {
    fn next_id(&self) -> String {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        format!("{}-{}", self.prefix, id)
    }
}
