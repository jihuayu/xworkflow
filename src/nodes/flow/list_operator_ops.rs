use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::domain::model::{ListOperatorNodeConfig, SortOrder, SubGraphDefinition};
use crate::error::NodeError;
use crate::nodes::flow::resolve_sub_graph_runner;

pub(super) async fn execute_filter(
    config: &ListOperatorNodeConfig,
    input: &Value,
    pool: &VariablePool,
    context: &RuntimeContext,
) -> Result<Value, NodeError> {
    let runner = resolve_sub_graph_runner(context);
    let items = input
        .as_array()
        .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

    let sub_graph = config
        .sub_graph
        .as_ref()
        .ok_or_else(|| NodeError::ConfigError("Filter operation requires sub_graph".to_string()))?;

    if config.parallel {
        return execute_filter_parallel(items, sub_graph, pool, context).await;
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
        keep_flags[index] = value.get("keep").and_then(|v| v.as_bool()).unwrap_or(false);
    }

    let mut results = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        if keep_flags[idx] {
            results.push(item.clone());
        }
    }

    Ok(Value::Array(results))
}

pub(super) async fn execute_map(
    config: &ListOperatorNodeConfig,
    input: &Value,
    pool: &VariablePool,
    context: &RuntimeContext,
) -> Result<Value, NodeError> {
    let runner = resolve_sub_graph_runner(context);
    let items = input
        .as_array()
        .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

    let sub_graph = config
        .sub_graph
        .as_ref()
        .ok_or_else(|| NodeError::ConfigError("Map operation requires sub_graph".to_string()))?;

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

pub(super) fn execute_sort(
    config: &ListOperatorNodeConfig,
    input: &Value,
) -> Result<Value, NodeError> {
    let mut items = input
        .as_array()
        .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?
        .clone();

    let sort_key = config
        .sort_key
        .as_ref()
        .ok_or_else(|| NodeError::ConfigError("Sort operation requires sort_key".to_string()))?;
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

pub(super) fn execute_slice(
    config: &ListOperatorNodeConfig,
    input: &Value,
) -> Result<Value, NodeError> {
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

pub(super) async fn execute_reduce(
    config: &ListOperatorNodeConfig,
    input: &Value,
    pool: &VariablePool,
    context: &RuntimeContext,
) -> Result<Value, NodeError> {
    let runner = resolve_sub_graph_runner(context);
    let items = input
        .as_array()
        .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

    let sub_graph = config
        .sub_graph
        .as_ref()
        .ok_or_else(|| NodeError::ConfigError("Reduce operation requires sub_graph".to_string()))?;

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
