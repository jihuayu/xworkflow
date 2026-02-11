use std::time::Duration;

use tokio::sync::mpsc;

use xworkflow::core::dispatcher::EventEmitter;
use xworkflow::core::event_bus::GraphEngineEvent;
use xworkflow::{ExecutionStatus, WorkflowRunner};

use super::helpers::{simple_workflow_schema, wait_for_condition, with_timeout};

#[tokio::test]
async fn test_event_channel_cleanup_after_workflow() {
    let schema = simple_workflow_schema();
    let handle = WorkflowRunner::builder(schema)
        .collect_events(true)
        .run()
        .await
        .expect("workflow run");

    let status = handle.wait().await;
    assert!(matches!(status, ExecutionStatus::Completed(_)));

    wait_for_condition(
        "event channel cleanup",
        Duration::from_secs(2),
        Duration::from_millis(50),
        || !handle.events_active(),
    )
    .await;
}

#[tokio::test]
async fn test_events_disabled_no_leak() {
    let schema = simple_workflow_schema();
    let handle = WorkflowRunner::builder(schema)
        .collect_events(false)
        .run()
        .await
        .expect("workflow run");

    let status = handle.wait().await;
    assert!(matches!(status, ExecutionStatus::Completed(_)));

    wait_for_condition(
        "events disabled",
        Duration::from_secs(1),
        Duration::from_millis(20),
        || !handle.events_active(),
    )
    .await;

    let events = handle.events().await;
    assert!(events.is_empty());
}

#[tokio::test]
async fn test_event_emitter_send_after_rx_drop() {
    let (tx, rx) = mpsc::channel(8);
    drop(rx);

    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let emitter = EventEmitter::new(tx, active);

    with_timeout(
        "emit after drop",
        Duration::from_secs(1),
        emitter.emit(GraphEngineEvent::GraphRunStarted),
    )
    .await;
}
