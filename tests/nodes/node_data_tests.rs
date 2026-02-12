#[cfg(all(
    feature = "builtin-core-nodes",
    feature = "builtin-transform-nodes",
))]
mod transform_tests {
    use std::collections::HashMap;
    use serde_json::json;
    use xworkflow::dsl::schema::*;

    #[test]
    fn test_template_node_data() {
        let data = TemplateTransformData {
            template: "Hello {{name}}".to_string(),
            output_variable: "output".to_string(),
        };
        
        let json = serde_json::to_value(&data).unwrap();
        let deserialized: TemplateTransformData = serde_json::from_value(json).unwrap();
        
        assert_eq!(deserialized.template, "Hello {{name}}");
        assert_eq!(deserialized.output_variable, "output");
    }

    #[test]
    fn test_variable_aggregator_data() {
        let data = VariableAggregatorData {
            variable: "result".to_string(),
            variables: vec![
                vec!["var1".to_string()],
                vec!["var2".to_string()],
            ],
        };
        
        let json = serde_json::to_value(&data).unwrap();
        let deserialized: VariableAggregatorData = serde_json::from_value(json).unwrap();
        
        assert_eq!(deserialized.variable, "result");
        assert_eq!(deserialized.variables.len(), 2);
    }

    #[test]
    fn test_variable_assigner_data() {
        let mut variables = HashMap::new();
        variables.insert("key".to_string(), json!("value"));
        
        let data = VariableAssignerData {
            variables,
            mode: AssignerMode::Overwrite,
        };
        
        assert_eq!(data.mode, AssignerMode::Overwrite);
        assert_eq!(data.variables.len(), 1);
    }

    #[test]
    fn test_assigner_modes() {
        let modes = vec![
            AssignerMode::Overwrite,
            AssignerMode::Append,
            AssignerMode::Clear,
        ];
        
        for mode in modes {
            let data = VariableAssignerData {
                variables: HashMap::new(),
                mode: mode.clone(),
            };
            
            assert_eq!(data.mode, mode);
        }
    }
}

#[cfg(feature = "builtin-core-nodes")]
mod control_flow_tests {
    use xworkflow::dsl::schema::*;

    #[test]
    fn test_ifelse_node_data() {
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
        
        assert_eq!(data.cases.len(), 1);
        assert_eq!(data.cases[0].conditions.len(), 1);
    }

    #[test]
    fn test_condition_operators() {
        let operators = vec![
            ConditionOperator::Equal,
            ConditionOperator::NotEqual,
            ConditionOperator::Contains,
            ConditionOperator::NotContains,
        ];
        
        assert_eq!(operators.len(), 4);
    }

    #[test]
    fn test_logical_operators() {
        let and_op = LogicalOperator::And;
        let or_op = LogicalOperator::Or;
        
        assert!(matches!(and_op, LogicalOperator::And));
        assert!(matches!(or_op, LogicalOperator::Or));
    }

    #[test]
    fn test_iteration_node_data() {
        let data = IterationData {
            iteration_variable: vec!["items".to_string()],
            output_variable: "results".to_string(),
            parallel: false,
            max_parallel: None,
            error_handling: ErrorHandlingStrategy::Terminated,
        };
        
        assert!(!data.parallel);
        assert_eq!(data.error_handling, ErrorHandlingStrategy::Terminated);
    }

    #[test]
    fn test_loop_node_data() {
        let data = LoopData {
            max_iterations: Some(100),
        };
        
        assert_eq!(data.max_iterations, Some(100));
    }

    #[test]
    fn test_answer_node_data() {
        let data = AnswerData {
            answer: "{{result}}".to_string(),
        };
        
        assert_eq!(data.answer, "{{result}}");
    }
}

#[cfg(feature = "builtin-http-node")]
mod http_node_tests {
    use std::collections::HashMap;
    use xworkflow::dsl::schema::*;

    #[test]
    fn test_http_node_data() {
        let data = HttpNodeData {
            url: "https://api.example.com".to_string(),
            method: HttpMethod::GET,
            headers: HashMap::new(),
            body: None,
            timeout: Some(30),
            authorization: None,
        };
        
        assert_eq!(data.method, HttpMethod::GET);
        assert_eq!(data.timeout, Some(30));
    }

    #[test]
    fn test_http_methods() {
        let methods = vec![
            HttpMethod::GET,
            HttpMethod::POST,
            HttpMethod::PUT,
            HttpMethod::DELETE,
            HttpMethod::PATCH,
        ];
        
        for method in methods {
            let data = HttpNodeData {
                url: "https://api.example.com".to_string(),
                method: method.clone(),
                headers: HashMap::new(),
                body: None,
                timeout: None,
                authorization: None,
            };
            
            assert_eq!(data.method, method);
        }
    }

    #[test]
    fn test_http_authorization() {
        let auth = HttpAuthorization {
            auth_type: AuthType::Bearer,
            token: Some("secret".to_string()),
            username: None,
            password: None,
        };
        
        assert_eq!(auth.auth_type, AuthType::Bearer);
        assert!(auth.token.is_some());
    }
}
