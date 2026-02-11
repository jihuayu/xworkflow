use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::Value;

use crate::core::variable_pool::{SegmentType, Selector};
use crate::dsl::schema::{
    AnswerNodeData, CodeNodeData, EndNodeData, HttpRequestNodeData, IfElseNodeData,
    StartNodeData, TemplateTransformNodeData, VariableAggregatorNodeData,
    VariableAssignerNodeData, WorkflowSchema,
};
use crate::nodes::subgraph_nodes::{IterationNodeConfig, ListOperatorNodeConfig, LoopNodeConfig};
use crate::nodes::utils::selector_from_value;
#[cfg(feature = "builtin-template-jinja")]
use crate::template::CompiledTemplate;

use super::known_types::{is_stub_node_type, BRANCH_NODE_TYPES, RESERVED_NAMESPACES};
use super::layer2_topology::TopologyInfo;
use super::types::{Diagnostic, DiagnosticLevel};

pub fn validate(schema: &WorkflowSchema, topo: &TopologyInfo) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let node_ids: HashSet<String> = schema.nodes.iter().map(|n| n.id.clone()).collect();
    let edges_by_source = build_edges_by_source(schema);

    for node in &schema.nodes {
        if is_stub_node_type(&node.data.node_type) {
            diags.push(warn(
                "W202",
                format!("Stub node type: {}", node.data.node_type),
                Some(node.id.clone()),
                None,
            ));
        }

        let config_value = Value::Object(node.data.extra.clone().into_iter().collect());

        match node.data.node_type.as_str() {
            "start" => match serde_json::from_value::<StartNodeData>(config_value.clone()) {
                Ok(data) => {
                    for (idx, var) in data.variables.iter().enumerate() {
                        if SegmentType::from_dsl_type(&var.var_type).is_none() {
                            diags.push(error(
                                "E201",
                                format!("Invalid start variable type: {}", var.var_type),
                                Some(node.id.clone()),
                                Some(format!("variables[{}].type", idx)),
                            ));
                        }
                    }
                }
                Err(_) => diags.push(error(
                    "E217",
                    "Failed to parse start node config".to_string(),
                    Some(node.id.clone()),
                    Some("data".to_string()),
                )),
            },
            "end" => match serde_json::from_value::<EndNodeData>(config_value.clone()) {
                Ok(data) => {
                    for (idx, out) in data.outputs.iter().enumerate() {
                        if out.value_selector.is_empty() {
                            diags.push(error(
                                "E202",
                                "End output value_selector is empty".to_string(),
                                Some(node.id.clone()),
                                Some(format!("outputs[{}].value_selector", idx)),
                            ));
                        }
                    }
                }
                Err(_) => diags.push(error(
                    "E217",
                    "Failed to parse end node config".to_string(),
                    Some(node.id.clone()),
                    Some("data".to_string()),
                )),
            },
            "answer" => match serde_json::from_value::<AnswerNodeData>(config_value.clone()) {
                Ok(data) => {
                    if data.answer.trim().is_empty() {
                        diags.push(error(
                            "E203",
                            "Answer template is empty".to_string(),
                            Some(node.id.clone()),
                            Some("answer".to_string()),
                        ));
                    }
                    validate_dify_template(
                        &data.answer,
                        &node_ids,
                        &mut diags,
                        Some(node.id.clone()),
                        "answer",
                    );
                }
                Err(_) => diags.push(error(
                    "E217",
                    "Failed to parse answer node config".to_string(),
                    Some(node.id.clone()),
                    Some("data".to_string()),
                )),
            },
            "if-else" => match serde_json::from_value::<IfElseNodeData>(config_value.clone()) {
                Ok(data) => {
                    if data.cases.is_empty() {
                        diags.push(error(
                            "E204",
                            "If-else cases are empty".to_string(),
                            Some(node.id.clone()),
                            Some("cases".to_string()),
                        ));
                    }
                    let mut case_ids = HashSet::new();
                    for (idx, case) in data.cases.iter().enumerate() {
                        if case.conditions.is_empty() {
                            diags.push(error(
                                "E205",
                                "If-else case conditions are empty".to_string(),
                                Some(node.id.clone()),
                                Some(format!("cases[{}].conditions", idx)),
                            ));
                        }
                        if !case_ids.insert(case.case_id.clone()) {
                            diags.push(error(
                                "E206",
                                "Duplicate case_id in if-else".to_string(),
                                Some(node.id.clone()),
                                Some("cases".to_string()),
                            ));
                        }
                    }
                }
                Err(_) => diags.push(error(
                    "E217",
                    "Failed to parse if-else node config".to_string(),
                    Some(node.id.clone()),
                    Some("data".to_string()),
                )),
            },
            "code" => {
                let code = config_value
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if code.trim().is_empty() {
                    diags.push(error(
                        "E207",
                        "Code is empty".to_string(),
                        Some(node.id.clone()),
                        Some("code".to_string()),
                    ));
                }

                if let Some(lang) = config_value.get("language").and_then(|v| v.as_str()) {
                    match lang {
                        "javascript" | "wasm" => {}
                        "python3" => {
                            diags.push(warn(
                                "W201",
                                "Python runtime is not fully supported".to_string(),
                                Some(node.id.clone()),
                                Some("language".to_string()),
                            ));
                        }
                        _ => {
                            diags.push(error(
                                "E208",
                                "Unsupported code language".to_string(),
                                Some(node.id.clone()),
                                Some("language".to_string()),
                            ));
                        }
                    }
                }
            }
            "http-request" => match serde_json::from_value::<HttpRequestNodeData>(config_value.clone()) {
                Ok(data) => {
                    if data.url.trim().is_empty() {
                        diags.push(error(
                            "E209",
                            "HTTP url is empty".to_string(),
                            Some(node.id.clone()),
                            Some("url".to_string()),
                        ));
                    }
                    validate_dify_template(
                        &data.url,
                        &node_ids,
                        &mut diags,
                        Some(node.id.clone()),
                        "url",
                    );
                    for (idx, header) in data.headers.iter().enumerate() {
                        validate_dify_template(
                            &header.value,
                            &node_ids,
                            &mut diags,
                            Some(node.id.clone()),
                            &format!("headers[{}].value", idx),
                        );
                    }
                    for (idx, param) in data.params.iter().enumerate() {
                        validate_dify_template(
                            &param.value,
                            &node_ids,
                            &mut diags,
                            Some(node.id.clone()),
                            &format!("params[{}].value", idx),
                        );
                    }
                }
                Err(_) => diags.push(error(
                    "E217",
                    "Failed to parse http-request node config".to_string(),
                    Some(node.id.clone()),
                    Some("data".to_string()),
                )),
            },
            "template-transform" => match serde_json::from_value::<TemplateTransformNodeData>(config_value.clone()) {
                Ok(data) => {
                    if data.template.trim().is_empty() {
                        diags.push(error(
                            "E210",
                            "Template is empty".to_string(),
                            Some(node.id.clone()),
                            Some("template".to_string()),
                        ));
                    }
                    #[cfg(feature = "builtin-template-jinja")]
                    if CompiledTemplate::new(&data.template, None).is_err() {
                        diags.push(error(
                            "E501",
                            "Jinja2 template syntax error".to_string(),
                            Some(node.id.clone()),
                            Some("template".to_string()),
                        ));
                    }
                }
                Err(_) => diags.push(error(
                    "E217",
                    "Failed to parse template-transform node config".to_string(),
                    Some(node.id.clone()),
                    Some("data".to_string()),
                )),
            },
            "variable-aggregator" => {
                if let Ok(data) = serde_json::from_value::<VariableAggregatorNodeData>(config_value.clone()) {
                    if data.variables.is_empty() {
                        diags.push(error(
                            "E215",
                            "Variable aggregator variables are empty".to_string(),
                            Some(node.id.clone()),
                            Some("variables".to_string()),
                        ));
                    }
                } else {
                    diags.push(error(
                        "E217",
                        "Failed to parse variable-aggregator node config".to_string(),
                        Some(node.id.clone()),
                        Some("data".to_string()),
                    ));
                }
            }
            "assigner" => {
                if serde_json::from_value::<VariableAssignerNodeData>(config_value.clone()).is_err() {
                    diags.push(error(
                        "E217",
                        "Failed to parse variable-assigner node config".to_string(),
                        Some(node.id.clone()),
                        Some("data".to_string()),
                    ));
                }
            }
            "iteration" => match serde_json::from_value::<IterationNodeConfig>(config_value.clone()) {
                Ok(data) => {
                    if data.sub_graph.nodes.is_empty() {
                        diags.push(error(
                            "E211",
                            "Iteration sub_graph is missing".to_string(),
                            Some(node.id.clone()),
                            Some("sub_graph".to_string()),
                        ));
                    }
                    let selector_val = data
                        .input_selector
                        .as_ref()
                        .or(data.iterator_selector.as_ref());
                    if selector_val
                        .and_then(|v| selector_from_value(v))
                        .map(|s| s.is_empty())
                        .unwrap_or(true)
                    {
                        diags.push(error(
                            "E212",
                            "Iteration iterator_selector is empty".to_string(),
                            Some(node.id.clone()),
                            Some("iterator_selector".to_string()),
                        ));
                    }
                }
                Err(_) => diags.push(error(
                    "E217",
                    "Failed to parse iteration node config".to_string(),
                    Some(node.id.clone()),
                    Some("data".to_string()),
                )),
            },
            "loop" => match serde_json::from_value::<LoopNodeConfig>(config_value.clone()) {
                Ok(data) => {
                    if data.sub_graph.nodes.is_empty() {
                        diags.push(error(
                            "E213",
                            "Loop sub_graph is missing".to_string(),
                            Some(node.id.clone()),
                            Some("sub_graph".to_string()),
                        ));
                    }
                    if selector_from_value(&data.condition.variable_selector).is_none() {
                        diags.push(error(
                            "E214",
                            "Loop break condition is missing".to_string(),
                            Some(node.id.clone()),
                            Some("condition.variable_selector".to_string()),
                        ));
                    }
                }
                Err(_) => diags.push(error(
                    "E217",
                    "Failed to parse loop node config".to_string(),
                    Some(node.id.clone()),
                    Some("data".to_string()),
                )),
            },
            "list-operator" => {
                if let Ok(data) = serde_json::from_value::<ListOperatorNodeConfig>(config_value.clone()) {
                    let _ = data.operation;
                } else {
                    diags.push(error(
                        "E216",
                        "List operator missing operation".to_string(),
                        Some(node.id.clone()),
                        Some("operation".to_string()),
                    ));
                }
            }
            _ => {}
        }
    }

    // Branch edge validation
    for node in &schema.nodes {
        let is_branch = BRANCH_NODE_TYPES.iter().any(|t| *t == node.data.node_type);
        let edges = edges_by_source.get(&node.id).cloned().unwrap_or_default();
        if is_branch {
            if let Ok(data) = serde_json::from_value::<IfElseNodeData>(Value::Object(
                node.data.extra.clone().into_iter().collect(),
            )) {
                let case_ids: HashSet<String> = data.cases.iter().map(|c| c.case_id.clone()).collect();
                let mut has_false = false;
                for edge in &edges {
                    let handle = edge.source_handle.as_deref().unwrap_or("source");
                    if handle == "false" {
                        has_false = true;
                    } else if handle == "source" {
                        diags.push(warn(
                            "W301",
                            "Branch node has extra edge".to_string(),
                            Some(node.id.clone()),
                            Some("edges".to_string()),
                        ));
                    } else if !case_ids.contains(handle) {
                        diags.push(error(
                            "E304",
                            "Branch edge has unknown source_handle".to_string(),
                            Some(node.id.clone()),
                            Some("edges".to_string()),
                        ));
                    }
                }
                if !has_false {
                    diags.push(error(
                        "E301",
                        "Branch node missing false edge".to_string(),
                        Some(node.id.clone()),
                        None,
                    ));
                }
                for case_id in case_ids {
                    let has_edge = edges.iter().any(|e| e.source_handle.as_deref() == Some(&case_id));
                    if !has_edge {
                        diags.push(error(
                            "E302",
                            format!("Branch case missing edge: {}", case_id),
                            Some(node.id.clone()),
                            None,
                        ));
                    }
                }
            }
        } else {
            for edge in &edges {
                if let Some(handle) = &edge.source_handle {
                    if handle != "source" {
                        let allow_fail_branch = node
                            .data
                            .error_strategy
                            .as_ref()
                            .map(|s| matches!(s.strategy_type, crate::dsl::schema::ErrorStrategyType::FailBranch))
                            .unwrap_or(false);
                        if !(allow_fail_branch && handle == "fail-branch") {
                            diags.push(error(
                                "E303",
                                "Non-branch node has source_handle".to_string(),
                                Some(node.id.clone()),
                                Some("edges".to_string()),
                            ));
                        }
                    }
                }
            }
        }
    }

    // Variable selector validation
    for node in &schema.nodes {
        let config_value = Value::Object(node.data.extra.clone().into_iter().collect());
        let node_id = node.id.clone();

        match node.data.node_type.as_str() {
            "end" => {
                if let Ok(data) = serde_json::from_value::<EndNodeData>(config_value.clone()) {
                    for (idx, out) in data.outputs.iter().enumerate() {
                        validate_selector(
                            &out.value_selector,
                            &node_id,
                            &format!("outputs[{}].value_selector", idx),
                            &node_ids,
                            topo,
                            &mut diags,
                        );
                    }
                }
            }
            "if-else" => {
                if let Ok(data) = serde_json::from_value::<IfElseNodeData>(config_value.clone()) {
                    for (case_idx, case) in data.cases.iter().enumerate() {
                        for (cond_idx, cond) in case.conditions.iter().enumerate() {
                            validate_selector(
                                &cond.variable_selector,
                                &node_id,
                                &format!("cases[{}].conditions[{}].variable_selector", case_idx, cond_idx),
                                &node_ids,
                                topo,
                                &mut diags,
                            );
                        }
                    }
                }
            }
            "template-transform" => {
                if let Ok(data) = serde_json::from_value::<TemplateTransformNodeData>(config_value.clone()) {
                    for (idx, var) in data.variables.iter().enumerate() {
                        validate_selector(
                            &var.value_selector,
                            &node_id,
                            &format!("variables[{}].value_selector", idx),
                            &node_ids,
                            topo,
                            &mut diags,
                        );
                    }
                }
            }
            "code" => {
                if let Ok(data) = serde_json::from_value::<CodeNodeData>(config_value.clone()) {
                    for (idx, var) in data.variables.iter().enumerate() {
                        validate_selector(
                            &var.value_selector,
                            &node_id,
                            &format!("variables[{}].value_selector", idx),
                            &node_ids,
                            topo,
                            &mut diags,
                        );
                    }
                }
            }
            "variable-aggregator" => {
                if let Ok(data) = serde_json::from_value::<VariableAggregatorNodeData>(config_value.clone()) {
                    for (idx, selector) in data.variables.iter().enumerate() {
                        validate_selector(
                            selector,
                            &node_id,
                            &format!("variables[{}]", idx),
                            &node_ids,
                            topo,
                            &mut diags,
                        );
                    }
                }
            }
            "assigner" => {
                if let Ok(data) = serde_json::from_value::<VariableAssignerNodeData>(config_value.clone()) {
                    validate_selector(
                        &data.assigned_variable_selector,
                        &node_id,
                        "assigned_variable_selector",
                        &node_ids,
                        topo,
                        &mut diags,
                    );
                    if let Some(sel) = data.input_variable_selector {
                        validate_selector(
                            &sel,
                            &node_id,
                            "input_variable_selector",
                            &node_ids,
                            topo,
                            &mut diags,
                        );
                    }
                }
            }
            "iteration" => {
                if let Ok(data) = serde_json::from_value::<IterationNodeConfig>(config_value.clone()) {
                    if let Some(selector) = data
                        .input_selector
                        .as_ref()
                        .or(data.iterator_selector.as_ref())
                        .and_then(|v| selector_from_value(v))
                    {
                        validate_selector(
                            &selector,
                            &node_id,
                            "iterator_selector",
                            &node_ids,
                            topo,
                            &mut diags,
                        );
                    }
                }
            }
            "loop" => {
                if let Ok(data) = serde_json::from_value::<LoopNodeConfig>(config_value.clone()) {
                    if let Some(selector) = selector_from_value(&data.condition.variable_selector) {
                        validate_selector(
                            &selector,
                            &node_id,
                            "condition.variable_selector",
                            &node_ids,
                            topo,
                            &mut diags,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    diags
}

fn build_edges_by_source(schema: &WorkflowSchema) -> HashMap<String, Vec<crate::dsl::schema::EdgeSchema>> {
    let mut map: HashMap<String, Vec<crate::dsl::schema::EdgeSchema>> = HashMap::new();
    for edge in &schema.edges {
        map.entry(edge.source.clone()).or_default().push(edge.clone());
    }
    map
}

fn validate_selector(
    selector: &Selector,
    current_node: &str,
    field_path: &str,
    node_ids: &HashSet<String>,
    topo: &TopologyInfo,
    diags: &mut Vec<Diagnostic>,
) {
    if selector.is_empty() {
        diags.push(error(
            "E401",
            "Variable selector too short".to_string(),
            Some(current_node.to_string()),
            Some(field_path.to_string()),
        ));
        return;
    }
    let root = selector.node_id();
    if !RESERVED_NAMESPACES.contains(&root) && !node_ids.contains(root) {
        diags.push(error(
            "E402",
            "Variable selector references unknown node".to_string(),
            Some(current_node.to_string()),
            Some(field_path.to_string()),
        ));
        return;
    }
    if node_ids.contains(root) {
        if let (Some(cur_level), Some(ref_level)) = (
            topo.topo_level.get(current_node),
            topo.topo_level.get(root),
        ) {
            if ref_level >= cur_level {
                diags.push(error(
                    "E403",
                    "Variable selector references downstream node".to_string(),
                    Some(current_node.to_string()),
                    Some(field_path.to_string()),
                ));
            }
        }
        if !topo.reachable.contains(root) {
            diags.push(warn(
                "W401",
                "Variable selector references unreachable node".to_string(),
                Some(current_node.to_string()),
                Some(field_path.to_string()),
            ));
        }
    }
}

fn validate_dify_template(
    template: &str,
    node_ids: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
    node_id: Option<String>,
    field_path: &str,
) {
    let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();
    for cap in re.captures_iter(template) {
        let expr = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        if !expr.contains('.') {
            diags.push(error(
                "E502",
                "Dify template selector malformed".to_string(),
                node_id.clone(),
                Some(field_path.to_string()),
            ));
            continue;
        }
        let root = expr.split('.').next().unwrap_or("");
        if !RESERVED_NAMESPACES.contains(&root) && !node_ids.contains(root) {
            diags.push(warn(
                "W502",
                "Dify template references unknown node".to_string(),
                node_id.clone(),
                Some(field_path.to_string()),
            ));
        }
    }
}

fn error(code: &str, message: String, node_id: Option<String>, field_path: Option<String>) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Error,
        code: code.to_string(),
        message,
        node_id,
        edge_id: None,
        field_path,
    }
}

fn warn(code: &str, message: String, node_id: Option<String>, field_path: Option<String>) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Warning,
        code: code.to_string(),
        message,
        node_id,
        edge_id: None,
        field_path,
    }
}
