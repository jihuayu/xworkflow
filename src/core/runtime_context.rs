//! Runtime context for workflow execution.
//!
//! [`RuntimeContext`] bundles time/ID providers and optional extensions (event
//! channels, sub-graph runners, plugin template functions, security context)
//! that are threaded through the entire execution pipeline.

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

/// Extension points carried alongside [`RuntimeContext`].
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

/// Provides the current wall-clock time for the workflow engine.
pub trait TimeProvider: Send + Sync {
    /// Return the current Unix timestamp in seconds.
    fn now_timestamp(&self) -> i64;
    /// Return the current Unix timestamp in milliseconds.
    fn now_millis(&self) -> i64;
    /// Return the elapsed seconds since the given timestamp.
    fn elapsed_secs(&self, since: i64) -> u64;
}

/// Generates unique identifiers (e.g. for run IDs, node execution IDs).
pub trait IdGenerator: Send + Sync {
    /// Return the next unique ID string.
    fn next_id(&self) -> String;
}

// --- Real implementations ---

/// Production [`TimeProvider`] using `SystemTime`.
pub struct RealTimeProvider {
    #[allow(dead_code)]
    start: Instant,
}

impl RealTimeProvider {
    /// Create a new `RealTimeProvider`.
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

/// Production [`IdGenerator`] using UUID v4.
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

/// Deterministic [`TimeProvider`] for testing. Always returns the same timestamp.
pub struct FakeTimeProvider {
    pub fixed_timestamp: i64,
}

impl FakeTimeProvider {
    /// Create a new `FakeTimeProvider` with the given fixed timestamp.
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

/// Deterministic [`IdGenerator`] for testing. Produces sequential IDs with a prefix.
pub struct FakeIdGenerator {
    pub prefix: String,
    pub counter: AtomicU64,
}

impl FakeIdGenerator {
    /// Create a new `FakeIdGenerator` with the given prefix.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_real_time_provider_now_timestamp() {
        let tp = RealTimeProvider::new();
        let ts = tp.now_timestamp();
        assert!(ts > 1_700_000_000); // After 2023
    }

    #[test]
    fn test_real_time_provider_now_millis() {
        let tp = RealTimeProvider::new();
        let ms = tp.now_millis();
        assert!(ms > 1_700_000_000_000); // After 2023 in ms
    }

    #[test]
    fn test_real_time_provider_elapsed_secs() {
        let tp = RealTimeProvider::new();
        let past = tp.now_timestamp() - 10;
        let elapsed = tp.elapsed_secs(past);
        assert!(elapsed >= 9 && elapsed <= 12);
    }

    #[test]
    fn test_real_time_provider_elapsed_future() {
        let tp = RealTimeProvider::new();
        let future = tp.now_timestamp() + 1000;
        assert_eq!(tp.elapsed_secs(future), 0);
    }

    #[test]
    fn test_real_time_provider_default() {
        let tp = RealTimeProvider::default();
        assert!(tp.now_timestamp() > 0);
    }

    #[test]
    fn test_real_id_generator() {
        let gen = RealIdGenerator::default();
        let id1 = gen.next_id();
        let id2 = gen.next_id();
        assert_ne!(id1, id2);
        assert_eq!(id1.len(), 36); // UUID format
    }

    #[test]
    fn test_fake_time_provider() {
        let tp = FakeTimeProvider::new(1000);
        assert_eq!(tp.now_timestamp(), 1000);
        assert_eq!(tp.now_millis(), 1_000_000);
    }

    #[test]
    fn test_fake_time_provider_elapsed_secs() {
        let tp = FakeTimeProvider::new(1000);
        assert_eq!(tp.elapsed_secs(990), 10);
        assert_eq!(tp.elapsed_secs(1000), 0);
        assert_eq!(tp.elapsed_secs(1010), 0);
    }

    #[test]
    fn test_fake_id_generator() {
        let gen = FakeIdGenerator::new("test".into());
        assert_eq!(gen.next_id(), "test-0");
        assert_eq!(gen.next_id(), "test-1");
        assert_eq!(gen.next_id(), "test-2");
    }

    #[test]
    fn test_runtime_context_default() {
        let ctx = RuntimeContext::default();
        assert!(ctx.event_tx().is_none());
        assert!(ctx.sub_graph_runner().is_none());
        assert!(ctx.node_executor_registry().is_none());
        assert!(!ctx.strict_template());
    }

    #[test]
    fn test_runtime_context_with_event_tx() {
        let (tx, _rx) = mpsc::channel::<GraphEngineEvent>(16);
        let ctx = RuntimeContext::default().with_event_tx(tx);
        assert!(ctx.event_tx().is_some());
    }

    #[test]
    fn test_runtime_context_with_node_executor_registry() {
        let registry = Arc::new(NodeExecutorRegistry::empty());
        let ctx = RuntimeContext::default().with_node_executor_registry(registry);
        assert!(ctx.node_executor_registry().is_some());
    }

    #[test]
    fn test_runtime_extensions_default() {
        let ext = RuntimeExtensions::default();
        assert!(ext.event_tx.is_none());
        assert!(ext.sub_graph_runner.is_none());
        assert!(ext.node_executor_registry.is_none());
        assert!(!ext.strict_template);
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_context_security_none_by_default() {
        let ctx = RuntimeContext::default();
        assert!(ctx.security().is_none());
        assert!(ctx.resource_group().is_none());
        assert!(ctx.security_policy().is_none());
        assert!(ctx.resource_governor().is_none());
        assert!(ctx.credential_provider().is_none());
        assert!(ctx.audit_logger().is_none());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_context_security_mut() {
        let mut ctx = RuntimeContext::default();
        assert!(ctx.security_mut().is_none());

        ctx.extensions.security = Some(SecurityContext {
            resource_group: None,
            security_policy: None,
            resource_governor: None,
            credential_provider: None,
            audit_logger: None,
        });
        assert!(ctx.security_mut().is_some());
    }
}
