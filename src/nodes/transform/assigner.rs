//! Variable Assigner node executor.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, Selector, VariablePool, SCOPE_NODE_ID};
use crate::dsl::schema::{
    EdgeHandle, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus, WriteMode,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;

/// Assigns a variable value, supporting overwrite/append/clear modes.
pub struct VariableAssignerExecutor;

#[async_trait]
impl NodeExecutor for VariableAssignerExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        // Parse config
        let assigned_sel: Selector = config
            .get("assigned_variable_selector")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_else(|| Selector::new(SCOPE_NODE_ID, "output"));

        let write_mode: WriteMode = config
            .get("write_mode")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(WriteMode::Overwrite);

        // Get source value
        let source_value = if let Some(input_sel) = config.get("input_variable_selector") {
            if let Some(sel) = selector_from_value(input_sel) {
                variable_pool.get_resolved(&sel).await
            } else {
                Segment::None
            }
        } else if let Some(val) = config.get("value") {
            Segment::from_value(val)
        } else {
            Segment::None
        };

        // Note: actual write to pool is done by the dispatcher after execution
        let mut outputs = HashMap::new();
        outputs.insert("output".to_string(), source_value.clone());
        outputs.insert(
            "write_mode".to_string(),
            Segment::from_value(&serde_json::to_value(&write_mode).unwrap_or(Value::Null)),
        );
        outputs.insert(
            "assigned_variable_selector".to_string(),
            Segment::from_value(&serde_json::to_value(&assigned_sel).unwrap_or(Value::Null)),
        );

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_variable_assigner() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("src", "val"), Segment::String("data".into()));

        let config = serde_json::json!({
            "assigned_variable_selector": ["target", "result"],
            "input_variable_selector": ["src", "val"],
            "write_mode": "overwrite"
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("va1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::String("data".into()))
        );
    }

    #[tokio::test]
    async fn test_variable_assigner_overwrite() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("src", "val"),
            Segment::String("hello".into()),
        );
        let config = serde_json::json!({
            "assigned_variable_selector": ["tgt", "out"],
            "write_mode": "overwrite",
            "input_variable_selector": ["src", "val"]
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("assign1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::String("hello".into()))
        );
    }

    #[tokio::test]
    async fn test_variable_assigner_append() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "assigned_variable_selector": ["tgt", "arr"],
            "write_mode": "append",
            "value": "new_item"
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("assign2", &config, &pool, &context)
            .await
            .unwrap();
        let wm = result.outputs.ready().get("write_mode").unwrap();
        assert_eq!(wm, &Value::String("append".into()));
    }

    #[tokio::test]
    async fn test_variable_assigner_clear() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "assigned_variable_selector": ["tgt", "x"],
            "write_mode": "clear"
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("assign3", &config, &pool, &context)
            .await
            .unwrap();
        let wm = result.outputs.ready().get("write_mode").unwrap();
        assert_eq!(wm, &Value::String("clear".into()));
    }

    #[tokio::test]
    async fn test_variable_assigner_default_mode() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "value": 42
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("assign4", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::Integer(42))
        );
    }
}
