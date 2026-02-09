use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::llm::provider::{OpenAiConfig, OpenAiProvider, WasmLlmProvider, WasmProviderManifest};

use self::error::LlmError;
use self::types::{ChatCompletionRequest, ChatCompletionResponse, ProviderInfo, StreamChunk};

pub mod error;
pub mod executor;
pub mod types;
pub mod provider;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn id(&self) -> &str;
    fn info(&self) -> ProviderInfo;

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LlmError>;

    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
        chunk_tx: mpsc::Sender<StreamChunk>,
    ) -> Result<ChatCompletionResponse, LlmError>;
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

    pub fn get(&self, provider_id: &str) -> Option<Arc<dyn LlmProvider>> {
        self.providers.get(provider_id).cloned()
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        self.providers.values().map(|p| p.info()).collect()
    }

    pub fn register_wasm_provider(
        &mut self,
        manifest: WasmProviderManifest,
        wasm_bytes: &[u8],
    ) -> Result<(), LlmError> {
        let provider = WasmLlmProvider::new(manifest, wasm_bytes)?;
        self.register(Arc::new(provider));
        Ok(())
    }

    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            let base_url = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into());
            let org_id = std::env::var("OPENAI_ORG_ID").ok();
            reg.register(Arc::new(OpenAiProvider::new(OpenAiConfig {
                api_key,
                base_url,
                org_id,
                default_model: "gpt-4o".into(),
            })));
        }
        reg
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
pub use types::*;
