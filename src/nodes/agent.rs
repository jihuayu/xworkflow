use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::RwLock;

#[cfg(feature = "security")]
use parking_lot::RwLock as ParkingRwLock;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::dsl::schema::{
    AgentNodeData,
    LlmUsage,
    McpTransport,
    McpServerConfig,
    NodeOutputs,
    NodeRunResult,
    WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::llm::executor::append_memory_messages;
#[cfg(feature = "security")]
use crate::llm::executor::audit_security_event;
use crate::llm::types::{
    ChatCompletionRequest,
    ChatContent,
    ChatMessage,
    ChatRole,
    ToolDefinition,
};
use crate::llm::LlmProviderRegistry;
use crate::mcp::client::McpClient;
use crate::mcp::pool::McpConnectionPool;
use crate::mcp::tools::discover_tools;
use crate::nodes::executor::NodeExecutor;
#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEventType};
#[cfg(feature = "security")]
use crate::security::network::validate_url;
use crate::template::render_template_async_with_config;

pub struct AgentNodeExecutor {
    llm_registry: Arc<LlmProviderRegistry>,
    mcp_pool: Arc<RwLock<McpConnectionPool>>,
}

impl AgentNodeExecutor {
    pub fn new(
        llm_registry: Arc<LlmProviderRegistry>,
        mcp_pool: Arc<RwLock<McpConnectionPool>>,
    ) -> Self {
        Self {
            llm_registry,
            mcp_pool,
        }
    }

    fn resolve_servers(
        &self,
        cfg: &AgentNodeData,
        variable_pool: &VariablePool,
    ) -> Result<Vec<McpServerConfig>, NodeError> {
        let mut servers = cfg.tools.mcp_servers.clone();

        if cfg.tools.mcp_server_refs.is_empty() {
            return Ok(servers);
        }

        let workflow_servers = variable_pool
            .get(&Selector::new("sys", "__mcp_servers"))
            .to_value();
        let workflow_servers: HashMap<String, McpServerConfig> = serde_json::from_value(workflow_servers)
            .map_err(|e| NodeError::ConfigError(format!("invalid workflow mcp_servers: {}", e)))?;

        for server_ref in &cfg.tools.mcp_server_refs {
            let Some(server) = workflow_servers.get(server_ref) else {
                return Err(NodeError::ConfigError(format!(
                    "mcp_server_ref '{}' not found",
                    server_ref
                )));
            };
            servers.push(server.clone());
        }

        Ok(servers)
    }

    async fn discover_all_tools(
        &self,
        cfg: &AgentNodeData,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
        node_id: &str,
    ) -> Result<(Vec<ToolDefinition>, HashMap<String, Arc<dyn McpClient>>), NodeError> {
        let servers = self.resolve_servers(cfg, variable_pool)?;
        let mut definitions = Vec::new();
        let mut tool_client_map: HashMap<String, Arc<dyn McpClient>> = HashMap::new();
        let mut tool_server_map: HashMap<String, String> = HashMap::new();

        for server in servers {
            let server_name = server.name.clone().unwrap_or_else(|| match &server.transport {
                McpTransport::Stdio { command, .. } => format!("stdio:{}", command),
                McpTransport::Http { url, .. } => format!("http:{}", url),
            });

            #[cfg(feature = "security")]
            if let McpTransport::Http { url, .. } = &server.transport {
                if let Some(policy) = context.security_policy().and_then(|p| p.network.as_ref()) {
                    if let Err(err) = validate_url(url, policy).await {
                        audit_security_event(
                            context,
                            SecurityEventType::SsrfBlocked {
                                url: url.clone(),
                                reason: err.to_string(),
                            },
                            EventSeverity::Warning,
                            Some(node_id.to_string()),
                        )
                        .await;
                        return Err(NodeError::InputValidationError(err.to_string()));
                    }
                }
            }

            let client = {
                let mut pool = self.mcp_pool.write().await;
                pool.get_or_connect(&server)
                    .await
                    .map_err(|e| NodeError::ExecutionError(e.to_string()))?
            };

            let discovered = discover_tools(client.as_ref())
                .await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

            for tool in discovered {
                if let Some(previous_server_name) = tool_server_map.get(&tool.name) {
                    return Err(NodeError::ConfigError(format!(
                        "duplicate MCP tool name '{}' from servers '{}' and '{}'",
                        tool.name, previous_server_name, server_name
                    )));
                }
                tool_server_map.insert(tool.name.clone(), server_name.clone());
                tool_client_map.insert(tool.name.clone(), client.clone());
                definitions.push(tool);
            }
        }

        Ok((definitions, tool_client_map))
    }

    #[cfg(feature = "security")]
    async fn check_tool_security(
        &self,
        context: &RuntimeContext,
        node_id: &str,
        tool_name: &str,
        arguments: &Value,
        variable_pool: &VariablePool,
    ) -> Result<(), NodeError> {
        let gate = crate::core::security_gate::new_security_gate(
            Arc::new(context.clone()),
            Arc::new(ParkingRwLock::new(variable_pool.clone())),
        );

        let tool_config = serde_json::json!({
            "mcp_tool": tool_name,
            "arguments": arguments,
        });
        gate
            .check_before_node(node_id, "mcp-tool-call", &tool_config)
            .await?;

        audit_security_event(
            context,
            SecurityEventType::ToolInvocation {
                tool_name: tool_name.to_string(),
            },
            EventSeverity::Info,
            Some(node_id.to_string()),
        )
        .await;

        Ok(())
    }
}

#[async_trait]
impl NodeExecutor for AgentNodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let cfg: AgentNodeData = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        let provider = self
            .llm_registry
            .get(&cfg.model.provider)
            .ok_or_else(|| NodeError::ConfigError(format!(
                "LLM provider '{}' not found",
                cfg.model.provider
            )))?;

        let (tool_defs, tool_map) = self
            .discover_all_tools(&cfg, variable_pool, context, node_id)
            .await?;

        let rendered_query = render_template_async_with_config(
            &cfg.query,
            variable_pool,
            context.strict_template(),
        )
        .await
        .map_err(|e| NodeError::VariableNotFound(e.selector))?;

        let mut messages = Vec::new();

        if let Some(memory) = &cfg.memory {
            append_memory_messages(&mut messages, memory, variable_pool).await?;
        }

        messages.push(ChatMessage {
            role: ChatRole::System,
            content: ChatContent::Text(cfg.system_prompt.clone()),
            tool_calls: vec![],
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: ChatContent::Text(rendered_query),
            tool_calls: vec![],
            tool_call_id: None,
        });

        let mut total_usage = LlmUsage::default();
        let mut tool_call_log: Vec<Value> = Vec::new();
        let mut final_text = String::new();
        let mut iterations: i64 = 0;

        let max_iterations = if cfg.max_iterations <= 0 {
            1
        } else {
            cfg.max_iterations as usize
        };

        for iteration in 0..max_iterations {
            let allow_tools = iteration + 1 < max_iterations;
            let completion = cfg.model.completion_params.clone();
            let request = ChatCompletionRequest {
                model: cfg.model.name.clone(),
                messages: messages.clone(),
                temperature: completion.as_ref().and_then(|p| p.temperature),
                top_p: completion.as_ref().and_then(|p| p.top_p),
                max_tokens: completion.as_ref().and_then(|p| p.max_tokens),
                stream: false,
                credentials: cfg.model.credentials.clone().unwrap_or_default(),
                tools: if allow_tools {
                    tool_defs.clone()
                } else {
                    vec![]
                },
            };

            let response = provider.chat_completion(request).await.map_err(NodeError::from)?;
            iterations += 1;
            accumulate_usage(&mut total_usage, &response.usage);

            if response.tool_calls.is_empty() || !allow_tools {
                final_text = response.content;
                break;
            }

            messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: ChatContent::Text(response.content.clone()),
                tool_calls: response.tool_calls.clone(),
                tool_call_id: None,
            });

            for tool_call in &response.tool_calls {
                #[cfg(feature = "security")]
                self
                    .check_tool_security(
                        context,
                        node_id,
                        &tool_call.name,
                        &tool_call.arguments,
                        variable_pool,
                    )
                    .await?;

                let (content, is_error) = match tool_map.get(&tool_call.name) {
                    Some(client) => match client.call_tool(&tool_call.name, &tool_call.arguments).await {
                        Ok(output) => (output, false),
                        Err(e) => (e.to_string(), true),
                    },
                    None => (
                        format!("tool '{}' not found on any MCP server", tool_call.name),
                        true,
                    ),
                };

                tool_call_log.push(json!({
                    "iteration": iteration,
                    "tool": &tool_call.name,
                    "arguments": &tool_call.arguments,
                    "result": &content,
                    "is_error": is_error,
                }));

                messages.push(ChatMessage {
                    role: ChatRole::Tool,
                    content: ChatContent::Text(content),
                    tool_calls: vec![],
                    tool_call_id: Some(tool_call.id.clone()),
                });
            }
        }

        let tool_calls_count = tool_call_log.len() as i64;
        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync({
                let mut outputs = HashMap::new();
                outputs.insert("text".to_string(), Segment::String(final_text));
                outputs.insert(
                    "tool_calls".to_string(),
                    Segment::from_value(&Value::Array(tool_call_log)),
                );
                outputs.insert("iterations".to_string(), Segment::Integer(iterations));
                outputs.insert("tool_calls_count".to_string(), Segment::Integer(tool_calls_count));
                outputs
            }),
            llm_usage: Some(total_usage),
            ..Default::default()
        })
    }
}

