//! Credential management for securely resolving secrets at runtime.

use async_trait::async_trait;
use std::collections::HashMap;

/// Errors from credential resolution.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("Credential not found: group={group_id}, provider={provider}")]
    NotFound { group_id: String, provider: String },
    #[error("Credential access denied: group={group_id}, provider={provider}")]
    AccessDenied { group_id: String, provider: String },
    #[error("Credential provider error: {0}")]
    ProviderError(String),
}

/// Provider that resolves credentials (API keys, tokens, etc.) for a resource group.
#[async_trait]
pub trait CredentialProvider: Send + Sync {
    /// Retrieve credentials for the given group and provider name.
    async fn get_credentials(
        &self,
        group_id: &str,
        provider_name: &str,
    ) -> Result<HashMap<String, String>, CredentialError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credential_error_not_found() {
        let err = CredentialError::NotFound {
            group_id: "g1".into(),
            provider: "openai".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("g1"));
        assert!(msg.contains("openai"));
        assert!(msg.contains("not found") || msg.contains("Not found"));
    }

    #[test]
    fn test_credential_error_access_denied() {
        let err = CredentialError::AccessDenied {
            group_id: "g2".into(),
            provider: "gpt".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("g2"));
        assert!(msg.contains("gpt"));
    }

    #[test]
    fn test_credential_error_provider_error() {
        let err = CredentialError::ProviderError("connection failed".into());
        assert!(err.to_string().contains("connection failed"));
    }
}
