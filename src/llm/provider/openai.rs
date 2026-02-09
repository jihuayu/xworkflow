use std::collections::HashMap;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use crate::dsl::schema::LlmUsage;
use crate::llm::error::LlmError;
use crate::llm::types::{
    ChatCompletionRequest, ChatCompletionResponse, ChatContent, ChatMessage, ChatRole, ProviderInfo,
    StreamChunk,
};
use crate::llm::LlmProvider;

#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub base_url: String,
    pub org_id: Option<String>,
    pub default_model: String,
}

pub struct OpenAiProvider {
    config: OpenAiConfig,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    fn build_headers(&self, credentials: &HashMap<String, String>) -> Result<HeaderMap, LlmError> {
        let mut headers = HeaderMap::new();
        let api_key = credentials
            .get("api_key")
            .cloned()
            .unwrap_or_else(|| self.config.api_key.clone());
        let auth = format!("Bearer {}", api_key);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth)
                .map_err(|e| LlmError::InvalidRequest(e.to_string()))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(org) = credentials
            .get("org_id")
            .cloned()
            .or_else(|| self.config.org_id.clone())
        {
            headers.insert(
                "OpenAI-Organization",
                HeaderValue::from_str(&org)
                    .map_err(|e| LlmError::InvalidRequest(e.to_string()))?,
            );
        }
        Ok(headers)
    }

    fn resolve_base_url(&self, credentials: &HashMap<String, String>) -> String {
        credentials
            .get("base_url")
            .cloned()
            .unwrap_or_else(|| self.config.base_url.clone())
    }

    fn build_payload(&self, request: &ChatCompletionRequest, stream: bool) -> Value {
        let messages = request
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    ChatRole::System => "system",
                    ChatRole::User => "user",
                    ChatRole::Assistant => "assistant",
                };
                let content = match &m.content {
                    ChatContent::Text(text) => Value::String(text.clone()),
                    ChatContent::MultiModal(parts) => serde_json::to_value(parts)
                        .unwrap_or_else(|_| Value::Array(vec![])),
                };
                serde_json::json!({
                    "role": role,
                    "content": content,
                })
            })
            .collect::<Vec<_>>();

        let mut payload = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": stream,
        });

        if let Some(temp) = request.temperature {
            payload["temperature"] = Value::Number(serde_json::Number::from_f64(temp).unwrap());
        }
        if let Some(top_p) = request.top_p {
            payload["top_p"] = Value::Number(serde_json::Number::from_f64(top_p).unwrap());
        }
        if let Some(max_tokens) = request.max_tokens {
            payload["max_tokens"] = Value::Number(serde_json::Number::from(max_tokens));
        }
        if stream {
            payload["stream_options"] = serde_json::json!({ "include_usage": true });
        }

        payload
    }

    fn parse_usage(body: &Value) -> LlmUsage {
        let usage = body.get("usage").cloned().unwrap_or(Value::Null);
        LlmUsage {
            prompt_tokens: usage.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
            completion_tokens: usage.get("completion_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
            total_tokens: usage.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0),
            ..LlmUsage::default()
        }
    }

    fn parse_response(body: &Value) -> Result<ChatCompletionResponse, LlmError> {
        let content = body
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let finish_reason = body
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        Ok(ChatCompletionResponse {
            content,
            usage: Self::parse_usage(body),
            model,
            finish_reason,
        })
    }

    fn parse_stream_chunk(data: &str) -> Result<Option<StreamChunk>, LlmError> {
        if data.trim() == "[DONE]" {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|e| LlmError::SerializationError(e.to_string()))?;
        let delta = value
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let finish_reason = value
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let usage = value
            .get("usage")
            .map(|_| Self::parse_usage(&value));

        Ok(Some(StreamChunk {
            delta,
            finish_reason,
            usage,
        }))
    }

    fn map_error(status: u16, body: &str) -> LlmError {
        if status == 401 || status == 403 {
            return LlmError::AuthenticationError(body.to_string());
        }
        if status == 429 {
            return LlmError::RateLimitExceeded { retry_after: None };
        }
        LlmError::ApiError {
            status,
            message: body.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id().to_string(),
            name: "OpenAI".into(),
            models: vec![],
        }
    }

    async fn chat_completion(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LlmError> {
        if request.model.is_empty() {
            request.model = self.config.default_model.clone();
        }

        let headers = self.build_headers(&request.credentials)?;
        let base_url = self.resolve_base_url(&request.credentials);
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        let payload = self.build_payload(&request, false);
        let response = self
            .client
            .post(url)
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|e| LlmError::NetworkError(e.to_string()))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| LlmError::NetworkError(e.to_string()))?;

        if !status.is_success() {
            return Err(Self::map_error(status.as_u16(), &text));
        }

        let body: Value = serde_json::from_str(&text)
            .map_err(|e| LlmError::SerializationError(e.to_string()))?;
        Self::parse_response(&body)
    }

    async fn chat_completion_stream(
        &self,
        mut request: ChatCompletionRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<ChatCompletionResponse, LlmError> {
        if request.model.is_empty() {
            request.model = self.config.default_model.clone();
        }

        let headers = self.build_headers(&request.credentials)?;
        let base_url = self.resolve_base_url(&request.credentials);
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        let payload = self.build_payload(&request, true);
        let response = self
            .client
            .post(url)
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|e| LlmError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response
                .text()
                .await
                .map_err(|e| LlmError::NetworkError(e.to_string()))?;
            return Err(Self::map_error(status.as_u16(), &text));
        }

        let mut stream = response.bytes_stream().eventsource();
        let mut content = String::new();
        let mut finish_reason = None;
        let mut usage = LlmUsage::default();

        while let Some(event) = stream.next().await {
            let event = event.map_err(|e| LlmError::StreamError(e.to_string()))?;
            let data = event.data;
            if let Some(chunk) = Self::parse_stream_chunk(&data)? {
                if !chunk.delta.is_empty() {
                    content.push_str(&chunk.delta);
                }
                if chunk.finish_reason.is_some() {
                    finish_reason = chunk.finish_reason.clone();
                }
                if let Some(u) = &chunk.usage {
                    usage = u.clone();
                }
                let _ = chunk_tx.send(chunk).await;
            } else {
                break;
            }
        }

        Ok(ChatCompletionResponse {
            content,
            usage,
            model: request.model,
            finish_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;
    use tokio::sync::mpsc;

    fn base_config(base_url: String) -> OpenAiConfig {
        OpenAiConfig {
            api_key: "test-key".into(),
            base_url,
            org_id: None,
            default_model: "gpt-4o".into(),
        }
    }

    #[tokio::test]
    async fn test_openai_non_stream() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "model": "gpt-4o",
                "choices": [{"message": {"content": "hello"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}
            }"#,
            )
            .create_async()
            .await;

        let provider = OpenAiProvider::new(base_config(server.url()));
        let request = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: ChatContent::Text("hi".into()),
            }],
            temperature: None,
            top_p: None,
            max_tokens: None,
            stream: false,
            credentials: HashMap::new(),
        };

        let resp = provider.chat_completion(request).await.unwrap();
        assert_eq!(resp.content, "hello");
        assert_eq!(resp.usage.total_tokens, 3);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_openai_stream() {
        let mut server = Server::new_async().await;
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
        data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n\
        data: [DONE]\n\n";

        let mock = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;

        let provider = OpenAiProvider::new(base_config(server.url()));
        let request = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: ChatContent::Text("hi".into()),
            }],
            temperature: None,
            top_p: None,
            max_tokens: None,
            stream: true,
            credentials: HashMap::new(),
        };

        let (tx, mut rx) = mpsc::channel(8);
        let resp = provider.chat_completion_stream(request, tx).await.unwrap();
        assert_eq!(resp.content, "Hello");
        assert_eq!(resp.usage.total_tokens, 2);
        assert!(rx.recv().await.is_some());
        mock.assert_async().await;
    }
}
