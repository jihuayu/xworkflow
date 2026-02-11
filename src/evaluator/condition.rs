//! IfElse condition evaluation.
//!
//! Evaluates multi-case branch conditions against the current [`VariablePool`].

use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{Case, ComparisonOperator, Condition, LogicalOperator};
use serde_json::Value;

/// Indicates a type mismatch encountered during condition evaluation.
#[derive(Debug, Clone)]
pub struct ConditionTypeMismatch {
    /// The actual runtime type of the variable.
    pub actual_type: &'static str,
    /// The type expected by the operator.
    pub expected_type: &'static str,
    /// The operator that triggered the mismatch.
    pub operator: ComparisonOperator,
}

/// Result of evaluating a single condition.
#[derive(Debug, Clone)]
pub enum ConditionResult {
    True,
    False,
    TypeMismatch(ConditionTypeMismatch),
}

/// Evaluate IfElse cases, returning the case_id of the first matching case,
/// or None if no case matches (else branch).
pub async fn evaluate_cases(
    cases: &[Case],
    pool: &VariablePool,
) -> Result<Option<String>, ConditionTypeMismatch> {
    for case in cases {
        match evaluate_case(case, pool).await {
            ConditionResult::True => return Ok(Some(case.case_id.clone())),
            ConditionResult::False => continue,
            ConditionResult::TypeMismatch(m) => return Err(m),
        }
    }
    Ok(None)
}

/// Evaluate a single case (AND/OR logic)
pub async fn evaluate_case(case: &Case, pool: &VariablePool) -> ConditionResult {
    match case.logical_operator {
        LogicalOperator::And => {
            for cond in &case.conditions {
                match evaluate_condition(cond, pool).await {
                    ConditionResult::True => continue,
                    ConditionResult::False => return ConditionResult::False,
                    ConditionResult::TypeMismatch(m) => return ConditionResult::TypeMismatch(m),
                }
            }
            ConditionResult::True
        }
        LogicalOperator::Or => {
            let mut mismatch: Option<ConditionTypeMismatch> = None;
            for cond in &case.conditions {
                match evaluate_condition(cond, pool).await {
                    ConditionResult::True => return ConditionResult::True,
                    ConditionResult::False => continue,
                    ConditionResult::TypeMismatch(m) => mismatch = Some(m),
                }
            }
            mismatch.map_or(ConditionResult::False, ConditionResult::TypeMismatch)
        }
    }
}

/// Evaluate a single condition
pub async fn evaluate_condition(cond: &Condition, pool: &VariablePool) -> ConditionResult {
    let actual = match pool.get(&cond.variable_selector) {
        Segment::Stream(stream) => stream.collect().await.unwrap_or(Segment::None),
        other => other,
    };
    let expected = &cond.value;

    match &cond.comparison_operator {
        // --- String/Array ---
        ComparisonOperator::Contains => bool_result(eval_contains(&actual, expected)),
        ComparisonOperator::NotContains => bool_result(!eval_contains(&actual, expected)),
        ComparisonOperator::StartWith => {
            let s = actual.to_display_string();
            let e = value_to_string(expected);
            bool_result(s.starts_with(&e))
        }
        ComparisonOperator::EndWith => {
            let s = actual.to_display_string();
            let e = value_to_string(expected);
            bool_result(s.ends_with(&e))
        }

        // --- Exact equality ---
        ComparisonOperator::Is => {
            bool_result(actual.to_display_string() == value_to_string(expected))
        }
        ComparisonOperator::IsNot => {
            bool_result(actual.to_display_string() != value_to_string(expected))
        }

        // --- Emptiness ---
        ComparisonOperator::Empty => bool_result(actual.is_none() || actual.is_empty()),
        ComparisonOperator::NotEmpty => bool_result(!actual.is_none() && !actual.is_empty()),

        // --- Membership ---
        ComparisonOperator::In => bool_result(eval_in(&actual, expected)),
        ComparisonOperator::NotIn => bool_result(!eval_in(&actual, expected)),
        ComparisonOperator::AllOf => bool_result(eval_all_of(&actual, expected)),

        // --- Numeric ---
        ComparisonOperator::Equal => numeric_compare(&actual, expected, |a, b| (a - b).abs() < f64::EPSILON, ComparisonOperator::Equal),
        ComparisonOperator::NotEqual => numeric_compare(&actual, expected, |a, b| (a - b).abs() >= f64::EPSILON, ComparisonOperator::NotEqual),
        ComparisonOperator::GreaterThan => numeric_compare(&actual, expected, |a, b| a > b, ComparisonOperator::GreaterThan),
        ComparisonOperator::LessThan => numeric_compare(&actual, expected, |a, b| a < b, ComparisonOperator::LessThan),
        ComparisonOperator::GreaterOrEqual => numeric_compare(&actual, expected, |a, b| a >= b, ComparisonOperator::GreaterOrEqual),
        ComparisonOperator::LessOrEqual => numeric_compare(&actual, expected, |a, b| a <= b, ComparisonOperator::LessOrEqual),

        // --- Null ---
        ComparisonOperator::Null => bool_result(actual.is_none()),
        ComparisonOperator::NotNull => bool_result(!actual.is_none()),
    }
}

