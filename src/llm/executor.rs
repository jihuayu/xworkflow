use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::core::event_bus::GraphEngineEvent;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, SegmentStream, Selector, VariablePool};
use crate::dsl::schema::{
    EdgeHandle, LlmNodeData, MemoryConfig, NodeOutputs, PromptMessage, WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEvent, SecurityEventType};
#[cfg(feature = "security")]
use crate::security::network::validate_url;
use crate::template::render_template_async_with_config;
#[cfg(feature = "builtin-agent-node")]
use crate::template::render_template_with_config;

use super::types::{
    ChatCompletionRequest, ChatContent, ChatMessage, ChatRole, ContentPart, ImageUrlDetail,
    StreamChunk,
};
use super::LlmProviderRegistry;

/// Node executor that drives LLM chat-completion calls.
///
/// Handles prompt rendering, context injection, vision/image URLs,
/// memory, streaming, and security auditing.
pub struct LlmNodeExecutor {
    registry: Arc<LlmProviderRegistry>,
}

impl LlmNodeExecutor {
    /// Create a new executor backed by the given provider registry.
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
            append_memory_messages(&mut messages, memory, variable_pool).await?;
        }

        for msg in &data.prompt_template {
            let rendered = render_template_async_with_config(
                &msg.text,
                variable_pool,
                context.strict_template(),
            )
            .await
            .map_err(|e| NodeError::VariableNotFound(e.selector))?;
            let role = map_role(&msg.role)?;
            messages.push(ChatMessage {
                role,
                content: ChatContent::Text(rendered),
                tool_calls: vec![],
                tool_call_id: None,
            });
        }

        if let Some(ctx) = &data.context {
            if ctx.enabled {
                if let Some(sel) = &ctx.variable_selector {
                    let seg = variable_pool.get_resolved(sel).await;
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
                    let seg = variable_pool.get_resolved(sel).await;
                    let urls = extract_image_urls(&seg);
                    if !urls.is_empty() {
                        #[cfg(feature = "security")]
                        if let Some(policy) =
                            context.security_policy().and_then(|p| p.network.as_ref())
                        {
                            for url in &urls {
                                if let Err(err) = validate_url(url, policy).await {
                                    audit_security_event(
                                        context,
                                        SecurityEventType::SsrfBlocked {
                                            url: url.clone(),
                                            reason: err.to_string(),
                                        },
                                        EventSeverity::Warning,
                                        Some(node_id.to_string()),
                                    )
                                    .await;
                                    return Err(NodeError::InputValidationError(err.to_string()));
                                }
                            }
                        }
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
                            tool_calls: vec![],
                            tool_call_id: None,
                        });
                    }
                }
            }
        }

        let provider = self.registry.get(&data.model.provider).ok_or_else(|| {
            NodeError::ExecutionError(format!("Provider not found: {}", data.model.provider))
        })?;

        let completion = data.model.completion_params.clone();
        let request = ChatCompletionRequest {
            model: data.model.name.clone(),
            messages,
            temperature: completion.as_ref().and_then(|c| c.temperature),
            top_p: completion.as_ref().and_then(|c| c.top_p),
            max_tokens: completion.as_ref().and_then(|c| c.max_tokens),
            stream: data.stream.unwrap_or(false),
            credentials: data.model.credentials.clone().unwrap_or_default(),
            tools: vec![],
        };

        #[cfg(feature = "security")]
        let request = {
            let mut req = request;
            if let (Some(provider), Some(group)) =
                (context.credential_provider(), context.resource_group())
            {
                match provider
                    .get_credentials(&group.group_id, &data.model.provider)
                    .await
                {
                    Ok(creds) => {
                        audit_security_event(
                            context,
                            SecurityEventType::CredentialAccess {
                                provider: data.model.provider.clone(),
                                success: true,
                            },
                            EventSeverity::Info,
                            Some(node_id.to_string()),
                        )
                        .await;
                        let mut merged = req.credentials.clone();
                        merged.extend(creds);
                        req.credentials = merged;
                    }
                    Err(err) => {
                        audit_security_event(
                            context,
                            SecurityEventType::CredentialAccess {
                                provider: data.model.provider.clone(),
                                success: false,
                            },
                            EventSeverity::Warning,
                            Some(node_id.to_string()),
                        )
                        .await;
                        return Err(NodeError::ExecutionError(err.to_string()));
                    }
                }
            }
            req
        };

        #[cfg(not(feature = "security"))]
        let request = request;

        if request.stream {
            let (stream, writer) = SegmentStream::channel();
            let provider = provider.clone();
            let event_tx = context.event_tx().cloned();
            let node_id = node_id.to_string();
            let exec_id = context.id_generator.next_id();

            tokio::spawn(async move {
                let (chunk_tx, mut chunk_rx) = mpsc::channel::<StreamChunk>(64);
                let writer_for_chunks = writer.clone();
                let event_tx_for_chunks = event_tx.clone();
                let node_id_for_chunks = node_id.clone();
                let exec_id_for_chunks = exec_id.clone();

                let forward = tokio::spawn(async move {
                    let mut accumulated = String::new();
                    while let Some(chunk) = chunk_rx.recv().await {
                        if !chunk.delta.is_empty() {
                            accumulated.push_str(&chunk.delta);
                            writer_for_chunks
                                .send(Segment::String(chunk.delta.clone()))
                                .await;
                        }
                        if let Some(tx) = &event_tx_for_chunks {
                            let _ = tx
                                .send(GraphEngineEvent::NodeRunStreamChunk {
                                    id: exec_id_for_chunks.clone(),
                                    node_id: node_id_for_chunks.clone(),
                                    node_type: "llm".to_string(),
                                    chunk: chunk.delta.clone(),
                                    selector: Selector::new(node_id_for_chunks.clone(), "text"),
                                    is_final: chunk.finish_reason.is_some(),
                                })
                                .await;
                        }
                    }
                    accumulated
                });

                let response = provider.chat_completion_stream(request, chunk_tx).await;
                let accumulated = forward.await.unwrap_or_default();

                match response {
                    Ok(resp) => {
                        let final_text = if resp.content.is_empty() {
                            accumulated
                        } else {
                            resp.content
                        };
                        writer.end(Segment::String(final_text)).await;
                    }
                    Err(e) => {
                        writer.error(e.to_string()).await;
                    }
                }
            });

            let mut stream_outputs = HashMap::new();
            stream_outputs.insert("text".to_string(), stream);

            return Ok(crate::dsl::schema::NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                outputs: NodeOutputs::Stream {
                    ready: HashMap::new(),
                    streams: stream_outputs,
                },
                edge_source_handle: EdgeHandle::Default,
                ..Default::default()
            });
        }

        let response = provider
            .chat_completion(request)
            .await
            .map_err(NodeError::from)?;

        let mut outputs = HashMap::new();
        outputs.insert(
            "text".to_string(),
            Segment::String(response.content.clone()),
        );
        outputs.insert(
            "usage".to_string(),
            Segment::from_value(
                &serde_json::to_value(&response.usage)
                    .map_err(|e| NodeError::SerializationError(e.to_string()))?,
            ),
        );

        let mut metadata = HashMap::new();
        metadata.insert("model".to_string(), Value::String(response.model.clone()));
        metadata.insert(
            "provider".to_string(),
            Value::String(data.model.provider.clone()),
        );

        Ok(crate::dsl::schema::NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            metadata,
            llm_usage: Some(response.usage.clone()),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

#[cfg(feature = "security")]
pub(crate) async fn audit_security_event(
    context: &RuntimeContext,
    event_type: SecurityEventType,
    severity: EventSeverity,
    node_id: Option<String>,
) {
    let logger = match context.audit_logger() {
        Some(l) => l,
        None => return,
    };
    let group_id = match context.resource_group() {
        Some(g) => g.group_id.clone(),
        None => return,
    };
    let event = SecurityEvent {
        timestamp: context.time_provider.now_timestamp(),
        group_id,
        workflow_id: None,
        node_id,
        event_type,
        details: serde_json::Value::Null,
        severity,
    };
    logger.log_event(event).await;
}

pub(crate) fn map_role(role: &str) -> Result<ChatRole, NodeError> {
    match role.to_lowercase().as_str() {
        "system" => Ok(ChatRole::System),
        "user" => Ok(ChatRole::User),
        "assistant" => Ok(ChatRole::Assistant),
        "tool" => Ok(ChatRole::Tool),
        other => Err(NodeError::ConfigError(format!(
            "Unsupported role: {}",
            other
        ))),
    }
}

fn inject_context(messages: &mut Vec<ChatMessage>, ctx: String) {
    if let Some(system_msg) = messages
        .iter_mut()
        .find(|m| matches!(m.role, ChatRole::System))
    {
        match &mut system_msg.content {
            ChatContent::Text(text) => {
                text.push('\n');
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
                tool_calls: vec![],
                tool_call_id: None,
            },
        );
    }
}

fn extract_image_urls(seg: &Segment) -> Vec<String> {
    match seg {
        Segment::String(s) => vec![s.clone()],
        Segment::ArrayString(arr) => arr.as_ref().clone(),
        Segment::Array(arr) => arr.iter().filter_map(|s| s.as_string()).collect(),
        _ => Vec::new(),
    }
}

pub(crate) async fn append_memory_messages(
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
    let seg = pool.get_resolved(selector).await;
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
                tool_calls: vec![],
                tool_call_id: None,
            });
        }
    }
    Ok(())
}

