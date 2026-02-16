//! LLM (Large Language Model) provider abstraction.
//!
//! This module defines the [`LlmProvider`] trait for chat-completion backends
//! and the [`LlmProviderRegistry`] that manages named providers at runtime.
//!
//! Sub-modules:
//! - [`types`] — Request / response DTOs for chat completion.
//! - [`error`] — LLM-specific error enum.
//! - [`executor`] — The node executor that drives LLM calls.
//! - [`provider`] — Concrete provider implementations (OpenAI, WASM, etc.).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

pub mod error;
pub mod executor;
pub mod provider;
pub mod question_classifier;
pub mod types;

/// A chat-completion provider (e.g. OpenAI, a WASM plugin, etc.).
///
/// Implementors must be `Send + Sync` so they can be shared across async tasks.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Unique identifier for this provider (e.g. `"openai"`).
    fn id(&self) -> &str;
    /// Metadata describing the provider and its supported models.
    fn info(&self) -> types::ProviderInfo;

    /// Perform a single chat-completion (non-streaming).
    async fn chat_completion(
        &self,
        request: types::ChatCompletionRequest,
    ) -> Result<types::ChatCompletionResponse, error::LlmError>;

    /// Perform a streaming chat-completion, sending chunks to `chunk_tx`.
    ///
    /// The returned response contains aggregate usage and the final content.
    async fn chat_completion_stream(
        &self,
        request: types::ChatCompletionRequest,
        chunk_tx: mpsc::Sender<types::StreamChunk>,
    ) -> Result<types::ChatCompletionResponse, error::LlmError>;
}

/// Registry of named [`LlmProvider`] instances.
///
/// The registry is used by the LLM node executor to resolve a provider by id.
pub struct LlmProviderRegistry {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
}

impl LlmProviderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Register a provider. Its `id()` is used as the lookup key.
    pub fn register(&mut self, provider: Arc<dyn LlmProvider>) {
        self.providers.insert(provider.id().to_string(), provider);
    }

    /// Create a shallow clone of this registry (providers are `Arc`-shared).
    pub fn clone_registry(&self) -> LlmProviderRegistry {
        let mut reg = LlmProviderRegistry::new();
        for provider in self.providers.values() {
            reg.register(provider.clone());
        }
        reg
    }

    /// Look up a provider by its id.
    pub fn get(&self, provider_id: &str) -> Option<Arc<dyn LlmProvider>> {
        self.providers.get(provider_id).cloned()
    }

    /// List metadata for all registered providers.
    pub fn list(&self) -> Vec<ProviderInfo> {
        self.providers.values().map(|p| p.info()).collect()
    }

    /// Create a registry pre-populated from environment variables.
    ///
    /// Currently reads `OPENAI_API_KEY`, `OPENAI_BASE_URL`, and `OPENAI_ORG_ID`.
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            let base_url = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into());
            let org_id = std::env::var("OPENAI_ORG_ID").ok();
            reg.register(Arc::new(provider::OpenAiProvider::new(
                provider::OpenAiConfig {
                    api_key,
                    base_url,
                    org_id,
                    default_model: "gpt-4o".into(),
                },
            )));
        }
        reg
    }

    /// Register all providers contributed by plugins.
    #[cfg(feature = "plugin-system")]
    pub fn apply_plugin_providers(&mut self, providers: &[Arc<dyn LlmProvider>]) {
        for provider in providers {
            self.register(provider.clone());
        }
    }
}

impl Default for LlmProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub use error::LlmError;
pub use executor::LlmNodeExecutor;
pub use provider::{OpenAiConfig, OpenAiProvider};
pub use question_classifier::QuestionClassifierExecutor;
pub use types::{
    ChatCompletionRequest, ChatCompletionResponse, ChatContent, ChatMessage, ChatRole, ContentPart,
    ImageUrlDetail, ModelInfo, ProviderInfo, StreamChunk, ToolCall, ToolDefinition, ToolResult,
};

#[cfg(test)]
mod tests {
    use super::types::*;
    use super::*;

    struct MockProvider {
        id: String,
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: self.id.clone(),
                name: format!("Mock {}", self.id),
                models: vec![ModelInfo {
                    id: "model-1".into(),
                    name: "Model 1".into(),
                    max_tokens: Some(4096),
                }],
            }
        }

        async fn chat_completion(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, error::LlmError> {
            unimplemented!()
        }

        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
            _chunk_tx: mpsc::Sender<StreamChunk>,
        ) -> Result<ChatCompletionResponse, error::LlmError> {
            unimplemented!()
        }
    }

    #[test]
    fn test_registry_new() {
        let reg = LlmProviderRegistry::new();
        assert!(reg.list().is_empty());
    }

    #[test]
    fn test_registry_default() {
        let reg = LlmProviderRegistry::default();
        assert!(reg.list().is_empty());
    }

    #[test]
    fn test_registry_register_and_get() {
        let mut reg = LlmProviderRegistry::new();
        reg.register(Arc::new(MockProvider { id: "mock".into() }));
        assert!(reg.get("mock").is_some());
        assert!(reg.get("unknown").is_none());
    }

    #[test]
    fn test_registry_list() {
        let mut reg = LlmProviderRegistry::new();
        reg.register(Arc::new(MockProvider { id: "p1".into() }));
        reg.register(Arc::new(MockProvider { id: "p2".into() }));
        let list = reg.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_registry_clone_registry() {
        let mut reg = LlmProviderRegistry::new();
        reg.register(Arc::new(MockProvider { id: "p1".into() }));
        let cloned = reg.clone_registry();
        assert!(cloned.get("p1").is_some());
    }

    #[test]
    fn test_registry_overwrite() {
        let mut reg = LlmProviderRegistry::new();
        reg.register(Arc::new(MockProvider { id: "p1".into() }));
        reg.register(Arc::new(MockProvider { id: "p1".into() }));
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn test_registry_with_builtins_no_api_key() {
        // Ensure OPENAI_API_KEY is not set for this test
        std::env::remove_var("OPENAI_API_KEY");
        let reg = LlmProviderRegistry::with_builtins();
        // Without API key, openai should not be registered
        assert!(reg.get("openai").is_none());
        assert_eq!(reg.list().len(), 0);
    }

    #[cfg(feature = "plugin-system")]
    #[test]
    fn test_apply_plugin_providers() {
        let mut reg = LlmProviderRegistry::new();
        let providers: Vec<Arc<dyn LlmProvider>> = vec![
            Arc::new(MockProvider {
                id: "plugin1".into(),
            }),
            Arc::new(MockProvider {
                id: "plugin2".into(),
            }),
        ];
        reg.apply_plugin_providers(&providers);
        assert_eq!(reg.list().len(), 2);
        assert!(reg.get("plugin1").is_some());
        assert!(reg.get("plugin2").is_some());
    }
}
