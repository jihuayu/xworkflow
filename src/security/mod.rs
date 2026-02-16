//! Security layer: audit logging, credential management, resource governance,
//! network policy enforcement, and DSL / template safety validation.
//!
//! All types in this module are gated behind the `security` feature flag.

pub mod audit;
pub mod credential;
pub mod governor;
pub mod network;
pub mod policy;
pub mod resource_group;
pub mod validation;

pub use audit::{AuditLogger, EventSeverity, SecurityEvent, SecurityEventType, TracingAuditLogger};
pub use credential::{CredentialError, CredentialProvider};
pub use governor::{GroupUsage, InMemoryResourceGovernor, QuotaError, ResourceGovernor};
pub use network::{domain_matches, is_blocked_ip, validate_url, NetworkError, NetworkPolicy, NetworkPolicyMode, SafeDnsResolver, SecureHttpClientFactory};
pub use policy::{MemorySecurityPolicy, NodeResourceLimits, SecurityLevel, SecurityPolicy};
pub use resource_group::{ResourceGroup, ResourceQuota};
pub use validation::{DslValidationConfig, SelectorValidation, TemplateSafetyConfig};