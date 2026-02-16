#![cfg(feature = "builtin-agent-node")]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::RwLock;

use xworkflow::dsl::schema::{LlmUsage, McpServerConfig, McpTransport};
use xworkflow::llm::error::LlmError;
use xworkflow::llm::types::{
    ChatCompletionRequest, ChatCompletionResponse, ModelInfo, ProviderInfo, StreamChunk, ToolCall,
};
use xworkflow::llm::{LlmProvider, LlmProviderRegistry};
use xworkflow::mcp::client::{McpClient, McpToolInfo};
use xworkflow::mcp::error::McpError;
use xworkflow::mcp::pool::McpConnectionPool;
use xworkflow::nodes::agent::AgentNodeExecutor;
use xworkflow::nodes::executor::NodeExecutor;
use xworkflow::nodes::tool::ToolNodeExecutor;
use xworkflow::{RuntimeContext, Segment, VariablePool};

struct MockMcpClient;

#[async_trait]
impl McpClient for MockMcpClient {
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        Ok(vec![McpToolInfo {
            name: "search".to_string(),
            description: "Search docs".to_string(),
            input_schema: serde_json::json!({"type":"object","properties":{"q":{"type":"string"}}}),
        }])
    }

    async fn call_tool(&self, _name: &str, _arguments: &Value) -> Result<String, McpError> {
        Ok("tool-result".to_string())
    }

    async fn close(&self) -> Result<(), McpError> {
        Ok(())
    }
}

struct MockProvider {
    responses: tokio::sync::Mutex<Vec<ChatCompletionResponse>>,
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "mock".to_string(),
            name: "Mock".to_string(),
            models: vec![ModelInfo {
                id: "mock-model".to_string(),
                name: "Mock Model".to_string(),
                max_tokens: Some(1024),
            }],
        }
    }

    async fn chat_completion(
        &self,
        _request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LlmError> {
        let mut responses = self.responses.lock().await;
        if responses.is_empty() {
            return Ok(ChatCompletionResponse {
                content: "done".to_string(),
                usage: LlmUsage::default(),
                model: "mock-model".to_string(),
                finish_reason: Some("stop".to_string()),
                tool_calls: vec![],
            });
        }
        Ok(responses.remove(0))
    }

    async fn chat_completion_stream(
        &self,
        _request: ChatCompletionRequest,
        _chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<ChatCompletionResponse, LlmError> {
        Err(LlmError::InvalidRequest(
            "stream not used in test".to_string(),
        ))
    }
}

fn make_server() -> McpServerConfig {
    McpServerConfig {
        name: Some("mock".to_string()),
        transport: McpTransport::Stdio {
            command: "mock".to_string(),
            args: vec![],
            env: HashMap::new(),
        },
    }
}

#[tokio::test]
async fn case_130_tool_node_basic() {
    let server = make_server();
    let pool = Arc::new(RwLock::new(McpConnectionPool::new()));
    pool.write()
        .await
        .insert_connection(&server, Arc::new(MockMcpClient));

    let executor = ToolNodeExecutor::new(pool);
    let config = serde_json::json!({
        "mcp_server": serde_json::to_value(&server).unwrap(),
        "tool_name": "search",
        "arguments": {"q": "hello"}
    });

    let variable_pool = VariablePool::new();
    let context = RuntimeContext::default();
    let result = executor
        .execute("tool1", &config, &variable_pool, &context)
        .await
        .unwrap();

    assert_eq!(
        result.outputs.ready().get("result"),
        Some(&Segment::String("tool-result".to_string()))
    );
}

#[tokio::test]
async fn case_131_agent_node_basic() {
    let mut reg = LlmProviderRegistry::new();
    reg.register(Arc::new(MockProvider {
        responses: tokio::sync::Mutex::new(vec![
            ChatCompletionResponse {
                content: "".to_string(),
                usage: LlmUsage::default(),
                model: "mock-model".to_string(),
                finish_reason: Some("tool_calls".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "search".to_string(),
                    arguments: serde_json::json!({"q":"rust"}),
                }],
            },
            ChatCompletionResponse {
                content: "final-answer".to_string(),
                usage: LlmUsage::default(),
                model: "mock-model".to_string(),
                finish_reason: Some("stop".to_string()),
                tool_calls: vec![],
            },
        ]),
    }));

    let server = make_server();
    let pool = Arc::new(RwLock::new(McpConnectionPool::new()));
    pool.write()
        .await
        .insert_connection(&server, Arc::new(MockMcpClient));

    let executor = AgentNodeExecutor::new(Arc::new(reg), pool);
    let config = serde_json::json!({
        "model": {"provider":"mock","name":"mock-model"},
        "system_prompt": "You are helpful",
        "query": "hello",
        "tools": {"mcp_servers": [serde_json::to_value(&server).unwrap()]},
        "max_iterations": 5
    });

    let variable_pool = VariablePool::new();
    let context = RuntimeContext::default();
    let result = executor
        .execute("agent1", &config, &variable_pool, &context)
        .await
        .expect("agent node should execute successfully");

    assert_eq!(
        result.outputs.ready().get("text"),
        Some(&Segment::String("final-answer".to_string()))
    );
    assert_eq!(
        result.outputs.ready().get("iterations"),
        Some(&Segment::Integer(2))
    );
    assert_eq!(
        result.outputs.ready().get("tool_calls_count"),
        Some(&Segment::Integer(1))
    );

    let log = result.outputs.ready().get("tool_calls").unwrap().to_value();
    assert_eq!(log.as_array().map(|v| v.len()), Some(1));
}

