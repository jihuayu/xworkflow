mod layer1_structure;
mod layer2_topology;
mod layer3_semantic;
mod known_types;
mod types;

use crate::dsl::parser::{parse_dsl, DslFormat};
use crate::dsl::schema::WorkflowSchema;
#[cfg(feature = "security")]
use crate::security::validation::{DslValidationConfig, SelectorValidation};

pub use types::{Diagnostic, DiagnosticLevel, ValidationReport};

pub fn validate_dsl(content: &str, format: DslFormat) -> ValidationReport {
    match parse_dsl(content, format) {
        Ok(schema) => validate_schema(&schema),
        Err(err) => ValidationReport {
            is_valid: false,
            diagnostics: vec![types::Diagnostic {
                level: types::DiagnosticLevel::Error,
                code: "E001".to_string(),
                message: format!("DSL parse error: {}", err),
                node_id: None,
                edge_id: None,
                field_path: None,
            }],
        },
    }
}

pub fn validate_schema(schema: &WorkflowSchema) -> ValidationReport {
    validate_schema_internal(schema, None)
}

#[cfg(feature = "security")]
pub fn validate_schema_with_config(
    schema: &WorkflowSchema,
    config: &DslValidationConfig,
) -> ValidationReport {
    validate_schema_internal(schema, Some(config))
}

fn validate_schema_internal(
    schema: &WorkflowSchema,
    #[cfg(feature = "security")] config: Option<&DslValidationConfig>,
    #[cfg(not(feature = "security"))] _config: Option<&()>,
) -> ValidationReport {
    let mut diagnostics = Vec::new();

    #[cfg(feature = "security")]
    if let Some(cfg) = config {
        if schema.nodes.len() > cfg.max_nodes {
            diagnostics.push(limit_error(
                "E020",
                format!("Too many nodes: {} (max {})", schema.nodes.len(), cfg.max_nodes),
            ));
        }
        if schema.edges.len() > cfg.max_edges {
            diagnostics.push(limit_error(
                "E021",
                format!("Too many edges: {} (max {})", schema.edges.len(), cfg.max_edges),
            ));
        }

        for node in &schema.nodes {
            if node.id.len() > cfg.max_node_id_length {
                diagnostics.push(types::Diagnostic {
                    level: types::DiagnosticLevel::Error,
                    code: "E022".to_string(),
                    message: format!(
                        "Node id too long: {} (max {})",
                        node.id.len(),
                        cfg.max_node_id_length
                    ),
                    node_id: Some(node.id.clone()),
                    edge_id: None,
                    field_path: Some("id".to_string()),
                });
            }

            let config_bytes = serde_json::to_vec(&node.data.extra).unwrap_or_default();
            if config_bytes.len() > cfg.max_node_config_bytes {
                diagnostics.push(types::Diagnostic {
                    level: types::DiagnosticLevel::Error,
                    code: "E023".to_string(),
                    message: format!(
                        "Node config too large: {} bytes (max {})",
                        config_bytes.len(),
                        cfg.max_node_config_bytes
                    ),
                    node_id: Some(node.id.clone()),
                    edge_id: None,
                    field_path: Some("data".to_string()),
                });
            }
        }

        let nesting_depth = max_subgraph_depth(schema);
        if nesting_depth > cfg.max_nesting_depth {
            diagnostics.push(limit_error(
                "E024",
                format!(
                    "Sub-graph nesting depth {} exceeds max {}",
                    nesting_depth, cfg.max_nesting_depth
                ),
            ));
        }
    }

    let layer1 = layer1_structure::validate(schema);
    diagnostics.extend(layer1);

    let has_fatal_structure = diagnostics.iter().any(|d| {
        d.level == types::DiagnosticLevel::Error
            && matches!(d.code.as_str(), "E003" | "E004" | "E005")
    });

    let topo_info = if has_fatal_structure {
        layer2_topology::TopologyInfo {
            reachable: std::collections::HashSet::new(),
            topo_level: std::collections::HashMap::new(),
        }
    } else {
        let (mut layer2, info) = layer2_topology::validate(schema);
        #[cfg(feature = "security")]
        if let Some(cfg) = config {
            if !cfg.enable_cycle_detection {
                layer2.retain(|d| d.code != "E101");
            }
        }
        diagnostics.extend(layer2);
        info
    };

    diagnostics.extend(layer3_semantic::validate(schema, &topo_info));

    #[cfg(feature = "security")]
    if let Some(cfg) = config {
        if let Some(selector_cfg) = cfg.selector_validation.as_ref() {
            diagnostics.extend(validate_selector_limits(schema, selector_cfg));
        }
    }

    let is_valid = diagnostics
        .iter()
        .all(|d| d.level != types::DiagnosticLevel::Error);

    ValidationReport {
        is_valid,
        diagnostics,
    }
}

#[cfg(feature = "security")]
fn max_subgraph_depth(schema: &WorkflowSchema) -> usize {
    use crate::nodes::subgraph::SubGraphDefinition;
    use serde_json::Value;

    fn depth_from_value(value: &Value, current: usize) -> usize {
        if let Ok(def) = serde_json::from_value::<SubGraphDefinition>(value.clone()) {
            return depth_from_subgraph(&def, current + 1);
        }
        current
    }

    fn depth_from_subgraph(def: &SubGraphDefinition, current: usize) -> usize {
        let mut max = current;
        for node in &def.nodes {
            if let Value::Object(map) = &node.data {
                if let Some(sub) = map.get("sub_graph").or_else(|| map.get("subGraph")) {
                    let depth = depth_from_value(sub, current);
                    if depth > max {
                        max = depth;
                    }
                }
            }
        }
        max
    }

    let mut max = 0;
    for node in &schema.nodes {
        if let Some(sub) = node
            .data
            .extra
            .get("sub_graph")
            .or_else(|| node.data.extra.get("subGraph"))
        {
            let depth = depth_from_value(sub, 0);
            if depth > max {
                max = depth;
            }
        }
    }

    if let Some(handler) = &schema.error_handler {
        let depth = depth_from_subgraph(&handler.sub_graph, 1);
        if depth > max {
            max = depth;
        }
    }

    max
}

