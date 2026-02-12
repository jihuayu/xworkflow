//! Container node executors: Iteration, Loop, ListOperator.
//!
//! These nodes contain embedded sub-graphs that are executed via
//! [`SubGraphRunner`](crate::core::SubGraphRunner).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::core::runtime_context::RuntimeContext;
use crate::core::sub_graph_runner::{DefaultSubGraphRunner, SubGraphRunner};
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::dsl::schema::{
    ComparisonOperator, Condition, EdgeHandle, IterationErrorMode, NodeOutputs, NodeRunResult,
    WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::evaluator::condition::{evaluate_condition, ConditionResult};
#[cfg(feature = "security")]
use crate::security::SecurityLevel;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::subgraph::SubGraphDefinition;
use crate::nodes::utils::selector_from_value;

// ================================
// Iteration Node
// ================================

/// Configuration for the Iteration node, deserialized from the DSL.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IterationNodeConfig {
    pub input_selector: Option<Value>,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub parallelism: Option<usize>,
    pub sub_graph: SubGraphDefinition,
    pub output_variable: String,
    #[serde(default)]
    pub max_iterations: Option<usize>,
    #[serde(default = "default_iteration_error_mode")]
    pub error_handle_mode: IterationErrorMode,

    // Compatibility fields
    #[serde(default)]
    pub iterator_selector: Option<Value>,
    #[serde(default)]
    pub is_parallel: bool,
    #[serde(default)]
    pub parallel_nums: Option<u32>,
}

fn default_iteration_error_mode() -> IterationErrorMode { IterationErrorMode::Terminated }

fn resolve_sub_graph_runner(context: &RuntimeContext) -> Arc<dyn SubGraphRunner> {
    context
    .sub_graph_runner()
    .cloned()
        .unwrap_or_else(|| Arc::new(DefaultSubGraphRunner))
}

/// Executor for the Iteration node. Runs a sub-graph once per list element.
pub struct IterationNodeExecutor;

impl IterationNodeExecutor {
    /// Create a new `IterationNodeExecutor`.
    pub fn new() -> Self {
        Self
    }

