use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{
    LlmNodeData, MemoryConfig, PromptMessage, WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::template::render_template;

use super::types::{
    ChatCompletionRequest, ChatContent, ChatMessage, ChatRole, ContentPart, ImageUrlDetail,
    StreamChunk,
};
use super::LlmProviderRegistry;

pub struct LlmNodeExecutor {
    registry: Arc<LlmProviderRegistry>,
}

impl LlmNodeExecutor {
    pub fn new(registry: Arc<LlmProviderRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl NodeExecutor for LlmNodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<crate::dsl::schema::NodeRunResult, NodeError> {
        let data: LlmNodeData = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::SerializationError(e.to_string()))?;

        let mut messages = Vec::new();
        if let Some(memory) = &data.memory {
            append_memory_messages(&mut messages, memory, variable_pool)?;
        }

        for msg in &data.prompt_template {
            let rendered = render_template(&msg.text, variable_pool);
            let role = map_role(&msg.role)?;
            messages.push(ChatMessage {
                role,
                content: ChatContent::Text(rendered),
            });
        }

        if let Some(ctx) = &data.context {
            if ctx.enabled {
                if let Some(sel) = &ctx.variable_selector {
                    let seg = variable_pool.get(sel);
                    let ctx_text = seg.to_display_string();
                    if !ctx_text.is_empty() {
                        inject_context(&mut messages, ctx_text);
                    }
                }
            }
        }

        if let Some(vision) = &data.vision {
            if vision.enabled {
                if let Some(sel) = &vision.variable_selector {
                    let seg = variable_pool.get(sel);
                    let urls = extract_image_urls(&seg);
                    if !urls.is_empty() {
                        let parts = urls
                            .into_iter()
                            .map(|url| ContentPart::ImageUrl {
                                image_url: ImageUrlDetail {
                                    url,
                                    detail: vision.detail.clone(),
                                },
                            })
                            .collect::<Vec<_>>();
                        messages.push(ChatMessage {
                            role: ChatRole::User,
                            content: ChatContent::MultiModal(parts),
                        });
                    }
                }
            }
        }

        let provider = self
            .registry
            .get(&data.model.provider)
            .ok_or_else(|| NodeError::ExecutionError(format!(
                "Provider not found: {}",
                data.model.provider
            )))?;

        let completion = data.model.completion_params.clone();
        let request = ChatCompletionRequest {
            model: data.model.name.clone(),
            messages,
            temperature: completion.as_ref().and_then(|c| c.temperature),
            top_p: completion.as_ref().and_then(|c| c.top_p),
            max_tokens: completion.as_ref().and_then(|c| c.max_tokens),
            stream: data.stream.unwrap_or(false),
            credentials: data.model.credentials.clone().unwrap_or_default(),
        };

        let response = if request.stream {
            if let Some(event_tx) = &context.event_tx {
                let (chunk_tx, mut chunk_rx) = mpsc::channel::<StreamChunk>(64);
                let event_tx = event_tx.clone();
                let node_id = node_id.to_string();
                let exec_id = context.id_generator.next_id();

                tokio::spawn(async move {
                    while let Some(chunk) = chunk_rx.recv().await {
                        let _ = event_tx
                            .send(GraphEngineEvent::NodeRunStreamChunk {
                                id: exec_id.clone(),
                                node_id: node_id.clone(),
                                node_type: "llm".to_string(),
                                chunk: chunk.delta.clone(),
                                selector: vec![node_id.clone(), "text".to_string()],
                                is_final: chunk.finish_reason.is_some(),
                            })
                            .await;
                    }
                });

                provider
                    .chat_completion_stream(request, chunk_tx)
                    .await
                    .map_err(NodeError::from)?
            } else {
                provider
                    .chat_completion(request)
                    .await
                    .map_err(NodeError::from)?
            }
        } else {
            provider
                .chat_completion(request)
                .await
                .map_err(NodeError::from)?
        };

        let mut outputs = HashMap::new();
        outputs.insert("text".to_string(), Value::String(response.content.clone()));
        outputs.insert(
            "usage".to_string(),
            serde_json::to_value(&response.usage)
                .map_err(|e| NodeError::SerializationError(e.to_string()))?,
        );

        let mut metadata = HashMap::new();
        metadata.insert(
            "model".to_string(),
            Value::String(response.model.clone()),
        );
        metadata.insert(
            "provider".to_string(),
            Value::String(data.model.provider.clone()),
        );

        Ok(crate::dsl::schema::NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            metadata,
            llm_usage: Some(response.usage.clone()),
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

fn map_role(role: &str) -> Result<ChatRole, NodeError> {
    match role.to_lowercase().as_str() {
        "system" => Ok(ChatRole::System),
        "user" => Ok(ChatRole::User),
        "assistant" => Ok(ChatRole::Assistant),
        other => Err(NodeError::ConfigError(format!(
            "Unsupported role: {}",
            other
        ))),
    }
}

fn inject_context(messages: &mut Vec<ChatMessage>, ctx: String) {
    if let Some(system_msg) = messages.iter_mut().find(|m| matches!(m.role, ChatRole::System)) {
        match &mut system_msg.content {
            ChatContent::Text(text) => {
                text.push_str("\n");
                text.push_str(&ctx);
            }
            ChatContent::MultiModal(parts) => {
                parts.push(ContentPart::Text { text: ctx });
            }
        }
    } else {
        messages.insert(
            0,
            ChatMessage {
                role: ChatRole::System,
                content: ChatContent::Text(ctx),
            },
        );
    }
}

fn extract_image_urls(seg: &Segment) -> Vec<String> {
    match seg {
        Segment::String(s) => vec![s.clone()],
        Segment::ArrayString(arr) => arr.clone(),
        Segment::ArrayAny(arr) => arr
            .iter()
            .filter_map(|s| s.as_string())
            .collect(),
        _ => Vec::new(),
    }
}

fn append_memory_messages(
    messages: &mut Vec<ChatMessage>,
    memory: &MemoryConfig,
    pool: &VariablePool,
) -> Result<(), NodeError> {
    if !memory.enabled {
        return Ok(());
    }
    let selector = match &memory.variable_selector {
        Some(sel) => sel,
        None => return Ok(()),
    };
    let seg = pool.get(selector);
    let value = seg.to_value();
    if let Value::Array(arr) = value {
        let start_index = memory
            .window_size
            .and_then(|w| if w > 0 { Some(w as usize) } else { None })
            .map(|w| arr.len().saturating_sub(w))
            .unwrap_or(0);
        for item in arr.into_iter().skip(start_index) {
            let prompt: PromptMessage = serde_json::from_value(item)
                .map_err(|e| NodeError::SerializationError(e.to_string()))?;
            let role = map_role(&prompt.role)?;
            messages.push(ChatMessage {
                role,
                content: ChatContent::Text(prompt.text),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{LlmError, LlmProvider};
    use crate::llm::types::{ChatCompletionResponse, ProviderInfo};
    use crate::dsl::schema::LlmUsage;
    use tokio::sync::mpsc;

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
    async fn test_llm_node_executor_basic() {
        let mut registry = LlmProviderRegistry::new();
        registry.register(Arc::new(MockProvider));
        let executor = LlmNodeExecutor::new(Arc::new(registry));

        let config = serde_json::json!({
            "model": { "provider": "mock", "name": "mock" },
            "prompt_template": [
                { "role": "user", "text": "Hello {{#start.query#}}" }
            ]
        });

        let mut pool = VariablePool::new();
        pool.set(&["start".to_string(), "query".to_string()], Segment::String("world".into()));

        let context = RuntimeContext::default();
        let result = executor.execute("llm1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("text"), Some(&Value::String("hello".into())));
    }
}
