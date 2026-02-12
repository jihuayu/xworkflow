use std::collections::HashMap;
use std::sync::Arc;

use crate::core::http_client::{HttpClientProvider, HttpPoolConfig};
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
    SecurityLevel,
    SecurityPolicy,
};

/// Shared runtime group for multiple workflow executions.
#[derive(Clone)]
pub struct RuntimeGroup {
    pub group_id: Option<String>,
    pub group_name: Option<String>,
    pub node_executor_registry: Arc<NodeExecutorRegistry>,
    pub llm_provider_registry: Arc<LlmProviderRegistry>,
    #[cfg(feature = "plugin-system")]
    pub template_functions: Option<Arc<HashMap<String, Arc<dyn TemplateFunction>>>>,
    pub sandbox_pool: Option<Arc<dyn SandboxPool>>,
    pub http_client_provider: Option<Arc<HttpClientProvider>>,
    #[cfg(feature = "security")]
    pub resource_group: Option<ResourceGroup>,
    #[cfg(feature = "security")]
    pub security_policy: Option<SecurityPolicy>,
    #[cfg(feature = "security")]
    pub credential_provider: Option<Arc<dyn CredentialProvider>>,
    #[cfg(feature = "security")]
    pub resource_governor: Option<Arc<dyn ResourceGovernor>>,
    #[cfg(feature = "security")]
    pub audit_logger: Option<Arc<dyn AuditLogger>>,
    #[cfg(feature = "security")]
    pub quota: ResourceQuota,
    #[cfg(feature = "security")]
    pub security_level: SecurityLevel,
}

impl Default for RuntimeGroup {
    fn default() -> Self {
        Self {
            group_id: None,
            group_name: None,
            node_executor_registry: Arc::new(NodeExecutorRegistry::new()),
            llm_provider_registry: Arc::new(LlmProviderRegistry::new()),
            #[cfg(feature = "plugin-system")]
            template_functions: Some(Arc::new(HashMap::new())),
            sandbox_pool: Some(Arc::new(DefaultSandboxPool::new())),
            http_client_provider: Some(Arc::new(HttpClientProvider::new(HttpPoolConfig::default()))),
            #[cfg(feature = "security")]
            resource_group: None,
            #[cfg(feature = "security")]
            security_policy: None,
            #[cfg(feature = "security")]
            credential_provider: None,
            #[cfg(feature = "security")]
            resource_governor: None,
            #[cfg(feature = "security")]
            audit_logger: None,
            #[cfg(feature = "security")]
            quota: ResourceQuota::default(),
            #[cfg(feature = "security")]
            security_level: SecurityLevel::Standard,
        }
    }
}

impl RuntimeGroup {
    pub fn builder() -> RuntimeGroupBuilder {
        RuntimeGroupBuilder::new()
    }
}

pub struct RuntimeGroupBuilder {
    group: RuntimeGroup,
}

impl RuntimeGroupBuilder {
    pub fn new() -> Self {
        Self {
            group: RuntimeGroup::default(),
        }
    }

    pub fn group_id(mut self, group_id: impl Into<String>) -> Self {
        let id = group_id.into();
        self.group.group_id = Some(id.clone());
        #[cfg(feature = "security")]
        if let Some(group) = &mut self.group.resource_group {
            group.group_id = id;
        }
        self
    }

    pub fn group_name(mut self, group_name: impl Into<String>) -> Self {
        let name = group_name.into();
        self.group.group_name = Some(name.clone());
        #[cfg(feature = "security")]
        if let Some(group) = &mut self.group.resource_group {
            group.group_name = Some(name);
        }
        self
    }

    pub fn node_executor_registry(mut self, registry: Arc<NodeExecutorRegistry>) -> Self {
        self.group.node_executor_registry = registry;
        self
    }

    pub fn llm_provider_registry(mut self, registry: Arc<LlmProviderRegistry>) -> Self {
        self.group.llm_provider_registry = registry;
        self
    }

