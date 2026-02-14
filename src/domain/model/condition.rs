use serde::{Deserialize, Serialize};

/// Comparison operators used by condition evaluation.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOperator {
    Contains,
    #[serde(alias = "not_contains")]
    NotContains,
    #[serde(alias = "start_with")]
    StartWith,
    #[serde(alias = "end_with")]
    EndWith,
    Is,
    #[serde(alias = "is_not")]
    IsNot,
    Empty,
    #[serde(alias = "not_empty")]
    NotEmpty,
    In,
    #[serde(alias = "not_in")]
    NotIn,
    #[serde(alias = "all_of")]
    AllOf,
    #[serde(alias = "=")]
    Equal,
    #[serde(alias = "≠")]
    NotEqual,
    #[serde(alias = ">", alias = "greater_than")]
    GreaterThan,
    #[serde(alias = "<", alias = "less_than")]
    LessThan,
    #[serde(
        alias = "≥",
        alias = "greater_than_or_equal",
        alias = "greater_or_equal"
    )]
    GreaterOrEqual,
    #[serde(alias = "≤", alias = "less_than_or_equal", alias = "less_or_equal")]
    LessOrEqual,
    Null,
    #[serde(alias = "not_null")]
    NotNull,
}

/// Error handling mode for iteration failures.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum IterationErrorMode {
    Terminated,
    RemoveAbnormal,
    ContinueOnError,
}
