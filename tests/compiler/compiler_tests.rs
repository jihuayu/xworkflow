#[cfg(all(feature = "compiler", feature = "workflow-cache"))]
mod cache_tests {
    use std::sync::Arc;
    use std::time::Duration;
    use xworkflow::compiler::{WorkflowCache, WorkflowCacheConfig, WorkflowCompiler};
    use xworkflow::dsl::DslFormat;

    #[test]
    fn test_cache_basic_workflow() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: Some(Duration::from_secs(300)),
        };
        
        let cache = WorkflowCache::new(config);
        
        let workflow = r#"
        {
            "nodes": [
                {
                    "id": "start",
                    "type": "start"
                },
                {
                    "id": "end",
                    "type": "end",
                    "data": {
                        "outputs": []
                    }
                }
            ],
            "edges": [
                {
                    "source": "start",
                    "target": "end"
                }
            ]
        }
        "#;
        
        let result1 = cache.get_or_compile("test_group", workflow, DslFormat::Json);
        assert!(result1.is_ok());
        
        // Second call should hit cache
        let result2 = cache.get_or_compile("test_group", workflow, DslFormat::Json);
        assert!(result2.is_ok());
        
        // Both should return same Arc
        assert!(Arc::ptr_eq(&result1.unwrap(), &result2.unwrap()));
    }

    #[test]
    fn test_cache_different_groups() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
        let workflow = r#"
        {
            "nodes": [
                {"id": "start", "type": "start"},
                {"id": "end", "type": "end", "data": {"outputs": []}}
            ],
            "edges": [{"source": "start", "target": "end"}]
        }
        "#;
        
        let result1 = cache.get_or_compile("group1", workflow, DslFormat::Json);
        let result2 = cache.get_or_compile("group2", workflow, DslFormat::Json);
        
        assert!(result1.is_ok());
        assert!(result2.is_ok());
    }

    #[test]
    fn test_cache_eviction() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 2,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
        let workflow1 = r#"{"nodes": [{"id": "start", "type": "start"}], "edges": []}"#;
        let workflow2 = r#"{"nodes": [{"id": "start2", "type": "start"}], "edges": []}"#;
        let workflow3 = r#"{"nodes": [{"id": "start3", "type": "start"}], "edges": []}"#;
        
        let _ = cache.get_or_compile("group", workflow1, DslFormat::Json);
        let _ = cache.get_or_compile("group", workflow2, DslFormat::Json);
        let _ = cache.get_or_compile("group", workflow3, DslFormat::Json);
        
        let stats = cache.get_group_stats("group");
        assert!(stats.is_some());
        assert!(stats.unwrap().entries <= 2);
    }

    #[test]
    fn test_cache_invalidate_group() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
        let workflow = r#"{"nodes": [{"id": "start", "type": "start"}], "edges": []}"#;
        
        let _ = cache.get_or_compile("group", workflow, DslFormat::Json);
        
        let stats_before = cache.get_group_stats("group");
        assert!(stats_before.is_some());
        assert_eq!(stats_before.unwrap().entries, 1);
        
        cache.invalidate_group("group");
        
        let stats_after = cache.get_group_stats("group");
        assert!(stats_after.is_none() || stats_after.unwrap().entries == 0);
    }

    #[test]
    fn test_cache_global_stats() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
        let workflow1 = r#"{"nodes": [{"id": "start", "type": "start"}], "edges": []}"#;
        let workflow2 = r#"{"nodes": [{"id": "start2", "type": "start"}], "edges": []}"#;
        
        let _ = cache.get_or_compile("group1", workflow1, DslFormat::Json);
        let _ = cache.get_or_compile("group2", workflow2, DslFormat::Json);
        
        let stats = cache.get_stats();
        assert_eq!(stats.group_count, 2);
        assert_eq!(stats.total_entries, 2);
    }

    #[test]
    fn test_cache_clear_all() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
        let workflow = r#"{"nodes": [{"id": "start", "type": "start"}], "edges": []}"#;
        
        let _ = cache.get_or_compile("group1", workflow, DslFormat::Json);
        let _ = cache.get_or_compile("group2", workflow, DslFormat::Json);
        
        cache.clear();
        
        let stats = cache.get_stats();
        assert_eq!(stats.group_count, 0);
        assert_eq!(stats.total_entries, 0);
    }

    #[test]
    fn test_cache_yaml_format() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
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
        
        let result = cache.get_or_compile("test", workflow, DslFormat::Yaml);
        assert!(result.is_ok());
    }

    #[test]
    fn test_cache_stats() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
        let initial_stats = cache.get_stats();
        assert_eq!(initial_stats.group_count, 0);
        assert_eq!(initial_stats.total_entries, 0);
    }

    #[test]
    fn test_cache_group_stats_nonexistent() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        let stats = cache.get_group_stats("nonexistent");
        assert!(stats.is_none());
    }

    #[test]
    fn test_cache_clear_group() {
        let config = WorkflowCacheConfig {
            max_entries_per_group: 10,
            max_total_entries: 100,
            ttl: None,
        };
        
        let cache = WorkflowCache::new(config);
        
        let workflow = r#"{"nodes": [{"id": "start", "type": "start"}], "edges": []}"#;
        
        let _ = cache.get_or_compile("group1", workflow, DslFormat::Json);
        let _ = cache.get_or_compile("group2", workflow, DslFormat::Json);
        
        cache.clear_group("group1");
        
        let stats = cache.get_stats();
        assert_eq!(stats.group_count, 1);
    }
}

