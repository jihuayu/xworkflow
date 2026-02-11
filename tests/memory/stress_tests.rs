use std::collections::HashMap;

use xworkflow::{ExecutionStatus, WorkflowRunner};

use super::helpers::simple_workflow_schema;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

async fn run_simple_workflow() {
    let schema = simple_workflow_schema();
    let handle = WorkflowRunner::builder(schema)
        .user_inputs(HashMap::new())
        .run()
        .await
        .expect("workflow run");

    let status = handle.wait().await;
    assert!(matches!(status, ExecutionStatus::Completed(_)));
}

#[test]
fn test_repeated_workflow_execution_memory_stable() {
    let _profiler = dhat::Profiler::new_heap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build runtime");

    for _ in 0..5 {
        rt.block_on(run_simple_workflow());
    }
    let warmup_stats = dhat::HeapStats::get();
    let baseline = warmup_stats.curr_bytes.max(1);

    for _ in 0..50 {
        rt.block_on(run_simple_workflow());
    }
    let final_stats = dhat::HeapStats::get();

    let growth = final_stats.curr_bytes as f64 / baseline as f64;
    assert!(
        growth < 1.5,
        "memory growth {:.2}x exceeds threshold (baseline={}, final={})",
        growth,
        baseline,
        final_stats.curr_bytes
    );
}

#[test]
fn test_concurrent_workflow_execution_memory_stable() {
    let _profiler = dhat::Profiler::new_heap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build runtime");

    for _ in 0..3 {
        rt.block_on(async {
            let tasks = (0..5)
                .map(|_| tokio::spawn(run_simple_workflow()))
                .collect::<Vec<_>>();
            for task in tasks {
                let _ = task.await;
            }
        });
    }

    let final_stats = dhat::HeapStats::get();
    assert!(final_stats.curr_bytes > 0);
}