fn accumulate_usage(total: &mut LlmUsage, current: &LlmUsage) {
    total.prompt_tokens += current.prompt_tokens;
    total.completion_tokens += current.completion_tokens;
    total.total_tokens += current.total_tokens;
    total.total_price += current.total_price;
    total.latency += current.latency;
    if total.currency.is_empty() {
        total.currency = current.currency.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::schema::McpTransport;
    use crate::llm::error::LlmError;
    use crate::llm::types::{
        ChatCompletionResponse,
        ModelInfo,
        ProviderInfo,
        StreamChunk,
        ToolCall,
    };
    use crate::llm::LlmProvider;
    use crate::mcp::client::McpToolInfo;
    use crate::mcp::error::McpError;

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
            Err(LlmError::InvalidRequest("stream not used in test".to_string()))
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
    async fn test_agent_no_tool_calls() {
        let mut reg = LlmProviderRegistry::new();
        reg.register(Arc::new(MockProvider {
            responses: tokio::sync::Mutex::new(vec![ChatCompletionResponse {
                content: "answer".to_string(),
                usage: LlmUsage::default(),
                model: "mock-model".to_string(),
                finish_reason: Some("stop".to_string()),
                tool_calls: vec![],
            }]),
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
            "tools": {"mcp_servers": [serde_json::to_value(&server).unwrap()]}
        });

        let variable_pool = VariablePool::new();
        let context = RuntimeContext::default();
        let result = executor.execute("agent1", &config, &variable_pool, &context).await.unwrap();

        assert_eq!(result.outputs.ready().get("text"), Some(&Segment::String("answer".to_string())));
        assert_eq!(result.outputs.ready().get("iterations"), Some(&Segment::Integer(1)));
        assert_eq!(result.outputs.ready().get("tool_calls_count"), Some(&Segment::Integer(0)));
    }

    #[tokio::test]
    async fn test_agent_single_iteration() {
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
                    content: "final".to_string(),
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
        let result = executor.execute("agent1", &config, &variable_pool, &context).await.unwrap();

        assert_eq!(result.outputs.ready().get("text"), Some(&Segment::String("final".to_string())));
        assert_eq!(result.outputs.ready().get("iterations"), Some(&Segment::Integer(2)));
        assert_eq!(result.outputs.ready().get("tool_calls_count"), Some(&Segment::Integer(1)));
    }

    #[tokio::test]
    async fn test_agent_max_iterations() {
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
                    content: "".to_string(),
                    usage: LlmUsage::default(),
                    model: "mock-model".to_string(),
                    finish_reason: Some("tool_calls".to_string()),
                    tool_calls: vec![ToolCall {
                        id: "call_2".to_string(),
                        name: "search".to_string(),
                        arguments: serde_json::json!({"q":"workflow"}),
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
        let result = executor.execute("agent1", &config, &variable_pool, &context).await.unwrap();

        assert_eq!(
            result.outputs.ready().get("text"),
            Some(&Segment::String("forced-final".to_string()))
        );
        assert_eq!(result.outputs.ready().get("iterations"), Some(&Segment::Integer(3)));
        assert_eq!(result.outputs.ready().get("tool_calls_count"), Some(&Segment::Integer(2)));
    }

    #[tokio::test]
    async fn test_agent_tool_error() {
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
                        name: "missing_tool".to_string(),
                        arguments: serde_json::json!({"q":"rust"}),
                    }],
                },
                ChatCompletionResponse {
                    content: "fallback".to_string(),
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
        let result = executor.execute("agent1", &config, &variable_pool, &context).await.unwrap();

        assert_eq!(
            result.outputs.ready().get("text"),
            Some(&Segment::String("fallback".to_string()))
        );

        let log = result
            .outputs
            .ready()
            .get("tool_calls")
            .unwrap()
            .to_value();
        assert_eq!(log[0]["is_error"], true);
        assert_eq!(result.outputs.ready().get("tool_calls_count"), Some(&Segment::Integer(1)));
    }

    #[tokio::test]
    async fn test_agent_empty_tools() {
        let mut reg = LlmProviderRegistry::new();
        reg.register(Arc::new(MockProvider {
            responses: tokio::sync::Mutex::new(vec![ChatCompletionResponse {
                content: "no-tools-answer".to_string(),
                usage: LlmUsage::default(),
                model: "mock-model".to_string(),
                finish_reason: Some("stop".to_string()),
                tool_calls: vec![],
            }]),
        }));

        let executor = AgentNodeExecutor::new(
            Arc::new(reg),
            Arc::new(RwLock::new(McpConnectionPool::new())),
        );
        let config = serde_json::json!({
            "model": {"provider":"mock","name":"mock-model"},
            "system_prompt": "You are helpful",
            "query": "hello",
            "tools": {},
            "max_iterations": 3
        });

        let variable_pool = VariablePool::new();
        let context = RuntimeContext::default();
        let result = executor.execute("agent1", &config, &variable_pool, &context).await.unwrap();

        assert_eq!(
            result.outputs.ready().get("text"),
            Some(&Segment::String("no-tools-answer".to_string()))
        );
        assert_eq!(result.outputs.ready().get("iterations"), Some(&Segment::Integer(1)));
        assert_eq!(result.outputs.ready().get("tool_calls_count"), Some(&Segment::Integer(0)));
    }

    #[tokio::test]
    async fn test_agent_duplicate_tool_name_conflict() {
        let mut reg = LlmProviderRegistry::new();
        reg.register(Arc::new(MockProvider {
            responses: tokio::sync::Mutex::new(vec![]),
        }));

        let server_a = McpServerConfig {
            name: Some("alpha".to_string()),
            transport: McpTransport::Stdio {
                command: "mock-a".to_string(),
                args: vec![],
                env: HashMap::new(),
            },
        };
        let server_b = McpServerConfig {
            name: Some("beta".to_string()),
            transport: McpTransport::Stdio {
                command: "mock-b".to_string(),
                args: vec![],
                env: HashMap::new(),
            },
        };

        let pool = Arc::new(RwLock::new(McpConnectionPool::new()));
        pool.write()
            .await
            .insert_connection(&server_a, Arc::new(MockMcpClient));
        pool.write()
            .await
            .insert_connection(&server_b, Arc::new(MockMcpClient));

        let executor = AgentNodeExecutor::new(Arc::new(reg), pool);
        let config = serde_json::json!({
            "model": {"provider":"mock","name":"mock-model"},
            "system_prompt": "You are helpful",
            "query": "hello",
            "tools": {"mcp_servers": [
                serde_json::to_value(&server_a).unwrap(),
                serde_json::to_value(&server_b).unwrap()
            ]},
        });

        let variable_pool = VariablePool::new();
        let context = RuntimeContext::default();
        let result = executor.execute("agent1", &config, &variable_pool, &context).await;

        match result {
            Err(NodeError::ConfigError(msg)) => {
                assert!(msg.contains("duplicate MCP tool name 'search'"));
                assert!(msg.contains("alpha"));
                assert!(msg.contains("beta"));
            }
            other => panic!("expected ConfigError for duplicate tool name, got {:?}", other),
        }
    }
}
