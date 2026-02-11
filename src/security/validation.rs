//! Security-focused validation configs for DSL schemas and templates.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Limits applied during DSL schema validation (max nodes, edges, nesting, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DslValidationConfig {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_nesting_depth: usize,
    pub enable_cycle_detection: bool,
    pub max_node_id_length: usize,
    pub max_node_config_bytes: usize,
    #[serde(default)]
    pub selector_validation: Option<SelectorValidation>,
}

impl Default for DslValidationConfig {
    fn default() -> Self {
        Self {
            max_nodes: 200,
            max_edges: 500,
            max_nesting_depth: 10,
            enable_cycle_detection: true,
            max_node_id_length: 128,
            max_node_config_bytes: 64 * 1024,
            selector_validation: None,
        }
    }
}

/// Safety limits for Jinja2 template rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateSafetyConfig {
    pub max_template_length: usize,
    pub max_output_length: usize,
    pub max_recursion_depth: u32,
    pub disabled_filters: Vec<String>,
    pub disabled_functions: Vec<String>,
    pub max_loop_iterations: usize,
}

impl Default for TemplateSafetyConfig {
    fn default() -> Self {
        Self {
            max_template_length: 10_000,
            max_output_length: 100_000,
            max_recursion_depth: 5,
            disabled_filters: vec![],
            disabled_functions: vec![],
            max_loop_iterations: 1000,
        }
    }
}

/// Rules for validating variable selectors in the DSL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectorValidation {
    pub allowed_prefixes: HashSet<String>,
    pub max_depth: usize,
    pub max_length: usize,
}

impl Default for SelectorValidation {
    fn default() -> Self {
        Self {
            allowed_prefixes: ["sys".into(), "env".into()].into_iter().collect(),
            max_depth: 5,
            max_length: 256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dsl_validation_config_default() {
        let config = DslValidationConfig::default();
        assert_eq!(config.max_nodes, 200);
        assert_eq!(config.max_edges, 500);
        assert_eq!(config.max_nesting_depth, 10);
        assert!(config.enable_cycle_detection);
        assert_eq!(config.max_node_id_length, 128);
        assert_eq!(config.max_node_config_bytes, 64 * 1024);
        assert!(config.selector_validation.is_none());
    }

    #[test]
    fn test_template_safety_config_default() {
        let config = TemplateSafetyConfig::default();
        assert_eq!(config.max_template_length, 10_000);
        assert_eq!(config.max_output_length, 100_000);
        assert_eq!(config.max_recursion_depth, 5);
        assert!(config.disabled_filters.is_empty());
        assert!(config.disabled_functions.is_empty());
        assert_eq!(config.max_loop_iterations, 1000);
    }

    #[test]
    fn test_selector_validation_default() {
        let config = SelectorValidation::default();
        assert!(config.allowed_prefixes.contains("sys"));
        assert!(config.allowed_prefixes.contains("env"));
        assert_eq!(config.allowed_prefixes.len(), 2);
        assert_eq!(config.max_depth, 5);
        assert_eq!(config.max_length, 256);
    }

    #[test]
    fn test_dsl_validation_config_serde_roundtrip() {
        let config = DslValidationConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: DslValidationConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_nodes, config.max_nodes);
        assert_eq!(deserialized.max_edges, config.max_edges);
        assert_eq!(deserialized.enable_cycle_detection, config.enable_cycle_detection);
    }

    #[test]
    fn test_template_safety_config_serde_roundtrip() {
        let config = TemplateSafetyConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: TemplateSafetyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_template_length, config.max_template_length);
        assert_eq!(deserialized.max_output_length, config.max_output_length);
    }

    #[test]
    fn test_dsl_validation_config_with_selector_validation() {
        let config = DslValidationConfig {
            selector_validation: Some(SelectorValidation::default()),
            ..DslValidationConfig::default()
        };
        assert!(config.selector_validation.is_some());
        let sv = config.selector_validation.unwrap();
        assert!(sv.allowed_prefixes.contains("sys"));
    }

    #[test]
    fn test_template_safety_config_custom() {
        let config = TemplateSafetyConfig {
            max_template_length: 5_000,
            max_output_length: 50_000,
            max_recursion_depth: 3,
            disabled_filters: vec!["debug".into()],
            disabled_functions: vec!["range".into()],
            max_loop_iterations: 100,
        };
        assert_eq!(config.max_template_length, 5_000);
        assert_eq!(config.disabled_filters.len(), 1);
        assert_eq!(config.disabled_functions.len(), 1);
    }
}