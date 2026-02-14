use super::*;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::domain::model::SortOrder;
use crate::nodes::executor::NodeExecutor;

fn make_pool() -> VariablePool {
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&serde_json::json!([1, 2, 3])),
    );
    pool
}

#[tokio::test]
#[cfg(all(feature = "builtin-core-nodes", feature = "builtin-subgraph-nodes"))]
async fn test_list_operator_filter() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();

    let config = serde_json::json!({
        "operation": "filter",
        "input_selector": "input.items",
        "sub_graph": {
            "nodes": [
                {"id": "start", "type": "start", "data": {}},
                {"id": "end", "type": "end", "data": {
                    "outputs": [
                        {"variable": "keep", "value_selector": "item"}
                    ]
                }}
            ],
            "edges": [
                {"id": "e1", "source": "start", "target": "end"}
            ]
        },
        "output_variable": "filtered"
    });

    let result = executor
        .execute("list1", &config, &make_pool(), &context)
        .await
        .unwrap();
    assert!(result.outputs.ready().get("filtered").is_some());
}

#[tokio::test]
async fn test_list_sort_asc() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&serde_json::json!([
            {"name": "c", "val": 3},
            {"name": "a", "val": 1},
            {"name": "b", "val": 2}
        ])),
    );

    let config = serde_json::json!({
        "operation": "sort",
        "input_selector": "input.items",
        "sort_key": "val",
        "sort_order": "asc",
        "output_variable": "sorted"
    });

    let result = executor
        .execute("ls1", &config, &pool, &context)
        .await
        .unwrap();
    let sorted = result.outputs.ready().get("sorted").unwrap();
    let arr = sorted.as_array().unwrap();
    assert_eq!(arr[0]["val"], 1);
    assert_eq!(arr[2]["val"], 3);
}

#[tokio::test]
async fn test_list_sort_desc() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&serde_json::json!([
            {"name": "a", "val": 1},
            {"name": "b", "val": 2}
        ])),
    );
    let config = serde_json::json!({
        "operation": "sort",
        "input_selector": "input.items",
        "sort_key": "val",
        "sort_order": "desc",
        "output_variable": "sorted"
    });
    let result = executor
        .execute("ls2", &config, &pool, &context)
        .await
        .unwrap();
    let sorted = result.outputs.ready().get("sorted").unwrap();
    let arr = sorted.as_array().unwrap();
    assert_eq!(arr[0]["val"], 2);
}

#[tokio::test]
async fn test_list_slice() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let config = serde_json::json!({
        "operation": "slice",
        "input_selector": "input.items",
        "start": 1,
        "end": 3,
        "output_variable": "sliced"
    });
    let result = executor
        .execute("ls3", &config, &make_pool(), &context)
        .await
        .unwrap();
    let sliced = result.outputs.ready().get("sliced").unwrap();
    assert_eq!(sliced.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_list_first() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let config = serde_json::json!({
        "operation": "first",
        "input_selector": "input.items",
        "output_variable": "first_item"
    });
    let result = executor
        .execute("ls4", &config, &make_pool(), &context)
        .await
        .unwrap();
    assert_eq!(
        result.outputs.ready().get("first_item"),
        Some(&Segment::Integer(1))
    );
}

#[tokio::test]
async fn test_list_last() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let config = serde_json::json!({
        "operation": "last",
        "input_selector": "input.items",
        "output_variable": "last_item"
    });
    let result = executor
        .execute("ls5", &config, &make_pool(), &context)
        .await
        .unwrap();
    assert_eq!(
        result.outputs.ready().get("last_item"),
        Some(&Segment::Integer(3))
    );
}

#[tokio::test]
async fn test_list_flatten() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&serde_json::json!([[1, 2], [3], 4])),
    );
    let config = serde_json::json!({
        "operation": "flatten",
        "input_selector": "input.items",
        "output_variable": "flat"
    });
    let result = executor
        .execute("ls6", &config, &pool, &context)
        .await
        .unwrap();
    let flat = result.outputs.ready().get("flat").unwrap();
    assert_eq!(flat.as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn test_list_unique() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&serde_json::json!([1, 2, 1, 3, 2])),
    );
    let config = serde_json::json!({
        "operation": "unique",
        "input_selector": "input.items",
        "output_variable": "uniq"
    });
    let result = executor
        .execute("ls7", &config, &pool, &context)
        .await
        .unwrap();
    let uniq = result.outputs.ready().get("uniq").unwrap();
    assert_eq!(uniq.as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn test_list_reverse() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let config = serde_json::json!({
        "operation": "reverse",
        "input_selector": "input.items",
        "output_variable": "reversed"
    });
    let result = executor
        .execute("ls8", &config, &make_pool(), &context)
        .await
        .unwrap();
    let reversed = result.outputs.ready().get("reversed").unwrap();
    let arr = reversed.as_array().unwrap();
    assert_eq!(arr[0], 3);
    assert_eq!(arr[2], 1);
}

