use serde_json::Value;

use crate::dsl::ComparisonOperator;
use crate::error::NodeError;

use super::operators;
use super::type_coercion;

/// 条件求值器
pub struct ConditionEvaluator;

impl ConditionEvaluator {
    pub fn new() -> Self {
        ConditionEvaluator
    }

    /// 评估条件
    ///
    /// # 参数
    /// - `value`: 要比较的值
    /// - `operator`: 比较运算符
    /// - `compare_to`: 比较目标值
    ///
    /// # 返回
    /// - `Ok(bool)`: 评估结果
    /// - `Err(NodeError)`: 评估失败
    pub fn evaluate(
        &self,
        value: &Value,
        operator: &ComparisonOperator,
        compare_to: &Value,
    ) -> Result<bool, NodeError> {
        match operator {
            ComparisonOperator::Contains => operators::contains(value, compare_to),
            ComparisonOperator::NotContains => {
                operators::contains(value, compare_to).map(|r| !r)
            }
            ComparisonOperator::StartWith => operators::starts_with(value, compare_to),
            ComparisonOperator::EndWith => operators::ends_with(value, compare_to),
            ComparisonOperator::IsEmpty => operators::is_empty(value),
            ComparisonOperator::IsNotEmpty => operators::is_empty(value).map(|r| !r),
            ComparisonOperator::Equal => operators::equal(value, compare_to),
            ComparisonOperator::NotEqual => operators::equal(value, compare_to).map(|r| !r),
            ComparisonOperator::GreaterThan => {
                type_coercion::compare_numeric(value, compare_to, |a, b| a > b)
            }
            ComparisonOperator::LessThan => {
                type_coercion::compare_numeric(value, compare_to, |a, b| a < b)
            }
            ComparisonOperator::GreaterThanOrEqual => {
                type_coercion::compare_numeric(value, compare_to, |a, b| a >= b)
            }
            ComparisonOperator::LessThanOrEqual => {
                type_coercion::compare_numeric(value, compare_to, |a, b| a <= b)
            }
        }
    }
}

impl Default for ConditionEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_contains() {
        let evaluator = ConditionEvaluator::new();

        let result = evaluator
            .evaluate(
                &json!("hello world"),
                &ComparisonOperator::Contains,
                &json!("world"),
            )
            .unwrap();
        assert!(result);

        let result = evaluator
            .evaluate(
                &json!("hello world"),
                &ComparisonOperator::Contains,
                &json!("xyz"),
            )
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn test_not_contains() {
        let evaluator = ConditionEvaluator::new();

        let result = evaluator
            .evaluate(
                &json!("hello world"),
                &ComparisonOperator::NotContains,
                &json!("xyz"),
            )
            .unwrap();
        assert!(result);
    }

    #[test]
    fn test_equal() {
        let evaluator = ConditionEvaluator::new();

        assert!(evaluator
            .evaluate(&json!("hello"), &ComparisonOperator::Equal, &json!("hello"))
            .unwrap());

        assert!(evaluator
            .evaluate(&json!(42), &ComparisonOperator::Equal, &json!(42))
            .unwrap());

        assert!(!evaluator
            .evaluate(&json!(42), &ComparisonOperator::Equal, &json!(43))
            .unwrap());
    }

    #[test]
    fn test_greater_than() {
        let evaluator = ConditionEvaluator::new();

        assert!(evaluator
            .evaluate(&json!(100), &ComparisonOperator::GreaterThan, &json!(60))
            .unwrap());

        assert!(!evaluator
            .evaluate(&json!(50), &ComparisonOperator::GreaterThan, &json!(60))
            .unwrap());
    }

    #[test]
    fn test_is_empty() {
        let evaluator = ConditionEvaluator::new();

        assert!(evaluator
            .evaluate(&json!(""), &ComparisonOperator::IsEmpty, &json!(null))
            .unwrap());

        assert!(evaluator
            .evaluate(&json!(null), &ComparisonOperator::IsEmpty, &json!(null))
            .unwrap());

        assert!(!evaluator
            .evaluate(&json!("hello"), &ComparisonOperator::IsEmpty, &json!(null))
            .unwrap());
    }

    #[test]
    fn test_starts_with() {
        let evaluator = ConditionEvaluator::new();

        assert!(evaluator
            .evaluate(
                &json!("hello world"),
                &ComparisonOperator::StartWith,
                &json!("hello"),
            )
            .unwrap());

        assert!(!evaluator
            .evaluate(
                &json!("hello world"),
                &ComparisonOperator::StartWith,
                &json!("world"),
            )
            .unwrap());
    }

    #[test]
    fn test_numeric_string_comparison() {
        let evaluator = ConditionEvaluator::new();

        // String "100" should be coerced to number for comparison
        assert!(evaluator
            .evaluate(&json!("100"), &ComparisonOperator::GreaterThan, &json!(60))
            .unwrap());
    }
}