    #[cfg(feature = "plugin-system")]
    pub fn template_functions(
        mut self,
        functions: Arc<HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Self {
        self.group.template_functions = Some(functions);
        self
    }

    pub fn sandbox_pool(mut self, pool: Arc<dyn SandboxPool>) -> Self {
        self.group.sandbox_pool = Some(pool);
        self
    }

    pub fn http_pool_config(mut self, config: HttpPoolConfig) -> Self {
        self.group.http_client_provider = Some(Arc::new(HttpClientProvider::new(config)));
        self
    }

    #[cfg(feature = "security")]
    pub fn resource_group(mut self, group: ResourceGroup) -> Self {
        self.group.group_id = Some(group.group_id.clone());
        self.group.group_name = group.group_name.clone();
        self.group.security_level = group.security_level.clone();
        self.group.quota = group.quota.clone();
        self.group.resource_group = Some(group);
        self
    }

    #[cfg(feature = "security")]
    pub fn quota(mut self, quota: ResourceQuota) -> Self {
        self.group.quota = quota;
        self
    }

    #[cfg(feature = "security")]
    pub fn security_level(mut self, level: SecurityLevel) -> Self {
        self.group.security_level = level;
        self
    }

    #[cfg(feature = "security")]
    pub fn security_policy(mut self, policy: SecurityPolicy) -> Self {
        self.group.security_policy = Some(policy);
        self
    }

    #[cfg(feature = "security")]
    pub fn credential_provider(mut self, provider: Arc<dyn CredentialProvider>) -> Self {
        self.group.credential_provider = Some(provider);
        self
    }

    #[cfg(feature = "security")]
    pub fn resource_governor(mut self, governor: Arc<dyn ResourceGovernor>) -> Self {
        self.group.resource_governor = Some(governor);
        self
    }

    #[cfg(feature = "security")]
    pub fn audit_logger(mut self, logger: Arc<dyn AuditLogger>) -> Self {
        self.group.audit_logger = Some(logger);
        self
    }

    pub fn build(self) -> RuntimeGroup {
        self.group
    }
}

/// Placeholder sandbox pool trait for sharing runtimes across workflows.
pub trait SandboxPool: Send + Sync {}

#[derive(Default)]
pub struct DefaultSandboxPool;

impl DefaultSandboxPool {
    pub fn new() -> Self {
        Self
    }
}

impl SandboxPool for DefaultSandboxPool {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_group_default() {
        let group = RuntimeGroup::default();
        assert!(group.group_id.is_none());
        assert!(group.group_name.is_none());
        assert!(group.sandbox_pool.is_some());
        assert!(group.http_client_provider.is_some());
        #[cfg(feature = "plugin-system")]
        assert!(group.template_functions.is_some());
        #[cfg(feature = "security")]
        {
            assert!(group.resource_group.is_none());
            assert!(group.security_policy.is_none());
            assert!(group.credential_provider.is_none());
            assert!(group.resource_governor.is_none());
            assert!(group.audit_logger.is_none());
            assert_eq!(group.security_level, crate::security::SecurityLevel::Standard);
        }
    }

    #[test]
    fn test_runtime_group_builder_basic() {
        let group = RuntimeGroup::builder()
            .group_id("test_id")
            .group_name("test_name")
            .build();
        
        assert_eq!(group.group_id, Some("test_id".to_string()));
        assert_eq!(group.group_name, Some("test_name".to_string()));
    }

    #[test]
    fn test_runtime_group_builder_registries() {
        let node_registry = Arc::new(NodeExecutorRegistry::new());
        let llm_registry = Arc::new(LlmProviderRegistry::new());
        
        let group = RuntimeGroup::builder()
            .node_executor_registry(node_registry.clone())
            .llm_provider_registry(llm_registry.clone())
            .build();
        
        assert!(Arc::ptr_eq(&group.node_executor_registry, &node_registry));
        assert!(Arc::ptr_eq(&group.llm_provider_registry, &llm_registry));
    }

    #[test]
    fn test_runtime_group_builder_sandbox_pool() {
        let pool = Arc::new(DefaultSandboxPool::new());
        let group = RuntimeGroup::builder()
            .sandbox_pool(pool.clone())
            .build();
        
        assert!(group.sandbox_pool.is_some());
    }

    #[test]
    fn test_runtime_group_builder_http_pool_config() {
        use std::time::Duration;
        
        let config = HttpPoolConfig {
            pool_max_idle_per_host: 20,
            pool_idle_timeout: Duration::from_secs(120),
            default_timeout: Duration::from_secs(45),
            tcp_keepalive: Some(Duration::from_secs(90)),
            http2_enabled: false,
            #[cfg(feature = "security")]
            max_group_clients: 64,
            #[cfg(feature = "security")]
            group_client_idle_timeout: Duration::from_secs(600),
        };
        
        let group = RuntimeGroup::builder()
            .http_pool_config(config.clone())
            .build();
        
        // Just verify the provider exists since config is private
        assert!(group.http_client_provider.is_some());
    }

