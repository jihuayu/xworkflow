use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::compiler::CompiledNodeConfig;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{
    ClassifierCategory, EdgeHandle, NodeOutputs, NodeRunResult, QuestionClassifierNodeData,
    WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::template::render_template_async_with_config;

#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEventType};

use super::executor::append_memory_messages;
#[cfg(feature = "security")]
use super::executor::audit_security_event;
use super::types::{ChatCompletionRequest, ChatContent, ChatMessage, ChatRole};
use super::LlmProviderRegistry;

const DEFAULT_BRANCH_HANDLE: &str = "default";

#[derive(Debug)]
enum ClassifierDecision {
    Matched(String),
    FallbackDefault,
}

pub struct QuestionClassifierExecutor {
    registry: Arc<LlmProviderRegistry>,
}

impl QuestionClassifierExecutor {
    pub fn new(registry: Arc<LlmProviderRegistry>) -> Self {
        Self { registry }
    }

    async fn execute_with_data(
        &self,
        node_id: &str,
        data: &QuestionClassifierNodeData,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        validate_categories(&data.categories)?;

        let valid_ids: HashSet<String> = data
            .categories
            .iter()
            .map(|c| c.category_id.clone())
            .collect();

        let category_name_map: HashMap<String, String> = data
            .categories
            .iter()
            .map(|c| (c.category_id.clone(), c.category_name.clone()))
            .collect();

        let query_text = variable_pool
            .get_resolved(&data.query_variable_selector)
            .await
            .to_display_string();

        let mut messages = Vec::new();
        if let Some(memory) = &data.memory {
            append_memory_messages(&mut messages, memory, variable_pool).await?;
        }

        let rendered_instruction = match data.instruction.as_deref() {
            Some(template) => Some(
                render_template_async_with_config(
                    template,
                    variable_pool,
                    context.strict_template(),
                )
                .await
                .map_err(|e| NodeError::VariableNotFound(e.selector))?,
            ),
            None => None,
        };

        let system_prompt = build_system_prompt(&data.categories, rendered_instruction.as_deref());

        messages.push(ChatMessage {
            role: ChatRole::System,
            content: ChatContent::Text(system_prompt),
            tool_calls: vec![],
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: ChatContent::Text(query_text),
            tool_calls: vec![],
            tool_call_id: None,
        });

        let provider = self.registry.get(&data.model.provider).ok_or_else(|| {
            NodeError::ExecutionError(format!("Provider not found: {}", data.model.provider))
        })?;

        let completion = data.model.completion_params.clone();
        let request = ChatCompletionRequest {
            model: data.model.name.clone(),
            messages,
            temperature: Some(
                completion
                    .as_ref()
                    .and_then(|c| c.temperature)
                    .unwrap_or(0.0),
            ),
            top_p: completion.as_ref().and_then(|c| c.top_p),
            max_tokens: Some(
                completion
                    .as_ref()
                    .and_then(|c| c.max_tokens)
                    .unwrap_or(256),
            ),
            stream: false,
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

        let response = provider
            .chat_completion(request)
            .await
            .map_err(NodeError::from)?;

        let decision = parse_classifier_response(&response.content, &valid_ids)?;

        let (category_id, class_name, edge_source_handle) = match decision {
            ClassifierDecision::Matched(id) => {
                let class_name = category_name_map
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| id.clone());
                (id.clone(), class_name, EdgeHandle::Branch(id))
            }
            ClassifierDecision::FallbackDefault => (
                DEFAULT_BRANCH_HANDLE.to_string(),
                DEFAULT_BRANCH_HANDLE.to_string(),
                EdgeHandle::Branch(DEFAULT_BRANCH_HANDLE.to_string()),
            ),
        };

        let mut outputs = HashMap::new();
        outputs.insert("category_id".to_string(), Segment::String(category_id));
        outputs.insert("class_name".to_string(), Segment::String(class_name));

        let mut metadata = HashMap::new();
        metadata.insert("model".to_string(), Value::String(response.model.clone()));
        metadata.insert(
            "provider".to_string(),
            Value::String(data.model.provider.clone()),
        );

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            metadata,
            llm_usage: Some(response.usage),
            edge_source_handle,
            ..Default::default()
        })
    }
}

#[async_trait]
impl NodeExecutor for QuestionClassifierExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let data: QuestionClassifierNodeData = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::SerializationError(e.to_string()))?;
        self.execute_with_data(node_id, &data, variable_pool, context)
            .await
    }

    async fn execute_compiled(
        &self,
        node_id: &str,
        compiled_config: &CompiledNodeConfig,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let CompiledNodeConfig::QuestionClassifier(config) = compiled_config else {
            return self
                .execute(node_id, compiled_config.as_value(), variable_pool, context)
                .await;
        };

        self.execute_with_data(node_id, &config.parsed, variable_pool, context)
            .await
    }
}