#[cfg(feature = "builtin-agent-node")]
pub(crate) fn render_arguments(
    arguments: &HashMap<String, Value>,
    variable_pool: &VariablePool,
    strict: bool,
) -> Result<Value, NodeError> {
    let mut rendered = serde_json::Map::new();
    for (key, value) in arguments {
        let rendered_value = match value {
            Value::String(text) => {
                let value = render_template_with_config(text, variable_pool, strict)
                    .map_err(|e| NodeError::VariableNotFound(e.selector))?;
                Value::String(value)
            }
            other => other.clone(),
        };
        rendered.insert(key.clone(), rendered_value);
    }
    Ok(Value::Object(rendered))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::SegmentArray;
    use crate::dsl::schema::LlmUsage;
    use crate::llm::types::{ChatCompletionResponse, ProviderInfo};
    use crate::llm::{LlmError, LlmProvider};
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
                tool_calls: vec![],
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
                tool_calls: vec![],
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
        pool.set(
            &Selector::new("start", "query"),
            Segment::String("world".into()),
        );

        let context = RuntimeContext::default();
        let result = executor
            .execute("llm1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("text"),
            Some(&Segment::String("hello".into()))
        );
    }

    #[test]
    fn test_map_role_system() {
        assert!(matches!(map_role("system").unwrap(), ChatRole::System));
        assert!(matches!(map_role("System").unwrap(), ChatRole::System));
        assert!(matches!(map_role("SYSTEM").unwrap(), ChatRole::System));
    }

    #[test]
    fn test_map_role_user() {
        assert!(matches!(map_role("user").unwrap(), ChatRole::User));
    }

    #[test]
    fn test_map_role_assistant() {
        assert!(matches!(
            map_role("assistant").unwrap(),
            ChatRole::Assistant
        ));
    }

    #[test]
    fn test_map_role_invalid() {
        let result = map_role("unknown_role");
        assert!(result.is_err());
    }

    #[test]
    fn test_inject_context_with_system_msg() {
        let mut messages = vec![
            ChatMessage {
                role: ChatRole::System,
                content: ChatContent::Text("You are a bot.".into()),
                tool_calls: vec![],
                tool_call_id: None,
            },
            ChatMessage {
                role: ChatRole::User,
                content: ChatContent::Text("Hi".into()),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];
        inject_context(&mut messages, "Context: foo".into());
        match &messages[0].content {
            ChatContent::Text(text) => {
                assert!(text.contains("You are a bot."));
                assert!(text.contains("Context: foo"));
            }
            _ => panic!("Expected Text content"),
        }
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_inject_context_without_system_msg() {
        let mut messages = vec![ChatMessage {
            role: ChatRole::User,
            content: ChatContent::Text("Hi".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }];
        inject_context(&mut messages, "Context: bar".into());
        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0].role, ChatRole::System));
        match &messages[0].content {
            ChatContent::Text(text) => assert_eq!(text, "Context: bar"),
            _ => panic!("Expected Text"),
        }
    }

    #[test]
    fn test_inject_context_multimodal_system() {
        let mut messages = vec![ChatMessage {
            role: ChatRole::System,
            content: ChatContent::MultiModal(vec![ContentPart::Text {
                text: "base".into(),
            }]),
            tool_calls: vec![],
            tool_call_id: None,
        }];
        inject_context(&mut messages, "extra context".into());
        match &messages[0].content {
            ChatContent::MultiModal(parts) => {
                assert_eq!(parts.len(), 2);
            }
            _ => panic!("Expected MultiModal"),
        }
    }

    #[test]
    fn test_extract_image_urls_string() {
        let seg = Segment::String("http://img.png".into());
        let urls = extract_image_urls(&seg);
        assert_eq!(urls, vec!["http://img.png"]);
    }

    #[test]
    fn test_extract_image_urls_array_string() {
        let seg = Segment::string_array(vec!["a.png".into(), "b.png".into()]);
        let urls = extract_image_urls(&seg);
        assert_eq!(urls, vec!["a.png", "b.png"]);
    }

    #[test]
    fn test_extract_image_urls_array() {
        let seg = Segment::Array(Arc::new(SegmentArray::new(vec![
            Segment::String("x.png".into()),
            Segment::Integer(42),
        ])));
        let urls = extract_image_urls(&seg);
        // as_string() converts Integer to string too
        assert_eq!(urls, vec!["x.png", "42"]);
    }

    #[test]
    fn test_extract_image_urls_other() {
        let seg = Segment::Integer(42);
        let urls = extract_image_urls(&seg);
        assert!(urls.is_empty());
    }

    #[tokio::test]
    async fn test_append_memory_disabled() {
        let memory = MemoryConfig {
            enabled: false,
            variable_selector: None,
            window_size: None,
        };
        let pool = VariablePool::new();
        let mut messages = Vec::new();
        append_memory_messages(&mut messages, &memory, &pool)
            .await
            .unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn test_append_memory_no_selector() {
        let memory = MemoryConfig {
            enabled: true,
            variable_selector: None,
            window_size: None,
        };
        let pool = VariablePool::new();
        let mut messages = Vec::new();
        append_memory_messages(&mut messages, &memory, &pool)
            .await
            .unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn test_append_memory_with_messages() {
        let memory = MemoryConfig {
            enabled: true,
            variable_selector: Some(Selector::new("sys", "history")),
            window_size: None,
        };
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "history"),
            Segment::from_value(&serde_json::json!([
                {"role": "user", "text": "Hello"},
                {"role": "assistant", "text": "Hi there"}
            ])),
        );
        let mut messages = Vec::new();
        append_memory_messages(&mut messages, &memory, &pool)
            .await
            .unwrap();
        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0].role, ChatRole::User));
        assert!(matches!(messages[1].role, ChatRole::Assistant));
    }

    #[tokio::test]
    async fn test_append_memory_with_window_size() {
        let memory = MemoryConfig {
            enabled: true,
            variable_selector: Some(Selector::new("sys", "history")),
            window_size: Some(1),
        };
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "history"),
            Segment::from_value(&serde_json::json!([
                {"role": "user", "text": "First"},
                {"role": "assistant", "text": "Second"},
                {"role": "user", "text": "Third"}
            ])),
        );
        let mut messages = Vec::new();
        append_memory_messages(&mut messages, &memory, &pool)
            .await
            .unwrap();
        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            ChatContent::Text(t) => assert_eq!(t, "Third"),
            _ => panic!("Expected text"),
        }
    }

    #[tokio::test]
    async fn test_llm_executor_missing_provider() {
        let registry = LlmProviderRegistry::new();
        let executor = LlmNodeExecutor::new(Arc::new(registry));

        let config = serde_json::json!({
            "model": { "provider": "nonexistent", "name": "m" },
            "prompt_template": [
                { "role": "user", "text": "hi" }
            ]
        });
        let pool = VariablePool::new();
        let context = RuntimeContext::default();
        let result = executor.execute("n1", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_llm_executor_bad_config() {
        let registry = LlmProviderRegistry::new();
        let executor = LlmNodeExecutor::new(Arc::new(registry));

        let config = serde_json::json!({"invalid": true});
        let pool = VariablePool::new();
        let context = RuntimeContext::default();
        let result = executor.execute("n1", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_llm_executor_with_context() {
        let mut registry = LlmProviderRegistry::new();
        registry.register(Arc::new(MockProvider));
        let executor = LlmNodeExecutor::new(Arc::new(registry));

        let config = serde_json::json!({
            "model": { "provider": "mock", "name": "mock" },
            "prompt_template": [
                { "role": "system", "text": "You are a bot" },
                { "role": "user", "text": "hi" }
            ],
            "context": {
                "enabled": true,
                "variable_selector": ["sys", "context"]
            }
        });

        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "context"),
            Segment::String("background info".into()),
        );
        let context = RuntimeContext::default();
        let result = executor
            .execute("n1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("text"),
            Some(&Segment::String("hello".into()))
        );
    }

    #[tokio::test]
    async fn test_llm_executor_stream_mode() {
        let mut registry = LlmProviderRegistry::new();
        registry.register(Arc::new(MockProvider));
        let executor = LlmNodeExecutor::new(Arc::new(registry));

        let config = serde_json::json!({
            "model": { "provider": "mock", "name": "mock" },
            "prompt_template": [
                { "role": "user", "text": "hi" }
            ],
            "stream": true
        });

        let pool = VariablePool::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("n1", &config, &pool, &context)
            .await
            .unwrap();
        // Stream result has stream outputs
        assert!(result.outputs.streams().unwrap().contains_key("text"));
    }

    #[tokio::test]
    async fn test_llm_executor_with_memory() {
        let mut registry = LlmProviderRegistry::new();
        registry.register(Arc::new(MockProvider));
        let executor = LlmNodeExecutor::new(Arc::new(registry));

        let config = serde_json::json!({
            "model": { "provider": "mock", "name": "mock" },
            "prompt_template": [
                { "role": "user", "text": "hi" }
            ],
            "memory": {
                "enabled": true,
                "variable_selector": ["sys", "history"],
                "window_size": 2
            }
        });

        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "history"),
            Segment::from_value(&serde_json::json!([
                {"role": "user", "text": "prev question"},
                {"role": "assistant", "text": "prev answer"}
            ])),
        );
        let context = RuntimeContext::default();
        let result = executor
            .execute("n1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("text"),
            Some(&Segment::String("hello".into()))
        );
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
            NodeOutputs::Stream { streams, .. } => {
                streams.get("text").cloned().expect("stream output")
            }
            other => panic!("expected stream outputs, got {:?}", other),
        };

        let collected = tokio::time::timeout(std::time::Duration::from_secs(2), stream.collect())
            .await
            .expect("llm stream collect timed out");

        assert!(collected.is_ok());
    }
}
