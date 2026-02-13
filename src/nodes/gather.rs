use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, SegmentArray, VariablePool};
use crate::dsl::schema::{EdgeHandle, GatherNodeData, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;

pub struct GatherExecutor;

#[async_trait]
impl NodeExecutor for GatherExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let gather = serde_json::from_value::<GatherNodeData>(config.clone()).unwrap_or(GatherNodeData {
            variables: Vec::new(),
            join_mode: crate::dsl::schema::JoinMode::All,
            cancel_remaining: true,
            timeout_secs: None,
            timeout_strategy: crate::dsl::schema::TimeoutStrategy::ProceedWithAvailable,
        });

        let mut results: Vec<Segment> = Vec::new();
        for selector in &gather.variables {
            let value = variable_pool.get_resolved(selector).await;
            if !value.is_none() {
                results.push(value);
            }
        }

        let completed_count = results.len() as i64;
        let first_result = results.first().cloned();

        let mut outputs = HashMap::new();
        outputs.insert(
            "results".to_string(),
            Segment::Array(Arc::new(SegmentArray::new(results))),
        );
        outputs.insert("completed_count".to_string(), Segment::Integer(completed_count));
        if let Some(first) = first_result {
            outputs.insert("first_result".to_string(), first);
        }

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

    fn create_test_context() -> RuntimeContext {
        RuntimeContext::default()
    }

    #[tokio::test]
    async fn test_gather_executor_empty_variables() {
        let executor = GatherExecutor;
        let pool = VariablePool::new();
        
        let config = serde_json::json!({
            "variables": [],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        assert_eq!(result.status, WorkflowNodeExecutionStatus::Succeeded);
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            if let Segment::Integer(count) = outputs.get("completed_count").unwrap() {
                assert_eq!(*count, 0);
            }
            if let Segment::Array(arr) = outputs.get("results").unwrap() {
                assert_eq!(arr.len(), 0);
            }
        }
    }

    #[tokio::test]
    async fn test_gather_executor_default_config() {
        let executor = GatherExecutor;
        let pool = VariablePool::new();
        
        // Use invalid config to trigger default
        let config = serde_json::json!({});
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        assert_eq!(result.status, WorkflowNodeExecutionStatus::Succeeded);
    }

    #[tokio::test]
    async fn test_gather_executor_edge_handle() {
        let executor = GatherExecutor;
        let pool = VariablePool::new();
        
        let config = serde_json::json!({
            "variables": [],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        assert_eq!(result.edge_source_handle, EdgeHandle::Default);
    }

    #[tokio::test]
    async fn test_gather_executor_output_structure() {
        let executor = GatherExecutor;
        let pool = VariablePool::new();
        
        let config = serde_json::json!({
            "variables": [],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            // Should always have these two keys
            assert!(outputs.contains_key("results"));
            assert!(outputs.contains_key("completed_count"));
            
            // results should be an array
            assert!(matches!(outputs.get("results"), Some(Segment::Array(_))));
            
            // completed_count should be an integer
            assert!(matches!(outputs.get("completed_count"), Some(Segment::Integer(_))));
        } else {
            panic!("Expected sync outputs");
        }
    }
}

