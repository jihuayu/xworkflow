//! Security policy and per-node resource limits.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::audit::{AuditLogger, TracingAuditLogger};
use super::network::{NetworkPolicy, NetworkPolicyMode};
use super::validation::{DslValidationConfig, TemplateSafetyConfig};

/// Overall security strictness level.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum SecurityLevel {
    Permissive,
    #[default]
    Standard,
    Strict,
}

/// Per-node-type execution constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResourceLimits {
    pub max_execution_time: Duration,
    pub max_output_bytes: usize,
    pub max_memory_bytes: Option<usize>,
}

impl NodeResourceLimits {
    /// Sensible defaults for code (sandbox) nodes.
    pub fn for_code_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(30),
            max_output_bytes: 1 * 1024 * 1024,
            max_memory_bytes: Some(32 * 1024 * 1024),
        }
    }

    /// Sensible defaults for HTTP request nodes.
    pub fn for_http_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(30),
            max_output_bytes: 10 * 1024 * 1024,
            max_memory_bytes: None,
        }
    }

    /// Sensible defaults for LLM nodes.
    pub fn for_llm_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(120),
            max_output_bytes: 1 * 1024 * 1024,
            max_memory_bytes: None,
        }
    }

    /// Sensible defaults for template transform nodes.
    pub fn for_template_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(5),
            max_output_bytes: 512 * 1024,
            max_memory_bytes: None,
        }
    }
}

#[derive(Clone)]
pub struct SecurityPolicy {
    pub level: SecurityLevel,
    pub network: Option<NetworkPolicy>,
    pub template: Option<TemplateSafetyConfig>,
    pub dsl_validation: Option<DslValidationConfig>,
    pub node_limits: HashMap<String, NodeResourceLimits>,
    pub audit_logger: Option<Arc<dyn AuditLogger>>,
}

impl SecurityPolicy {
    pub fn network_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.network.hash(&mut hasher);
        hasher.finish()
    }

    pub fn permissive() -> Self {
        Self {
            level: SecurityLevel::Permissive,
            network: None,
            template: None,
            dsl_validation: None,
            node_limits: HashMap::new(),
            audit_logger: None,
        }
    }

    pub fn standard() -> Self {
        Self {
            level: SecurityLevel::Standard,
            network: Some(NetworkPolicy::default()),
            template: Some(TemplateSafetyConfig::default()),
            dsl_validation: Some(DslValidationConfig::default()),
            node_limits: default_node_limits(),
            audit_logger: Some(Arc::new(TracingAuditLogger)),
        }
    }

    pub fn strict() -> Self {
        Self {
            level: SecurityLevel::Strict,
            network: Some(NetworkPolicy {
                mode: NetworkPolicyMode::AllowList,
                allowed_domains: vec![],
                block_private_ips: true,
                block_metadata_endpoints: true,
                block_loopback: true,
                allowed_ports: vec![443],
                max_redirects: 0,
                dns_rebinding_protection: true,
                ..Default::default()
            }),
            template: Some(TemplateSafetyConfig {
                max_template_length: 5_000,
                max_output_length: 50_000,
                max_recursion_depth: 3,
                max_loop_iterations: 100,
                disabled_filters: vec!["debug".into()],
                disabled_functions: vec!["range".into()],
            }),
            dsl_validation: Some(DslValidationConfig {
                max_nodes: 100,
                max_edges: 200,
                max_nesting_depth: 5,
                max_node_config_bytes: 16 * 1024,
                ..DslValidationConfig::default()
            }),
            node_limits: strict_node_limits(),
            audit_logger: Some(Arc::new(TracingAuditLogger)),
        }
    }
}

pub fn default_node_limits() -> HashMap<String, NodeResourceLimits> {
    let mut m = HashMap::new();
    m.insert("code".into(), NodeResourceLimits::for_code_node());
    m.insert("http-request".into(), NodeResourceLimits::for_http_node());
    m.insert("llm".into(), NodeResourceLimits::for_llm_node());
    m.insert(
        "template-transform".into(),
        NodeResourceLimits::for_template_node(),
    );
    m
}