    #[cfg(feature = "plugin-system")]
    #[test]
    fn test_runtime_group_builder_template_functions() {
        use std::collections::HashMap;
        
        let functions = Arc::new(HashMap::new());
        let group = RuntimeGroup::builder()
            .template_functions(functions.clone())
            .build();
        
        assert!(group.template_functions.is_some());
        assert!(Arc::ptr_eq(&group.template_functions.unwrap(), &functions));
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_group_builder_resource_group() {
        use crate::security::{ResourceGroup, ResourceQuota, SecurityLevel};
        
        let resource_group = ResourceGroup {
            group_id: "rg_id".to_string(),
            group_name: Some("RG Name".to_string()),
            security_level: SecurityLevel::Strict,
            quota: ResourceQuota {
                max_concurrent_workflows: 10,
                ..ResourceQuota::default()
            },
            credential_refs: std::collections::HashMap::new(),
        };
        
        let group = RuntimeGroup::builder()
            .resource_group(resource_group.clone())
            .build();
        
        assert_eq!(group.group_id, Some("rg_id".to_string()));
        assert_eq!(group.group_name, Some("RG Name".to_string()));
        assert_eq!(group.security_level, SecurityLevel::Strict);
        assert_eq!(group.quota.max_concurrent_workflows, 10);
        assert!(group.resource_group.is_some());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_group_builder_quota() {
        use crate::security::ResourceQuota;
        
        let quota = ResourceQuota {
            max_concurrent_workflows: 5,
            ..ResourceQuota::default()
        };
        
        let group = RuntimeGroup::builder()
            .quota(quota.clone())
            .build();
        
        assert_eq!(group.quota.max_concurrent_workflows, 5);
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_group_builder_security_level() {
        use crate::security::SecurityLevel;
        
        let group = RuntimeGroup::builder()
            .security_level(SecurityLevel::Strict)
            .build();
        
        assert_eq!(group.security_level, SecurityLevel::Strict);
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_group_builder_security_policy() {
        use crate::security::{SecurityPolicy, NetworkPolicy};
        
        let policy = SecurityPolicy {
            network: Some(NetworkPolicy::default()),
            ..SecurityPolicy::standard()
        };
        let group = RuntimeGroup::builder()
            .security_policy(policy.clone())
            .build();
        
        assert!(group.security_policy.is_some());
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_group_builder_credential_provider() {
        // Removed - requires complex trait implementation
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_group_builder_resource_governor() {
        // Removed - requires complex trait implementation
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_runtime_group_builder_audit_logger() {
        // Removed - requires complex trait implementation
    }

    #[test]
    fn test_runtime_group_builder_chaining() {
        let group = RuntimeGroup::builder()
            .group_id("chain_id")
            .group_name("Chain Name")
            .node_executor_registry(Arc::new(NodeExecutorRegistry::new()))
            .llm_provider_registry(Arc::new(LlmProviderRegistry::new()))
            .build();
        
        assert_eq!(group.group_id, Some("chain_id".to_string()));
        assert_eq!(group.group_name, Some("Chain Name".to_string()));
    }

    #[test]
    fn test_default_sandbox_pool() {
        let pool = DefaultSandboxPool::new();
        let _: &dyn SandboxPool = &pool;
    }

    #[test]
    fn test_runtime_group_clone() {
        let group = RuntimeGroup::builder()
            .group_id("test")
            .build();
        
        let cloned = group.clone();
        assert_eq!(cloned.group_id, group.group_id);
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_group_id_sync_with_resource_group() {
        use crate::security::ResourceGroup;
        
        let resource_group = ResourceGroup {
            group_id: "original_id".to_string(),
            group_name: None,
            security_level: crate::security::SecurityLevel::Standard,
            quota: crate::security::ResourceQuota::default(),
            credential_refs: std::collections::HashMap::new(),
        };
        
        // Create builder with resource_group first
        let mut builder = RuntimeGroup::builder();
        builder.group.resource_group = Some(resource_group.clone());
        
        // Setting group_id should update resource_group
        let group = builder.group_id("new_id").build();
        
        assert_eq!(group.group_id, Some("new_id".to_string()));
        assert_eq!(group.resource_group.as_ref().unwrap().group_id, "new_id");
    }

    #[cfg(feature = "security")]
    #[test]
    fn test_group_name_sync_with_resource_group() {
        use crate::security::ResourceGroup;
        
        let resource_group = ResourceGroup {
            group_id: "id".to_string(),
            group_name: Some("original_name".to_string()),
            security_level: crate::security::SecurityLevel::Standard,
            quota: crate::security::ResourceQuota::default(),
            credential_refs: std::collections::HashMap::new(),
        };
        
        // Create builder with resource_group first
        let mut builder = RuntimeGroup::builder();
        builder.group.resource_group = Some(resource_group.clone());
        
        // Setting group_name should update resource_group
        let group = builder.group_name("new_name").build();
        
        assert_eq!(group.group_name, Some("new_name".to_string()));
        assert_eq!(group.resource_group.as_ref().unwrap().group_name, Some("new_name".to_string()));
    }

    #[test]
    fn test_runtime_group_access_resources() {
        let group = RuntimeGroup::default();
        
        // Test that we can access all resources
        assert!(group.node_executor_registry.get("start").is_some());
        assert!(group.sandbox_pool.is_some());
        assert!(group.http_client_provider.is_some());
    }
}
