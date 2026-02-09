use std::collections::HashSet;

use crate::dsl::schema::{EdgeSchema, WorkflowSchema, SUPPORTED_DSL_VERSIONS};

use super::known_types::is_known_node_type;
use super::types::{Diagnostic, DiagnosticLevel};

pub fn validate(schema: &WorkflowSchema) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    if !SUPPORTED_DSL_VERSIONS.contains(&schema.version.as_str()) {
        diags.push(error(
            "E002",
            format!(
                "Unsupported DSL version: {}, supported versions: {}",
                schema.version,
                SUPPORTED_DSL_VERSIONS.join(", ")
            ),
            None,
            None,
            None,
        ));
    }

    if schema.nodes.is_empty() {
        diags.push(error("E003", "No nodes defined".to_string(), None, None, None));
        return diags;
    }

    let mut ids = HashSet::new();
    let mut duplicates = HashSet::new();

    for node in &schema.nodes {
        if node.id.trim().is_empty() {
            diags.push(error(
                "E008",
                "Node id is empty".to_string(),
                None,
                None,
                Some("id".to_string()),
            ));
        }
        if !ids.insert(node.id.clone()) {
            duplicates.insert(node.id.clone());
        }
        if !is_known_node_type(&node.data.node_type) {
            diags.push(error(
                "E009",
                format!("Unknown node type: {}", node.data.node_type),
                Some(node.id.clone()),
                None,
                Some("type".to_string()),
            ));
        }
        if node.data.title.trim().is_empty() {
            diags.push(warn(
                "W001",
                "Node title is empty".to_string(),
                Some(node.id.clone()),
                None,
                Some("title".to_string()),
            ));
        }
    }

    for dup in duplicates {
        diags.push(error(
            "E007",
            format!("Duplicate node id: {}", dup),
            Some(dup),
            None,
            None,
        ));
    }

    let start_nodes: Vec<_> = schema
        .nodes
        .iter()
        .filter(|n| n.data.node_type == "start")
        .collect();
    if start_nodes.is_empty() {
        diags.push(error(
            "E004",
            "No start node".to_string(),
            None,
            None,
            None,
        ));
    } else if start_nodes.len() > 1 {
        diags.push(error(
            "E005",
            "Multiple start nodes".to_string(),
            None,
            None,
            None,
        ));
    }

    let has_end = schema
        .nodes
        .iter()
        .any(|n| n.data.node_type == "end" || n.data.node_type == "answer");
    if !has_end {
        diags.push(error(
            "E006",
            "No end or answer node".to_string(),
            None,
            None,
            None,
        ));
    }

    let node_ids: HashSet<String> = schema.nodes.iter().map(|n| n.id.clone()).collect();
    let mut edge_keys = HashSet::new();
    for edge in &schema.edges {
        if !node_ids.contains(&edge.source) {
            diags.push(error(
                "E010",
                format!("Edge source not found: {}", edge.source),
                None,
                Some(edge.id.clone()),
                Some("source".to_string()),
            ));
        }
        if !node_ids.contains(&edge.target) {
            diags.push(error(
                "E011",
                format!("Edge target not found: {}", edge.target),
                None,
                Some(edge.id.clone()),
                Some("target".to_string()),
            ));
        }
        if edge.source == edge.target {
            diags.push(error(
                "E012",
                "Edge has same source and target".to_string(),
                None,
                Some(edge.id.clone()),
                None,
            ));
        }

        let key = edge_key(edge);
        if !edge_keys.insert(key) {
            diags.push(error(
                "E013",
                "Duplicate edge".to_string(),
                None,
                Some(edge.id.clone()),
                None,
            ));
        }
    }

    diags
}

fn edge_key(edge: &EdgeSchema) -> (String, String, String) {
    (
        edge.source.clone(),
        edge.target.clone(),
        edge.source_handle.clone().unwrap_or_else(|| "".to_string()),
    )
}

fn error(
    code: &str,
    message: String,
    node_id: Option<String>,
    edge_id: Option<String>,
    field_path: Option<String>,
) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Error,
        code: code.to_string(),
        message,
        node_id,
        edge_id,
        field_path,
    }
}

fn warn(
    code: &str,
    message: String,
    node_id: Option<String>,
    edge_id: Option<String>,
    field_path: Option<String>,
) -> Diagnostic {
    Diagnostic {
        level: DiagnosticLevel::Warning,
        code: code.to_string(),
        message,
        node_id,
        edge_id,
        field_path,
    }
}
