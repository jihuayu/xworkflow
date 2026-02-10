use std::collections::HashMap;
use std::sync::Arc;

use super::error::SandboxError;
use super::types::*;

use xworkflow_types::LanguageProvider;

#[cfg(feature = "builtin-sandbox-js")]
use super::BuiltinSandboxConfig;
#[cfg(feature = "builtin-sandbox-wasm")]
use super::WasmSandboxConfig;

/// Sandbox manager configuration
#[derive(Clone, Debug)]
pub struct SandboxManagerConfig {
    /// Built-in sandbox configuration
    #[cfg(feature = "builtin-sandbox-js")]
    pub builtin_config: BuiltinSandboxConfig,
    /// WASM sandbox configuration
    #[cfg(feature = "builtin-sandbox-wasm")]
    pub wasm_config: WasmSandboxConfig,
}

impl Default for SandboxManagerConfig {
    fn default() -> Self {
        Self {
            #[cfg(feature = "builtin-sandbox-js")]
            builtin_config: BuiltinSandboxConfig::default(),
            #[cfg(feature = "builtin-sandbox-wasm")]
            wasm_config: WasmSandboxConfig::default(),
        }
    }
}

/// Sandbox manager - manages multiple sandbox implementations
pub struct SandboxManager {
    /// Default sandbox
    default_sandbox: Option<Arc<dyn CodeSandbox>>,

    /// Sandboxes registered by language
    sandboxes: HashMap<CodeLanguage, Arc<dyn CodeSandbox>>,

    /// Configuration
    #[allow(dead_code)]
    config: SandboxManagerConfig,
}

impl SandboxManager {
    /// Create a new sandbox manager
    pub fn new(config: SandboxManagerConfig) -> Self {
        let mut manager = Self::new_empty(config);

        #[cfg(feature = "builtin-sandbox-js")]
        {
            let sandbox = Arc::new(super::BuiltinSandbox::new(
                manager.config.builtin_config.clone(),
            )) as Arc<dyn CodeSandbox>;
            manager.register_sandbox(CodeLanguage::JavaScript, sandbox.clone());
            if manager.default_sandbox.is_none() {
                manager.default_sandbox = Some(sandbox);
            }
        }

        #[cfg(feature = "builtin-sandbox-wasm")]
        {
            let sandbox = Arc::new(super::WasmSandbox::new(
                manager.config.wasm_config.clone(),
            )) as Arc<dyn CodeSandbox>;
            manager.register_sandbox(CodeLanguage::Wasm, sandbox.clone());
            if manager.default_sandbox.is_none() {
                manager.default_sandbox = Some(sandbox);
            }
        }

        manager
    }

    /// Create an empty sandbox manager
    pub fn new_empty(config: SandboxManagerConfig) -> Self {
        Self {
            default_sandbox: None,
            sandboxes: HashMap::new(),
            config,
        }
    }

    /// Build a sandbox manager from language providers
    pub fn from_providers(providers: &[Arc<dyn LanguageProvider>]) -> Self {
        let mut manager = Self::new_empty(SandboxManagerConfig::default());
        for provider in providers {
            manager.register_sandbox(provider.language(), provider.sandbox());
        }
        if let Some(first) = providers.first() {
            manager.default_sandbox = Some(first.sandbox());
        }
        manager
    }

    /// Register a sandbox implementation
    pub fn register_sandbox(
        &mut self,
        language: CodeLanguage,
        sandbox: Arc<dyn CodeSandbox>,
    ) {
        self.sandboxes.insert(language, sandbox);
        if self.default_sandbox.is_none() {
            self.default_sandbox = self.sandboxes.get(&language).cloned();
        }
    }

    #[cfg(feature = "plugin-system")]
    pub fn apply_plugin_sandboxes(
        &mut self,
        sandboxes: &[(CodeLanguage, Arc<dyn CodeSandbox>)],
    ) {
        for (language, sandbox) in sandboxes {
            self.register_sandbox(*language, sandbox.clone());
        }
    }

