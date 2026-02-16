//! Loop Node executor.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::domain::model::{LoopConditionConfig, LoopNodeConfig};
use crate::dsl::schema::{
    Condition, EdgeHandle, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::evaluator::condition::{evaluate_condition, ConditionResult};
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;
#[cfg(feature = "security")]
use crate::security::SecurityLevel;

use super::resolve_sub_graph_runner;

pub struct LoopNodeExecutor;

impl Default for LoopNodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopNodeExecutor {
    pub fn new() -> Self {
        Self
    }

    async fn evaluate_condition(
        &self,
        condition: &LoopConditionConfig,
        loop_vars: &HashMap<String, Value>,
    ) -> Result<ConditionResult, NodeError> {
        let selector = selector_from_value(&condition.variable_selector)
            .ok_or_else(|| NodeError::ConfigError("Invalid loop condition selector".to_string()))?;

        let mut pool = VariablePool::new();
        for (k, v) in loop_vars {
            pool.set(&Selector::new("loop", k.clone()), Segment::from_value(v));
        }

        let cond = Condition {
            variable_selector: selector,
            comparison_operator: condition.comparison_operator.clone(),
            value: condition.value.clone(),
        };

        Ok(evaluate_condition(&cond, &pool).await)
    }
}

#[async_trait]
impl NodeExecutor for LoopNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let config: LoopNodeConfig = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        let mut loop_vars = config.initial_vars.clone();
        let mut iteration_count = 0usize;
        let max_iterations = config.max_iterations.unwrap_or(100);
        let runner = resolve_sub_graph_runner(context);

        loop {
            if iteration_count >= max_iterations {
                return Err(NodeError::ExecutionError(format!(
                    "Max iterations {} exceeded",
                    max_iterations
                )));
            }

            match self
                .evaluate_condition(&config.condition, &loop_vars)
                .await?
            {
                ConditionResult::True => {}
                ConditionResult::False => break,
                ConditionResult::TypeMismatch(mismatch) => {
                    let message = format!(
                        "Condition type mismatch: expected {}, got {} for {:?}",
                        mismatch.expected_type, mismatch.actual_type, mismatch.operator
                    );

                    #[cfg(feature = "security")]
                    let is_strict = context
                        .security_policy()
                        .map(|policy| policy.level == SecurityLevel::Strict)
                        .or_else(|| {
                            context
                                .resource_group()
                                .map(|group| group.security_level == SecurityLevel::Strict)
                        })
                        .unwrap_or(false);

                    #[cfg(not(feature = "security"))]
                    let is_strict = false;

                    if is_strict {
                        return Err(NodeError::TypeError(message));
                    }

                    tracing::warn!("{}; terminating loop", message);
                    break;
                }
            }

            let mut scope_vars = HashMap::new();
            for (k, v) in &loop_vars {
                scope_vars.insert(format!("loop.{}", k), v.clone());
            }
            scope_vars.insert("_iteration".to_string(), serde_json::json!(iteration_count));

            let result = runner
                .run_sub_graph(&config.sub_graph, pool, scope_vars, context)
                .await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

            if let Some(obj) = result.as_object() {
                for (k, v) in obj {
                    loop_vars.insert(k.clone(), v.clone());
                }
            }

            iteration_count += 1;
        }

        let mut outputs = HashMap::new();
        outputs.insert(
            config.output_variable.clone(),
            Segment::from_value(&Value::Object(loop_vars.into_iter().collect())),
        );
        outputs.insert(
            "_iterations".to_string(),
            Segment::Integer(iteration_count as i64),
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

    fn make_pool() -> VariablePool {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([1, 2, 3])),
        );
        pool
    }

    #[tokio::test]
    async fn test_loop_basic() {
        let executor = LoopNodeExecutor::new();
        let context = RuntimeContext::default();

        let config = serde_json::json!({
            "condition": {
                "variable_selector": "loop.counter",
                "comparison_operator": "less_than",
                "value": 0
            },
            "max_iterations": 10,
            "initial_vars": {"counter": 0},
            "sub_graph": {
                "nodes": [
                    {"id": "start", "type": "start", "data": {}},
                    {"id": "end", "type": "end", "data": {
                        "outputs": [
                            {"variable": "counter", "value_selector": "loop.counter"}
                        ]
                    }}
                ],
                "edges": [
                    {"id": "e1", "source": "start", "target": "end"}
                ]
            },
            "output_variable": "loop_result"
        });

        let result = executor
            .execute("loop1", &config, &make_pool(), &context)
            .await
            .unwrap();
        assert!(result.outputs.ready().contains_key("loop_result"));
        assert_eq!(
            result.outputs.ready().get("_iterations"),
            Some(&Segment::Integer(0))
        );
    }

    #[tokio::test]
    async fn test_loop_missing_condition() {
        let executor = LoopNodeExecutor::new();
        let context = RuntimeContext::default();

        let config = serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "result"
        });

        let result = executor
            .execute("loop_no_cond", &config, &make_pool(), &context)
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_loop_config_serde() {
        let json = serde_json::json!({
            "condition": {
                "variable_selector": ["loop", "done"],
                "comparison_operator": "is",
                "value": "true"
            },
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "result"
        });
        let config: LoopNodeConfig = serde_json::from_value(json).unwrap();
        assert!(config.initial_vars.is_empty());
        assert!(config.max_iterations.is_none());
    }

    #[tokio::test]
    async fn test_loop_condition_evaluation_error() {
        let executor = LoopNodeExecutor::new();
        let context = RuntimeContext::default();

        let config = serde_json::json!({
            "condition": {
                "variable_selector": "nonexistent.var",
                "comparison_operator": "is",
                "value": true
            },
            "max_iterations": 5,
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "result"
        });

        let result = executor
            .execute("loop_cond_err", &config, &make_pool(), &context)
            .await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_sub_graph_runner() {
        let context = RuntimeContext::default();
        let runner = super::super::resolve_sub_graph_runner(&context);
        let _clone = runner.clone();
    }
}
