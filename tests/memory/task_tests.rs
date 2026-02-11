use std::time::Duration;

use xworkflow::{ExecutionStatus, WorkflowRunner};

use super::helpers::{simple_workflow_schema, wait_for_condition};

#[tokio::test]
async fn test_scheduler_tasks_complete_after_workflow() {
    let schema = simple_workflow_schema();
    let handle = WorkflowRunner::builder(schema)
        .collect_events(true)
        .run()
        .await
        .expect("workflow run");

    let status = handle.wait().await;
    assert!(matches!(status, ExecutionStatus::Completed(_)));

    wait_for_condition(
        "event collector exit",
        Duration::from_secs(2),
        Duration::from_millis(50),
        || !handle.events_active(),
    )
    .await;
}
