use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;


pub mod error;
pub mod executor;
pub mod types;
pub mod provider;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;
    fn info(&self) -> types::ProviderInfo;

    async fn chat_completion(
        &self,
        request: types::ChatCompletionRequest,
    ) -> Result<types::ChatCompletionResponse, error::LlmError>;

    async fn chat_completion_stream(
        &self,
        request: types::ChatCompletionRequest,
        chunk_tx: mpsc::Sender<types::StreamChunk>,
    ) -> Result<types::ChatCompletionResponse, error::LlmError>;
}

pub struct LlmProviderRegistry {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
}

impl LlmProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub fn register(&mut self, provider: Arc<dyn LlmProvider>) {
        self.providers.insert(provider.id().to_string(), provider);
    }

    pub fn clone_registry(&self) -> LlmProviderRegistry {
        let mut reg = LlmProviderRegistry::new();
        for provider in self.providers.values() {
            reg.register(provider.clone());
        }
        reg
    }

    pub fn get(&self, provider_id: &str) -> Option<Arc<dyn LlmProvider>> {
        self.providers.get(provider_id).cloned()
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        self.providers.values().map(|p| p.info()).collect()
    }

    pub fn register_wasm_provider(
        &mut self,
        manifest: provider::WasmProviderManifest,
        wasm_bytes: &[u8],
    ) -> Result<(), error::LlmError> {
        let provider = provider::WasmLlmProvider::new(manifest, wasm_bytes)?;
        self.register(Arc::new(provider));
        Ok(())
    }

    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            let base_url = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into());
            let org_id = std::env::var("OPENAI_ORG_ID").ok();
            reg.register(Arc::new(provider::OpenAiProvider::new(provider::OpenAiConfig {
                api_key,
                base_url,
                org_id,
                default_model: "gpt-4o".into(),
            })));
        }
        reg
    }

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
pub use provider::{OpenAiConfig, OpenAiProvider, WasmLlmProvider, WasmProviderManifest};
pub use types::{
    ChatCompletionRequest,
    ChatCompletionResponse,
    ChatContent,
    ChatMessage,
    ChatRole,
    ContentPart,
    ImageUrlDetail,
    ModelInfo,
    ProviderInfo,
    StreamChunk,
};
