use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("Memory not found: namespace={namespace}, key={key}")]
    NotFound { namespace: String, key: String },
    #[error("Memory access denied: namespace={namespace}")]
    AccessDenied { namespace: String },
    #[error("Memory provider error: {0}")]
    ProviderError(String),
    #[error("Memory quota exceeded: {0}")]
    QuotaExceeded(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub namespace: String,
    pub key: String,
    pub value: Value,
    pub metadata: Option<Value>,
    pub score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub namespace: String,
    pub key: Option<String>,
    pub query_text: Option<String>,
    pub filter: Option<Value>,
    pub top_k: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStoreParams {
    pub namespace: String,
    pub key: String,
    pub value: Value,
    pub metadata: Option<Value>,
}

#[async_trait]
pub trait MemoryProvider: Send + Sync {
    async fn store(&self, params: MemoryStoreParams) -> Result<(), MemoryError>;

    async fn recall(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError>;

    async fn delete(&self, namespace: &str, key: &str) -> Result<(), MemoryError>;

    async fn clear_namespace(&self, namespace: &str) -> Result<(), MemoryError>;
}
