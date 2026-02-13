use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[cfg(feature = "security")]
use crate::core::runtime_context::RuntimeContext;

use super::types::{ChangeSeverity, ContextFingerprint, EnvironmentChange, ResumeDiagnostic};

impl ContextFingerprint {
    #[cfg(feature = "security")]
    pub fn capture(context: &RuntimeContext, schema_hash: String, engine_config_hash: String) -> Self {
        let mut llm_providers: Vec<String> = context
            .llm_provider_registry()
            .list()
            .into_iter()
            .map(|p| p.id)
            .collect();
        llm_providers.sort();

        let mut registered_node_types = context
            .node_executor_registry()
            .list_registered_types();
        registered_node_types.sort();

        let mut credential_groups: Vec<String> = context
            .resource_group()
            .map(|g| g.credential_refs.keys().cloned().collect())
            .unwrap_or_default();
        credential_groups.sort();

        let security_level = context
            .security_policy()
            .map(|policy| format!("{:?}", policy.level));

        let network_policy_hash = context
            .security_policy()
            .map(|policy| format!("{:016x}", policy.network_hash()));

        Self {
            security_level,
            llm_providers,
            registered_node_types,
            network_policy_hash,
            schema_hash,
            credential_groups,
            engine_config_hash,
        }
    }

    #[cfg(not(feature = "security"))]
    pub fn capture(
        llm_providers: Vec<String>,
        registered_node_types: Vec<String>,
        schema_hash: String,
        engine_config_hash: String,
    ) -> Self {
        let mut llm_providers = llm_providers;
        llm_providers.sort();
        let mut registered_node_types = registered_node_types;
        registered_node_types.sort();

        Self {
            security_level: None,
            llm_providers,
            registered_node_types,
            network_policy_hash: None,
            schema_hash,
            credential_groups: Vec::new(),
            engine_config_hash,
        }
    }
}

pub fn diff_fingerprints(saved: &ContextFingerprint, current: &ContextFingerprint) -> ResumeDiagnostic {
    let mut changes = Vec::new();

    if saved.schema_hash != current.schema_hash {
        changes.push(EnvironmentChange {
            component: "schema".to_string(),
            description:
                "Workflow schema has changed since checkpoint was saved. DAG structure may be incompatible.".to_string(),
            severity: ChangeSeverity::Danger,
        });
    }

    match (&saved.security_level, &current.security_level) {
        (Some(saved_level), Some(current_level)) if saved_level != current_level => {
            let severity = if is_security_downgrade(saved_level, current_level) {
                ChangeSeverity::Danger
            } else {
                ChangeSeverity::Info
            };
            changes.push(EnvironmentChange {
                component: "security_level".to_string(),
                description: format!("Security level changed: {} → {}", saved_level, current_level),
                severity,
            });
        }
        (Some(_), None) => {
            changes.push(EnvironmentChange {
                component: "security_level".to_string(),
                description: "Security was enabled at checkpoint time but is now disabled.".to_string(),
                severity: ChangeSeverity::Danger,
            });
        }
        _ => {}
    }

    let removed_providers: Vec<_> = saved
        .llm_providers
        .iter()
        .filter(|provider| !current.llm_providers.contains(*provider))
        .cloned()
        .collect();
    if !removed_providers.is_empty() {
        changes.push(EnvironmentChange {
            component: "llm_providers".to_string(),
            description: format!(
                "LLM providers removed since checkpoint: [{}]. Subsequent LLM/agent nodes using these providers may fail.",
                removed_providers.join(", ")
            ),
            severity: ChangeSeverity::Warning,
        });
    }

    let removed_node_types: Vec<_> = saved
        .registered_node_types
        .iter()
        .filter(|node_type| !current.registered_node_types.contains(*node_type))
        .cloned()
        .collect();
    if !removed_node_types.is_empty() {
        changes.push(EnvironmentChange {
            component: "node_types".to_string(),
            description: format!(
                "Node types removed: [{}]. Pending nodes of these types may fail.",
                removed_node_types.join(", ")
            ),
            severity: ChangeSeverity::Danger,
        });
    }

    if saved.network_policy_hash != current.network_policy_hash {
        changes.push(EnvironmentChange {
            component: "network_policy".to_string(),
            description:
                "Network policy has changed. HTTP/MCP nodes may be blocked by new restrictions.".to_string(),
            severity: ChangeSeverity::Warning,
        });
    }

    let removed_credential_groups: Vec<_> = saved
        .credential_groups
        .iter()
        .filter(|group| !current.credential_groups.contains(*group))
        .cloned()
        .collect();
    if !removed_credential_groups.is_empty() {
        changes.push(EnvironmentChange {
            component: "credentials".to_string(),
            description: format!(
                "Credential groups no longer available: [{}].",
                removed_credential_groups.join(", ")
            ),
            severity: ChangeSeverity::Warning,
        });
    }

    if saved.engine_config_hash != current.engine_config_hash {
        changes.push(EnvironmentChange {
            component: "engine_config".to_string(),
            description: "Engine configuration has changed (max_steps, timeout, etc.).".to_string(),
            severity: ChangeSeverity::Info,
        });
    }

    ResumeDiagnostic { changes }
}

pub fn is_security_downgrade(saved: &str, current: &str) -> bool {
    let level_order = ["Strict", "Standard", "Permissive"];
    let saved_idx = level_order.iter().position(|level| level == &saved);
    let current_idx = level_order.iter().position(|level| level == &current);
    match (saved_idx, current_idx) {
        (Some(saved), Some(current)) => current > saved,
        _ => false,
    }
}

pub fn hash_json<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_fingerprints_security_downgrade() {
        let saved = ContextFingerprint {
            security_level: Some("Strict".to_string()),
            llm_providers: vec!["openai".to_string()],
            registered_node_types: vec!["start".to_string()],
            network_policy_hash: Some("a".to_string()),
            schema_hash: "s1".to_string(),
            credential_groups: vec!["g1".to_string()],
            engine_config_hash: "e1".to_string(),
        };
        let current = ContextFingerprint {
            security_level: Some("Permissive".to_string()),
            llm_providers: vec!["openai".to_string()],
            registered_node_types: vec!["start".to_string()],
            network_policy_hash: Some("a".to_string()),
            schema_hash: "s1".to_string(),
            credential_groups: vec!["g1".to_string()],
            engine_config_hash: "e1".to_string(),
        };

        let diagnostic = diff_fingerprints(&saved, &current);
        assert!(diagnostic.has_danger());
    }
}
