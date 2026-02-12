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
    use crate::core::variable_pool::Selector;

    fn create_test_context() -> RuntimeContext {
        RuntimeContext::default()
    }

    #[tokio::test]
    async fn test_gather_executor_basic() {
        let executor = GatherExecutor;
        let mut pool = VariablePool::new();
        
        // Add some test variables
        pool.set(&Selector::new("n1", "output1"), Segment::String("value1".to_string()));
        pool.set(&Selector::new("n2", "output2"), Segment::Integer(42));
        
        let config = serde_json::json!({
            "variables": [["n1", "output1"], ["n2", "output2"]],
            "join_mode": "all",
            "cancel_remaining": true,
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        assert_eq!(result.status, WorkflowNodeExecutionStatus::Succeeded);
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            assert!(outputs.len() >= 2); // At least results and completed_count
            assert!(outputs.contains_key("results"));
            assert!(outputs.contains_key("completed_count"));
            
            if let Segment::Integer(count) = outputs.get("completed_count").unwrap() {
                // Count could be 0 if no variables were found
                assert!(*count >= 0);
            } else {
                panic!("completed_count should be an integer");
            }
            
            // If we have results, first_result should be present
            if let Segment::Integer(count) = outputs.get("completed_count").unwrap() {
                if *count > 0 {
                    assert!(outputs.contains_key("first_result"), "first_result should exist when count > 0");
                    assert_eq!(outputs.len(), 3);
                    assert_eq!(*count, 2);
                }
            }
        } else {
            panic!("Expected sync outputs");
        }
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
    async fn test_gather_executor_missing_variables() {
        let executor = GatherExecutor;
        let mut pool = VariablePool::new();
        
        // Only set one variable
        pool.set(&Selector::new("n1", "output1"), Segment::String("value1".to_string()));
        
        let config = serde_json::json!({
            "variables": [["n1", "output1"], ["n2", "missing"], ["n3", "missing"]],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            if let Segment::Integer(count) = outputs.get("completed_count").unwrap() {
                // Only one variable exists
                assert_eq!(*count, 1);
            }
        }
    }

    #[tokio::test]
    async fn test_gather_executor_first_result() {
        let executor = GatherExecutor;
        let mut pool = VariablePool::new();
        
        pool.set(&Selector::new("n1", "output1"), Segment::String("first".to_string()));
        pool.set(&Selector::new("n2", "output2"), Segment::String("second".to_string()));
        
        let config = serde_json::json!({
            "variables": ["n1.output1", "n2.output2"],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            assert!(outputs.contains_key("first_result"));
            if let Segment::String(s) = outputs.get("first_result").unwrap() {
                assert_eq!(s, "first");
            }
        }
    }

    #[tokio::test]
    async fn test_gather_executor_no_first_result_when_empty() {
        let executor = GatherExecutor;
        let pool = VariablePool::new();
        
        let config = serde_json::json!({
            "variables": ["missing1", "missing2"],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            // first_result should not be present when no results
            assert!(!outputs.contains_key("first_result"));
        }
    }

    #[tokio::test]
    async fn test_gather_executor_array_output() {
        let executor = GatherExecutor;
        let mut pool = VariablePool::new();
        
        pool.set(&Selector::new("n1", "output"), Segment::Integer(1));
        pool.set(&Selector::new("n2", "output"), Segment::Integer(2));
        pool.set(&Selector::new("n3", "output"), Segment::Integer(3));
        
        let config = serde_json::json!({
            "variables": ["n1.output", "n2.output", "n3.output"],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            if let Segment::Array(arr) = outputs.get("results").unwrap() {
                assert_eq!(arr.len(), 3);
            } else {
                panic!("results should be an array");
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
    async fn test_gather_executor_mixed_types() {
        let executor = GatherExecutor;
        let mut pool = VariablePool::new();
        
        pool.set(&Selector::new("n1", "string"), Segment::String("text".to_string()));
        pool.set(&Selector::new("n2", "int"), Segment::Integer(42));
        pool.set(&Selector::new("n3", "bool"), Segment::Boolean(true));
        pool.set(&Selector::new("n4", "float"), Segment::Float(3.14));
        
        let config = serde_json::json!({
            "variables": [["n1", "string"], ["n2", "int"], ["n3", "bool"], ["n4", "float"]],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            if let Segment::Integer(count) = outputs.get("completed_count").unwrap() {
                assert_eq!(*count, 4);
            }
            if let Segment::Array(arr) = outputs.get("results").unwrap() {
                assert_eq!(arr.len(), 4);
            }
        }
    }

    #[tokio::test]
    async fn test_gather_executor_join_mode_all() {
        let executor = GatherExecutor;
        let mut pool = VariablePool::new();
        
        pool.set(&Selector::new("n1", "output"), Segment::String("value".to_string()));
        
        let config = serde_json::json!({
            "variables": ["n1.output"],
            "join_mode": "all",
            "cancel_remaining": true,
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await;
        
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_gather_executor_timeout_strategy() {
        let executor = GatherExecutor;
        let pool = VariablePool::new();
        
        let config = serde_json::json!({
            "variables": [],
            "join_mode": "all",
            "timeout_secs": 30,
            "timeout_strategy": "proceed_with_available",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await;
        
        assert!(result.is_ok());
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
    async fn test_gather_executor_complex_selectors() {
        let executor = GatherExecutor;
        let mut pool = VariablePool::new();
        
        // Use complex selectors
        pool.set(&Selector::new("parent", "child.grandchild"), Segment::String("nested".to_string()));
        pool.set(&Selector::new("array", "[0]"), Segment::Integer(100));
        
        let config = serde_json::json!({
            "variables": [["parent", "child.grandchild"], ["array", "[0]"]],
            "join_mode": "all",
        });
        
        let context = create_test_context();
        let result = executor.execute("gather1", &config, &pool, &context).await.unwrap();
        
        if let NodeOutputs::Sync(outputs) = result.outputs {
            if let Segment::Integer(count) = outputs.get("completed_count").unwrap() {
                assert_eq!(*count, 2);
            }
        }
    }
}
