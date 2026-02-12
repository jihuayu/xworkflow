use xworkflow::evaluator::condition::{
    evaluate_condition, Condition as EvalCondition, ConditionOperator as EvalOp,
};
use xworkflow::core::variable_pool::{VariablePool, Segment};
use serde_json::json;

#[tokio::test]
async fn test_evaluate_equal_condition() {
    let pool = VariablePool::new();
    pool.set(&["value".to_string()], Segment::from_value(json!("test"))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["value".to_string()],
        operator: EvalOp::Equal,
        value: Some("test".to_string()),
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_not_equal_condition() {
    let pool = VariablePool::new();
    pool.set(&["value".to_string()], Segment::from_value(json!("test"))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["value".to_string()],
        operator: EvalOp::NotEqual,
        value: Some("other".to_string()),
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_contains_condition() {
    let pool = VariablePool::new();
    pool.set(&["text".to_string()], Segment::from_value(json!("hello world"))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["text".to_string()],
        operator: EvalOp::Contains,
        value: Some("world".to_string()),
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_starts_with_condition() {
    let pool = VariablePool::new();
    pool.set(&["text".to_string()], Segment::from_value(json!("hello world"))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["text".to_string()],
        operator: EvalOp::StartsWith,
        value: Some("hello".to_string()),
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_ends_with_condition() {
    let pool = VariablePool::new();
    pool.set(&["text".to_string()], Segment::from_value(json!("hello world"))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["text".to_string()],
        operator: EvalOp::EndsWith,
        value: Some("world".to_string()),
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_is_empty_condition() {
    let pool = VariablePool::new();
    pool.set(&["value".to_string()], Segment::from_value(json!(""))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["value".to_string()],
        operator: EvalOp::IsEmpty,
        value: None,
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_is_not_empty_condition() {
    let pool = VariablePool::new();
    pool.set(&["value".to_string()], Segment::from_value(json!("data"))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["value".to_string()],
        operator: EvalOp::IsNotEmpty,
        value: None,
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_is_null_condition() {
    let pool = VariablePool::new();
    pool.set(&["value".to_string()], Segment::from_value(json!(null))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["value".to_string()],
        operator: EvalOp::IsNull,
        value: None,
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_is_not_null_condition() {
    let pool = VariablePool::new();
    pool.set(&["value".to_string()], Segment::from_value(json!("data"))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["value".to_string()],
        operator: EvalOp::IsNotNull,
        value: None,
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_greater_than_condition() {
    let pool = VariablePool::new();
    pool.set(&["value".to_string()], Segment::from_value(json!(10))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["value".to_string()],
        operator: EvalOp::GreaterThan,
        value: Some("5".to_string()),
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_evaluate_less_than_condition() {
    let pool = VariablePool::new();
    pool.set(&["value".to_string()], Segment::from_value(json!(5))).await;
    
    let condition = EvalCondition {
        variable_selector: vec!["value".to_string()],
        operator: EvalOp::LessThan,
        value: Some("10".to_string()),
    };
    
    let result = evaluate_condition(&condition, &pool).await;
    assert!(result.unwrap());
}