fn validate_categories(categories: &[ClassifierCategory]) -> Result<(), NodeError> {
    if categories.is_empty() {
        return Err(NodeError::ConfigError(
            "question-classifier categories must not be empty".to_string(),
        ));
    }

    let mut ids = HashSet::new();
    for category in categories {
        if category.category_id.trim().is_empty() {
            return Err(NodeError::ConfigError(
                "question-classifier category_id must not be empty".to_string(),
            ));
        }
        if category.category_name.trim().is_empty() {
            return Err(NodeError::ConfigError(
                "question-classifier category_name must not be empty".to_string(),
            ));
        }
        if category.category_id == DEFAULT_BRANCH_HANDLE {
            return Err(NodeError::ConfigError(
                "question-classifier category_id must not be 'default'".to_string(),
            ));
        }
        if !ids.insert(category.category_id.as_str()) {
            return Err(NodeError::ConfigError(format!(
                "duplicate question-classifier category_id: {}",
                category.category_id
            )));
        }
    }

    Ok(())
}

fn build_system_prompt(categories: &[ClassifierCategory], instruction: Option<&str>) -> String {
    let mut prompt = String::from(
        "You are a text classification engine. Classify the input text into exactly one category.\n\n",
    );
    prompt.push_str("### Categories\n");
    for category in categories {
        prompt.push_str(&format!(
            "- category_id: \"{}\", category_name: \"{}\"\n",
            category.category_id, category.category_name
        ));
    }

    if let Some(instruction) = instruction {
        if !instruction.trim().is_empty() {
            prompt.push_str("\n### Instructions\n");
            prompt.push_str(instruction.trim());
            prompt.push('\n');
        }
    }

    prompt.push_str(
        "\n### Output format\nRespond ONLY with a JSON object: {\"category_id\":\"<id>\"}\nDo not include any other text or markdown formatting.",
    );
    prompt
}

fn parse_classifier_response(
    response: &str,
    valid_ids: &HashSet<String>,
) -> Result<ClassifierDecision, NodeError> {
    let trimmed = response.trim();

    if let Some(category_id) = extract_category_id_from_json(trimmed) {
        return Ok(classifier_decision_from_id(category_id, valid_ids));
    }

    if let Some(inner) = strip_markdown_code_fence(trimmed) {
        if let Some(category_id) = extract_category_id_from_json(&inner) {
            return Ok(classifier_decision_from_id(category_id, valid_ids));
        }
    }

    let bare = trimmed.trim_matches('"').trim();
    if is_bare_category_id_candidate(bare) {
        return Ok(classifier_decision_from_id(bare.to_string(), valid_ids));
    }

    Err(NodeError::ExecutionError(format!(
        "Failed to parse question-classifier response: {}",
        response
    )))
}

fn classifier_decision_from_id(
    category_id: String,
    valid_ids: &HashSet<String>,
) -> ClassifierDecision {
    if valid_ids.contains(&category_id) {
        ClassifierDecision::Matched(category_id)
    } else {
        ClassifierDecision::FallbackDefault
    }
}