#[tokio::test]
async fn case_132_agent_node_max_iterations() {
    let mut reg = LlmProviderRegistry::new();
    reg.register(Arc::new(MockProvider {
        responses: tokio::sync::Mutex::new(vec![
            ChatCompletionResponse {
                content: "".to_string(),
                usage: LlmUsage::default(),
                model: "mock-model".to_string(),
                finish_reason: Some("tool_calls".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "search".to_string(),
                    arguments: serde_json::json!({"q":"a"}),
                }],
            },
            ChatCompletionResponse {
                content: "".to_string(),
                usage: LlmUsage::default(),
                model: "mock-model".to_string(),
                finish_reason: Some("tool_calls".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call_2".to_string(),
                    name: "search".to_string(),
                    arguments: serde_json::json!({"q":"b"}),
                }],
            },
            ChatCompletionResponse {
                content: "forced-final".to_string(),
                usage: LlmUsage::default(),
                model: "mock-model".to_string(),
                finish_reason: Some("stop".to_string()),
                tool_calls: vec![],
            },
        ]),
    }));

    let server = make_server();
    let pool = Arc::new(RwLock::new(McpConnectionPool::new()));
    pool.write()
        .await
        .insert_connection(&server, Arc::new(MockMcpClient));

    let executor = AgentNodeExecutor::new(Arc::new(reg), pool);
    let config = serde_json::json!({
        "model": {"provider":"mock","name":"mock-model"},
        "system_prompt": "You are helpful",
        "query": "hello",
        "tools": {"mcp_servers": [serde_json::to_value(&server).unwrap()]},
        "max_iterations": 3
    });

    let variable_pool = VariablePool::new();
    let context = RuntimeContext::default();
    let result = executor
        .execute("agent1", &config, &variable_pool, &context)
        .await
        .expect("agent node should execute successfully");

    assert_eq!(
        result.outputs.ready().get("text"),
        Some(&Segment::String("forced-final".to_string()))
    );
    assert_eq!(
        result.outputs.ready().get("iterations"),
        Some(&Segment::Integer(3))
    );
    assert_eq!(
        result.outputs.ready().get("tool_calls_count"),
        Some(&Segment::Integer(2))
    );
}
