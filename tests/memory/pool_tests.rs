use std::sync::Arc;

use xworkflow::core::variable_pool::{Segment, SegmentObject, Selector, VariablePool};

use super::helpers::{make_realistic_pool, simple_workflow_schema, DispatcherSetup};

#[tokio::test]
async fn test_pool_memory_growth_under_repeated_execution() {
    let schema = simple_workflow_schema();
    let setup = DispatcherSetup::from_schema(&schema);

    let mut max_bytes = 0usize;
    for _ in 0..50 {
        let pool = make_realistic_pool(10);
        let initial_bytes = pool.estimate_total_bytes();
        setup.run_hot(pool).await;
        if initial_bytes > max_bytes {
            max_bytes = initial_bytes;
        }
    }

    assert!(max_bytes < 1_000_000, "pool size grew unexpectedly");
}

#[tokio::test]
async fn test_pool_clone_structural_sharing() {
    let mut pool = VariablePool::new();
    let mut entries = std::collections::HashMap::new();
    entries.insert("k".to_string(), Segment::String("v".into()));
    let obj = Arc::new(SegmentObject::new(entries));

    pool.set(&Selector::new("node", "obj"), Segment::Object(obj.clone()));
    assert_eq!(Arc::strong_count(&obj), 2);

    let mut clone = pool.clone();
    assert_eq!(Arc::strong_count(&obj), 2);

    clone.set(
        &Selector::new("node", "obj"),
        Segment::String("override".into()),
    );

    assert_eq!(Arc::strong_count(&obj), 2);
}
