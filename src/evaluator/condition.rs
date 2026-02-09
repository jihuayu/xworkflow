use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{Case, ComparisonOperator, Condition, LogicalOperator};
use serde_json::Value;

/// Evaluate IfElse cases, returning the case_id of the first matching case,
/// or "false" if no case matches (else branch).
pub async fn evaluate_cases(cases: &[Case], pool: &VariablePool) -> String {
    for case in cases {
        if evaluate_case(case, pool).await {
            return case.case_id.clone();
        }
    }
    "false".to_string()
}

/// Evaluate a single case (AND/OR logic)
pub async fn evaluate_case(case: &Case, pool: &VariablePool) -> bool {
    match case.logical_operator {
        LogicalOperator::And => {
            for cond in &case.conditions {
                if !evaluate_condition(cond, pool).await {
                    return false;
                }
            }
            true
        }
        LogicalOperator::Or => {
            for cond in &case.conditions {
                if evaluate_condition(cond, pool).await {
                    return true;
                }
            }
            false
        }
    }
}

/// Evaluate a single condition
pub async fn evaluate_condition(cond: &Condition, pool: &VariablePool) -> bool {
    let actual = match pool.get(&cond.variable_selector) {
        Segment::Stream(stream) => stream.collect().await.unwrap_or(Segment::None),
        other => other,
    };
    let expected = &cond.value;

    match &cond.comparison_operator {
        // --- String/Array ---
        ComparisonOperator::Contains => eval_contains(&actual, expected),
        ComparisonOperator::NotContains => !eval_contains(&actual, expected),
        ComparisonOperator::StartWith => {
            let s = actual.to_display_string();
            let e = value_to_string(expected);
            s.starts_with(&e)
        }
        ComparisonOperator::EndWith => {
            let s = actual.to_display_string();
            let e = value_to_string(expected);
            s.ends_with(&e)
        }

        // --- Exact equality ---
        ComparisonOperator::Is => {
            actual.to_display_string() == value_to_string(expected)
        }
        ComparisonOperator::IsNot => {
            actual.to_display_string() != value_to_string(expected)
        }

        // --- Emptiness ---
        ComparisonOperator::Empty => actual.is_none() || actual.is_empty(),
        ComparisonOperator::NotEmpty => !actual.is_none() && !actual.is_empty(),

        // --- Membership ---
        ComparisonOperator::In => eval_in(&actual, expected),
        ComparisonOperator::NotIn => !eval_in(&actual, expected),
        ComparisonOperator::AllOf => eval_all_of(&actual, expected),

        // --- Numeric ---
        ComparisonOperator::Equal => {
            match (actual.as_f64(), value_to_f64(expected)) {
                (Some(a), Some(b)) => (a - b).abs() < f64::EPSILON,
                _ => false,
            }
        }
        ComparisonOperator::NotEqual => {
            match (actual.as_f64(), value_to_f64(expected)) {
                (Some(a), Some(b)) => (a - b).abs() >= f64::EPSILON,
                _ => true,
            }
        }
        ComparisonOperator::GreaterThan => {
            match (actual.as_f64(), value_to_f64(expected)) {
                (Some(a), Some(b)) => a > b,
                _ => false,
            }
        }
        ComparisonOperator::LessThan => {
            match (actual.as_f64(), value_to_f64(expected)) {
                (Some(a), Some(b)) => a < b,
                _ => false,
            }
        }
        ComparisonOperator::GreaterOrEqual => {
            match (actual.as_f64(), value_to_f64(expected)) {
                (Some(a), Some(b)) => a >= b,
                _ => false,
            }
        }
        ComparisonOperator::LessOrEqual => {
            match (actual.as_f64(), value_to_f64(expected)) {
                (Some(a), Some(b)) => a <= b,
                _ => false,
            }
        }

        // --- Null ---
        ComparisonOperator::Null => actual.is_none(),
        ComparisonOperator::NotNull => !actual.is_none(),
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

fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
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

    fn make_pool(vars: Vec<(&str, &str, Segment)>) -> VariablePool {
        let mut pool = VariablePool::new();
        for (node_id, var, val) in vars {
            pool.set(&[node_id.to_string(), var.to_string()], val);
        }
        pool
    }

    fn make_condition(sel: Vec<&str>, op: ComparisonOperator, val: Value) -> Condition {
        Condition {
            variable_selector: sel.into_iter().map(|s| s.to_string()).collect(),
            comparison_operator: op,
            value: val,
        }
    }

    #[tokio::test]
    async fn test_is() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::Is, Value::String("hello".into()));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_is_not() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::IsNot, Value::String("world".into()));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_contains() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello world".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::Contains, Value::String("world".into()));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_not_contains() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::NotContains, Value::String("xyz".into()));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_starts_with() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello world".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::StartWith, Value::String("hello".into()));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_ends_with() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello world".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::EndWith, Value::String("world".into()));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_empty() {
        let pool = make_pool(vec![("n", "x", Segment::String("".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::Empty, Value::Null);
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_not_empty() {
        let pool = make_pool(vec![("n", "x", Segment::String("hello".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::NotEmpty, Value::Null);
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_numeric_gt() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(10))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::GreaterThan, serde_json::json!(5));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_numeric_lt() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(3))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::LessThan, serde_json::json!(5));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_numeric_eq() {
        let pool = make_pool(vec![("n", "x", Segment::Float(3.14))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::Equal, serde_json::json!(3.14));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_null() {
        let pool = make_pool(vec![]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::Null, Value::Null);
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_not_null() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(1))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::NotNull, Value::Null);
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_in() {
        let pool = make_pool(vec![("n", "x", Segment::String("b".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::In, serde_json::json!(["a", "b", "c"]));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_not_in() {
        let pool = make_pool(vec![("n", "x", Segment::String("d".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::NotIn, serde_json::json!(["a", "b", "c"]));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_all_of() {
        let pool = make_pool(vec![("n", "x", Segment::ArrayString(vec!["a".into(), "b".into(), "c".into()]))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::AllOf, serde_json::json!(["a", "b"]));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_string_numeric_coercion() {
        let pool = make_pool(vec![("n", "x", Segment::String("42".into()))]);
        let cond = make_condition(vec!["n", "x"], ComparisonOperator::GreaterThan, serde_json::json!("10"));
        assert!(evaluate_condition(&cond, &pool).await);
    }

    #[tokio::test]
    async fn test_and_logic() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(10))]);
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::And,
            conditions: vec![
                make_condition(vec!["n", "x"], ComparisonOperator::GreaterThan, serde_json::json!(5)),
                make_condition(vec!["n", "x"], ComparisonOperator::LessThan, serde_json::json!(20)),
            ],
        };
        assert!(evaluate_case(&case, &pool).await);
    }

    #[tokio::test]
    async fn test_and_short_circuit() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(3))]);
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::And,
            conditions: vec![
                make_condition(vec!["n", "x"], ComparisonOperator::GreaterThan, serde_json::json!(5)),
                make_condition(vec!["n", "x"], ComparisonOperator::LessThan, serde_json::json!(20)),
            ],
        };
        assert!(!evaluate_case(&case, &pool).await);
    }

    #[tokio::test]
    async fn test_or_logic() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(3))]);
        let case = Case {
            case_id: "c1".into(),
            logical_operator: LogicalOperator::Or,
            conditions: vec![
                make_condition(vec!["n", "x"], ComparisonOperator::GreaterThan, serde_json::json!(5)),
                make_condition(vec!["n", "x"], ComparisonOperator::LessThan, serde_json::json!(5)),
            ],
        };
        assert!(evaluate_case(&case, &pool).await);
    }

    #[tokio::test]
    async fn test_evaluate_cases_match() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(10))]);
        let cases = vec![
            Case {
                case_id: "c1".into(),
                logical_operator: LogicalOperator::And,
                conditions: vec![
                    make_condition(vec!["n", "x"], ComparisonOperator::GreaterThan, serde_json::json!(5)),
                ],
            },
            Case {
                case_id: "c2".into(),
                logical_operator: LogicalOperator::And,
                conditions: vec![
                    make_condition(vec!["n", "x"], ComparisonOperator::LessThan, serde_json::json!(5)),
                ],
            },
        ];
        assert_eq!(evaluate_cases(&cases, &pool).await, "c1");
    }

    #[tokio::test]
    async fn test_evaluate_cases_else() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(3))]);
        let cases = vec![
            Case {
                case_id: "c1".into(),
                logical_operator: LogicalOperator::And,
                conditions: vec![
                    make_condition(vec!["n", "x"], ComparisonOperator::GreaterThan, serde_json::json!(5)),
                ],
            },
        ];
        assert_eq!(evaluate_cases(&cases, &pool).await, "false");
    }

    #[tokio::test]
    async fn test_ge_le() {
        let pool = make_pool(vec![("n", "x", Segment::Integer(5))]);
        let ge = make_condition(vec!["n", "x"], ComparisonOperator::GreaterOrEqual, serde_json::json!(5));
        assert!(evaluate_condition(&ge, &pool).await);
        let le = make_condition(vec!["n", "x"], ComparisonOperator::LessOrEqual, serde_json::json!(5));
        assert!(evaluate_condition(&le, &pool).await);
    }
}
