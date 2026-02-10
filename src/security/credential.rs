use async_trait::async_trait;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("Credential not found: group={group_id}, provider={provider}")]
    NotFound { group_id: String, provider: String },
    #[error("Credential access denied: group={group_id}, provider={provider}")]
    AccessDenied { group_id: String, provider: String },
    #[error("Credential provider error: {0}")]
    ProviderError(String),
}

#[async_trait]
pub trait CredentialProvider: Send + Sync {
    async fn get_credentials(
        &self,
        group_id: &str,
        provider_name: &str,
    ) -> Result<HashMap<String, String>, CredentialError>;
}