#[cfg(feature = "security")]
fn validate_selector_limits(
    schema: &WorkflowSchema,
    selector_cfg: &SelectorValidation,
) -> Vec<types::Diagnostic> {
    use crate::nodes::utils::selector_from_value;
    use serde_json::Value;

    let mut diags = Vec::new();
    let allow_all = selector_cfg
        .allowed_prefixes
        .contains("*");

    for node in &schema.nodes {
        let mut stack = vec![("data".to_string(), Value::Object(
            node.data.extra.clone().into_iter().collect(),
        ))];
        while let Some((path, value)) = stack.pop() {
            match value {
                Value::Object(map) => {
                    for (k, v) in map {
                        let next_path = format!("{}.{}", path, k);
                        if k.ends_with("_selector") || k == "value_selector" || k == "inputs" {
                            if k == "inputs" {
                                if let Value::Object(inputs) = &v {
                                    for (ik, iv) in inputs {
                                        if let Some(sel) = selector_from_value(iv) {
                                            apply_selector_limits(
                                                &sel,
                                                &node.id,
                                                &format!("{}.inputs.{}", path, ik),
                                                selector_cfg,
                                                allow_all,
                                                &mut diags,
                                            );
                                        }
                                    }
                                }
                            } else if let Some(sel) = selector_from_value(&v) {
                                apply_selector_limits(
                                    &sel,
                                    &node.id,
                                    &next_path,
                                    selector_cfg,
                                    allow_all,
                                    &mut diags,
                                );
                            }
                        }
                        stack.push((next_path, v));
                    }
                }
                Value::Array(arr) => {
                    for (idx, item) in arr.into_iter().enumerate() {
                        stack.push((format!("{}[{}]", path, idx), item));
                    }
                }
                _ => {}
            }
        }
    }

    diags
}

#[cfg(feature = "security")]
fn apply_selector_limits(
    selector: &crate::core::variable_pool::Selector,
    node_id: &str,
    field_path: &str,
    cfg: &SelectorValidation,
    allow_all: bool,
    diags: &mut Vec<types::Diagnostic>,
) {
    let depth = if selector.node_id() == crate::core::variable_pool::SCOPE_NODE_ID {
        1
    } else {
        2
    };

    if depth > cfg.max_depth {
        diags.push(types::Diagnostic {
            level: types::DiagnosticLevel::Error,
            code: "E420".to_string(),
            message: format!(
                "Selector depth exceeds max {}: {}",
                cfg.max_depth,
                depth
            ),
            node_id: Some(node_id.to_string()),
            edge_id: None,
            field_path: Some(field_path.to_string()),
        });
    }

    let length = if selector.node_id() == crate::core::variable_pool::SCOPE_NODE_ID {
        selector.variable_name().len()
    } else {
        selector.node_id().len() + 1 + selector.variable_name().len()
    };
    if length > cfg.max_length {
        diags.push(types::Diagnostic {
            level: types::DiagnosticLevel::Error,
            code: "E421".to_string(),
            message: format!(
                "Selector length exceeds max {}: {}",
                cfg.max_length,
                length
            ),
            node_id: Some(node_id.to_string()),
            edge_id: None,
            field_path: Some(field_path.to_string()),
        });
    }

    if !cfg.allowed_prefixes.is_empty() && !allow_all {
        let root = if selector.node_id() == crate::core::variable_pool::SCOPE_NODE_ID {
            selector.variable_name()
        } else {
            selector.node_id()
        };
        if !cfg.allowed_prefixes.contains(root) {
            diags.push(types::Diagnostic {
                level: types::DiagnosticLevel::Error,
                code: "E422".to_string(),
                message: format!("Selector prefix not allowed: {}", root),
                node_id: Some(node_id.to_string()),
                edge_id: None,
                field_path: Some(field_path.to_string()),
            });
        }
    }
}

fn limit_error(code: &str, message: String) -> types::Diagnostic {
    types::Diagnostic {
        level: types::DiagnosticLevel::Error,
        code: code.to_string(),
        message,
        node_id: None,
        edge_id: None,
        field_path: None,
    }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::dsl::parser::DslFormat;
        use crate::dsl::schema::WorkflowSchema;

        #[test]
        fn test_validate_dsl_parse_error() {
                let report = validate_dsl("{{invalid", DslFormat::Json);
                assert!(!report.is_valid);
                assert!(report.diagnostics.iter().any(|d| d.code == "E001"));
        }

        #[test]
        fn test_cycle_detected() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"Start"}},{"id":"a","data":{"type":"code","title":"A","code":"x","language":"javascript"}},{"id":"end","data":{"type":"end","title":"End","outputs":[]}}],"edges":[{"source":"start","target":"a"},{"source":"a","target":"start"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E101"));
        }

        #[test]
        fn test_invalid_start_var_type() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"Start","variables":[{"variable":"query","type":"invalid_type"}]}},{"id":"end","data":{"type":"end","title":"End","outputs":[{"variable":"out","value_selector":["start","query"]}]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E201"));
        }
}
