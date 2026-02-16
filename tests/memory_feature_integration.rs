#![cfg(all(feature = "memory", feature = "builtin-memory-nodes", feature = "builtin-core-nodes"))]

use std::collections::HashMap;
use std::sync::Arc;

#[cfg(feature = "builtin-llm-node")]
use async_trait::async_trait;
use serde_json::{json, Value};
use xworkflow::memory::InMemoryProvider;
use xworkflow::{parse_dsl, DslFormat, ExecutionStatus, WorkflowRunner};
#[cfg(feature = "builtin-llm-node")]
use xworkflow::llm::{LlmProvider, LlmProviderRegistry};
#[cfg(feature = "builtin-llm-node")]
use xworkflow::llm::error::LlmError;
#[cfg(feature = "builtin-llm-node")]
use xworkflow::llm::types::{
  ChatCompletionRequest,
  ChatCompletionResponse,
  ChatContent,
  ChatMessage,
  ChatRole,
  ModelInfo,
  ProviderInfo,
  StreamChunk,
};
#[cfg(feature = "builtin-llm-node")]
use xworkflow::dsl::LlmUsage;
#[cfg(feature = "plugin-system")]
use xworkflow::plugin_system::{
  Plugin,
  PluginCategory,
  PluginContext,
  PluginError,
  PluginMetadata,
  PluginSource,
};

#[cfg(feature = "builtin-llm-node")]
#[derive(Default)]
struct EchoLlmProvider;

#[cfg(feature = "builtin-llm-node")]
impl EchoLlmProvider {
  fn flatten_messages(messages: &[ChatMessage]) -> String {
    messages
      .iter()
      .map(|message| {
        let role = match message.role {
          ChatRole::System => "system",
          ChatRole::User => "user",
          ChatRole::Assistant => "assistant",
        };
        format!("{}:{}", role, Self::content_to_string(&message.content))
      })
      .collect::<Vec<_>>()
      .join("\n")
  }

  fn content_to_string(content: &ChatContent) -> String {
    match content {
      ChatContent::Text(text) => text.clone(),
      ChatContent::MultiModal(parts) => format!("multimodal_parts={}", parts.len()),
    }
  }

  fn usage() -> LlmUsage {
    LlmUsage {
      prompt_tokens: 1,
      completion_tokens: 1,
      total_tokens: 2,
      total_price: 0.0,
      currency: "USD".to_string(),
      latency: 0.0,
    }
  }
}

#[cfg(feature = "builtin-llm-node")]
#[async_trait]
impl LlmProvider for EchoLlmProvider {
  fn id(&self) -> &str {
    "mock"
  }

  fn info(&self) -> ProviderInfo {
    ProviderInfo {
      id: "mock".to_string(),
      name: "Mock Echo Provider".to_string(),
      models: vec![ModelInfo {
        id: "mock-model".to_string(),
        name: "mock-model".to_string(),
        max_tokens: Some(1024),
      }],
    }
  }

  async fn chat_completion(
    &self,
    request: ChatCompletionRequest,
  ) -> Result<ChatCompletionResponse, LlmError> {
    Ok(ChatCompletionResponse {
      content: Self::flatten_messages(&request.messages),
      usage: Self::usage(),
      model: request.model,
      finish_reason: Some("stop".to_string()),
    })
  }

  async fn chat_completion_stream(
    &self,
    request: ChatCompletionRequest,
    _chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
  ) -> Result<ChatCompletionResponse, LlmError> {
    Ok(ChatCompletionResponse {
      content: Self::flatten_messages(&request.messages),
      usage: Self::usage(),
      model: request.model,
      finish_reason: Some("stop".to_string()),
    })
  }
}

#[cfg(feature = "builtin-llm-node")]
fn llm_registry_with_echo() -> Arc<LlmProviderRegistry> {
  let mut registry = LlmProviderRegistry::new();
  registry.register(Arc::new(EchoLlmProvider));
  Arc::new(registry)
}

#[cfg(feature = "plugin-system")]
struct MemoryProviderPlugin {
  metadata: PluginMetadata,
  provider: Arc<InMemoryProvider>,
}

#[cfg(feature = "plugin-system")]
impl MemoryProviderPlugin {
  fn new(provider: Arc<InMemoryProvider>) -> Self {
    Self {
      metadata: PluginMetadata {
        id: "memory-provider-plugin".to_string(),
        name: "Memory Provider Plugin".to_string(),
        version: "0.1.0".to_string(),
        category: PluginCategory::Normal,
        description: "register memory provider for tests".to_string(),
        source: PluginSource::Host,
        capabilities: None,
      },
      provider,
    }
  }
}

