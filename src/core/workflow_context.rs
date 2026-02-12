use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_group::RuntimeGroup;
use crate::core::sub_graph_runner::SubGraphRunner;
use crate::llm::LlmProviderRegistry;
use crate::nodes::executor::NodeExecutorRegistry;

#[cfg(feature = "plugin-system")]
use crate::plugin_system::TemplateFunction;

#[cfg(feature = "security")]
use crate::security::{
    AuditLogger,
    CredentialProvider,
    ResourceGovernor,
    ResourceGroup,
    ResourceQuota,
    SecurityPolicy,
};

/// Workflow execution context (instance-specific).
#[derive(Clone)]
pub struct WorkflowContext {
    pub runtime_group: Arc<RuntimeGroup>,
    pub execution_id: String,
    pub workflow_id: Option<String>,
    pub start_time: i64,
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    pub sub_graph_runner: Option<Arc<dyn SubGraphRunner>>,
    pub strict_template: bool,
}

impl WorkflowContext {
    pub fn new(runtime_group: Arc<RuntimeGroup>) -> Self {
        let time_provider = Arc::new(RealTimeProvider::default());
        let id_generator = Arc::new(RealIdGenerator::default());
        let execution_id = id_generator.next_id();
        let start_time = time_provider.now_timestamp();

        Self {
            runtime_group,
            execution_id,
            workflow_id: None,
            start_time,
            time_provider,
            id_generator,
            event_tx: None,
            sub_graph_runner: None,
            strict_template: false,
        }
    }

    pub fn with_event_tx(mut self, event_tx: mpsc::Sender<GraphEngineEvent>) -> Self {
        self.event_tx = Some(event_tx);
        self
    }

    pub fn with_sub_graph_runner(mut self, runner: Arc<dyn SubGraphRunner>) -> Self {
        self.sub_graph_runner = Some(runner);
        self
    }

    pub fn with_node_executor_registry(mut self, registry: Arc<NodeExecutorRegistry>) -> Self {
        self.update_runtime_group(|group| {
            group.node_executor_registry = registry;
        });
        self
    }

    pub fn with_llm_provider_registry(mut self, registry: Arc<LlmProviderRegistry>) -> Self {
        self.update_runtime_group(|group| {
            group.llm_provider_registry = registry;
        });
        self
    }

    #[cfg(feature = "plugin-system")]
    pub fn with_template_functions(
        mut self,
        functions: Arc<std::collections::HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Self {
        self.update_runtime_group(|group| {
            group.template_functions = Some(functions);
        });
        self
    }

    pub fn event_tx(&self) -> Option<&mpsc::Sender<GraphEngineEvent>> {
        self.event_tx.as_ref()
    }

    pub fn sub_graph_runner(&self) -> Option<&Arc<dyn SubGraphRunner>> {
        self.sub_graph_runner.as_ref()
    }

    pub fn node_executor_registry(&self) -> &Arc<NodeExecutorRegistry> {
        &self.runtime_group.node_executor_registry
    }

    pub fn llm_provider_registry(&self) -> &Arc<LlmProviderRegistry> {
        &self.runtime_group.llm_provider_registry
    }

    pub fn strict_template(&self) -> bool {
        self.strict_template
    }

    #[cfg(feature = "security")]
    pub fn quota(&self) -> &ResourceQuota {
        &self.runtime_group.quota
    }

    #[cfg(feature = "plugin-system")]
    pub fn template_functions(
        &self,
    ) -> Option<&Arc<std::collections::HashMap<String, Arc<dyn TemplateFunction>>>> {
        self.runtime_group.template_functions.as_ref()
    }

    #[cfg(feature = "security")]
    pub fn resource_group(&self) -> Option<&ResourceGroup> {
        self.runtime_group.resource_group.as_ref()
    }

    #[cfg(feature = "security")]
    pub fn security_policy(&self) -> Option<&SecurityPolicy> {
        self.runtime_group.security_policy.as_ref()
    }

    #[cfg(feature = "security")]
    pub fn resource_governor(&self) -> Option<&Arc<dyn ResourceGovernor>> {
        self.runtime_group.resource_governor.as_ref()
    }

    #[cfg(feature = "security")]
    pub fn credential_provider(&self) -> Option<&Arc<dyn CredentialProvider>> {
        self.runtime_group.credential_provider.as_ref()
    }

    #[cfg(feature = "security")]
    pub fn audit_logger(&self) -> Option<&Arc<dyn AuditLogger>> {
        self.runtime_group.audit_logger.as_ref()
    }

    pub fn update_runtime_group<F>(&mut self, updater: F)
    where
        F: FnOnce(&mut RuntimeGroup),
    {
        if let Some(group) = Arc::get_mut(&mut self.runtime_group) {
            updater(group);
            return;
        }

        let mut group = (*self.runtime_group).clone();
        updater(&mut group);
        self.runtime_group = Arc::new(group);
    }

    #[cfg(feature = "security")]
    pub fn set_resource_group(&mut self, group: ResourceGroup) {
        self.update_runtime_group(|runtime_group| {
            runtime_group.group_id = Some(group.group_id.clone());
            runtime_group.group_name = group.group_name.clone();
            runtime_group.security_level = group.security_level.clone();
            runtime_group.quota = group.quota.clone();
            runtime_group.resource_group = Some(group);
        });
    }

    #[cfg(feature = "security")]
    pub fn set_security_policy(&mut self, policy: SecurityPolicy) {
        self.update_runtime_group(|runtime_group| {
            runtime_group.security_policy = Some(policy);
        });
    }

    #[cfg(feature = "security")]
    pub fn set_resource_governor(&mut self, governor: Arc<dyn ResourceGovernor>) {
        self.update_runtime_group(|runtime_group| {
            runtime_group.resource_governor = Some(governor);
        });
    }

    #[cfg(feature = "security")]
    pub fn set_credential_provider(&mut self, provider: Arc<dyn CredentialProvider>) {
        self.update_runtime_group(|runtime_group| {
            runtime_group.credential_provider = Some(provider);
        });
    }

    #[cfg(feature = "security")]
    pub fn set_audit_logger(&mut self, logger: Arc<dyn AuditLogger>) {
        self.update_runtime_group(|runtime_group| {
            runtime_group.audit_logger = Some(logger);
        });
    }
}

impl Default for WorkflowContext {
    fn default() -> Self {
        Self::new(Arc::new(RuntimeGroup::default()))
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
        assert!(ts > 1_700_000_000);
    }

    #[test]
    fn test_real_time_provider_now_millis() {
        let tp = RealTimeProvider::new();
        let ms = tp.now_millis();
        assert!(ms > 1_700_000_000_000);
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
        assert_eq!(id1.len(), 36);
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
    fn test_workflow_context_default() {
        let ctx = WorkflowContext::default();
        assert!(ctx.event_tx().is_none());
        assert!(ctx.sub_graph_runner().is_none());
        let _ = ctx.node_executor_registry();
        assert!(!ctx.strict_template());
    }

    #[test]
    fn test_workflow_context_with_event_tx() {
        let (tx, _rx) = mpsc::channel::<GraphEngineEvent>(16);
        let ctx = WorkflowContext::default().with_event_tx(tx);
        assert!(ctx.event_tx().is_some());
    }

    #[test]
    fn test_workflow_context_with_node_executor_registry() {
        let registry = Arc::new(NodeExecutorRegistry::empty());
        let ctx = WorkflowContext::default().with_node_executor_registry(Arc::clone(&registry));
        assert!(Arc::ptr_eq(ctx.node_executor_registry(), &registry));
    }
}