#[cfg(feature = "compiler")]
mod runner_tests {
    use std::collections::HashMap;
    use xworkflow::compiler::WorkflowCompiler;
    use xworkflow::dsl::DslFormat;

    #[tokio::test]
    async fn test_compile_basic_workflow() {
        let compiler = WorkflowCompiler::new();
        
        let workflow = r#"
        {
            "nodes": [
                {
                    "id": "start",
                    "type": "start"
                },
                {
                    "id": "end",
                    "type": "end",
                    "data": {
                        "outputs": []
                    }
                }
            ],
            "edges": [
                {
                    "source": "start",
                    "target": "end"
                }
            ]
        }
        "#;
        
        let result = compiler.compile(workflow, DslFormat::Json);
        assert!(result.is_ok());
        
        let compiled = result.unwrap();
        assert!(compiled.execution_graph().contains_node("start"));
        assert!(compiled.execution_graph().contains_node("end"));
    }

    #[tokio::test]
    async fn test_compile_with_variables() {
        let compiler = WorkflowCompiler::new();
        
        let workflow = r#"
        {
            "nodes": [
                {
                    "id": "start",
                    "type": "start"
                },
                {
                    "id": "end",
                    "type": "end",
                    "data": {
                        "outputs": [
                            {"variable": "result", "value": "{{sys.conversation_id}}"}
                        ]
                    }
                }
            ],
            "edges": [
                {
                    "source": "start",
                    "target": "end"
                }
            ]
        }
        "#;
        
        let result = compiler.compile(workflow, DslFormat::Json);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_compile_invalid_workflow() {
        let compiler = WorkflowCompiler::new();
        
        let invalid_json = "not valid json";
        
        let result = compiler.compile(invalid_json, DslFormat::Json);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_compile_missing_start_node() {
        let compiler = WorkflowCompiler::new();
        
        let workflow = r#"
        {
            "nodes": [
                {
                    "id": "end",
                    "type": "end",
                    "data": {
                        "outputs": []
                    }
                }
            ],
            "edges": []
        }
        "#;
        
        let result = compiler.compile(workflow, DslFormat::Json);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_compile_cycle_detection() {
        let compiler = WorkflowCompiler::new();
        
        let workflow = r#"
        {
            "nodes": [
                {"id": "start", "type": "start"},
                {"id": "node1", "type": "answer", "data": {"answer": "test"}},
                {"id": "node2", "type": "answer", "data": {"answer": "test2"}},
                {"id": "end", "type": "end", "data": {"outputs": []}}
            ],
            "edges": [
                {"source": "start", "target": "node1"},
                {"source": "node1", "target": "node2"},
                {"source": "node2", "target": "node1"},
                {"source": "node2", "target": "end"}
            ]
        }
        "#;
        
        let result = compiler.compile(workflow, DslFormat::Json);
        assert!(result.is_err());
    }
}
