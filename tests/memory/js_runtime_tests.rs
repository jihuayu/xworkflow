use std::time::Duration;

use serde_json::json;

use super::helpers::wait_for_condition;
use xworkflow::nodes::data_transform::spawn_js_stream_runtime_with_exit_flag;

#[tokio::test]
async fn test_js_runtime_drop_without_shutdown() {
    let code = "function main(inputs) { return inputs; }";
    let inputs = json!({"x": 1});

    let (_initial, _has_callbacks, runtime, exit_flag) =
        spawn_js_stream_runtime_with_exit_flag(code.into(), inputs, vec![])
            .await
            .expect("spawn runtime");

    drop(runtime);

    wait_for_condition(
        "js runtime exit",
        Duration::from_secs(2),
        Duration::from_millis(50),
        || exit_flag.load(std::sync::atomic::Ordering::SeqCst),
    )
    .await;
}

#[tokio::test]
async fn test_js_runtime_explicit_shutdown() {
    let code = "function main(inputs) { return inputs; }";
    let inputs = json!({"x": 1});

    let (_initial, _has_callbacks, runtime, exit_flag) =
        spawn_js_stream_runtime_with_exit_flag(code.into(), inputs, vec![])
            .await
            .expect("spawn runtime");

    runtime.shutdown().await;

    wait_for_condition(
        "js runtime shutdown",
        Duration::from_secs(2),
        Duration::from_millis(50),
        || exit_flag.load(std::sync::atomic::Ordering::SeqCst),
    )
    .await;
}
