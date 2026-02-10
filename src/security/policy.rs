use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::audit::{AuditLogger, TracingAuditLogger};
use super::network::{NetworkPolicy, NetworkPolicyMode};
use super::validation::{DslValidationConfig, TemplateSafetyConfig};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum SecurityLevel {
    Permissive,
    #[default]
    Standard,
    Strict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResourceLimits {
    pub max_execution_time: Duration,
    pub max_output_bytes: usize,
    pub max_memory_bytes: Option<usize>,
}

impl NodeResourceLimits {
    pub fn for_code_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(30),
            max_output_bytes: 1 * 1024 * 1024,
            max_memory_bytes: Some(32 * 1024 * 1024),
        }
    }

    pub fn for_http_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(30),
            max_output_bytes: 10 * 1024 * 1024,
            max_memory_bytes: None,
        }
    }

    pub fn for_llm_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(120),
            max_output_bytes: 1 * 1024 * 1024,
            max_memory_bytes: None,
        }
    }

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