fn extract_category_id_from_json(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    value
        .get("category_id")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn is_bare_category_id_candidate(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    !text.chars().any(char::is_whitespace)
}

fn strip_markdown_code_fence(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return None;
    }

    let mut lines = trimmed.lines();
    let first_line = lines.next()?;
    if !first_line.trim_start().starts_with("```") {
        return None;
    }

    let mut body = Vec::new();
    for line in lines {
        if line.trim_start().starts_with("```") {
            break;
        }
        body.push(line);
    }

    if body.is_empty() {
        return None;
    }

    Some(body.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::Selector;
    use crate::dsl::schema::LlmUsage;
    use crate::llm::error::LlmError;
    use crate::llm::types::{ChatCompletionResponse, ProviderInfo};
    use crate::llm::LlmProvider;
    use tokio::sync::mpsc;

    struct MockProvider {
        content: String,
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn id(&self) -> &str {
            "mock"
        }

        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "mock".to_string(),
                name: "Mock".to_string(),
                models: vec![],
            }
        }

        async fn chat_completion(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, LlmError> {
            Ok(ChatCompletionResponse {
                content: self.content.clone(),
                usage: LlmUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    total_price: 0.0,
                    currency: String::new(),
                    latency: 0.0,
                },
                model: "mock-model".to_string(),
                finish_reason: Some("stop".to_string()),
                tool_calls: vec![],
            })
        }

        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
            _chunk_tx: mpsc::Sender<super::super::types::StreamChunk>,
        ) -> Result<ChatCompletionResponse, LlmError> {
            Err(LlmError::StreamError(
                "stream unsupported in mock".to_string(),
            ))
        }
    }

    fn build_valid_ids() -> HashSet<String> {
        ["cat_support".to_string(), "cat_sales".to_string()]
            .into_iter()
            .collect()
    }

    #[test]
    fn parse_valid_json() {
        let decision =
            parse_classifier_response(r#"{"category_id":"cat_support"}"#, &build_valid_ids())
                .unwrap();
        assert!(matches!(decision, ClassifierDecision::Matched(id) if id == "cat_support"));
    }

    #[test]
    fn parse_markdown_json() {
        let decision = parse_classifier_response(
            "```json\n{\"category_id\":\"cat_sales\"}\n```",
            &build_valid_ids(),
        )
        .unwrap();
        assert!(matches!(decision, ClassifierDecision::Matched(id) if id == "cat_sales"));
    }

    #[test]
    fn parse_bare_id() {
        let decision = parse_classifier_response("cat_support", &build_valid_ids()).unwrap();
        assert!(matches!(decision, ClassifierDecision::Matched(id) if id == "cat_support"));
    }

    #[test]
    fn parse_malformed_fails() {
        let err = parse_classifier_response("   ", &build_valid_ids()).unwrap_err();
        assert!(err
            .to_string()
            .contains("Failed to parse question-classifier response"));
    }

    #[test]
    fn parse_sentence_fails() {
        let err =
            parse_classifier_response("I think this is support", &build_valid_ids()).unwrap_err();
        assert!(err
            .to_string()
            .contains("Failed to parse question-classifier response"));
    }

    #[tokio::test]
    async fn invalid_id_routes_default() {
        let mut registry = LlmProviderRegistry::new();
        registry.register(Arc::new(MockProvider {
            content: r#"{"category_id":"cat_unknown"}"#.to_string(),
        }));
        let executor = QuestionClassifierExecutor::new(Arc::new(registry));

        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "query"),
            Segment::String("hello".to_string()),
        );

        let config = serde_json::json!({
            "type": "question-classifier",
            "query_variable_selector": ["sys", "query"],
            "categories": [
                { "category_id": "cat_support", "category_name": "Support" },
                { "category_id": "cat_sales", "category_name": "Sales" }
            ],
            "model": {
                "provider": "mock",
                "name": "mock-model"
            }
        });

        let result = executor
            .execute("qc1", &config, &pool, &RuntimeContext::default())
            .await
            .unwrap();

        assert_eq!(result.status, WorkflowNodeExecutionStatus::Succeeded);
        assert_eq!(
            result.outputs.ready().get("category_id"),
            Some(&Segment::String("default".to_string()))
        );
        assert_eq!(
            result.outputs.ready().get("class_name"),
            Some(&Segment::String("default".to_string()))
        );
        assert_eq!(
            result.edge_source_handle,
            EdgeHandle::Branch("default".to_string())
        );
    }

    #[tokio::test]
    async fn empty_categories_config_error() {
        let mut registry = LlmProviderRegistry::new();
        registry.register(Arc::new(MockProvider {
            content: r#"{"category_id":"cat_support"}"#.to_string(),
        }));
        let executor = QuestionClassifierExecutor::new(Arc::new(registry));

        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "query"),
            Segment::String("hello".to_string()),
        );

        let config = serde_json::json!({
            "type": "question-classifier",
            "query_variable_selector": ["sys", "query"],
            "categories": [],
            "model": {
                "provider": "mock",
                "name": "mock-model"
            }
        });

        let err = executor
            .execute("qc1", &config, &pool, &RuntimeContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, NodeError::ConfigError(_)));
    }

    #[tokio::test]
    async fn missing_provider_error() {
        let executor = QuestionClassifierExecutor::new(Arc::new(LlmProviderRegistry::new()));

        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "query"),
            Segment::String("hello".to_string()),
        );

        let config = serde_json::json!({
            "type": "question-classifier",
            "query_variable_selector": ["sys", "query"],
            "categories": [
                { "category_id": "cat_support", "category_name": "Support" }
            ],
            "model": {
                "provider": "missing",
                "name": "mock-model"
            }
        });

        let err = executor
            .execute("qc1", &config, &pool, &RuntimeContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, NodeError::ExecutionError(_)));
    }

    #[tokio::test]
    async fn class_name_from_config() {
        let mut registry = LlmProviderRegistry::new();
        registry.register(Arc::new(MockProvider {
            content: r#"{"category_id":"cat_support","category_name":"Other"}"#.to_string(),
        }));
        let executor = QuestionClassifierExecutor::new(Arc::new(registry));

        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "query"),
            Segment::String("hello".to_string()),
        );

        let config = serde_json::json!({
            "type": "question-classifier",
            "query_variable_selector": ["sys", "query"],
            "categories": [
                { "category_id": "cat_support", "category_name": "Support Config Name" }
            ],
            "model": {
                "provider": "mock",
                "name": "mock-model"
            }
        });

        let result = executor
            .execute("qc1", &config, &pool, &RuntimeContext::default())
            .await
            .unwrap();

        assert_eq!(
            result.outputs.ready().get("class_name"),
            Some(&Segment::String("Support Config Name".to_string()))
        );
    }
}
