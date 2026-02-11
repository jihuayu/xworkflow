use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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