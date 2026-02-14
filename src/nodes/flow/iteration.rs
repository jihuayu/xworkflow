//! Iteration Node executor.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::domain::model::{IterationErrorMode, IterationNodeConfig};
use crate::dsl::schema::{EdgeHandle, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;

use super::resolve_sub_graph_runner;

/// Executor for the Iteration node. Runs a sub-graph once per list element.
pub struct IterationNodeExecutor;

impl IterationNodeExecutor {
    /// Create a new `IterationNodeExecutor`.
    pub fn new() -> Self {
        Self
    }

    fn resolve_input_selector(
        config: &IterationNodeConfig,
    ) -> Result<crate::domain::model::Selector, NodeError> {
        let selector_val = config
            .input_selector
            .as_ref()
            .or(config.iterator_selector.as_ref())
            .ok_or_else(|| NodeError::ConfigError("input_selector is required".to_string()))?;

        selector_from_value(selector_val)
            .ok_or_else(|| NodeError::ConfigError("Invalid input_selector".to_string()))
    }

    fn resolve_parallelism(config: &IterationNodeConfig) -> usize {
        config
            .parallelism
            .or_else(|| config.parallel_nums.map(|v| v as usize))
            .unwrap_or(10)
    }

    fn resolve_parallel(config: &IterationNodeConfig) -> bool {
        if config.parallel {
            true
        } else {
            config.is_parallel
        }
    }

    fn resolve_max_iterations(config: &IterationNodeConfig) -> usize {
        config.max_iterations.unwrap_or(1000)
    }
}

impl Default for IterationNodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeExecutor for IterationNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let config: IterationNodeConfig = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        let input_selector = Self::resolve_input_selector(&config)?;
        let input_value = variable_pool.get(&input_selector).snapshot_to_value();
        let items = input_value
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let max_iterations = Self::resolve_max_iterations(&config);
        if items.len() > max_iterations {
            return Err(NodeError::ExecutionError(format!(
                "Array size {} exceeds max iterations {}",
                items.len(),
                max_iterations
            )));
        }

        let results = if Self::resolve_parallel(&config) {
            self.execute_parallel(&config, items, variable_pool, context)
                .await?
        } else {
            self.execute_sequential(&config, items, variable_pool, context)
                .await?
        };

        let mut outputs = HashMap::new();
        outputs.insert(
            config.output_variable.clone(),
            Segment::from_value(&Value::Array(results)),
        );

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

impl IterationNodeExecutor {
    async fn execute_sequential(
        &self,
        config: &IterationNodeConfig,
        items: &[Value],
        pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<Vec<Value>, NodeError> {
        let mut results = Vec::with_capacity(items.len());

        let runner = resolve_sub_graph_runner(context);

        for (index, item) in items.iter().enumerate() {
            let mut scope_vars = HashMap::new();
            scope_vars.insert("item".to_string(), item.clone());
            scope_vars.insert("index".to_string(), serde_json::json!(index));

            let result = runner
                .run_sub_graph(&config.sub_graph, pool, scope_vars, context)
                .await;

            match result {
                Ok(value) => results.push(value),
                Err(e) => match config.error_handle_mode {
                    IterationErrorMode::Terminated => {
                        return Err(NodeError::ExecutionError(e.to_string()))
                    }
                    IterationErrorMode::RemoveAbnormal => {
                        continue;
                    }
                    IterationErrorMode::ContinueOnError => {
                        results.push(Value::Null);
                    }
                },
            }
        }

        Ok(results)
    }

    async fn execute_parallel(
        &self,
        config: &IterationNodeConfig,
        items: &[Value],
        pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<Vec<Value>, NodeError> {
        let semaphore = Arc::new(Semaphore::new(Self::resolve_parallelism(config)));
        let runner = resolve_sub_graph_runner(context);
        let mut tasks = Vec::with_capacity(items.len());

        for (index, item) in items.iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let runner = runner.clone();
            let sub_graph = config.sub_graph.clone();
            let pool_clone = pool.clone();
            let item_clone = item.clone();
            let context_clone = context.clone();

            let task = tokio::spawn(async move {
                let mut scope_vars = HashMap::new();
                scope_vars.insert("item".to_string(), item_clone);
                scope_vars.insert("index".to_string(), serde_json::json!(index));

                let result = runner
                    .run_sub_graph(&sub_graph, &pool_clone, scope_vars, &context_clone)
                    .await;
                drop(permit);
                (index, result)
            });

            tasks.push(task);
        }

        let mut results = vec![Value::Null; items.len()];
        let mut error_indices: Vec<usize> = Vec::new();
        for task in tasks {
            let (index, result) = task
                .await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?;
            match result {
                Ok(value) => {
                    results[index] = value;
                }
                Err(e) => match config.error_handle_mode {
                    IterationErrorMode::Terminated => {
                        return Err(NodeError::ExecutionError(e.to_string()))
                    }
                    IterationErrorMode::RemoveAbnormal => {
                        error_indices.push(index);
                    }
                    IterationErrorMode::ContinueOnError => {
                        results[index] = Value::Null;
                    }
                },
            }
        }

        if matches!(config.error_handle_mode, IterationErrorMode::RemoveAbnormal) {
            error_indices.sort_unstable_by(|a, b| b.cmp(a));
            for idx in error_indices {
                results.remove(idx);
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::Selector;
    use crate::domain::model::SubGraphDefinition;

    fn make_pool() -> VariablePool {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([1, 2, 3])),
        );
        pool
    }

    #[tokio::test]
    #[cfg(all(feature = "builtin-core-nodes", feature = "builtin-subgraph-nodes"))]
    async fn test_iteration_sequential() {
        let executor = IterationNodeExecutor::new();
        let context = RuntimeContext::default();

        let config = serde_json::json!({
            "input_selector": "input.items",
            "parallel": false,
            "sub_graph": {
                "nodes": [
                    {"id": "start", "type": "start", "data": {}},
                    {"id": "end", "type": "end", "data": {
                        "outputs": [{"variable": "value", "value_selector": "item"}]
                    }}
                ],
                "edges": [
                    {"id": "e1", "source": "start", "target": "end"}
                ]
            },
            "output_variable": "results"
        });

        let result = executor
            .execute("iter1", &config, &make_pool(), &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("results"),
            Some(&Segment::from_value(&serde_json::json!([
                {"value": 1},
                {"value": 2},
                {"value": 3}
            ])))
        );
    }

    #[test]
    fn test_iteration_resolve_parallelism_default() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out"
        }))
        .unwrap();
        assert_eq!(IterationNodeExecutor::resolve_parallelism(&config), 10);
    }

    #[test]
    fn test_iteration_resolve_parallelism_custom() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out",
            "parallelism": 4
        }))
        .unwrap();
        assert_eq!(IterationNodeExecutor::resolve_parallelism(&config), 4);
    }

    #[test]
    fn test_iteration_resolve_parallel_compat() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out",
            "is_parallel": true
        }))
        .unwrap();
        assert!(IterationNodeExecutor::resolve_parallel(&config));
    }

    #[test]
    fn test_iteration_max_iterations_default() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out"
        }))
        .unwrap();
        assert_eq!(IterationNodeExecutor::resolve_max_iterations(&config), 1000);
    }

    #[tokio::test]
    async fn test_iteration_exceeds_max() {
        let executor = IterationNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([1, 2, 3, 4, 5])),
        );
        let config = serde_json::json!({
            "input_selector": "input.items",
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out",
            "max_iterations": 2
        });
        let result = executor.execute("iter_max", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_iteration_input_not_array() {
        let executor = IterationNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::String("not array".into()),
        );
        let config = serde_json::json!({
            "input_selector": "input.items",
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out"
        });
        let result = executor.execute("iter_err", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_input_selector_both_none() {
        let config = IterationNodeConfig {
            input_selector: None,
            parallel: false,
            parallelism: None,
            sub_graph: SubGraphDefinition {
                nodes: vec![],
                edges: vec![],
            },
            output_variable: "out".into(),
            max_iterations: None,
            error_handle_mode: IterationErrorMode::Terminated,
            iterator_selector: None,
            is_parallel: false,
            parallel_nums: None,
        };
        let result = IterationNodeExecutor::resolve_input_selector(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_input_selector_invalid_value() {
        let config = IterationNodeConfig {
            input_selector: Some(serde_json::json!(42)),
            parallel: false,
            parallelism: None,
            sub_graph: SubGraphDefinition {
                nodes: vec![],
                edges: vec![],
            },
            output_variable: "out".into(),
            max_iterations: None,
            error_handle_mode: IterationErrorMode::Terminated,
            iterator_selector: None,
            is_parallel: false,
            parallel_nums: None,
        };
        let result = IterationNodeExecutor::resolve_input_selector(&config);
        assert!(result.is_err());
    }

    #[test]
    fn test_default_iteration_error_mode() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out"
        }))
        .unwrap();
        assert!(matches!(
            config.error_handle_mode,
            IterationErrorMode::Terminated
        ));
    }

    #[test]
    fn test_iteration_config_serde_compat() {
        let json = serde_json::json!({
            "iterator_selector": ["node", "items"],
            "is_parallel": true,
            "parallel_nums": 4,
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out"
        });
        let config: IterationNodeConfig = serde_json::from_value(json).unwrap();
        assert!(config.is_parallel);
        assert_eq!(config.parallel_nums, Some(4));
        assert!(config.input_selector.is_none());
        assert!(config.iterator_selector.is_some());
    }

    #[tokio::test]
    async fn test_iteration_error_mode_continue() {
        let executor = IterationNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([1, 2, 3])),
        );

        let config = serde_json::json!({
            "input_selector": "input.items",
            "error_handle_mode": "continue_on_error",
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "results"
        });

        let result = executor
            .execute("iter_continue", &config, &pool, &context)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_iteration_parallel_execution() {
        let executor = IterationNodeExecutor::new();
        let context = RuntimeContext::default();

        let config = serde_json::json!({
            "input_selector": "input.items",
            "parallel": true,
            "parallelism": 2,
            "sub_graph": {
                "nodes": [
                    {"id": "start", "type": "start", "data": {}},
                    {"id": "end", "type": "end", "data": {
                        "outputs": [{"variable": "value", "value_selector": "item"}]
                    }}
                ],
                "edges": [
                    {"id": "e1", "source": "start", "target": "end"}
                ]
            },
            "output_variable": "results"
        });

        let result = executor
            .execute("iter_parallel", &config, &make_pool(), &context)
            .await;
        assert!(result.is_ok());
    }
}
