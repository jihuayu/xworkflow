//! ListOperator Node executor.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::domain::model::{ListOperation, ListOperatorNodeConfig};
use crate::dsl::schema::{EdgeHandle, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;

#[path = "list_operator_ops.rs"]
mod ops;

pub struct ListOperatorNodeExecutor;

impl Default for ListOperatorNodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

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
            ListOperation::Filter => ops::execute_filter(&config, &input, pool, context).await?,
            ListOperation::Map => ops::execute_map(&config, &input, pool, context).await?,
            ListOperation::Sort => ops::execute_sort(&config, &input)?,
            ListOperation::Slice => ops::execute_slice(&config, &input)?,
            ListOperation::First => self.execute_first(&input)?,
            ListOperation::Last => self.execute_last(&input)?,
            ListOperation::Flatten => self.execute_flatten(&input)?,
            ListOperation::Unique => self.execute_unique(&input)?,
            ListOperation::Reverse => self.execute_reverse(&input)?,
            ListOperation::Concat => self.execute_concat(&config, &input, pool)?,
            ListOperation::Reduce => ops::execute_reduce(&config, &input, pool, context).await?,
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
            let canonical = Self::canonicalize_value(item);
            let key = serde_json::to_string(&canonical).unwrap_or_default();
            if seen.insert(key) {
                unique.push(item.clone());
            }
        }

        Ok(Value::Array(unique))
    }

    fn canonicalize_value(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut pairs: Vec<_> = map.iter().collect();
                pairs.sort_by_key(|(k, _)| *k);
                let canonical_map: serde_json::Map<String, Value> = pairs
                    .into_iter()
                    .map(|(k, v)| (k.clone(), Self::canonicalize_value(v)))
                    .collect();
                Value::Object(canonical_map)
            }
            Value::Array(arr) => {
                let canonical_arr: Vec<Value> = arr.iter().map(Self::canonicalize_value).collect();
                Value::Array(canonical_arr)
            }
            other => other.clone(),
        }
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

    fn execute_length(&self, input: &Value) -> Result<Value, NodeError> {
        let items = input
            .as_array()
            .ok_or_else(|| NodeError::TypeError("Input must be an array".to_string()))?;

        Ok(Value::Number(serde_json::Number::from(items.len())))
    }
}

#[cfg(test)]
#[path = "list_operator_tests.rs"]
mod tests;