#[cfg(feature = "plugin-system")]
#[async_trait::async_trait]
impl Plugin for MemoryProviderPlugin {
  fn metadata(&self) -> &PluginMetadata {
    &self.metadata
  }

  async fn register(&self, context: &mut PluginContext) -> Result<(), PluginError> {
    context.register_memory_provider(self.provider.clone())
  }

  fn as_any(&self) -> &dyn std::any::Any {
    self
  }
}

fn system_vars() -> HashMap<String, Value> {
    HashMap::from([
        ("conversation_id".to_string(), json!("conv-001")),
        ("user_id".to_string(), json!("user-001")),
        ("app_id".to_string(), json!("app-001")),
    ])
}

fn user_inputs(query: &str) -> HashMap<String, Value> {
    HashMap::from([("query".to_string(), json!(query))])
}

fn completed_outputs(status: ExecutionStatus) -> HashMap<String, Value> {
    match status {
        ExecutionStatus::Completed(outputs) => outputs,
        other => panic!("expected completed status, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_memory_store_and_recall_e2e() {
    let store_yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          type: string
          required: true
  - id: store
    data:
      type: memory-store
      title: Store
      scope: conversation
      key: "k1"
      value_selector: ["start", "query"]
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: stored
          value_selector: ["store", "stored"]
edges:
  - source: start
    target: store
  - source: store
    target: end
"#;

    let recall_yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          type: string
          required: true
  - id: recall
    data:
      type: memory-recall
      title: Recall
      scope: conversation
      key: "k1"
      top_k: 5
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: count
          value_selector: ["recall", "count"]
        - variable: results
          value_selector: ["recall", "results"]
edges:
  - source: start
    target: recall
  - source: recall
    target: end
"#;

    let store_schema = parse_dsl(store_yaml, DslFormat::Yaml).expect("parse store schema");
    let recall_schema = parse_dsl(recall_yaml, DslFormat::Yaml).expect("parse recall schema");

    let memory = Arc::new(InMemoryProvider::new());

    let first = WorkflowRunner::builder(store_schema)
        .user_inputs(user_inputs("hello memory"))
        .system_vars(system_vars())
        .memory_provider(memory.clone())
        .run()
        .await
        .expect("run store workflow")
        .wait()
        .await;
    let first_outputs = completed_outputs(first);
    assert_eq!(first_outputs.get("stored"), Some(&json!(true)));

    let second = WorkflowRunner::builder(recall_schema)
        .user_inputs(user_inputs("unused"))
        .system_vars(system_vars())
        .memory_provider(memory.clone())
        .run()
        .await
        .expect("run recall workflow")
        .wait()
        .await;
    let second_outputs = completed_outputs(second);

    assert_eq!(second_outputs.get("count"), Some(&json!(1)));

    let results = second_outputs
        .get("results")
        .and_then(|v| v.as_array())
        .expect("results should be an array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].get("key"), Some(&json!("k1")));
    assert_eq!(results[0].get("value"), Some(&json!("hello memory")));
}

#[cfg(feature = "builtin-llm-node")]
#[tokio::test]
async fn test_memory_full_flow_recall_llm_store_end() {
    let workflow_yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          type: string
          required: true
  - id: recall
    data:
      type: memory-recall
      title: Recall
      scope: conversation
      key: "seed"
      top_k: 5
  - id: llm1
    data:
      type: llm
      title: LLM
      model:
        provider: mock
        name: mock-model
      prompt_template:
        - role: system
          text: "ctx={{#recall.results#}}"
        - role: user
          text: "{{#start.query#}}"
  - id: store
    data:
      type: memory-store
      title: Store
      scope: conversation
      key: "latest"
      value_selector: ["llm1", "text"]
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: text
          value_selector: ["llm1", "text"]
        - variable: stored
          value_selector: ["store", "stored"]
edges:
  - source: start
    target: recall
  - source: recall
    target: llm1
  - source: llm1
    target: store
  - source: store
    target: end
"#;

    let schema = parse_dsl(workflow_yaml, DslFormat::Yaml).expect("parse full flow schema");
    let memory = Arc::new(InMemoryProvider::new());

    use xworkflow::memory::{MemoryProvider, MemoryStoreParams};
    memory
        .store(MemoryStoreParams {
            namespace: "conv:conv-001".to_string(),
            key: "seed".to_string(),
            value: json!("seed-context"),
            metadata: None,
        })
        .await
        .expect("seed memory");

    let result = WorkflowRunner::builder(schema)
        .user_inputs(user_inputs("hello from user"))
        .system_vars(system_vars())
        .memory_provider(memory)
        .llm_providers(llm_registry_with_echo())
        .run()
        .await
        .expect("run full flow")
        .wait()
        .await;

    let outputs = completed_outputs(result);
    assert_eq!(outputs.get("stored"), Some(&json!(true)));
    let text = outputs
        .get("text")
        .and_then(|v| v.as_str())
        .expect("llm text output");
    assert!(text.contains("seed-context"), "expected recalled seed context in llm output: {text}");
    assert!(text.contains("hello from user"), "expected user input in llm output: {text}");
}

#[cfg(all(feature = "builtin-llm-node", feature = "plugin-system"))]
#[tokio::test]
async fn test_memory_provider_registered_by_plugin() {
    let workflow_yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          type: string
          required: true
  - id: store
    data:
      type: memory-store
      title: Store
      scope: conversation
      key: "plugin_key"
      value_selector: ["start", "query"]
  - id: recall
    data:
      type: memory-recall
      title: Recall
      scope: conversation
      key: "plugin_key"
      top_k: 5
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: count
          value_selector: ["recall", "count"]
edges:
  - source: start
    target: store
  - source: store
    target: recall
  - source: recall
    target: end
"#;

    let schema = parse_dsl(workflow_yaml, DslFormat::Yaml).expect("parse plugin flow schema");
    let memory = Arc::new(InMemoryProvider::new());
    let plugin = MemoryProviderPlugin::new(memory);

    let result = WorkflowRunner::builder(schema)
        .user_inputs(user_inputs("from plugin"))
        .system_vars(system_vars())
        .plugin(Box::new(plugin))
        .run()
        .await
        .expect("run workflow with plugin provided memory")
        .wait()
        .await;

    let outputs = completed_outputs(result);
    assert_eq!(outputs.get("count"), Some(&json!(1)));
}

#[cfg(feature = "builtin-llm-node")]
#[tokio::test]
async fn test_multiple_llm_nodes_share_same_recall_results() {
    let store_yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          type: string
          required: true
  - id: store
    data:
      type: memory-store
      title: Store
      scope: conversation
      key: "shared"
      value_selector: ["start", "query"]
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: stored
          value_selector: ["store", "stored"]
edges:
  - source: start
    target: store
  - source: store
    target: end
"#;

    let llm_yaml = r#"
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          type: string
          required: true
  - id: recall
    data:
      type: memory-recall
      title: Recall
      scope: conversation
      key: "shared"
      top_k: 5
  - id: llm1
    data:
      type: llm
      title: LLM1
      model:
        provider: mock
        name: mock-model
      prompt_template:
        - role: system
          text: "recall={{#recall.results#}}"
        - role: user
          text: "node1 {{#start.query#}}"
  - id: llm2
    data:
      type: llm
      title: LLM2
      model:
        provider: mock
        name: mock-model
      prompt_template:
        - role: system
          text: "recall={{#recall.results#}}"
        - role: user
          text: "node2 {{#start.query#}}"
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: text1
          value_selector: ["llm1", "text"]
        - variable: text2
          value_selector: ["llm2", "text"]
edges:
  - source: start
    target: recall
  - source: recall
    target: llm1
  - source: llm1
    target: llm2
  - source: llm2
    target: end
"#;

    let store_schema = parse_dsl(store_yaml, DslFormat::Yaml).expect("parse store schema");
    let llm_schema = parse_dsl(llm_yaml, DslFormat::Yaml).expect("parse llm schema");
    let memory = Arc::new(InMemoryProvider::new());

    let store_result = WorkflowRunner::builder(store_schema)
        .user_inputs(user_inputs("shared-memory-value"))
        .system_vars(system_vars())
        .memory_provider(memory.clone())
        .run()
        .await
        .expect("store shared memory")
        .wait()
        .await;
    let store_outputs = completed_outputs(store_result);
    assert_eq!(store_outputs.get("stored"), Some(&json!(true)));

    let result = WorkflowRunner::builder(llm_schema)
        .user_inputs(user_inputs("question"))
        .system_vars(system_vars())
        .memory_provider(memory)
        .llm_providers(llm_registry_with_echo())
        .run()
        .await
        .expect("run multi-llm workflow")
        .wait()
        .await;

    let outputs = completed_outputs(result);
    let text1 = outputs
        .get("text1")
        .and_then(|v| v.as_str())
        .expect("llm1 text output");
    let text2 = outputs
        .get("text2")
        .and_then(|v| v.as_str())
        .expect("llm2 text output");

    assert!(text1.contains("shared-memory-value"), "llm1 should include shared recall data: {text1}");
    assert!(text2.contains("shared-memory-value"), "llm2 should include shared recall data: {text2}");
}
