//! Shared helper functions for workflow compilation and execution setup.
//!
//! These helpers operate on DSL schema types (`WorkflowSchema`) and runtime errors
//! (`WorkflowError`), so they must not live in the pure `domain` layer.
//!
//! Extracted from the former `scheduler.rs` during the architecture reorg.

use serde_json::Value;
use std::collections::HashMap;

use crate::domain::model::SegmentType;
use crate::dsl::schema::{StartVariable, WorkflowSchema};
use crate::error::WorkflowError;

/// Build an error context map for error-handler workflows.
pub fn build_error_context(
    error: &WorkflowError,
    schema: &WorkflowSchema,
    partial_outputs: &HashMap<String, Value>,
) -> HashMap<String, Value> {
    let (node_id, node_type) = extract_error_node_info(error, schema);
    let mut ctx = HashMap::new();
    ctx.insert(
        "sys.error_message".to_string(),
        Value::String(error.to_string()),
    );
    ctx.insert(
        "sys.error_node_id".to_string(),
        Value::String(node_id.unwrap_or_default()),
    );
    ctx.insert(
        "sys.error_node_type".to_string(),
        Value::String(node_type.unwrap_or_default()),
    );
    ctx.insert(
        "sys.error_type".to_string(),
        Value::String(error_type_name(error).to_string()),
    );
    ctx.insert(
        "sys.workflow_outputs".to_string(),
        Value::Object(partial_outputs.clone().into_iter().collect()),
    );
    ctx
}

pub(crate) fn error_type_name(error: &WorkflowError) -> &'static str {
    match error {
        WorkflowError::NodeExecutionError { .. } => "NodeExecutionError",
        WorkflowError::Timeout | WorkflowError::ExecutionTimeout => "Timeout",
        WorkflowError::MaxStepsExceeded(_) => "MaxStepsExceeded",
        WorkflowError::Aborted(_) => "Aborted",
        _ => "InternalError",
    }
}

/// Collect start variable types from the schema's start node.
pub fn collect_start_variable_types(schema: &WorkflowSchema) -> HashMap<String, SegmentType> {
    let mut map = HashMap::new();
    for node in &schema.nodes {
        if node.data.node_type != "start" {
            continue;
        }
        if let Some(vars_val) = node.data.extra.get("variables") {
            if let Ok(vars) = serde_json::from_value::<Vec<StartVariable>>(vars_val.clone()) {
                for var in vars {
                    if let Some(seg_type) = SegmentType::from_dsl_type(&var.var_type) {
                        map.insert(var.variable, seg_type);
                    }
                }
            }
        }
    }
    map
}

/// Collect conversation variable types from the schema.
pub fn collect_conversation_variable_types(
    schema: &WorkflowSchema,
) -> HashMap<String, SegmentType> {
    let mut map = HashMap::new();
    for var in &schema.conversation_variables {
        if let Some(seg_type) = SegmentType::from_dsl_type(&var.var_type) {
            map.insert(var.name.clone(), seg_type);
        }
    }
    map
}

/// Extract error node info (id, type) from a workflow error.
pub fn extract_error_node_info(
    error: &WorkflowError,
    schema: &WorkflowSchema,
) -> (Option<String>, Option<String>) {
    match error {
        WorkflowError::NodeExecutionError { node_id, .. } => {
            let node_type = schema
                .nodes
                .iter()
                .find(|n| n.id == *node_id)
                .map(|n| n.data.node_type.clone());
            (Some(node_id.clone()), node_type)
        }
        _ => (None, None),
    }
}