#[tokio::test]
async fn test_list_concat() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = make_pool();
    pool.set(
        &Selector::new("other", "list"),
        Segment::from_value(&serde_json::json!([4, 5])),
    );
    let config = serde_json::json!({
        "operation": "concat",
        "input_selector": "input.items",
        "second_input_selector": "other.list",
        "output_variable": "concated"
    });
    let result = executor
        .execute("ls9", &config, &pool, &context)
        .await
        .unwrap();
    let concated = result.outputs.ready().get("concated").unwrap();
    assert_eq!(concated.as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn test_list_length() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let config = serde_json::json!({
        "operation": "length",
        "input_selector": "input.items",
        "output_variable": "len"
    });
    let result = executor
        .execute("ls10", &config, &make_pool(), &context)
        .await
        .unwrap();
    assert_eq!(
        result.outputs.ready().get("len"),
        Some(&Segment::Integer(3))
    );
}

#[tokio::test]
async fn test_list_first_empty() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&serde_json::json!([])),
    );
    let config = serde_json::json!({
        "operation": "first",
        "input_selector": "input.items",
        "output_variable": "first_item"
    });
    let result = executor
        .execute("ls11", &config, &pool, &context)
        .await
        .unwrap();
    assert_eq!(
        result.outputs.ready().get("first_item"),
        Some(&Segment::None)
    );
}

#[tokio::test]
async fn test_list_not_array_error() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::String("not array".into()),
    );
    let config = serde_json::json!({
        "operation": "length",
        "input_selector": "input.items",
        "output_variable": "len"
    });
    let result = executor.execute("ls12", &config, &pool, &context).await;
    assert!(result.is_err());
}

#[test]
fn test_list_operation_serde_roundtrip() {
    let ops = vec![
        ListOperation::Filter,
        ListOperation::Map,
        ListOperation::Sort,
        ListOperation::Slice,
        ListOperation::First,
        ListOperation::Last,
        ListOperation::Flatten,
        ListOperation::Unique,
        ListOperation::Reverse,
        ListOperation::Concat,
        ListOperation::Reduce,
        ListOperation::Length,
    ];
    for op in ops {
        let json = serde_json::to_string(&op).unwrap();
        let back: ListOperation = serde_json::from_str(&json).unwrap();
        assert_eq!(op, back);
    }
}