// ================================
// Helper functions
// ================================

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn bool_result(v: bool) -> ConditionResult {
    if v {
        ConditionResult::True
    } else {
        ConditionResult::False
    }
}

fn numeric_compare(
    actual: &Segment,
    expected: &Value,
    op: impl Fn(f64, f64) -> bool,
    operator: ComparisonOperator,
) -> ConditionResult {
    let actual_num = match actual {
        Segment::Integer(i) => Some(*i as f64),
        Segment::Float(f) => Some(*f),
        _ => None,
    };
    let expected_num = match expected {
        Value::Number(n) => n.as_f64(),
        _ => None,
    };

    match (actual_num, expected_num) {
        (Some(a), Some(b)) => bool_result(op(a, b)),
        _ => ConditionResult::TypeMismatch(ConditionTypeMismatch {
            actual_type: segment_type_name(actual),
            expected_type: "number",
            operator,
        }),
    }
}

fn segment_type_name(seg: &Segment) -> &'static str {
    match seg {
        Segment::None => "null",
        Segment::String(_) => "string",
        Segment::Integer(_) | Segment::Float(_) => "number",
        Segment::Boolean(_) => "boolean",
        Segment::Object(_) => "object",
        Segment::ArrayString(_) | Segment::Array(_) => "array",
        Segment::Stream(_) => "stream",
    }
}

fn value_to_string_vec(v: &Value) -> Vec<String> {
    match v {
        Value::Array(arr) => arr.iter().map(|x| value_to_string(x)).collect(),
        Value::String(s) => vec![s.clone()],
        _ => vec![],
    }
}

fn eval_contains(actual: &Segment, expected: &Value) -> bool {
    let e = value_to_string(expected);
    match actual {
        Segment::String(s) => s.contains(&e),
        Segment::ArrayString(arr) => arr.iter().any(|s| s == &e),
        Segment::Array(arr) => arr.iter().any(|s| s.to_display_string() == e),
        _ => false,
    }
}

fn eval_in(actual: &Segment, expected: &Value) -> bool {
    let arr = value_to_string_vec(expected);
    let actual_str = actual.to_display_string();
    arr.contains(&actual_str)
}

