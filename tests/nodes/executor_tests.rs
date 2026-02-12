#[cfg(all(
    feature = "builtin-core-nodes",
    feature = "builtin-transform-nodes",
))]
mod executor_integration_tests {
    use std::collections::HashMap;
    use serde_json::json;
    use xworkflow::core::variable_pool::VariablePool;
    use xworkflow::core::runtime_context::RuntimeContext;
    use xworkflow::nodes::data_transform::*;
    use xworkflow::nodes::executor::NodeExecutor;

    #[tokio::test]
    async fn test_template_executor() {
        let pool = VariablePool::new();
        pool.set(&["name".to_string()], xworkflow::core::variable_pool::Segment::from_value(json!("World"))).await;
        
        let data = TemplateTransformData {
            template: "Hello {{name}}!".to_string(),
            output_variable: "greeting".to_string(),
        };
        
        let config = serde_json::to_value(&data).unwrap();
        let context = RuntimeContext::new();
        
        let executor = TemplateTransformExecutor::new();
        let result = executor.execute("test_node", &config, &pool, &context).await;
        
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_variable_aggregator_executor() {
        let pool = VariablePool::new();
        pool.set(&["var1".to_string()], xworkflow::core::variable_pool::Segment::from_value(json!("value1"))).await;
        pool.set(&["var2".to_string()], xworkflow::core::variable_pool::Segment::from_value(json!("value2"))).await;
        
        let data = VariableAggregatorData {
            variable: "result".to_string(),
            variables: vec![
                vec!["var1".to_string()],
                vec!["var2".to_string()],
            ],
        };
        
        let config = serde_json::to_value(&data).unwrap();
        let context = RuntimeContext::new();
        
        let executor = VariableAggregatorExecutor::new();
        let result = executor.execute("test_node", &config, &pool, &context).await;
        
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_variable_assigner_executor() {
        let pool = VariablePool::new();
        
        let mut variables = HashMap::new();
        variables.insert("key".to_string(), json!("value"));
        
        let data = VariableAssignerData {
            variables,
            mode: AssignerMode::Overwrite,
        };
        
        let config = serde_json::to_value(&data).unwrap();
        let context = RuntimeContext::new();
        
        let executor = VariableAssignerExecutor::new();
        let result = executor.execute("test_node", &config, &pool, &context).await;
        
        assert!(result.is_ok());
        
        // Verify the variable was set
        let value = pool.get_resolved(&["key".to_string()]).await;
        assert_eq!(value.to_display_string(), "value");
    }
}

#[cfg(feature = "builtin-core-nodes")]
mod control_flow_executor_tests {
    use serde_json::json;
    use xworkflow::core::variable_pool::VariablePool;
    use xworkflow::core::runtime_context::RuntimeContext;
    use xworkflow::nodes::control_flow::*;
    use xworkflow::nodes::executor::NodeExecutor;
    use xworkflow::dsl::schema::*;

    #[tokio::test]
    async fn test_ifelse_executor_true_case() {
        let pool = VariablePool::new();
        pool.set(&["value".to_string()], xworkflow::core::variable_pool::Segment::from_value(json!("test"))).await;
        
        let data = IfElseData {
            cases: vec![
                IfCase {
                    conditions: vec![
                        Condition {
                            variable_selector: vec!["value".to_string()],
                            operator: ConditionOperator::Equal,
                            value: Some("test".to_string()),
                        }
                    ],
                    logical_operator: LogicalOperator::And,
                }
            ],
        };
        
        let config = serde_json::to_value(&data).unwrap();
        let context = RuntimeContext::new();
        
        let executor = IfElseExecutor::new();
        let result = executor.execute("test_node", &config, &pool, &context).await;
        
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_answer_executor() {
        let pool = VariablePool::new();
        pool.set(&["result".to_string()], xworkflow::core::variable_pool::Segment::from_value(json!("success"))).await;
        
        let data = AnswerData {
            answer: "The result is: {{result}}".to_string(),
        };
        
        let config = serde_json::to_value(&data).unwrap();
        let context = RuntimeContext::new();
        
        let executor = AnswerExecutor::new();
        let result = executor.execute("test_node", &config, &pool, &context).await;
        
        assert!(result.is_ok());
    }
}
