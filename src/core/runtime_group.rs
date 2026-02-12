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