    /// Execute code (automatically select the appropriate sandbox)
    pub async fn execute(
        &self,
        request: SandboxRequest,
    ) -> Result<SandboxResult, SandboxError> {
        // Select sandbox by language
        let sandbox = self
            .sandboxes
            .get(&request.language)
            .or_else(|| self.default_sandbox.as_ref())
            .ok_or_else(|| SandboxError::UnsupportedLanguage(request.language))?;

        // Check if the sandbox supports the language
        if !sandbox.supported_languages().contains(&request.language) {
            return Err(SandboxError::UnsupportedLanguage(request.language));
        }

        // Execute
        sandbox.execute(request).await
    }

    /// Validate code
    pub async fn validate(
        &self,
        code: &str,
        language: CodeLanguage,
    ) -> Result<(), SandboxError> {
        let sandbox = self
            .sandboxes
            .get(&language)
            .or_else(|| self.default_sandbox.as_ref())
            .ok_or_else(|| SandboxError::UnsupportedLanguage(language))?;

        sandbox.validate(code, language).await
    }

    /// Perform health check for all sandboxes
    pub async fn health_check_all(&self) -> Vec<(SandboxType, HealthStatus)> {
        let mut results = Vec::new();
        let mut checked = std::collections::HashSet::new();

        for sandbox in self.sandboxes.values() {
            let st = sandbox.sandbox_type();
            if checked.contains(&st) {
                continue;
            }
            checked.insert(st);
            match sandbox.health_check().await {
                Ok(status) => results.push((st, status)),
                Err(_) => results.push((st, HealthStatus::Unhealthy)),
            }
        }

        results
    }

    /// Get sandbox by sandbox type
    pub fn get_sandbox_by_type(&self, sandbox_type: SandboxType) -> Option<&Arc<dyn CodeSandbox>> {
        self.sandboxes
            .values()
            .find(|s| s.sandbox_type() == sandbox_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_sandbox_manager_creation() {
        let manager = SandboxManager::new(SandboxManagerConfig::default());
        let health = manager.health_check_all().await;
        assert!(!health.is_empty());
        for (_st, status) in &health {
            assert_eq!(*status, HealthStatus::Healthy);
        }
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_sandbox_manager_execute_js() {
        let manager = SandboxManager::new(SandboxManagerConfig::default());

        let request = SandboxRequest {
            code: r#"function main(inputs) { return { result: inputs.value * 2 }; }"#
                .to_string(),
            language: CodeLanguage::JavaScript,
            inputs: json!({ "value": 21 }),
            config: ExecutionConfig::default(),
        };

        let result = manager.execute(request).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output["result"], json!(42));
    }

    #[tokio::test]
    async fn test_sandbox_manager_unsupported_language() {
        let manager = SandboxManager::new(SandboxManagerConfig::default());

        let request = SandboxRequest {
            code: "def main(inputs): pass".to_string(),
            language: CodeLanguage::Python,
            inputs: json!({}),
            config: ExecutionConfig::default(),
        };

        let result = manager.execute(request).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::UnsupportedLanguage(lang) => {
                assert_eq!(lang, CodeLanguage::Python);
            }
            other => panic!("Expected UnsupportedLanguage, got: {:?}", other),
        }
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_sandbox_manager_validate() {
        let manager = SandboxManager::new(SandboxManagerConfig::default());
        let result = manager
            .validate(
                "function main(inputs) { return {}; }",
                CodeLanguage::JavaScript,
            )
            .await;
        assert!(result.is_ok());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_sandbox_manager_dangerous_code() {
        let manager = SandboxManager::new(SandboxManagerConfig::default());
        let result = manager
            .validate("eval('malicious')", CodeLanguage::JavaScript)
            .await;
        assert!(result.is_err());
    }
}
