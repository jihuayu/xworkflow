use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use xworkflow::core::variable_pool::VariablePool;
use xworkflow::dsl::schema::{LlmUsage, NodeOutputs};
use xworkflow::llm::{
    ChatCompletionRequest, ChatCompletionResponse, LlmError, LlmNodeExecutor, LlmProvider,
    LlmProviderRegistry, ProviderInfo, StreamChunk,
};
use xworkflow::nodes::executor::NodeExecutor;
use xworkflow::{ExecutionStatus, RuntimeContext, WorkflowRunner};

use super::helpers::{simple_workflow_schema, wait_for_condition, with_timeout};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "mock".into(),
            name: "Mock".into(),
            models: vec![],
        }
    }

    async fn chat_completion(
        &self,
        _request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LlmError> {
        Ok(ChatCompletionResponse {
            content: "hello".into(),
            usage: LlmUsage::default(),
            model: "mock".into(),
            finish_reason: Some("stop".into()),
        })
    }

    async fn chat_completion_stream(
        &self,
        _request: ChatCompletionRequest,
        chunk_tx: mpsc::Sender<StreamChunk>,
    ) -> Result<ChatCompletionResponse, LlmError> {
        let _ = chunk_tx
            .send(StreamChunk {
                delta: "hello".into(),
                finish_reason: Some("stop".into()),
                usage: Some(LlmUsage::default()),
            })
            .await;
        Ok(ChatCompletionResponse {
            content: "hello".into(),
            usage: LlmUsage::default(),
            model: "mock".into(),
            finish_reason: Some("stop".into()),
        })
    }
}

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

#[tokio::test]
async fn test_event_collector_exits_on_channel_close() {
    let (tx, mut rx) = mpsc::channel::<()>(256);
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let exited_clone = exited.clone();

    tokio::spawn(async move {
        while let Some(_event) = rx.recv().await {}
        exited_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    drop(tx);

    wait_for_condition(
        "collector exit",
        Duration::from_secs(2),
        Duration::from_millis(50),
        || exited.load(std::sync::atomic::Ordering::SeqCst),
    )
    .await;
}

#[tokio::test]
async fn test_llm_stream_task_completes() {
    let mut registry = LlmProviderRegistry::new();
    registry.register(Arc::new(MockProvider));
    let executor = LlmNodeExecutor::new(Arc::new(registry));

    let config = serde_json::json!({
        "model": { "provider": "mock", "name": "mock" },
        "prompt_template": [
            { "role": "user", "text": "hello" }
        ],
        "stream": true
    });

    let pool = VariablePool::new();
    let context = RuntimeContext::default();
    let result = executor
        .execute("llm1", &config, &pool, &context)
        .await
        .expect("llm execute");

    let stream = match result.outputs {
        NodeOutputs::Stream { streams, .. } => streams
            .get("text")
            .cloned()
            .expect("stream output"),
        other => panic!("expected stream outputs, got {:?}", other),
    };

    let collected = with_timeout(
        "llm stream collect",
        Duration::from_secs(2),
        stream.collect(),
    )
    .await;

    assert!(collected.is_ok());
}
