//! Data types for the LLM chat-completion API.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::dsl::schema::LlmUsage;

/// Role of a chat message participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

/// Content of a chat message — plain text or multi-modal parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatContent {
    /// Plain text content.
    Text(String),
    /// Multi-modal content (text + images).
    MultiModal(Vec<ContentPart>),
}

/// A single part of multi-modal content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlDetail },
}

/// Details for an image URL reference in multi-modal content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrlDetail {
    pub url: String,
    #[serde(default)]
    pub detail: Option<String>,
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatContent,
}

/// Request payload for a chat-completion API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i32>,
    pub stream: bool,
    pub credentials: HashMap<String, String>,
}

/// Response from a chat-completion API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub content: String,
    pub usage: LlmUsage,
    pub model: String,
    pub finish_reason: Option<String>,
}

/// A single chunk in a streaming chat-completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub delta: String,
    pub finish_reason: Option<String>,
    pub usage: Option<LlmUsage>,
}

/// Metadata describing an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub models: Vec<ModelInfo>,
}

/// Information about a single model offered by a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub max_tokens: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_role_serde() {
        let json = serde_json::to_string(&ChatRole::System).unwrap();
        assert_eq!(json, "\"system\"");
        let json = serde_json::to_string(&ChatRole::User).unwrap();
        assert_eq!(json, "\"user\"");
        let json = serde_json::to_string(&ChatRole::Assistant).unwrap();
        assert_eq!(json, "\"assistant\"");
    }

    #[test]
    fn test_chat_role_deserialize() {
        let role: ChatRole = serde_json::from_str("\"system\"").unwrap();
        assert!(matches!(role, ChatRole::System));
    }

    #[test]
    fn test_chat_message_text() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: ChatContent::Text("hello".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: ChatMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized.role, ChatRole::User));
        match deserialized.content {
            ChatContent::Text(t) => assert_eq!(t, "hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn test_chat_content_multimodal() {
        let content = ChatContent::MultiModal(vec![
            ContentPart::Text { text: "hello".into() },
            ContentPart::ImageUrl {
                image_url: ImageUrlDetail {
                    url: "https://example.com/img.png".into(),
                    detail: Some("low".into()),
                },
            },
        ]);
        let json = serde_json::to_string(&content).unwrap();
        let deserialized: ChatContent = serde_json::from_str(&json).unwrap();
        match deserialized {
            ChatContent::MultiModal(parts) => assert_eq!(parts.len(), 2),
            _ => panic!("expected MultiModal"),
        }
    }

    #[test]
    fn test_chat_completion_request_serde() {
        let req = ChatCompletionRequest {
            model: "gpt-4".into(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: ChatContent::Text("hi".into()),
            }],
            temperature: Some(0.7),
            top_p: None,
            max_tokens: Some(100),
            stream: false,
            credentials: HashMap::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: ChatCompletionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(de.model, "gpt-4");
        assert_eq!(de.temperature, Some(0.7));
        assert!(!de.stream);
    }

    #[test]
    fn test_chat_completion_response_serde() {
        let resp = ChatCompletionResponse {
            content: "response".into(),
            usage: LlmUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                total_price: 0.0,
                currency: String::new(),
                latency: 0.0,
            },
            model: "gpt-4".into(),
            finish_reason: Some("stop".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let de: ChatCompletionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(de.content, "response");
        assert_eq!(de.usage.total_tokens, 30);
    }

    #[test]
    fn test_stream_chunk_serde() {
        let chunk = StreamChunk {
            delta: "tok".into(),
            finish_reason: None,
            usage: None,
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let de: StreamChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(de.delta, "tok");
        assert!(de.finish_reason.is_none());
    }

    #[test]
    fn test_provider_info_serde() {
        let info = ProviderInfo {
            id: "openai".into(),
            name: "OpenAI".into(),
            models: vec![ModelInfo {
                id: "gpt-4".into(),
                name: "GPT-4".into(),
                max_tokens: Some(8192),
            }],
        };
        let json = serde_json::to_string(&info).unwrap();
        let de: ProviderInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(de.id, "openai");
        assert_eq!(de.models.len(), 1);
        assert_eq!(de.models[0].max_tokens, Some(8192));
    }

    #[test]
    fn test_image_url_detail_default() {
        let detail = ImageUrlDetail {
            url: "https://example.com".into(),
            detail: None,
        };
        let json = serde_json::to_string(&detail).unwrap();
        let de: ImageUrlDetail = serde_json::from_str(&json).unwrap();
        assert!(de.detail.is_none());
    }
}