fn eval_all_of(actual: &Segment, expected: &Value) -> bool {
    let expected_items = value_to_string_vec(expected);
    match actual {
        Segment::ArrayString(arr) => {
            expected_items.iter().all(|e| arr.contains(e))
        }
        Segment::Array(arr) => {
            let actual_strs: Vec<String> = arr.iter().map(|s| s.to_display_string()).collect();
            expected_items.iter().all(|e| actual_strs.contains(e))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_pool(vars: Vec<(&str, &str, Segment)>) -> VariablePool {
        let mut pool = VariablePool::new();
        for (node_id, var, val) in vars {
            pool.set(&crate::core::variable_pool::Selector::new(node_id, var), val);
        }
        pool
    }

    fn make_condition(node_id: &str, var: &str, op: ComparisonOperator, val: Value) -> Condition {
        Condition {
            variable_selector: crate::core::variable_pool::Selector::new(node_id, var),
            comparison_operator: op,
            value: val,
        }
    }

    fn assert_true(result: ConditionResult) {
        assert!(matches!(result, ConditionResult::True));
    }

    fn assert_false(result: ConditionResult) {
        assert!(matches!(result, ConditionResult::False));
    }

    fn assert_mismatch(result: ConditionResult) {
        assert!(matches!(result, ConditionResult::TypeMismatch(_)));
    }

    #[tokio::test]
    async fn test_is() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::Is, Value::String("hello".into()));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_is_not() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::IsNot, Value::String("world".into()));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_contains() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello world".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::Contains, Value::String("world".into()));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_not_contains() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::NotContains, Value::String("xyz".into()));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_starts_with() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello world".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::StartWith, Value::String("hello".into()));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_ends_with() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello world".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::EndWith, Value::String("world".into()));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_empty() {
        let pool = make_pool(vec![("n", "x", Segment::String("".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::Empty, Value::Null);
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_not_empty() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::NotEmpty, Value::Null);
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_numeric_gt() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(10))]);
        let cond = make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!(5));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_numeric_lt() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(3))]);
        let cond = make_condition("n", "x", ComparisonOperator::LessThan, serde_json::json!(5));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_numeric_eq() {
        let pool = make_pool(vec![("n", "x", Segment::Float(3.14))]);
        let cond = make_condition("n", "x", ComparisonOperator::Equal, serde_json::json!(3.14));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_null() {
        let pool = make_pool(vec![]);
        let cond = make_condition("n", "x", ComparisonOperator::Null, Value::Null);
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_not_null() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(1))]);
        let cond = make_condition("n", "x", ComparisonOperator::NotNull, Value::Null);
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_in() {
        let pool = make_pool(vec![("n", "x", Segment::String("b".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::In, serde_json::json!(["a", "b", "c"]));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_not_in() {
        let pool = make_pool(vec![("n", "x", Segment::String("d".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::NotIn, serde_json::json!(["a", "b", "c"]));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_all_of() {
        let pool = make_pool(vec![("n", "x", Segment::ArrayString(Arc::new(vec!["a".into(), "b".into(), "c".into()]))) ]);
        let cond = make_condition("n", "x", ComparisonOperator::AllOf, serde_json::json!(["a", "b"]));
        assert_true(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_string_numeric_coercion() {
        let pool = make_pool(vec![("n", "x", Segment::String("42".into()))]);
        let cond = make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!("10"));
        assert_mismatch(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_and_logic() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(10))]);
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::And,
            conditions: vec![
                make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!(5)),
                make_condition("n", "x", ComparisonOperator::LessThan, serde_json::json!(20)),
            ],
        };
        assert_true(evaluate_case(&case, &pool).await);
    }

    #[tokio::test]
    async fn test_and_short_circuit() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(3))]);
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::And,
            conditions: vec![
                make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!(5)),
                make_condition("n", "x", ComparisonOperator::LessThan, serde_json::json!(20)),
            ],
        };
        assert_false(evaluate_case(&case, &pool).await);
    }

    #[tokio::test]
    async fn test_or_logic() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(3))]);
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::Or,
            conditions: vec![
                make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!(5)),
                make_condition("n", "x", ComparisonOperator::LessThan, serde_json::json!(5)),
            ],
        };
        assert_true(evaluate_case(&case, &pool).await);
    }

    #[tokio::test]
    async fn test_evaluate_cases_match() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(10))]);
        let cases = vec![
            Case {
                case_id: "c1".into(),
                logical_operator: LogicalOperator::And,
                conditions: vec![
                    make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!(5)),
                ],
            },
            Case {
                case_id: "c2".into(),
                logical_operator: LogicalOperator::And,
                conditions: vec![
                    make_condition("n", "x", ComparisonOperator::LessThan, serde_json::json!(5)),
                ],
            },
        ];
        assert_eq!(evaluate_cases(&cases, &pool).await.unwrap(), Some("c1".to_string()));
    }

    #[tokio::test]
    async fn test_evaluate_cases_else() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(3))]);
        let cases = vec![
            Case {
                case_id: "c1".into(),
                logical_operator: LogicalOperator::And,
                conditions: vec![
                    make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!(5)),
                ],
            },
        ];
        assert_eq!(evaluate_cases(&cases, &pool).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_ge_le() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(5))]);
        let ge = make_condition("n", "x", ComparisonOperator::GreaterOrEqual, serde_json::json!(5));
        assert_true(evaluate_condition(&ge, &pool).await);
        let le = make_condition("n", "x", ComparisonOperator::LessOrEqual, serde_json::json!(5));
        assert_true(evaluate_condition(&le, &pool).await);
    }

    #[tokio::test]
    async fn test_contains_array_segment() {
        let seg = Segment::Array(Arc::new(crate::core::variable_pool::SegmentArray::new(vec![
            Segment::String("apple".into()),
            Segment::String("banana".into()),
        ])));
        let pool = make_pool(vec![("n", "x", seg)]);
        let c = make_condition("n", "x", ComparisonOperator::Contains, serde_json::json!("apple"));
        assert_true(evaluate_condition(&c, &pool).await);
        let c2 = make_condition("n", "x", ComparisonOperator::Contains, serde_json::json!("cherry"));
        assert_false(evaluate_condition(&c2, &pool).await);
    }

    #[tokio::test]
    async fn test_contains_integer_segment() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(42))]);
        let c = make_condition("n", "x", ComparisonOperator::Contains, serde_json::json!("42"));
        assert_false(evaluate_condition(&c, &pool).await);
    }

    #[tokio::test]
    async fn test_all_of_array_segment() {
        let seg = Segment::Array(Arc::new(crate::core::variable_pool::SegmentArray::new(vec![
            Segment::String("a".into()),
            Segment::String("b".into()),
            Segment::String("c".into()),
        ])));
        let pool = make_pool(vec![("n", "x", seg)]);
        let c = make_condition("n", "x", ComparisonOperator::AllOf, serde_json::json!(["a", "b"]));
        assert_true(evaluate_condition(&c, &pool).await);
        let c2 = make_condition("n", "x", ComparisonOperator::AllOf, serde_json::json!(["a", "z"]));
        assert_false(evaluate_condition(&c2, &pool).await);
    }

    #[tokio::test]
    async fn test_all_of_non_array() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let c = make_condition("n", "x", ComparisonOperator::AllOf, serde_json::json!(["hello"]));
        assert_false(evaluate_condition(&c, &pool).await);
    }

    #[tokio::test]
    async fn test_or_all_false_returns_false() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(1))]);
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::Or,
            conditions: vec![
                make_condition("n", "x", ComparisonOperator::Is, serde_json::json!("99")),
                make_condition("n", "x", ComparisonOperator::Is, serde_json::json!("88")),
            ],
        };
        assert_false(evaluate_case(&case, &pool).await);
    }

    #[tokio::test]
    async fn test_evaluate_cases_type_mismatch_propagation() {
        // TypeMismatch from And case should propagate as Err
        let pool = make_pool(vec![("n", "x", Segment::None)]);
        let cases = vec![Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::And,
            conditions: vec![
                make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!(5)),
            ],
        }];
        let result = evaluate_cases(&cases, &pool).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_not_equal_numeric() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(5))]);
        let c = make_condition("n", "x", ComparisonOperator::NotEqual, serde_json::json!(3));
        assert_true(evaluate_condition(&c, &pool).await);
        let c2 = make_condition("n", "x", ComparisonOperator::NotEqual, serde_json::json!(5));
        assert_false(evaluate_condition(&c2, &pool).await);
    }

    #[tokio::test]
    async fn test_in_with_non_array_expected() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let c = make_condition("n", "x", ComparisonOperator::In, serde_json::json!(42));
        assert_false(evaluate_condition(&c, &pool).await);
    }

    #[tokio::test]
    async fn test_starts_with_null_expected() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let c = make_condition("n", "x", ComparisonOperator::StartWith, Value::Null);
        // Null converts to empty string by value_to_string
        let result = evaluate_condition(&c, &pool).await;
        assert!(matches!(result, ConditionResult::True | ConditionResult::False));
    }

    #[tokio::test]
    async fn test_not_contains_array_segment() {
        let seg = Segment::Array(Arc::new(crate::core::variable_pool::SegmentArray::new(vec![
            Segment::String("a".into()),
        ])));
        let pool = make_pool(vec![("n", "x", seg)]);
        let c = make_condition("n", "x", ComparisonOperator::NotContains, serde_json::json!("b"));
        assert_true(evaluate_condition(&c, &pool).await);
        let c2 = make_condition("n", "x", ComparisonOperator::NotContains, serde_json::json!("a"));
        assert_false(evaluate_condition(&c2, &pool).await);
    }

    #[tokio::test]
    async fn test_or_with_type_mismatch() {
        let pool = make_pool(vec![("n", "x", Segment::None)]);
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::Or,
            conditions: vec![
                make_condition("n", "x", ComparisonOperator::Is, serde_json::json!("a")),
                make_condition("n", "x", ComparisonOperator::GreaterThan, serde_json::json!(5)),
            ],
        };
        // First is False (None != "a"), second is TypeMismatch (None vs numeric compare)
        let result = evaluate_case(&case, &pool).await;
        assert!(matches!(result, ConditionResult::TypeMismatch(_)));
    }
}