pub fn strict_node_limits() -> HashMap<String, NodeResourceLimits> {
    let mut m = HashMap::new();
    m.insert(
        "code".into(),
        NodeResourceLimits {
            max_execution_time: Duration::from_secs(10),
            max_output_bytes: 256 * 1024,
            max_memory_bytes: Some(16 * 1024 * 1024),
        },
    );
    m.insert(
        "http-request".into(),
        NodeResourceLimits {
            max_execution_time: Duration::from_secs(10),
            max_output_bytes: 1 * 1024 * 1024,
            max_memory_bytes: None,
        },
    );
    m.insert(
        "llm".into(),
        NodeResourceLimits {
            max_execution_time: Duration::from_secs(60),
            max_output_bytes: 256 * 1024,
            max_memory_bytes: None,
        },
    );
    m.insert(
        "template-transform".into(),
        NodeResourceLimits {
            max_execution_time: Duration::from_secs(2),
            max_output_bytes: 128 * 1024,
            max_memory_bytes: None,
        },
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_security_level_default() {
        let level = SecurityLevel::default();
        assert_eq!(level, SecurityLevel::Standard);
    }

    #[test]
    fn test_security_level_variants() {
        assert_eq!(SecurityLevel::Permissive, SecurityLevel::Permissive);
        assert_eq!(SecurityLevel::Standard, SecurityLevel::Standard);
        assert_eq!(SecurityLevel::Strict, SecurityLevel::Strict);
        assert_ne!(SecurityLevel::Permissive, SecurityLevel::Standard);
    }

    #[test]
    fn test_node_resource_limits_for_code_node() {
        let limits = NodeResourceLimits::for_code_node();
        assert_eq!(limits.max_execution_time, Duration::from_secs(30));
        assert_eq!(limits.max_output_bytes, 1 * 1024 * 1024);
        assert_eq!(limits.max_memory_bytes, Some(32 * 1024 * 1024));
    }

    #[test]
    fn test_node_resource_limits_for_http_node() {
        let limits = NodeResourceLimits::for_http_node();
        assert_eq!(limits.max_execution_time, Duration::from_secs(30));
        assert_eq!(limits.max_output_bytes, 10 * 1024 * 1024);
        assert!(limits.max_memory_bytes.is_none());
    }

    #[test]
    fn test_node_resource_limits_for_llm_node() {
        let limits = NodeResourceLimits::for_llm_node();
        assert_eq!(limits.max_execution_time, Duration::from_secs(120));
        assert_eq!(limits.max_output_bytes, 1 * 1024 * 1024);
        assert!(limits.max_memory_bytes.is_none());
    }

    #[test]
    fn test_node_resource_limits_for_template_node() {
        let limits = NodeResourceLimits::for_template_node();
        assert_eq!(limits.max_execution_time, Duration::from_secs(5));
        assert_eq!(limits.max_output_bytes, 512 * 1024);
        assert!(limits.max_memory_bytes.is_none());
    }

    #[test]
    fn test_security_policy_permissive() {
        let policy = SecurityPolicy::permissive();
        assert_eq!(policy.level, SecurityLevel::Permissive);
        assert!(policy.network.is_none());
        assert!(policy.template.is_none());
        assert!(policy.dsl_validation.is_none());
        assert!(policy.node_limits.is_empty());
        assert!(policy.audit_logger.is_none());
    }

    #[test]
    fn test_security_policy_standard() {
        let policy = SecurityPolicy::standard();
        assert_eq!(policy.level, SecurityLevel::Standard);
        assert!(policy.network.is_some());
        assert!(policy.template.is_some());
        assert!(policy.dsl_validation.is_some());
        assert!(!policy.node_limits.is_empty());
        assert!(policy.audit_logger.is_some());
        assert!(policy.node_limits.contains_key("code"));
        assert!(policy.node_limits.contains_key("http-request"));
        assert!(policy.node_limits.contains_key("llm"));
        assert!(policy.node_limits.contains_key("template-transform"));
    }

    #[test]
    fn test_security_policy_strict() {
        let policy = SecurityPolicy::strict();
        assert_eq!(policy.level, SecurityLevel::Strict);
        assert!(policy.network.is_some());
        let net = policy.network.unwrap();
        assert!(matches!(net.mode, NetworkPolicyMode::AllowList));
        assert!(net.allowed_domains.is_empty());
        assert_eq!(net.allowed_ports, vec![443]);
        assert_eq!(net.max_redirects, 0);

        assert!(policy.template.is_some());
        let tmpl = policy.template.unwrap();
        assert_eq!(tmpl.max_template_length, 5_000);
        assert_eq!(tmpl.max_output_length, 50_000);
        assert_eq!(tmpl.max_recursion_depth, 3);
        assert_eq!(tmpl.max_loop_iterations, 100);
        assert!(tmpl.disabled_filters.contains(&"debug".to_string()));
        assert!(tmpl.disabled_functions.contains(&"range".to_string()));

        assert!(policy.dsl_validation.is_some());
        let dsl = policy.dsl_validation.unwrap();
        assert_eq!(dsl.max_nodes, 100);
        assert_eq!(dsl.max_edges, 200);
        assert_eq!(dsl.max_nesting_depth, 5);
        assert_eq!(dsl.max_node_config_bytes, 16 * 1024);

        assert!(policy.audit_logger.is_some());
    }

    #[test]
    fn test_default_node_limits() {
        let limits = default_node_limits();
        assert_eq!(limits.len(), 4);
        assert!(limits.contains_key("code"));
        assert!(limits.contains_key("http-request"));
        assert!(limits.contains_key("llm"));
        assert!(limits.contains_key("template-transform"));

        let code = &limits["code"];
        assert_eq!(code.max_execution_time, Duration::from_secs(30));
    }

    #[test]
    fn test_strict_node_limits() {
        let limits = strict_node_limits();
        assert_eq!(limits.len(), 4);

        let code = &limits["code"];
        assert_eq!(code.max_execution_time, Duration::from_secs(10));
        assert_eq!(code.max_output_bytes, 256 * 1024);
        assert_eq!(code.max_memory_bytes, Some(16 * 1024 * 1024));

        let http = &limits["http-request"];
        assert_eq!(http.max_execution_time, Duration::from_secs(10));
        assert_eq!(http.max_output_bytes, 1 * 1024 * 1024);

        let llm = &limits["llm"];
        assert_eq!(llm.max_execution_time, Duration::from_secs(60));

        let tmpl = &limits["template-transform"];
        assert_eq!(tmpl.max_execution_time, Duration::from_secs(2));
        assert_eq!(tmpl.max_output_bytes, 128 * 1024);
    }

    #[test]
    fn test_security_level_serde() {
        let json = serde_json::to_string(&SecurityLevel::Strict).unwrap();
        let level: SecurityLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(level, SecurityLevel::Strict);
    }
}