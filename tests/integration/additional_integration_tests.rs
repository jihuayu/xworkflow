#[cfg(all(
    feature = "compiler",
    feature = "builtin-core-nodes",
    feature = "builtin-transform-nodes",
))]
mod workflow_execution_tests {
    use xworkflow::compiler::WorkflowCompiler;
    use xworkflow::dsl::DslFormat;
    use xworkflow::core::dispatcher::Dispatcher;
    use xworkflow::core::workflow_context::WorkflowContext;
    use std::collections::HashMap;
    use serde_json::json;

    #[tokio::test]
    async fn test_execute_simple_workflow() {
        let workflow = r#"
        {
            "nodes": [
                {"id": "start", "type": "start"},
                {
                    "id": "template1",
                    "type": "template",
                    "data": {
                        "template": "Hello {{name}}",
                        "output_variable": "greeting"
                    }
                },
                {
                    "id": "answer",
                    "type": "answer",
                    "data": {
                        "answer": "{{greeting}}"
                    }
                },
                {"id": "end", "type": "end", "data": {"outputs": []}}
            ],
            "edges": [
                {"source": "start", "target": "template1"},
                {"source": "template1", "target": "answer"},
                {"source": "answer", "target": "end"}
            ]
        }
        "#;
        
        let compiler = WorkflowCompiler::new();
        let compiled = compiler.compile(workflow, DslFormat::Json).unwrap();
        
        let mut inputs = HashMap::new();
        inputs.insert("name".to_string(), json!("World"));
        
        let dispatcher = Dispatcher::new(compiled);
        let context = WorkflowContext::new();
        
        let result = dispatcher.run(inputs, &context).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_ifelse_workflow() {
        let workflow = r#"
        {
            "nodes": [
                {"id": "start", "type": "start"},
                {
                    "id": "ifelse",
                    "type": "if-else",
                    "data": {
                        "cases": [
                            {
                                "conditions": [
                                    {
                                        "variable_selector": ["value"],
                                        "operator": "equal",
                                        "value": "test"
                                    }
                                ],
                                "logical_operator": "and"
                            }
                        ]
                    }
                },
                {"id": "answer_true", "type": "answer", "data": {"answer": "True branch"}},
                {"id": "answer_false", "type": "answer", "data": {"answer": "False branch"}},
                {"id": "end", "type": "end", "data": {"outputs": []}}
            ],
            "edges": [
                {"source": "start", "target": "ifelse"},
                {"source": "ifelse", "source_handle": "true", "target": "answer_true"},
                {"source": "ifelse", "source_handle": "false", "target": "answer_false"},
                {"source": "answer_true", "target": "end"},
                {"source": "answer_false", "target": "end"}
            ]
        }
        "#;
        
        let compiler = WorkflowCompiler::new();
        let compiled = compiler.compile(workflow, DslFormat::Json).unwrap();
        
        let mut inputs = HashMap::new();
        inputs.insert("value".to_string(), json!("test"));
        
        let dispatcher = Dispatcher::new(compiled);
        let context = WorkflowContext::new();
        
        let result = dispatcher.run(inputs, &context).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_aggregator_workflow() {
        let workflow = r#"
        {
            "nodes": [
                {"id": "start", "type": "start"},
                {
                    "id": "aggregator",
                    "type": "variable-aggregator",
                    "data": {
                        "variable": "result",
                        "variables": [["input1"], ["input2"]]
                    }
                },
                {"id": "end", "type": "end", "data": {"outputs": [{"variable": "result"}]}}
            ],
            "edges": [
                {"source": "start", "target": "aggregator"},
                {"source": "aggregator", "target": "end"}
            ]
        }
        "#;
        
        let compiler = WorkflowCompiler::new();
        let compiled = compiler.compile(workflow, DslFormat::Json).unwrap();
        
        let mut inputs = HashMap::new();
        inputs.insert("input1".to_string(), json!("value1"));
        inputs.insert("input2".to_string(), json!("value2"));
        
        let dispatcher = Dispatcher::new(compiled);
        let context = WorkflowContext::new();
        
        let result = dispatcher.run(inputs, &context).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_assigner_workflow() {
        let workflow = r#"
        {
            "nodes": [
                {"id": "start", "type": "start"},
                {
                    "id": "assigner",
                    "type": "variable-assigner",
                    "data": {
                        "mode": "overwrite",
                        "variables": {
                            "output": "assigned_value"
                        }
                    }
                },
                {"id": "end", "type": "end", "data": {"outputs": [{"variable": "output"}]}}
            ],
            "edges": [
                {"source": "start", "target": "assigner"},
                {"source": "assigner", "target": "end"}
            ]
        }
        "#;
        
        let compiler = WorkflowCompiler::new();
        let compiled = compiler.compile(workflow, DslFormat::Json).unwrap();
        
        let inputs = HashMap::new();
        let dispatcher = Dispatcher::new(compiled);
        let context = WorkflowContext::new();
        
        let result = dispatcher.run(inputs, &context).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_variable_pool_operations() {
        use xworkflow::core::variable_pool::{VariablePool, Segment};
        
        let pool = VariablePool::new();
        
        pool.set(&["test".to_string()], Segment::from_value(json!("value"))).await;
        let value = pool.get_resolved(&["test".to_string()]).await;
        assert_eq!(value.to_display_string(), "value");
        
        pool.set(&["nested".to_string(), "key".to_string()], Segment::from_value(json!(42))).await;
        let nested = pool.get_resolved(&["nested".to_string(), "key".to_string()]).await;
        assert_eq!(nested.to_display_string(), "42");
    }
}

#[cfg(feature = "compiler")]
mod compiler_advanced_tests {
    use xworkflow::compiler::{WorkflowCompiler, WorkflowCache, WorkflowCacheConfig};
    use xworkflow::dsl::DslFormat;
    use std::time::Duration;

    #[test]
    fn test_compiler_yaml_workflow() {
        let compiler = WorkflowCompiler::new();
        let workflow = r#"
nodes:
  - id: start
    type: start
  - id: end
    type: end
    data:
      outputs: []
edges:
  - source: start
    target: end
"#;
        let result = compiler.compile(workflow, DslFormat::Yaml);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cache_with_ttl() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: Some(Duration::from_secs(1)),
        };
        
        let cache = WorkflowCache::new(config);
        let workflow = r#"{"nodes": [{"id": "start", "type": "start"}], "edges": []}"#;
        
        let result1 = cache.get_or_compile("test", workflow, DslFormat::Json);
        assert!(result1.is_ok());
        
        std::thread::sleep(Duration::from_millis(1100));
        
        let result2 = cache.get_or_compile("test", workflow, DslFormat::Json);
        assert!(result2.is_ok());
    }

    #[test]
    fn test_cache_multiple_workflows() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 5,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
        for i in 0..10 {
            let workflow = format!(r#"{{"nodes": [{{"id": "start{}", "type": "start"}}], "edges": []}}"#, i);
            let _ = cache.get_or_compile("group", &workflow, DslFormat::Json);
        }
        
        let stats = cache.get_group_stats("group");
        assert!(stats.is_some());
        assert!(stats.unwrap().entries <= 5);
    }
}
