use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::core::runtime_context::RuntimeContext;
use crate::core::sub_graph_runner::{DefaultSubGraphRunner, SubGraphRunner};
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{
    ComparisonOperator, Condition, IterationErrorMode, NodeRunResult, WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::evaluator::condition::evaluate_condition;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::subgraph::SubGraphDefinition;
use crate::nodes::utils::selector_from_value;

// ================================
// Iteration Node
// ================================

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
        .sub_graph_runner
        .as_ref()
        .cloned()
        .unwrap_or_else(|| Arc::new(DefaultSubGraphRunner))
}

pub struct IterationNodeExecutor;

impl IterationNodeExecutor {
    pub fn new() -> Self {
        Self
    }

    fn resolve_input_selector(config: &IterationNodeConfig) -> Result<Vec<String>, NodeError> {
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
        let input_value = variable_pool.get(&input_selector).to_value();
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
        outputs.insert(config.output_variable.clone(), Value::Array(results));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: "source".to_string(),
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
    ) -> Result<bool, NodeError> {
        let selector = selector_from_value(&condition.variable_selector)
            .ok_or_else(|| NodeError::ConfigError("Invalid loop condition selector".to_string()))?;

        let mut pool = VariablePool::new();
        for (k, v) in loop_vars {
            pool.set(&["loop".to_string(), k.clone()], Segment::from_value(v));
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

            if !self.evaluate_condition(&config.condition, &loop_vars).await? {
                break;
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
        outputs.insert(config.output_variable.clone(), Value::Object(loop_vars.into_iter().collect()));
        outputs.insert("_iterations".to_string(), serde_json::json!(iteration_count));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: "source".to_string(),
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
        let input = pool.get(&input_selector).to_value();

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
        outputs.insert(config.output_variable.clone(), result);

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: "source".to_string(),
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

        let input2 = pool.get(&second_selector).to_value();
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
            &["input".to_string(), "items".to_string()],
            Segment::from_value(&serde_json::json!([1, 2, 3])),
        );
        pool
    }

    #[tokio::test]
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
        assert_eq!(result.outputs.get("results"), Some(&serde_json::json!([
            {"value": 1},
            {"value": 2},
            {"value": 3}
        ])));
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
        assert!(result.outputs.contains_key("loop_result"));
        assert_eq!(result.outputs.get("_iterations"), Some(&serde_json::json!(0)));
    }

    #[tokio::test]
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
        assert!(result.outputs.get("filtered").is_some());
    }
}
