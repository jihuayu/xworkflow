#[cfg(feature = "builtin-llm-node")]
mod llm_executor_tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use serde_json::json;
    use xworkflow::llm::{LlmProviderRegistry, ChatCompletionRequest, ChatMessage, ChatRole, ChatContent};
    use xworkflow::llm::executor::LlmNodeExecutor;
    use xworkflow::core::variable_pool::{VariablePool, Segment};
    use xworkflow::core::runtime_context::RuntimeContext;
    use xworkflow::dsl::schema::LlmNodeData;
    use xworkflow::nodes::executor::NodeExecutor;

    #[tokio::test]
    async fn test_llm_executor_creation() {
        let registry = Arc::new(LlmProviderRegistry::new());
        let executor = LlmNodeExecutor::new(registry);
        
        // Just verify it can be created
        drop(executor);
    }

    #[tokio::test]
    async fn test_llm_node_data_serialization() {
        let data = LlmNodeData {
            model_provider: "openai".to_string(),
            model_name: "gpt-4".to_string(),
            system_prompt: Some("You are a helpful assistant".to_string()),
            prompt_template: vec![],
            context: None,
            vision: None,
            memory: None,
            parameters: HashMap::new(),
            stream: false,
        };
        
        let json = serde_json::to_value(&data).unwrap();
        let deserialized: LlmNodeData = serde_json::from_value(json).unwrap();
        
        assert_eq!(deserialized.model_provider, "openai");
        assert_eq!(deserialized.model_name, "gpt-4");
    }

    #[tokio::test]
    async fn test_llm_chat_message_types() {
        let messages = vec![
            ChatMessage {
                role: ChatRole::System,
                content: ChatContent::Text("System message".to_string()),
            },
            ChatMessage {
                role: ChatRole::User,
                content: ChatContent::Text("User message".to_string()),
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: ChatContent::Text("Assistant message".to_string()),
            },
        ];
        
        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[0].role, ChatRole::System));
        assert!(matches!(messages[1].role, ChatRole::User));
        assert!(matches!(messages[2].role, ChatRole::Assistant));
    }

    #[tokio::test]
    async fn test_llm_request_structure() {
        let request = ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                ChatMessage {
                    role: ChatRole::User,
                    content: ChatContent::Text("Hello".to_string()),
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(100),
            stream: false,
            extra: HashMap::new(),
        };
        
        assert_eq!(request.model, "gpt-4");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.temperature, Some(0.7));
    }

    #[tokio::test]
    async fn test_llm_streaming_flag() {
        let data_stream = LlmNodeData {
            model_provider: "openai".to_string(),
            model_name: "gpt-4".to_string(),
            system_prompt: None,
            prompt_template: vec![],
            context: None,
            vision: None,
            memory: None,
            parameters: HashMap::new(),
            stream: true,
        };
        
        assert!(data_stream.stream);
        
        let data_no_stream = LlmNodeData {
            stream: false,
            ..data_stream.clone()
        };
        
        assert!(!data_no_stream.stream);
    }

    #[tokio::test]
    async fn test_llm_parameters() {
        let mut params = HashMap::new();
        params.insert("temperature".to_string(), json!(0.8));
        params.insert("top_p".to_string(), json!(0.9));
        params.insert("max_tokens".to_string(), json!(500));
        
        let data = LlmNodeData {
            model_provider: "openai".to_string(),
            model_name: "gpt-4".to_string(),
            system_prompt: None,
            prompt_template: vec![],
            context: None,
            vision: None,
            memory: None,
            parameters: params.clone(),
            stream: false,
        };
        
        assert_eq!(data.parameters.get("temperature"), Some(&json!(0.8)));
        assert_eq!(data.parameters.get("top_p"), Some(&json!(0.9)));
        assert_eq!(data.parameters.get("max_tokens"), Some(&json!(500)));
    }

    #[tokio::test]
    async fn test_llm_memory_config() {
        use xworkflow::dsl::schema::MemoryConfig;
        
        let memory = MemoryConfig {
            role_prefix: None,
            window: Some(10),
            variable_selector: vec!["conversation".to_string(), "history".to_string()],
        };
        
        let data = LlmNodeData {
            model_provider: "openai".to_string(),
            model_name: "gpt-4".to_string(),
            system_prompt: None,
            prompt_template: vec![],
            context: None,
            vision: None,
            memory: Some(memory.clone()),
            parameters: HashMap::new(),
            stream: false,
        };
        
        assert!(data.memory.is_some());
        assert_eq!(data.memory.unwrap().window, Some(10));
    }

    #[tokio::test]
    async fn test_llm_vision_config() {
        use xworkflow::dsl::schema::VisionConfig;
        
        let vision = VisionConfig {
            enabled: true,
            variable_selector: Some(vec!["images".to_string()]),
            detail: Some("auto".to_string()),
        };
        
        let data = LlmNodeData {
            model_provider: "openai".to_string(),
            model_name: "gpt-4".to_string(),
            system_prompt: None,
            prompt_template: vec![],
            context: None,
            vision: Some(vision.clone()),
            memory: None,
            parameters: HashMap::new(),
            stream: false,
        };
        
        assert!(data.vision.is_some());
        let v = data.vision.unwrap();
        assert!(v.enabled);
        assert_eq!(v.detail, Some("auto".to_string()));
    }

    #[tokio::test]
    async fn test_llm_context_config() {
        use xworkflow::dsl::schema::ContextConfig;
        
        let context = ContextConfig {
            enabled: true,
            variable_selector: Some(vec!["context".to_string(), "data".to_string()]),
            prompt_template: None,
        };
        
        let data = LlmNodeData {
            model_provider: "openai".to_string(),
            model_name: "gpt-4".to_string(),
            system_prompt: None,
            prompt_template: vec![],
            context: Some(context.clone()),
            vision: None,
            memory: None,
            parameters: HashMap::new(),
            stream: false,
        };
        
        assert!(data.context.is_some());
        assert!(data.context.unwrap().enabled);
    }
}
