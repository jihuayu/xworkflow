use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::{ComparisonOperator, IterationErrorMode, SubGraphDefinition};

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
    #[serde(default)]
    pub iterator_selector: Option<Value>,
    #[serde(default)]
    pub is_parallel: bool,
    #[serde(default)]
    pub parallel_nums: Option<u32>,
}

fn default_iteration_error_mode() -> IterationErrorMode {
    IterationErrorMode::Terminated
}

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