#[test]
fn test_sort_order_serde() {
    let asc: SortOrder = serde_json::from_str(r#""asc""#).unwrap();
    assert_eq!(asc, SortOrder::Asc);
    let desc: SortOrder = serde_json::from_str(r#""desc""#).unwrap();
    assert_eq!(desc, SortOrder::Desc);
}

#[tokio::test]
async fn test_list_sort_missing_sort_key() {
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("src", "items"),
        Segment::from_value(&serde_json::json!([{"a":1},{"a":2}])),
    );
    let config = serde_json::json!({
        "operation": "sort",
        "input_selector": ["src", "items"],
        "output_variable": "out"
    });
    let executor = ListOperatorNodeExecutor;
    let context = RuntimeContext::default();
    let result = executor.execute("sort_err", &config, &pool, &context).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_concat_missing_second_input() {
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("src", "items"),
        Segment::from_value(&serde_json::json!([1, 2])),
    );
    let config = serde_json::json!({
        "operation": "concat",
        "input_selector": ["src", "items"],
        "output_variable": "out"
    });
    let executor = ListOperatorNodeExecutor;
    let context = RuntimeContext::default();
    let result = executor
        .execute("concat_err", &config, &pool, &context)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_concat_second_not_array() {
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("src", "items"),
        Segment::from_value(&serde_json::json!([1, 2])),
    );
    pool.set(
        &Selector::new("src", "other"),
        Segment::String("not array".into()),
    );
    let config = serde_json::json!({
        "operation": "concat",
        "input_selector": ["src", "items"],
        "second_input_selector": ["src", "other"],
        "output_variable": "out"
    });
    let executor = ListOperatorNodeExecutor;
    let context = RuntimeContext::default();
    let result = executor
        .execute("concat_type", &config, &pool, &context)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_sort_mixed_types() {
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("src", "items"),
        Segment::from_value(&serde_json::json!([
            {"name": "a", "val": 1},
            {"name": "b", "val": "string"},
            {"name": "c", "val": true}
        ])),
    );
    let config = serde_json::json!({
        "operation": "sort",
        "input_selector": ["src", "items"],
        "sort_key": "val",
        "sort_order": "asc",
        "output_variable": "out"
    });
    let executor = ListOperatorNodeExecutor;
    let context = RuntimeContext::default();
    let result = executor
        .execute("sort_mixed", &config, &pool, &context)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_list_slice_defaults() {
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("src", "items"),
        Segment::from_value(&serde_json::json!([1, 2, 3])),
    );
    let config = serde_json::json!({
        "operation": "slice",
        "input_selector": ["src", "items"],
        "output_variable": "out"
    });
    let executor = ListOperatorNodeExecutor;
    let context = RuntimeContext::default();
    let result = executor
        .execute("slice_def", &config, &pool, &context)
        .await
        .unwrap();
    let out = result.outputs.ready().get("out").unwrap();
    assert_eq!(out, &serde_json::json!([1, 2, 3]));
}

#[tokio::test]
async fn test_list_slice_start_beyond_length() {
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("src", "items"),
        Segment::from_value(&serde_json::json!([1, 2, 3])),
    );
    let config = serde_json::json!({
        "operation": "slice",
        "input_selector": ["src", "items"],
        "start": 100,
        "output_variable": "out"
    });
    let executor = ListOperatorNodeExecutor;
    let context = RuntimeContext::default();
    let result = executor
        .execute("slice_far", &config, &pool, &context)
        .await
        .unwrap();
    let out = result.outputs.ready().get("out").unwrap();
    assert_eq!(out, &serde_json::json!([]));
}

#[tokio::test]
async fn test_list_operator_map() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();

    let config = serde_json::json!({
        "operation": "map",
        "input_selector": "input.items",
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
        "output_variable": "mapped"
    });

    let result = executor
        .execute("list_map", &config, &make_pool(), &context)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_list_operator_reduce() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();

    let config = serde_json::json!({
        "operation": "reduce",
        "input_selector": "input.items",
        "initial_value": 0,
        "sub_graph": {
            "nodes": [
                {"id": "start", "type": "start", "data": {}},
                {"id": "end", "type": "end", "data": {
                    "outputs": [
                        {"variable": "accumulator", "value_selector": "item"}
                    ]
                }}
            ],
            "edges": [
                {"id": "e1", "source": "start", "target": "end"}
            ]
        },
        "output_variable": "reduced"
    });

    let result = executor
        .execute("list_reduce", &config, &make_pool(), &context)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_list_operator_invalid_operation() {
    let config = serde_json::json!({
        "operation": "invalid_op",
        "input_selector": "input.items",
        "output_variable": "out"
    });

    let result: Result<ListOperatorNodeConfig, _> = serde_json::from_value(config);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_flatten_nested_arrays() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&serde_json::json!([[[1, 2]], [[3]], 4])),
    );

    let config = serde_json::json!({
        "operation": "flatten",
        "input_selector": "input.items",
        "output_variable": "flat"
    });

    let result = executor
        .execute("flatten_nested", &config, &pool, &context)
        .await
        .unwrap();
    let flat = result.outputs.ready().get("flat").unwrap();
    assert!(flat.as_array().is_some());
}

#[tokio::test]
async fn test_list_unique_complex_objects() {
    let executor = ListOperatorNodeExecutor::new();
    let context = RuntimeContext::default();
    let mut pool = VariablePool::new();
    pool.set(
        &Selector::new("input", "items"),
        Segment::from_value(&serde_json::json!([
            {"id": 1, "name": "a"},
            {"id": 2, "name": "b"},
            {"id": 1, "name": "a"}
        ])),
    );

    let config = serde_json::json!({
        "operation": "unique",
        "input_selector": "input.items",
        "output_variable": "uniq"
    });

    let result = executor
        .execute("uniq_obj", &config, &pool, &context)
        .await
        .unwrap();
    let uniq = result.outputs.ready().get("uniq").unwrap();
    assert_eq!(uniq.as_array().unwrap().len(), 2);
}
