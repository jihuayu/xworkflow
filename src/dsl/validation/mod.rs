//! Three-layer DSL validation.
//!
//! Validation is performed in three passes:
//! 1. **Structure** — Checks required fields, data shapes, enum validity.
//! 2. **Topology** — Ensures the graph is a valid DAG (no cycles, reachable nodes).
//! 3. **Semantics** — Validates variable selectors, type compatibility, edge handles.
//!
//! Results are collected into a [`ValidationReport`] containing [`Diagnostic`]s.

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

/// Validate raw DSL text (parse + schema validation in one step).
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

/// Validate a parsed [`WorkflowSchema`] and return a diagnostic report.
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

        // ---- Layer1 structural errors ----

        #[test]
        fn test_unsupported_dsl_version() {
                let json = r#"{"version":"99.0.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E002"));
        }

        #[test]
        fn test_no_nodes() {
                let json = r#"{"version":"0.1.0","nodes":[],"edges":[]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E003"));
        }

        #[test]
        fn test_no_start_node() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"end","data":{"type":"end","title":"End","outputs":[]}}],"edges":[]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E004"));
        }

        #[test]
        fn test_multiple_start_nodes() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"s1","data":{"type":"start","title":"S1"}},{"id":"s2","data":{"type":"start","title":"S2"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"s1","target":"end"},{"source":"s2","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E005"));
        }

        #[test]
        fn test_no_end_node() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"a","data":{"type":"code","title":"A","code":"x","language":"javascript"}}],"edges":[{"source":"start","target":"a"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E006"));
        }

        #[test]
        fn test_duplicate_node_ids() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"start","data":{"type":"start","title":"S2"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E007"));
        }

        #[test]
        fn test_empty_node_id() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"","data":{"type":"code","title":"X","code":"x","language":"javascript"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E008"));
        }

        #[test]
        fn test_unknown_node_type() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"x","data":{"type":"quantum-teleport","title":"X"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"x"},{"source":"x","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E009"));
        }

        #[test]
        fn test_edge_source_not_found() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"missing","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E010"));
        }

        #[test]
        fn test_edge_target_not_found() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"missing"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E011"));
        }

        #[test]
        fn test_edge_self_loop() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"a","data":{"type":"code","title":"A","code":"x","language":"javascript"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"a"},{"source":"a","target":"a"},{"source":"a","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E012"));
        }

        #[test]
        fn test_duplicate_edge() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"},{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E013"));
        }

        #[test]
        fn test_empty_title_warning() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":""}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "W001"));
        }

        // ---- Layer2 topology errors ----

        #[test]
        fn test_unreachable_node() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"a","data":{"type":"code","title":"A","code":"x","language":"javascript"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                // Node 'a' should generate an error for being unreachable
                assert!(report.diagnostics.iter().any(|d| d.code == "E102"), "expected E102, got: {:?}", report.diagnostics);
        }

        // ---- Layer3 semantic errors ----

        #[test]
        fn test_end_empty_value_selector() {
                // Empty array selector fails to deserialize, so E217 is expected
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[{"variable":"out","value_selector":[]}]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217" || d.code == "E202" || d.code == "E401"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_answer_empty_template() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"ans","data":{"type":"answer","title":"A","answer":""}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"ans"},{"source":"ans","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E203"));
        }

        #[test]
        fn test_if_else_empty_cases() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"if1","data":{"type":"if-else","title":"IF","cases":[]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"if1"},{"source":"if1","target":"end","source_handle":"false"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E204"));
        }

        #[test]
        fn test_code_empty() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"c","data":{"type":"code","title":"C","code":"","language":"javascript"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"c"},{"source":"c","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E207"));
        }

        #[test]
        fn test_code_unsupported_language() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"c","data":{"type":"code","title":"C","code":"x=1","language":"ruby"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"c"},{"source":"c","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E208"));
        }

        #[test]
        fn test_code_python_warning() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"c","data":{"type":"code","title":"C","code":"x=1","language":"python3"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"c"},{"source":"c","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "W201"));
        }

        #[test]
        fn test_http_empty_url() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"h","data":{"type":"http-request","title":"H","url":"","method":"GET","headers":[],"params":[]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"h"},{"source":"h","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E209"));
        }

        #[test]
        fn test_template_transform_empty() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"t","data":{"type":"template-transform","title":"T","template":"","variables":[]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"t"},{"source":"t","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E210"));
        }

        #[test]
        fn test_variable_aggregator_empty() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"va","data":{"type":"variable-aggregator","title":"VA","variables":[]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"va"},{"source":"va","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E215"));
        }

        #[test]
        fn test_selector_unknown_node() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[{"variable":"out","value_selector":["nonexistent","val"]}]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E402"));
        }

        #[test]
        fn test_if_else_branch_missing_false_edge() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"if1","data":{"type":"if-else","title":"IF","cases":[{"case_id":"c1","logical_operator":"and","conditions":[{"variable_selector":["start","query"],"comparison_operator":"is_not","value":""}]}]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"if1"},{"source":"if1","target":"end","source_handle":"c1"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E301"));
        }

        #[test]
        fn test_non_branch_source_handle() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"c","data":{"type":"code","title":"C","code":"x","language":"javascript"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"c"},{"source":"c","target":"end","source_handle":"custom"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E303"));
        }

        #[test]
        fn test_dify_template_malformed_selector() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"ans","data":{"type":"answer","title":"A","answer":"{{#noseparator#}}"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"ans"},{"source":"ans","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E502"));
        }

        #[test]
        fn test_if_else_empty_conditions() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"if1","data":{"type":"if-else","title":"IF","cases":[{"case_id":"c1","logical_operator":"and","conditions":[]}]}},{"id":"a","data":{"type":"end","title":"A","outputs":[]}},{"id":"b","data":{"type":"end","title":"B","outputs":[]}}],"edges":[{"source":"start","target":"if1"},{"source":"if1","target":"a","source_handle":"c1"},{"source":"if1","target":"b","source_handle":"false"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E205"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_if_else_duplicate_case_id() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"if1","data":{"type":"if-else","title":"IF","cases":[{"case_id":"dup","logical_operator":"and","conditions":[{"variable_selector":["start","x"],"comparison_operator":"is","value":"1"}]},{"case_id":"dup","logical_operator":"and","conditions":[{"variable_selector":["start","x"],"comparison_operator":"is","value":"2"}]}]}},{"id":"a","data":{"type":"end","title":"A","outputs":[]}},{"id":"b","data":{"type":"end","title":"B","outputs":[]}}],"edges":[{"source":"start","target":"if1"},{"source":"if1","target":"a","source_handle":"dup"},{"source":"if1","target":"b","source_handle":"false"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E206"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_iteration_empty_sub_graph() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"iter","data":{"type":"iteration","title":"I","sub_graph":{"nodes":[],"edges":[]},"iterator_selector":["start","items"],"output_variable":"result"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"iter"},{"source":"iter","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E211"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_iteration_empty_selector() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"iter","data":{"type":"iteration","title":"I","sub_graph":{"nodes":[{"id":"s","data":{"type":"start","title":"S"}}],"edges":[]},"iterator_selector":[],"output_variable":"result"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"iter"},{"source":"iter","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E212"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_loop_empty_sub_graph() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"lp","data":{"type":"loop","title":"L","sub_graph":{"nodes":[],"edges":[]},"condition":{"variable_selector":["start","x"],"comparison_operator":"is","value":"done"},"max_iterations":10,"output_variable":"result"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"lp"},{"source":"lp","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E213"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_loop_missing_break_condition() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"lp","data":{"type":"loop","title":"L","sub_graph":{"nodes":[{"id":"s","data":{"type":"start","title":"S"}}],"edges":[]},"condition":{"variable_selector":[],"comparison_operator":"is","value":"x"},"max_iterations":10,"output_variable":"result"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"lp"},{"source":"lp","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E214"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_list_operator_missing_operation() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"lo","data":{"type":"list-operator","title":"LO"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"lo"},{"source":"lo","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E216"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_branch_case_missing_edge() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"if1","data":{"type":"if-else","title":"IF","cases":[{"case_id":"c1","logical_operator":"and","conditions":[{"variable_selector":["start","x"],"comparison_operator":"is","value":"1"}]},{"case_id":"c2","logical_operator":"and","conditions":[{"variable_selector":["start","x"],"comparison_operator":"is","value":"2"}]}]}},{"id":"a","data":{"type":"end","title":"A","outputs":[]}},{"id":"b","data":{"type":"end","title":"B","outputs":[]}}],"edges":[{"source":"start","target":"if1"},{"source":"if1","target":"a","source_handle":"c1"},{"source":"if1","target":"b","source_handle":"false"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E302"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_branch_unknown_source_handle() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"if1","data":{"type":"if-else","title":"IF","cases":[{"case_id":"c1","logical_operator":"and","conditions":[{"variable_selector":["start","x"],"comparison_operator":"is","value":"1"}]}]}},{"id":"a","data":{"type":"end","title":"A","outputs":[]}},{"id":"b","data":{"type":"end","title":"B","outputs":[]}},{"id":"c","data":{"type":"end","title":"C","outputs":[]}}],"edges":[{"source":"start","target":"if1"},{"source":"if1","target":"a","source_handle":"c1"},{"source":"if1","target":"b","source_handle":"false"},{"source":"if1","target":"c","source_handle":"bogus"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E304"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_selector_downstream_reference() {
                // end node references itself (downstream)
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"c1","data":{"type":"code","title":"C","code":"x","language":"javascript","variables":[{"variable":"v","value_selector":["end","out"]}]}},{"id":"end","data":{"type":"end","title":"E","outputs":[{"variable":"out","value_selector":["start","q"]}]}}],"edges":[{"source":"start","target":"c1"},{"source":"c1","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E403"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_dify_template_unknown_node_ref() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"ans","data":{"type":"answer","title":"A","answer":"Result: {{#ghost_node.val#}}"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"ans"},{"source":"ans","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "W502"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_start_invalid_variable_type() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S","variables":[{"variable":"x","label":"X","type":"custom_type","required":false}]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E201"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_http_dify_template_in_url() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"h","data":{"type":"http-request","title":"H","url":"https://api.com/{{#missing_node.path#}}","method":"GET","headers":[],"params":[]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"h"},{"source":"h","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "W502"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_assigner_bad_config() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"a","data":{"type":"assigner","title":"A","bad_field":"bad"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"a"},{"source":"a","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_stub_node_type_warning() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"tool","data":{"type":"tool","title":"T"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"tool"},{"source":"tool","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "W202"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_valid_workflow_passes() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"Start"}},{"id":"end","data":{"type":"end","title":"End","outputs":[{"variable":"out","value_selector":["start","query"]}]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.is_valid, "diagnostics: {:?}", report.diagnostics);
        }

        #[test]
        fn test_validate_dsl_yaml_parse_error() {
                let report = validate_dsl("{{invalid yaml: [", DslFormat::Yaml);
                assert!(!report.is_valid);
                assert!(report.diagnostics.iter().any(|d| d.code == "E001"));
        }

        #[cfg(feature = "security")]
        #[test]
        fn test_validate_with_config_max_nodes() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"a","data":{"type":"code","title":"A","code":"x","language":"javascript"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"a"},{"source":"a","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let config = DslValidationConfig {
                        max_nodes: 1,
                        ..Default::default()
                };
                let report = validate_schema_with_config(&schema, &config);
                assert!(report.diagnostics.iter().any(|d| d.code == "E020"));
        }

        #[cfg(feature = "security")]
        #[test]
        fn test_validate_with_config_max_edges() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let config = DslValidationConfig {
                        max_edges: 0,
                        ..Default::default()
                };
                let report = validate_schema_with_config(&schema, &config);
                assert!(report.diagnostics.iter().any(|d| d.code == "E021"));
        }

        #[cfg(feature = "security")]
        #[test]
        fn test_validate_with_config_node_id_too_long() {
                let long_id = "a".repeat(300);
                let json = format!(r#"{{"version":"0.1.0","nodes":[{{"id":"start","data":{{"type":"start","title":"S"}}}},{{"id":"{}","data":{{"type":"code","title":"C","code":"x","language":"javascript"}}}},{{"id":"end","data":{{"type":"end","title":"E","outputs":[]}}}}],"edges":[{{"source":"start","target":"{}"}},{{"source":"{}","target":"end"}}]}}"#, long_id, long_id, long_id);
                let schema: WorkflowSchema = serde_json::from_str(&json).unwrap();
                let config = DslValidationConfig {
                        max_node_id_length: 64,
                        ..Default::default()
                };
                let report = validate_schema_with_config(&schema, &config);
                assert!(report.diagnostics.iter().any(|d| d.code == "E022"));
        }

        #[cfg(feature = "security")]
        #[test]
        fn test_validate_with_config_selector_limits() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[{"variable":"out","value_selector":["start","query"]}]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let config = DslValidationConfig {
                        selector_validation: Some(SelectorValidation {
                                max_depth: 1,
                                max_length: 5,
                                allowed_prefixes: std::collections::HashSet::new(),
                        }),
                        ..Default::default()
                };
                let report = validate_schema_with_config(&schema, &config);
                assert!(report.diagnostics.iter().any(|d| d.code == "E420" || d.code == "E421" || d.code == "E422"));
        }

        // ---- Additional layer3 semantic coverage (new paths) ----

        #[cfg(feature = "builtin-template-jinja")]
        #[test]
        fn test_jinja_template_syntax_error_e501() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"t","data":{"type":"template-transform","title":"T","template":"{% if x %}unclosed","variables":[]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"t"},{"source":"t","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E501"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_branch_extra_source_edge_w301() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"if1","data":{"type":"if-else","title":"IF","cases":[{"case_id":"c1","logical_operator":"and","conditions":[{"variable_selector":["start","x"],"comparison_operator":"is","value":"1"}]}]}},{"id":"a","data":{"type":"end","title":"A","outputs":[]}},{"id":"b","data":{"type":"end","title":"B","outputs":[]}},{"id":"c","data":{"type":"end","title":"C","outputs":[]}}],"edges":[{"source":"start","target":"if1"},{"source":"if1","target":"a","source_handle":"c1"},{"source":"if1","target":"b","source_handle":"false"},{"source":"if1","target":"c","source_handle":"source"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "W301"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_template_selector_unknown_ref() {
                // Template variables that reference unknown nodes should generate warnings
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"t","data":{"type":"template-transform","title":"T","template":"Hello {{ name }}","variables":[{"variable":"name","value_selector":["ghost","val"]}]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"t"},{"source":"t","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E402"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_start_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S","variables":"not_an_array"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_end_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":"bad"}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_answer_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"ans","data":{"type":"answer","title":"A","answer":123}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"ans"},{"source":"ans","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_ifelse_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"if1","data":{"type":"if-else","title":"IF","cases":"bad"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"if1"},{"source":"if1","target":"end","source_handle":"false"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_http_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"h","data":{"type":"http-request","title":"H","url":123}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"h"},{"source":"h","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_template_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"t","data":{"type":"template-transform","title":"T","template":123}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"t"},{"source":"t","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_aggregator_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"va","data":{"type":"variable-aggregator","title":"VA","variables":"bad"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"va"},{"source":"va","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_iteration_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"it","data":{"type":"iteration","title":"IT","sub_graph":"bad"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"it"},{"source":"it","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_loop_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"lp","data":{"type":"loop","title":"LP","sub_graph":"bad"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"lp"},{"source":"lp","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_fail_branch_allows_source_handle() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"c","data":{"type":"code","title":"C","code":"x","language":"javascript","error_strategy":{"type":"fail-branch"}}},{"id":"ok","data":{"type":"end","title":"OK","outputs":[]}},{"id":"err","data":{"type":"end","title":"ERR","outputs":[]}}],"edges":[{"source":"start","target":"c"},{"source":"c","target":"ok","source_handle":"source"},{"source":"c","target":"err","source_handle":"fail-branch"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(!report.diagnostics.iter().any(|d| d.code == "E303"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_http_dify_template_headers_params_w502() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"h","data":{"type":"http-request","title":"H","url":"https://api.com","method":"GET","headers":[{"key":"Authorization","value":"Bearer {{#unknown.token#}}"}],"params":[{"key":"q","value":"{{#missing.query#}}"}]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"h"},{"source":"h","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                let w502_count = report.diagnostics.iter().filter(|d| d.code == "W502").count();
                assert!(w502_count >= 2, "expected >=2 W502, got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_selector_sys_namespace_ok() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[{"variable":"out","value_selector":["sys","query"]}]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(!report.diagnostics.iter().any(|d| d.code == "E402"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_code_with_numeric_code_field() {
                // When code field is a number it becomes empty string → E207
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"c","data":{"type":"code","title":"C","code":123,"language":"javascript"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"c"},{"source":"c","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E207" || d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_list_operator_parse_error_e217() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"lo","data":{"type":"list-operator","title":"LO","operation":123}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"lo"},{"source":"lo","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E216" || d.code == "E217"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_llm_node_basic_validation() {
                // LLM node with minimal data also works through validation
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"l","data":{"type":"llm","title":"L"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"l"},{"source":"l","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                // LLM may or may not produce specific diagnostics
                let _ = report;
        }

        #[test]
        fn test_answer_dify_template_validation() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"ans","data":{"type":"answer","title":"A","answer":"Hi {{#start.query#}}!"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"ans"},{"source":"ans","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                // Valid dify template referencing start.query should pass
                assert!(!report.diagnostics.iter().any(|d| d.code == "E502" || d.code == "W502"), "got: {:?}", report.diagnostics);
        }

        #[test]
        fn test_validate_dsl_json_format() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let report = validate_dsl(json, DslFormat::Json);
                assert!(report.is_valid, "diagnostics: {:?}", report.diagnostics);
        }

        #[test]
        fn test_validate_dsl_json_parse_error() {
                let report = validate_dsl("not json at all {{{", DslFormat::Json);
                assert!(!report.is_valid);
                assert!(report.diagnostics.iter().any(|d| d.code == "E001"));
        }

        #[test]
        fn test_validate_dsl_yaml_valid() {
                let yaml = "version: '0.1.0'\nnodes:\n  - id: start\n    data:\n      type: start\n      title: S\n  - id: end\n    data:\n      type: end\n      title: E\n      outputs: []\nedges:\n  - source: start\n    target: end\n";
                let report = validate_dsl(yaml, DslFormat::Yaml);
                assert!(report.is_valid, "diagnostics: {:?}", report.diagnostics);
        }

        #[test]
        fn test_multiple_errors_in_single_workflow() {
                // Workflow with multiple validation issues: empty code + unsupported lang + empty url
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"S"}},{"id":"c","data":{"type":"code","title":"C","code":"","language":"ruby"}},{"id":"h","data":{"type":"http-request","title":"H","url":"","method":"GET","headers":[],"params":[]}},{"id":"end","data":{"type":"end","title":"E","outputs":[]}}],"edges":[{"source":"start","target":"c"},{"source":"c","target":"h"},{"source":"h","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                // Should have E207, E208, E209
                assert!(report.diagnostics.iter().any(|d| d.code == "E207"), "missing E207: {:?}", report.diagnostics);
                assert!(report.diagnostics.iter().any(|d| d.code == "E208"), "missing E208: {:?}", report.diagnostics);
                assert!(report.diagnostics.iter().any(|d| d.code == "E209"), "missing E209: {:?}", report.diagnostics);
        }
}
