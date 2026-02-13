use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;

use xworkflow::core::sub_graph_runner::{DefaultSubGraphRunner, SubGraphRunner};
use xworkflow::core::variable_pool::{Segment, Selector, VariablePool};
use xworkflow::nodes::subgraph::{SubGraphDefinition, SubGraphEdge, SubGraphNode};
use xworkflow::RuntimeContext;

use super::helpers::with_timeout;

fn simple_sub_graph() -> SubGraphDefinition {
    SubGraphDefinition {
        nodes: vec![
            SubGraphNode {
                id: "start".to_string(),
                node_type: Some("start".to_string()),
                title: None,
                data: json!({}),
            },
            SubGraphNode {
                id: "end".to_string(),
                node_type: Some("end".to_string()),
                title: None,
                data: json!({
                    "outputs": [
                        {"variable": "value", "value_selector": "item"}
                    ]
                }),
            },
        ],
        edges: vec![SubGraphEdge {
            id: "e1".to_string(),
            source: "start".to_string(),
            target: "end".to_string(),
            source_handle: None,
        }],
    }
}

#[tokio::test]
#[cfg(feature = "builtin-core-nodes")]
async fn test_subgraph_resources_release() {
    let runner = DefaultSubGraphRunner;
    let context = RuntimeContext::default();
    let mut scope_vars = HashMap::new();
    scope_vars.insert("item".to_string(), json!(42));

    let result = with_timeout(
        "subgraph run",
        Duration::from_secs(2),
        runner.run_sub_graph(
            &simple_sub_graph(),
            &VariablePool::new(),
            scope_vars,
            &context,
        ),
    )
    .await;

    assert!(result.is_ok());
}

#[tokio::test]
#[cfg(feature = "builtin-core-nodes")]
async fn test_subgraph_pool_isolation() {
    let mut parent_pool = VariablePool::new();
    parent_pool.set(
        &Selector::new("start", "x"),
        Segment::String("hello".into()),
    );
    let before_bytes = parent_pool.estimate_total_bytes();

    let runner = DefaultSubGraphRunner;
    let context = RuntimeContext::default();
    let mut scope_vars = HashMap::new();
    scope_vars.insert("item".to_string(), json!("value"));

    let result = runner
        .run_sub_graph(&simple_sub_graph(), &parent_pool, scope_vars, &context)
        .await;
    assert!(result.is_ok());

    let after_bytes = parent_pool.estimate_total_bytes();
    assert_eq!(before_bytes, after_bytes);
}

#[tokio::test]
#[cfg(all(feature = "builtin-core-nodes", feature = "builtin-subgraph-nodes"))]
async fn test_nested_subgraph_resource_release() {
    let nested = SubGraphDefinition {
        nodes: vec![
            SubGraphNode {
                id: "start".to_string(),
                node_type: Some("start".to_string()),
                title: None,
                data: json!({}),
            },
            SubGraphNode {
                id: "iter".to_string(),
                node_type: Some("iteration".to_string()),
                title: None,
                data: json!({
                    "input_selector": "input.items",
                    "parallel": false,
                    "sub_graph": {
                        "nodes": [
                            {"id": "start", "type": "start", "data": {}},
                            {"id": "end", "type": "end", "data": {
                                "outputs": [
                                    {"variable": "value", "value_selector": "item"}
                                ]
                            }}
                        ],
                        "edges": [
                            {"id": "e1", "source": "start", "target": "end"}
                        ]
                    },
                    "output_variable": "results"
                }),
            },
            SubGraphNode {
                id: "end".to_string(),
                node_type: Some("end".to_string()),
                title: None,
                data: json!({
                    "outputs": [
                        {"variable": "results", "value_selector": ["iter", "results"]}
                    ]
                }),
            },
        ],
        edges: vec![
            SubGraphEdge {
                id: "e1".to_string(),
                source: "start".to_string(),
                target: "iter".to_string(),
                source_handle: None,
            },
            SubGraphEdge {
                id: "e2".to_string(),
                source: "iter".to_string(),
                target: "end".to_string(),
                source_handle: None,
            },
        ],
    };

    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&json!([1, 2, 3])),
    );

    let runner = DefaultSubGraphRunner;
    let context = RuntimeContext::default();
    let result = with_timeout(
        "nested subgraph run",
        Duration::from_secs(3),
        runner.run_sub_graph(&nested, &pool, HashMap::new(), &context),
    )
    .await
    .expect("nested subgraph");

    assert!(result.get("results").is_some());
}