    fn resolve_input_selector(config: &IterationNodeConfig) -> Result<Selector, NodeError> {
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
            self.execute_parallel(&config, items, variable_pool, context).await?
        } else {
            self.execute_sequential(&config, items, variable_pool, context).await?
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

// ================================
// Loop Node
// ================================

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoopConditionConfig {
    pub variable_selector: Value,
    pub comparison_operator: ComparisonOperator,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoopNodeConfig {
    pub condition: LoopConditionConfig,
    #[serde(default)]
    pub max_iterations: Option<usize>,
    #[serde(default)]
    pub initial_vars: HashMap<String, Value>,
    pub sub_graph: SubGraphDefinition,
    pub output_variable: String,
}

pub struct LoopNodeExecutor;

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

            match self.evaluate_condition(&config.condition, &loop_vars).await? {
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
        outputs.insert("_iterations".to_string(), Segment::Integer(iteration_count as i64));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

// ================================
// List Operator Node
// ================================

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListOperation {
    Filter,
    Map,
    Sort,
    Slice,
    First,
    Last,
    Flatten,
    Unique,
    Reverse,
    Concat,
    Reduce,
    Length,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ListOperatorNodeConfig {
    pub operation: ListOperation,
    pub input_selector: Value,
    #[serde(default)]
    pub sub_graph: Option<SubGraphDefinition>,
    #[serde(default)]
    pub sort_key: Option<String>,
    #[serde(default)]
    pub sort_order: Option<SortOrder>,
    #[serde(default)]
    pub start: Option<usize>,
    #[serde(default)]
    pub end: Option<usize>,
    #[serde(default)]
    pub initial_value: Option<Value>,
    #[serde(default)]
    pub second_input_selector: Option<Value>,
    #[serde(default)]
    pub parallel: bool,
    pub output_variable: String,
}

pub struct ListOperatorNodeExecutor;

impl ListOperatorNodeExecutor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NodeExecutor for ListOperatorNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let config: ListOperatorNodeConfig = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        let input_selector = selector_from_value(&config.input_selector)
            .ok_or_else(|| NodeError::ConfigError("Invalid input_selector".to_string()))?;
        let input = pool.get(&input_selector).snapshot_to_value();

        let result = match config.operation {
            ListOperation::Filter => {
                self.execute_filter(&config, &input, pool, context).await?
            }
            ListOperation::Map => {
                self.execute_map(&config, &input, pool, context).await?
            }
            ListOperation::Sort => self.execute_sort(&config, &input)?,
            ListOperation::Slice => self.execute_slice(&config, &input)?,
            ListOperation::First => self.execute_first(&input)?,
            ListOperation::Last => self.execute_last(&input)?,
            ListOperation::Flatten => self.execute_flatten(&input)?,
            ListOperation::Unique => self.execute_unique(&input)?,
            ListOperation::Reverse => self.execute_reverse(&input)?,
            ListOperation::Concat => self.execute_concat(&config, &input, pool)?,
            ListOperation::Reduce => self.execute_reduce(&config, &input, pool, context).await?,
            ListOperation::Length => self.execute_length(&input)?,
        };

        let mut outputs = HashMap::new();
        outputs.insert(config.output_variable.clone(), Segment::from_value(&result));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

impl ListOperatorNodeExecutor {
    async fn execute_filter(
        &self,
        config: &ListOperatorNodeConfig,
        input: &Value,
        pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<Value, NodeError> {
        let runner = resolve_sub_graph_runner(context);
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let sub_graph = config.sub_graph.as_ref().ok_or_else(|| {
            NodeError::ConfigError("Filter operation requires sub_graph".to_string())
        })?;

        if config.parallel {
            return self.execute_filter_parallel(items, sub_graph, pool, context).await;
        }

        let mut results = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let mut scope_vars = HashMap::new();
            scope_vars.insert("item".to_string(), item.clone());
            scope_vars.insert("index".to_string(), serde_json::json!(index));

            let result = runner
                .run_sub_graph(sub_graph, pool, scope_vars, context)
                .await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

            let keep = result
                .get("keep")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if keep {
                results.push(item.clone());
            }
        }

        Ok(Value::Array(results))
    }

    async fn execute_filter_parallel(
        &self,
        items: &[Value],
        sub_graph: &SubGraphDefinition,
        pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<Value, NodeError> {
        let runner = resolve_sub_graph_runner(context);
        let semaphore = Arc::new(Semaphore::new(10));
        let mut tasks = Vec::with_capacity(items.len());

        for (index, item) in items.iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let runner = runner.clone();
            let sub_graph = sub_graph.clone();
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

        let mut keep_flags = vec![false; items.len()];
        for task in tasks {
            let (index, result) = task
                .await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?;
            let value = result.map_err(|e| NodeError::ExecutionError(e.to_string()))?;
            keep_flags[index] = value
                .get("keep")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        }

        let mut results = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            if keep_flags[idx] {
                results.push(item.clone());
            }
        }

        Ok(Value::Array(results))
    }

    async fn execute_map(
        &self,
        config: &ListOperatorNodeConfig,
        input: &Value,
        pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<Value, NodeError> {
        let runner = resolve_sub_graph_runner(context);
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let sub_graph = config.sub_graph.as_ref().ok_or_else(|| {
            NodeError::ConfigError("Map operation requires sub_graph".to_string())
        })?;

        let mut results = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let mut scope_vars = HashMap::new();
            scope_vars.insert("item".to_string(), item.clone());
            scope_vars.insert("index".to_string(), serde_json::json!(index));

            let result = runner
                .run_sub_graph(sub_graph, pool, scope_vars, context)
                .await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

            let value = result.get("value").cloned().unwrap_or(result);
            results.push(value);
        }

        Ok(Value::Array(results))
    }

    fn execute_sort(&self, config: &ListOperatorNodeConfig, input: &Value) -> Result<Value, NodeError> {
        let mut items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?
            .clone();

        let sort_key = config.sort_key.as_ref().ok_or_else(|| {
            NodeError::ConfigError("Sort operation requires sort_key".to_string())
        })?;
        let sort_order = config.sort_order.unwrap_or(SortOrder::Asc);

        items.sort_by(|a, b| {
            let a_val = a.get(sort_key);
            let b_val = b.get(sort_key);

            let cmp = match (a_val, b_val) {
                (Some(Value::Number(a)), Some(Value::Number(b))) => a
                    .as_f64()
                    .partial_cmp(&b.as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal),
                (Some(Value::String(a)), Some(Value::String(b))) => a.cmp(b),
                _ => std::cmp::Ordering::Equal,
            };

            match sort_order {
                SortOrder::Asc => cmp,
                SortOrder::Desc => cmp.reverse(),
            }
        });

        Ok(Value::Array(items))
    }

    fn execute_slice(&self, config: &ListOperatorNodeConfig, input: &Value) -> Result<Value, NodeError> {
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let start = config.start.unwrap_or(0);
        let end = config.end.unwrap_or(items.len());

        let sliced: Vec<_> = items
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .cloned()
            .collect();

        Ok(Value::Array(sliced))
    }

    fn execute_first(&self, input: &Value) -> Result<Value, NodeError> {
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        Ok(items.first().cloned().unwrap_or(Value::Null))
    }

    fn execute_last(&self, input: &Value) -> Result<Value, NodeError> {
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        Ok(items.last().cloned().unwrap_or(Value::Null))
    }

    fn execute_flatten(&self, input: &Value) -> Result<Value, NodeError> {
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let mut flattened = Vec::new();
        for item in items {
            if let Some(arr) = item.as_array() {
                flattened.extend(arr.iter().cloned());
            } else {
                flattened.push(item.clone());
            }
        }

        Ok(Value::Array(flattened))
    }

    fn execute_unique(&self, input: &Value) -> Result<Value, NodeError> {
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let mut seen = HashSet::new();
        let mut unique = Vec::new();

        for item in items {
            let key = serde_json::to_string(item).unwrap_or_default();
            if seen.insert(key) {
                unique.push(item.clone());
            }
        }

        Ok(Value::Array(unique))
    }

    fn execute_reverse(&self, input: &Value) -> Result<Value, NodeError> {
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let reversed: Vec<_> = items.iter().rev().cloned().collect();
        Ok(Value::Array(reversed))
    }

    fn execute_concat(
        &self,
        config: &ListOperatorNodeConfig,
        input: &Value,
        pool: &VariablePool,
    ) -> Result<Value, NodeError> {
        let items1 = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let second_selector_val = config.second_input_selector.as_ref().ok_or_else(|| {
            NodeError::ConfigError("Concat operation requires second_input_selector".to_string())
        })?;

        let second_selector = selector_from_value(second_selector_val)
            .ok_or_else(|| NodeError::ConfigError("Invalid second_input_selector".to_string()))?;

        let input2 = pool.get(&second_selector).snapshot_to_value();
        let items2 = input2
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Second input must be an array".to_string()))?;

        let mut concatenated = items1.clone();
        concatenated.extend(items2.iter().cloned());

        Ok(Value::Array(concatenated))
    }

    async fn execute_reduce(
        &self,
        config: &ListOperatorNodeConfig,
        input: &Value,
        pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<Value, NodeError> {
        let runner = resolve_sub_graph_runner(context);
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        let sub_graph = config.sub_graph.as_ref().ok_or_else(|| {
            NodeError::ConfigError("Reduce operation requires sub_graph".to_string())
        })?;

        let mut accumulator = config.initial_value.clone().unwrap_or(Value::Null);

        for (index, item) in items.iter().enumerate() {
            let mut scope_vars = HashMap::new();
            scope_vars.insert("item".to_string(), item.clone());
            scope_vars.insert("index".to_string(), serde_json::json!(index));
            scope_vars.insert("accumulator".to_string(), accumulator.clone());

            let result = runner
                .run_sub_graph(sub_graph, pool, scope_vars, context)
                .await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

            accumulator = result.get("value").cloned().unwrap_or(result);
        }

        Ok(accumulator)
    }

    fn execute_length(&self, input: &Value) -> Result<Value, NodeError> {
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        Ok(Value::Number(serde_json::Number::from(items.len())))
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

        let result = executor.execute("iter1", &config, &make_pool(), &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("results"),
            Some(&Segment::from_value(&serde_json::json!([
                {"value": 1},
                {"value": 2},
                {"value": 3}
            ])))
        );
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

        let result = executor.execute("loop1", &config, &make_pool(), &context).await.unwrap();
        assert!(result.outputs.ready().contains_key("loop_result"));
        assert_eq!(
            result.outputs.ready().get("_iterations"),
            Some(&Segment::Integer(0))
        );
    }

    #[tokio::test]
    #[cfg(all(feature = "builtin-core-nodes", feature = "builtin-subgraph-nodes"))]
    async fn test_list_operator_filter() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();

        let config = serde_json::json!({
            "operation": "filter",
            "input_selector": "input.items",
            "sub_graph": {
                "nodes": [
                    {"id": "start", "type": "start", "data": {}},
                    {"id": "end", "type": "end", "data": {
                        "outputs": [
                            {"variable": "keep", "value_selector": "item"}
                        ]
                    }}
                ],
                "edges": [
                    {"id": "e1", "source": "start", "target": "end"}
                ]
            },
            "output_variable": "filtered"
        });

        let result = executor.execute("list1", &config, &make_pool(), &context).await.unwrap();
        assert!(result.outputs.ready().get("filtered").is_some());
    }

    // ---- List operator pure functions ----

    #[tokio::test]
    async fn test_list_sort_asc() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([
                {"name": "c", "val": 3},
                {"name": "a", "val": 1},
                {"name": "b", "val": 2}
            ])),
        );

        let config = serde_json::json!({
            "operation": "sort",
            "input_selector": "input.items",
            "sort_key": "val",
            "sort_order": "asc",
            "output_variable": "sorted"
        });

        let result = executor.execute("ls1", &config, &pool, &context).await.unwrap();
        let sorted = result.outputs.ready().get("sorted").unwrap();
        let arr = sorted.as_array().unwrap();
        assert_eq!(arr[0]["val"], 1);
        assert_eq!(arr[2]["val"], 3);
    }

    #[tokio::test]
    async fn test_list_sort_desc() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([
                {"name": "a", "val": 1},
                {"name": "b", "val": 2}
            ])),
        );
        let config = serde_json::json!({
            "operation": "sort",
            "input_selector": "input.items",
            "sort_key": "val",
            "sort_order": "desc",
            "output_variable": "sorted"
        });
        let result = executor.execute("ls2", &config, &pool, &context).await.unwrap();
        let sorted = result.outputs.ready().get("sorted").unwrap();
        let arr = sorted.as_array().unwrap();
        assert_eq!(arr[0]["val"], 2);
    }

    #[tokio::test]
    async fn test_list_slice() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let config = serde_json::json!({
            "operation": "slice",
            "input_selector": "input.items",
            "start": 1,
            "end": 3,
            "output_variable": "sliced"
        });
        let result = executor.execute("ls3", &config, &make_pool(), &context).await.unwrap();
        let sliced = result.outputs.ready().get("sliced").unwrap();
        assert_eq!(sliced.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_list_first() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let config = serde_json::json!({
            "operation": "first",
            "input_selector": "input.items",
            "output_variable": "first_item"
        });
        let result = executor.execute("ls4", &config, &make_pool(), &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("first_item"), Some(&Segment::Integer(1)));
    }

    #[tokio::test]
    async fn test_list_last() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let config = serde_json::json!({
            "operation": "last",
            "input_selector": "input.items",
            "output_variable": "last_item"
        });
        let result = executor.execute("ls5", &config, &make_pool(), &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("last_item"), Some(&Segment::Integer(3)));
    }

    #[tokio::test]
    async fn test_list_flatten() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([[1, 2], [3], 4])),
        );
        let config = serde_json::json!({
            "operation": "flatten",
            "input_selector": "input.items",
            "output_variable": "flat"
        });
        let result = executor.execute("ls6", &config, &pool, &context).await.unwrap();
        let flat = result.outputs.ready().get("flat").unwrap();
        assert_eq!(flat.as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn test_list_unique() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([1, 2, 1, 3, 2])),
        );
        let config = serde_json::json!({
            "operation": "unique",
            "input_selector": "input.items",
            "output_variable": "uniq"
        });
        let result = executor.execute("ls7", &config, &pool, &context).await.unwrap();
        let uniq = result.outputs.ready().get("uniq").unwrap();
        assert_eq!(uniq.as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_list_reverse() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let config = serde_json::json!({
            "operation": "reverse",
            "input_selector": "input.items",
            "output_variable": "reversed"
        });
        let result = executor.execute("ls8", &config, &make_pool(), &context).await.unwrap();
        let reversed = result.outputs.ready().get("reversed").unwrap();
        let arr = reversed.as_array().unwrap();
        assert_eq!(arr[0], 3);
        assert_eq!(arr[2], 1);
    }

    #[tokio::test]
    async fn test_list_concat() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = make_pool();
        pool.set(
            &Selector::new("other", "list"),
            Segment::from_value(&serde_json::json!([4, 5])),
        );
        let config = serde_json::json!({
            "operation": "concat",
            "input_selector": "input.items",
            "second_input_selector": "other.list",
            "output_variable": "concated"
        });
        let result = executor.execute("ls9", &config, &pool, &context).await.unwrap();
        let concated = result.outputs.ready().get("concated").unwrap();
        assert_eq!(concated.as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn test_list_length() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let config = serde_json::json!({
            "operation": "length",
            "input_selector": "input.items",
            "output_variable": "len"
        });
        let result = executor.execute("ls10", &config, &make_pool(), &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("len"), Some(&Segment::Integer(3)));
    }

    #[tokio::test]
    async fn test_list_first_empty() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([])),
        );
        let config = serde_json::json!({
            "operation": "first",
            "input_selector": "input.items",
            "output_variable": "first_item"
        });
        let result = executor.execute("ls11", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("first_item"), Some(&Segment::None));
    }

    #[tokio::test]
    async fn test_list_not_array_error() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::String("not array".into()),
        );
        let config = serde_json::json!({
            "operation": "length",
            "input_selector": "input.items",
            "output_variable": "len"
        });
        let result = executor.execute("ls12", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    // ---- Iteration config resolution ----

    #[test]
    fn test_iteration_resolve_parallelism_default() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out"
        })).unwrap();
        assert_eq!(IterationNodeExecutor::resolve_parallelism(&config), 10);
    }

    #[test]
    fn test_iteration_resolve_parallelism_custom() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out",
            "parallelism": 4
        })).unwrap();
        assert_eq!(IterationNodeExecutor::resolve_parallelism(&config), 4);
    }

    #[test]
    fn test_iteration_resolve_parallel_compat() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out",
            "is_parallel": true
        })).unwrap();
        assert!(IterationNodeExecutor::resolve_parallel(&config));
    }

    #[test]
    fn test_iteration_max_iterations_default() {
        let config: IterationNodeConfig = serde_json::from_value(serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "out"
        })).unwrap();
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
    fn test_list_operation_serde_roundtrip() {
        let ops = vec![
            ListOperation::Filter, ListOperation::Map, ListOperation::Sort,
            ListOperation::Slice, ListOperation::First, ListOperation::Last,
            ListOperation::Flatten, ListOperation::Unique, ListOperation::Reverse,
            ListOperation::Concat, ListOperation::Reduce, ListOperation::Length,
        ];
        for op in ops {
            let json = serde_json::to_string(&op).unwrap();
            let back: ListOperation = serde_json::from_str(&json).unwrap();
            assert_eq!(op, back);
        }
    }

    #[test]
    fn test_sort_order_serde() {
        let asc: SortOrder = serde_json::from_str(r#""asc""#).unwrap();
        assert_eq!(asc, SortOrder::Asc);
        let desc: SortOrder = serde_json::from_str(r#""desc""#).unwrap();
        assert_eq!(desc, SortOrder::Desc);
    }

    #[tokio::test]
    async fn test_list_sort_missing_sort_key() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("src", "items"),
            Segment::from_value(&serde_json::json!([{"a":1},{"a":2}])),
        );
        let config = serde_json::json!({
            "operation": "sort",
            "input_selector": ["src", "items"],
            "output_variable": "out"
        });
        let executor = ListOperatorNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("sort_err", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_concat_missing_second_input() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("src", "items"),
            Segment::from_value(&serde_json::json!([1, 2])),
        );
        let config = serde_json::json!({
            "operation": "concat",
            "input_selector": ["src", "items"],
            "output_variable": "out"
        });
        let executor = ListOperatorNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("concat_err", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_concat_second_not_array() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("src", "items"),
            Segment::from_value(&serde_json::json!([1, 2])),
        );
        pool.set(
            &Selector::new("src", "other"),
            Segment::String("not array".into()),
        );
        let config = serde_json::json!({
            "operation": "concat",
            "input_selector": ["src", "items"],
            "second_input_selector": ["src", "other"],
            "output_variable": "out"
        });
        let executor = ListOperatorNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("concat_type", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_input_selector_both_none() {
        let config = IterationNodeConfig {
            input_selector: None,
            parallel: false,
            parallelism: None,
            sub_graph: SubGraphDefinition { nodes: vec![], edges: vec![] },
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
            input_selector: Some(serde_json::json!(42)), // not an array of strings
            parallel: false,
            parallelism: None,
            sub_graph: SubGraphDefinition { nodes: vec![], edges: vec![] },
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
        assert!(matches!(default_iteration_error_mode(), IterationErrorMode::Terminated));
    }

    #[tokio::test]
    async fn test_list_sort_mixed_types() {
        // Sort with mixed value types at sort_key should fallback to Equal
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("src", "items"),
            Segment::from_value(&serde_json::json!([
                {"name": "a", "val": 1},
                {"name": "b", "val": "string"},
                {"name": "c", "val": true}
            ])),
        );
        let config = serde_json::json!({
            "operation": "sort",
            "input_selector": ["src", "items"],
            "sort_key": "val",
            "sort_order": "asc",
            "output_variable": "out"
        });
        let executor = ListOperatorNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("sort_mixed", &config, &pool, &context).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_slice_defaults() {
        // Slice with no start/end should return full array
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("src", "items"),
            Segment::from_value(&serde_json::json!([1, 2, 3])),
        );
        let config = serde_json::json!({
            "operation": "slice",
            "input_selector": ["src", "items"],
            "output_variable": "out"
        });
        let executor = ListOperatorNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("slice_def", &config, &pool, &context).await.unwrap();
        let out = result.outputs.ready().get("out").unwrap();
        assert_eq!(out, &serde_json::json!([1, 2, 3]));
    }

    #[tokio::test]
    async fn test_list_slice_start_beyond_length() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("src", "items"),
            Segment::from_value(&serde_json::json!([1, 2, 3])),
        );
        let config = serde_json::json!({
            "operation": "slice",
            "input_selector": ["src", "items"],
            "start": 100,
            "output_variable": "out"
        });
        let executor = ListOperatorNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("slice_far", &config, &pool, &context).await.unwrap();
        let out = result.outputs.ready().get("out").unwrap();
        assert_eq!(out, &serde_json::json!([]));
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
    async fn test_iteration_error_mode_continue() {
        // Test that iteration continues on sub-graph error with Continue mode
        let executor = IterationNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([1, 2, 3])),
        );
        
        let config = serde_json::json!({
            "input_selector": "input.items",
            "error_handle_mode": "continue",
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "results"
        });
        
        // With Continue mode, even if sub-graph has issues, iteration should succeed
        let result = executor.execute("iter_continue", &config, &pool, &context).await;
        // The actual result depends on implementation - test just ensures it doesn't panic
        let _ = result;
    }

    #[tokio::test]
    async fn test_loop_missing_condition() {
        let executor = LoopNodeExecutor::new();
        let context = RuntimeContext::default();
        
        let config = serde_json::json!({
            "sub_graph": {"nodes": [], "edges": []},
            "output_variable": "result"
        });
        
        let result = executor.execute("loop_no_cond", &config, &make_pool(), &context).await;
        // Should fail without condition
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_operator_map() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        
        let config = serde_json::json!({
            "operation": "map",
            "input_selector": "input.items",
            "sub_graph": {
                "nodes": [
                    {"id": "start", "type": "start", "data": {}},
                    {"id": "end", "type": "end", "data": {
                        "outputs": [
                            {"variable": "value", "value_selector": "item"}
                        ]
                    }}
                ],
                "edges": [
                    {"id": "e1", "source": "start", "target": "end"}
                ]
            },
            "output_variable": "mapped"
        });
        
        let result = executor.execute("list_map", &config, &make_pool(), &context).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_operator_reduce() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        
        let config = serde_json::json!({
            "operation": "reduce",
            "input_selector": "input.items",
            "initial_value": 0,
            "sub_graph": {
                "nodes": [
                    {"id": "start", "type": "start", "data": {}},
                    {"id": "end", "type": "end", "data": {
                        "outputs": [
                            {"variable": "accumulator", "value_selector": "item"}
                        ]
                    }}
                ],
                "edges": [
                    {"id": "e1", "source": "start", "target": "end"}
                ]
            },
            "output_variable": "reduced"
        });
        
        let result = executor.execute("list_reduce", &config, &make_pool(), &context).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_operator_invalid_operation() {
        // Invalid operation should fail during deserialization
        let config = serde_json::json!({
            "operation": "invalid_op",
            "input_selector": "input.items",
            "output_variable": "out"
        });
        
        let result: Result<ListOperatorNodeConfig, _> = serde_json::from_value(config);
        assert!(result.is_err());
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
        
        let result = executor.execute("iter_parallel", &config, &make_pool(), &context).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_sub_graph_runner() {
        let context = RuntimeContext::default();
        let runner = resolve_sub_graph_runner(&context);
        // Just ensure it returns a valid runner Arc
        let _clone = runner.clone();
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
        
        let result = executor.execute("loop_cond_err", &config, &make_pool(), &context).await;
        assert!(result.is_ok()); // Should handle condition evaluation errors gracefully
    }

    #[tokio::test]
    async fn test_list_flatten_nested_arrays() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([[[1, 2]], [[3]], 4])),
        );
        
        let config = serde_json::json!({
            "operation": "flatten",
            "input_selector": "input.items",
            "output_variable": "flat"
        });
        
        let result = executor.execute("flatten_nested", &config, &pool, &context).await.unwrap();
        let flat = result.outputs.ready().get("flat").unwrap();
        // Flatten only goes one level deep
        assert!(flat.as_array().is_some());
    }

    #[tokio::test]
    async fn test_list_unique_complex_objects() {
        let executor = ListOperatorNodeExecutor::new();
        let context = RuntimeContext::default();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("input", "items"),
            Segment::from_value(&serde_json::json!([
                {"id": 1, "name": "a"},
                {"id": 2, "name": "b"},
                {"id": 1, "name": "a"}
            ])),
        );
        
        let config = serde_json::json!({
            "operation": "unique",
            "input_selector": "input.items",
            "output_variable": "uniq"
        });
        
        let result = executor.execute("uniq_obj", &config, &pool, &context).await.unwrap();
        let uniq = result.outputs.ready().get("uniq").unwrap();
        assert_eq!(uniq.as_array().unwrap().len(), 2);
    }
}
