//! Variable Aggregator node executors.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::dsl::schema::{EdgeHandle, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;

/// Returns the first non-null variable from a list of selectors.
pub struct VariableAggregatorExecutor;

#[async_trait]
impl NodeExecutor for VariableAggregatorExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let selectors: Vec<Selector> = config
            .get("variables")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let mut result_val = Segment::None;
        for selector in &selectors {
            let val = variable_pool.get_resolved(selector).await;
            if !val.is_none() {
                result_val = val;
                break;
            }
        }

        let mut outputs = HashMap::new();
        outputs.insert("output".to_string(), result_val);

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

/// Legacy variable-assigner (same behavior as variable-aggregator).
pub struct LegacyVariableAggregatorExecutor;

#[async_trait]
impl NodeExecutor for LegacyVariableAggregatorExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        VariableAggregatorExecutor
            .execute(node_id, config, variable_pool, context)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_variable_aggregator() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n2", "out"), Segment::String("found".into()));

        let config = serde_json::json!({
            "variables": [["n1", "out"], ["n2", "out"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("agg1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::String("found".into()))
        );
    }

    #[tokio::test]
    async fn test_variable_aggregator_all_null() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "variables": [["n1", "out"], ["n2", "out"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("agg1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&Segment::None));
    }

    #[tokio::test]
    async fn test_variable_aggregator_first_non_null() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("a", "x"), Segment::None);
        pool.set(&Selector::new("b", "y"), Segment::String("found".into()));

        let config = serde_json::json!({
            "variables": [["a", "x"], ["b", "y"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("va1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::String("found".into()))
        );
    }

    #[tokio::test]
    async fn test_variable_aggregator_all_missing() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "variables": [["missing1", "x"], ["missing2", "y"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("va2", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&Segment::None));
    }

    #[tokio::test]
    async fn test_variable_aggregator_empty_selectors() {
        let pool = VariablePool::new();
        let config = serde_json::json!({ "variables": [] });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("va3", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&Segment::None));
    }

    #[tokio::test]
    async fn test_legacy_variable_aggregator() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "v"), Segment::Integer(99));
        let config = serde_json::json!({ "variables": [["n1", "v"]] });

        let executor = LegacyVariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("lva", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::Integer(99))
        );
    }

    #[tokio::test]
    async fn test_variable_aggregator_mixed_types() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("n1", "text"),
            Segment::String("hello".into()),
        );
        pool.set(&Selector::new("n2", "num"), Segment::Integer(42));
        let config = serde_json::json!({
            "groups": [{
                "output_type": "array-any",
                "variables": [
                    {"value_selector": ["n1", "text"]},
                    {"value_selector": ["n2", "num"]}
                ]
            }]
        });
        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("agg_mix", &config, &pool, &context)
            .await
            .unwrap();
        let output = result.outputs.ready().get("output");
        assert!(output.is_some());
    }
}
