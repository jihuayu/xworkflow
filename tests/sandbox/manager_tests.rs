#[cfg(any(feature = "builtin-sandbox-js", feature = "builtin-sandbox-wasm"))]
mod manager_tests {
    use std::collections::HashMap;
    use xworkflow::sandbox::{SandboxManager, SandboxManagerConfig, CodeLanguage, SandboxRequest};

    #[tokio::test]
    async fn test_sandbox_manager_creation() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        // Manager should be created successfully
        drop(manager);
    }

    #[tokio::test]
    #[cfg(feature = "builtin-sandbox-js")]
    async fn test_sandbox_manager_js_registered() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        let has_js = manager.has_sandbox(&CodeLanguage::JavaScript);
        assert!(has_js);
    }

    #[tokio::test]
    #[cfg(feature = "builtin-sandbox-wasm")]
    async fn test_sandbox_manager_wasm_registered() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        let has_wasm = manager.has_sandbox(&CodeLanguage::Wasm);
        assert!(has_wasm);
    }

    #[tokio::test]
    #[cfg(feature = "builtin-sandbox-js")]
    async fn test_sandbox_manager_execute_js() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        let code = r#"
            function main(inputs) {
                return {result: "success"};
            }
        "#;
        
        let request = SandboxRequest {
            code: code.to_string(),
            language: CodeLanguage::JavaScript,
            inputs: HashMap::new(),
            timeout: Some(std::time::Duration::from_secs(5)),
        };
        
        let result = manager.execute(request).await;
        assert!(result.is_ok());
        
        let sandbox_result = result.unwrap();
        assert!(sandbox_result.output.contains_key("result"));
    }

    #[tokio::test]
    async fn test_sandbox_manager_has_default() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        let has_default = manager.has_default_sandbox();
        
        #[cfg(any(feature = "builtin-sandbox-js", feature = "builtin-sandbox-wasm"))]
        assert!(has_default);
        
        #[cfg(not(any(feature = "builtin-sandbox-js", feature = "builtin-sandbox-wasm")))]
        assert!(!has_default);
    }

    #[tokio::test]
    #[cfg(feature = "builtin-sandbox-js")]
    async fn test_sandbox_manager_with_inputs() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        let code = r#"
            function main(inputs) {
                return {
                    sum: inputs.a + inputs.b,
                    product: inputs.a * inputs.b
                };
            }
        "#;
        
        let mut inputs = HashMap::new();
        inputs.insert("a".to_string(), serde_json::json!(5));
        inputs.insert("b".to_string(), serde_json::json!(3));
        
        let request = SandboxRequest {
            code: code.to_string(),
            language: CodeLanguage::JavaScript,
            inputs,
            timeout: Some(std::time::Duration::from_secs(5)),
        };
        
        let result = manager.execute(request).await.unwrap();
        
        assert_eq!(result.output.get("sum"), Some(&serde_json::json!(8)));
        assert_eq!(result.output.get("product"), Some(&serde_json::json!(15)));
    }

    #[tokio::test]
    #[cfg(feature = "builtin-sandbox-js")]
    async fn test_sandbox_manager_error_handling() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        let code = r#"
            function main(inputs) {
                throw new Error("Test error");
            }
        "#;
        
        let request = SandboxRequest {
            code: code.to_string(),
            language: CodeLanguage::JavaScript,
            inputs: HashMap::new(),
            timeout: Some(std::time::Duration::from_secs(5)),
        };
        
        let result = manager.execute(request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(feature = "builtin-sandbox-js")]
    async fn test_sandbox_manager_timeout() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        let code = r#"
            function main(inputs) {
                while(true) {
                    // infinite loop
                }
                return {done: true};
            }
        "#;
        
        let request = SandboxRequest {
            code: code.to_string(),
            language: CodeLanguage::JavaScript,
            inputs: HashMap::new(),
            timeout: Some(std::time::Duration::from_millis(100)),
        };
        
        let result = manager.execute(request).await;
        // Should timeout or error
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_sandbox_manager_unknown_language() {
        let config = SandboxManagerConfig::default();
        let manager = SandboxManager::new(config);
        
        // Try to check for a language that's not registered
        let has_python = manager.has_sandbox(&CodeLanguage::Python);
        
        // Python sandbox is not registered by default
        assert!(!has_python);
    }
}
