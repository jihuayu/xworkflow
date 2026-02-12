# Agent 节点与 MCP 集成设计文档

## 1. 概述

本文档定义 xworkflow 对 AI Agent 协议（以 MCP — Model Context Protocol 为首选）的集成方案。设计涵盖三个层次的工具调用能力，从确定性到非确定性：

| 层次 | 节点类型 | 行为 | LLM 参与 |
|------|----------|------|----------|
| L1 确定性 | `tool` | 用户指定工具名和参数，直接调用 | 无 |
| L2 单轮 | `llm`（增强） | LLM 可选择调用工具，仅一轮 | 单次 |
| L3 自主 | `agent` | LLM 自主多轮循环，决定调什么、调几次 | 多次 |

当前状态：`tool` 和 `agent` 均为 `StubExecutor`（`src/nodes/executor.rs:125,148`）。

遵循项目三大原则：**Security > Performance > Obviousness**。

### 协议选型

| 协议 | 定位 | Rust SDK | 成熟度 | 本设计支持 |
|------|------|----------|--------|-----------|
| MCP (Model Context Protocol) | Agent↔Tool | 官方 `rmcp` v0.13 (Tokio) | 极高（97M+月下载） | P0（本文档） |
| A2A (Agent-to-Agent) | Agent↔Agent | 社区 `a2a-rs` | 高（150+组织） | P1（未来扩展） |
| AG-UI | Agent↔User | 社区 `ag-ui-client` | 中 | P2（可选） |

MCP 优先的理由：
1. 生态压倒性优势，OpenAI/Google/Microsoft 全部采纳
2. Tool 的 `input_schema` 即 JSON Schema，与 OpenAI function calling 的 `parameters` 格式完全一致，零转换
3. 官方 Rust SDK 基于 Tokio，与 xworkflow 架构完全兼容
4. 已由 Linux Foundation AAIF 治理，长期稳定

---

## 2. LLM Types 扩展 — Tool Calling 支持

**文件**: `src/llm/types.rs`

### 2.1 新增类型

```rust
/// Tool definition sent to LLM (OpenAI function calling format)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolDefinition {
    /// Tool name (must match MCP tool name)
    pub name: String,
    /// Human-readable description for LLM to understand when to use this tool
    pub description: String,
    /// JSON Schema describing the tool's parameters
    pub parameters: Value,
}

/// Tool call returned by LLM in response
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCall {
    /// Unique ID for this tool call (from LLM response)
    pub id: String,
    /// Tool name to invoke
    pub name: String,
    /// Arguments as JSON (parsed from LLM response string)
    pub arguments: Value,
}

/// Tool execution result fed back to LLM
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolResult {
    /// Correlates with ToolCall.id
    pub tool_call_id: String,
    /// Tool output content (text)
    pub content: String,
    /// Whether this result represents an error
    pub is_error: bool,
}
```

### 2.2 现有类型修改

```rust
// ChatRole 增加 Tool 角色
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,       // 新增：工具结果消息的角色
}

// ChatMessage 增加 tool 相关字段
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatContent,
    /// Tool calls made by assistant (only present when role=Assistant)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Correlates tool result with its call (only present when role=Tool)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

// ChatCompletionRequest 增加 tools 字段
#[derive(Debug, Clone)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i32>,
    pub stream: bool,
    pub credentials: HashMap<String, String>,
    /// Tool definitions available to the LLM (empty = no tool calling)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
}

// ChatCompletionResponse 增加 tool_calls 字段
#[derive(Debug, Clone)]
pub struct ChatCompletionResponse {
    pub content: String,
    pub usage: LlmUsage,
    pub model: String,
    pub finish_reason: Option<String>,
    /// Tool calls requested by LLM (empty if finish_reason != "tool_calls")
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}
```

### 设计决策

| 决策 | 理由 |
|------|------|
| `tools: Vec<ToolDefinition>` 而非 `Option<Vec<>>` | 空 Vec 即无工具，语义更清晰，避免 `Option` 嵌套 |
| `tool_calls` 在 `ChatMessage` 和 `ChatCompletionResponse` 都有 | `ChatMessage` 中用于构建对话历史，`Response` 中用于提取 LLM 决策 |
| `ToolCall.arguments` 为 `Value` 而非 `String` | LLM 返回的是 JSON 字符串，Provider 层解析后传入，避免重复解析 |
| 不增加 `tool_choice` 字段 | 保持简单；auto 是最常用默认值；如果未来需要可后加 |

### 向后兼容

- `tools` 默认为空 Vec，不影响现有 `llm` 节点的任何行为
- `tool_calls` 默认为空 Vec，不影响现有响应解析
- `ChatRole::Tool` 新增变体，不影响现有三种角色
- `ChatMessage` 新字段使用 `skip_serializing_if`，序列化行为不变

---

## 3. OpenAI Provider 增强

**文件**: `src/llm/provider/openai.rs`

### 3.1 build_payload() 修改

```rust
fn build_payload(&self, request: &ChatCompletionRequest) -> Value {
    let mut payload = json!({
        "model": &request.model,
        "messages": self.build_messages(&request.messages),
        // ... 现有参数 ...
    });

    // 仅当有 tools 时才发送（不影响无工具的请求）
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request.tools.iter().map(|t| json!({
                "type": "function",
                "function": {
                    "name": &t.name,
                    "description": &t.description,
                    "parameters": &t.parameters,
                }
            })).collect()
        );
    }

    payload
}
```

### 3.2 build_messages() 修改

```rust
fn build_messages(&self, messages: &[ChatMessage]) -> Vec<Value> {
    messages.iter().map(|msg| {
        let mut m = json!({
            "role": msg.role,
            "content": msg.content.to_api_string(),
        });

        // Assistant message with tool calls
        if !msg.tool_calls.is_empty() {
            m["tool_calls"] = Value::Array(
                msg.tool_calls.iter().map(|tc| json!({
                    "id": &tc.id,
                    "type": "function",
                    "function": {
                        "name": &tc.name,
                        "arguments": tc.arguments.to_string(),
                    }
                })).collect()
            );
        }

        // Tool result message
        if let Some(ref id) = msg.tool_call_id {
            m["tool_call_id"] = Value::String(id.clone());
        }

        m
    }).collect()
}
```

### 3.3 parse_response() 修改

```rust
fn parse_response(&self, body: &Value) -> Result<ChatCompletionResponse, LlmError> {
    let choice = &body["choices"][0];
    let message = &choice["message"];

    let tool_calls = message.get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter().filter_map(|tc| {
                Some(ToolCall {
                    id: tc["id"].as_str()?.to_string(),
                    name: tc["function"]["name"].as_str()?.to_string(),
                    arguments: serde_json::from_str(
                        tc["function"]["arguments"].as_str().unwrap_or("{}")
                    ).unwrap_or(Value::Object(Default::default())),
                })
            }).collect()
        })
        .unwrap_or_default();

    Ok(ChatCompletionResponse {
        content: message["content"].as_str().unwrap_or("").to_string(),
        tool_calls,
        usage: self.parse_usage(&body["usage"]),
        model: body["model"].as_str().unwrap_or("").to_string(),
        finish_reason: choice.get("finish_reason")
            .and_then(|f| f.as_str())
            .map(String::from),
    })
}
```

---

## 4. MCP Client 模块

### 4.1 模块结构

MCP 客户端作为 `src/mcp/` 核心模块实现，与 `src/llm/` 平行。

```
src/
  mcp/
    mod.rs                 # pub mod config, client, pool, tools, error
    config.rs              # McpServerConfig, McpTransport
    client.rs              # McpClient 封装（基于 rmcp）
    pool.rs                # McpConnectionPool
    tools.rs               # MCP tool → ToolDefinition 转换
    error.rs               # McpError
```

**为什么不用独立 crate？**

| 独立 crate (`crates/xworkflow-mcp/`) | 核心模块 (`src/mcp/`) |
|---------------------------------------|----------------------|
| workspace 成员+1（当前已 8 个） | 零膨胀 |
| 跨 crate 引用 `llm::types::ToolDefinition` 需要通过 `xworkflow-types` 中转 | 直接 `use crate::llm::types::ToolDefinition` |
| 需要独立 Cargo.toml 管理版本 | 随主 crate 版本 |
| MCP 客户端无外部复用场景 | 仅被 `src/nodes/tool.rs` 和 `src/nodes/agent.rs` 使用 |
| Feature gating 需要 `dep:xworkflow-mcp` | 直接 `#[cfg(feature = "builtin-agent-node")]` 门控整个模块 |

**lib.rs 中声明**：

```rust
#[cfg(feature = "builtin-agent-node")]
pub mod mcp;
```

### 4.2 Cargo.toml 依赖

`rmcp` 作为主 crate 的可选依赖，由 `builtin-agent-node` feature 触发：

```toml
[dependencies]
rmcp = { version = "0.1", features = ["client", "transport-child-process", "transport-sse"], optional = true }

[features]
builtin-agent-node = ["dep:rmcp", "builtin-llm-node"]
```

### 4.3 核心类型

**文件**: `src/mcp/config.rs`

```rust
/// MCP Server 连接配置
#[derive(Deserialize, Serialize, Debug, Clone, Hash, PartialEq, Eq)]
pub struct McpServerConfig {
    /// Transport type and settings
    pub transport: McpTransport,
    /// Optional: server name for logging/debugging
    #[serde(default)]
    pub name: Option<String>,
}

/// Transport mechanism
#[derive(Deserialize, Serialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// Local subprocess via stdin/stdout
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Remote server via Streamable HTTP
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}
```

### 4.4 McpClient — rmcp 封装

**文件**: `src/mcp/client.rs`

```rust
use rmcp::{ServiceExt, model::*, transport::*};

pub struct McpClient {
    inner: rmcp::service::RunningService<rmcp::RoleClient, ()>,
}

impl McpClient {
    /// Connect via stdio (spawn child process)
    pub async fn connect_stdio(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let transport = TokioChildProcess::new(command, args, env)?;
        let inner = ().serve(transport).await?;
        Ok(Self { inner })
    }

    /// Connect via Streamable HTTP
    pub async fn connect_http(
        url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        let transport = SseTransport::new(url, headers)?;
        let inner = ().serve(transport).await?;
        Ok(Self { inner })
    }

    /// List all tools exposed by this server
    pub async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>, McpError> {
        let response = self.inner.list_tools(Default::default()).await?;
        Ok(response.tools)
    }

    /// Call a tool by name with JSON arguments
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &Value,
    ) -> Result<String, McpError> {
        let params = CallToolRequestParam {
            name: name.into(),
            arguments: Some(arguments.as_object().cloned().unwrap_or_default()),
        };
        let result = self.inner.call_tool(params).await?;

        // Extract text content from MCP result
        let text = result.content.iter()
            .filter_map(|c| match c {
                Content::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if result.is_error.unwrap_or(false) {
            Err(McpError::ToolError(text))
        } else {
            Ok(text)
        }
    }

    /// Shutdown connection
    pub async fn close(self) -> Result<(), McpError> {
        self.inner.cancel().await;
        Ok(())
    }
}
```

### 4.5 MCP Tool → ToolDefinition 转换

**文件**: `src/mcp/tools.rs`

```rust
use crate::mcp::client::McpClient;
use crate::llm::types::ToolDefinition;

/// 从 MCP Server 发现所有工具并转换为 LLM ToolDefinition
pub async fn discover_tools(client: &McpClient) -> Result<Vec<ToolDefinition>, McpError> {
    let mcp_tools = client.list_tools().await?;

    Ok(mcp_tools.into_iter().map(|t| ToolDefinition {
        name: t.name.to_string(),
        description: t.description.unwrap_or_default().to_string(),
        parameters: t.input_schema,  // MCP 用 JSON Schema，直接透传
    }).collect())
}
```

MCP 的 `input_schema` 和 OpenAI 的 `parameters` 都是 JSON Schema 格式，**零转换成本**。

### 4.6 连接池

**文件**: `src/mcp/pool.rs`

```rust
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

pub struct McpConnectionPool {
    connections: HashMap<u64, Arc<McpClient>>,
}

impl McpConnectionPool {
    pub fn new() -> Self {
        Self { connections: HashMap::new() }
    }

    /// Get existing connection or create new one
    pub async fn get_or_connect(
        &mut self,
        config: &McpServerConfig,
    ) -> Result<Arc<McpClient>, McpError> {
        let key = self.config_hash(config);
        if let Some(client) = self.connections.get(&key) {
            return Ok(client.clone());
        }

        let client = match &config.transport {
            McpTransport::Stdio { command, args, env } => {
                McpClient::connect_stdio(command, args, env).await?
            }
            McpTransport::Http { url, headers } => {
                McpClient::connect_http(url, headers).await?
            }
        };

        let client = Arc::new(client);
        self.connections.insert(key, client.clone());
        Ok(client)
    }

    /// Close all connections
    pub async fn close_all(&mut self) {
        for (_, client) in self.connections.drain() {
            if let Ok(client) = Arc::try_unwrap(client) {
                let _ = client.close().await;
            }
        }
    }

    fn config_hash(&self, config: &McpServerConfig) -> u64 {
        let mut hasher = DefaultHasher::new();
        config.hash(&mut hasher);
        hasher.finish()
    }
}
```

---

## 5. Tool 节点 — L1 确定性调用

**文件**: 新建 `src/nodes/tool.rs`

### 5.1 DSL Schema

```rust
/// Tool 节点配置 — 确定性地调用指定 MCP 工具
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ToolNodeData {
    /// MCP Server 配置（内联）
    #[serde(default)]
    pub mcp_server: Option<McpServerConfig>,
    /// 引用 workflow 级别声明的 MCP Server 名称
    #[serde(default)]
    pub mcp_server_ref: Option<String>,
    /// 要调用的工具名称（必须）
    pub tool_name: String,
    /// 工具参数（支持 {{#node.var#}} 模板）
    #[serde(default)]
    pub arguments: HashMap<String, Value>,
}
```

### 5.2 Executor

```rust
pub struct ToolNodeExecutor {
    mcp_pool: Arc<RwLock<McpConnectionPool>>,
}

#[async_trait]
impl NodeExecutor for ToolNodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let cfg: ToolNodeData = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        // 1. 解析 MCP Server 配置
        let server_config = self.resolve_server_config(&cfg, context)?;

        // 2. 获取连接
        let client = self.mcp_pool.write().await
            .get_or_connect(&server_config).await
            .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

        // 3. 渲染模板参数
        let arguments = render_arguments(&cfg.arguments, variable_pool)?;

        // 4. SecurityGate 检查
        #[cfg(feature = "security")]
        self.check_tool_security(context, node_id, &cfg.tool_name, &arguments).await?;

        // 5. 调用 MCP tool
        let result = client.call_tool(&cfg.tool_name, &arguments).await
            .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

        // 6. 返回结果
        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync({
                let mut m = HashMap::new();
                m.insert("result".to_string(), Segment::String(result));
                m
            }),
            ..Default::default()
        })
    }
}
```

### 5.3 DSL 示例

```yaml
- id: query_db
  data:
    type: tool
    title: "Query Database"
    mcp_server:
      transport: stdio
      command: "python"
      args: ["-m", "db_mcp_server"]
    tool_name: "query"
    arguments:
      sql: "SELECT * FROM users WHERE id = {{start.user_id}}"
```

### 设计决策

| 决策 | 理由 |
|------|------|
| 参数必须用户指定，不经过 LLM | Obviousness: `tool` 节点就是确定性调用，名副其实 |
| 支持 `mcp_server`（内联）和 `mcp_server_ref`（引用） | 简单场景内联即可，复杂场景复用 workflow 级声明 |
| 输出固定为 `result` 字段 | MCP tool 返回文本内容，统一为单字段；后续可按 content type 扩展 |

---

## 6. Agent 节点 — L3 自主循环

**文件**: 新建 `src/nodes/agent.rs`

### 6.1 DSL Schema

```rust
/// Agent 节点配置 — LLM 自主 Tool Use 循环
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AgentNodeData {
    /// LLM 模型配置（复用已有 ModelConfig）
    pub model: ModelConfig,
    /// Agent 系统提示词
    pub system_prompt: String,
    /// 用户查询（支持 {{#node.var#}} 模板）
    pub query: String,
    /// 可用工具配置
    pub tools: AgentToolsConfig,
    /// 最大 LLM 调用轮次（安全边界），默认 10
    #[serde(default = "default_max_iterations")]
    pub max_iterations: i32,
    /// 可选：对话记忆
    #[serde(default)]
    pub memory: Option<MemoryConfig>,
}

fn default_max_iterations() -> i32 { 10 }

/// Agent 可用工具来源
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AgentToolsConfig {
    /// 内联 MCP Server 配置
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// 引用 workflow 级别声明的 MCP Server
    #[serde(default)]
    pub mcp_server_refs: Vec<String>,
}
```

### 6.2 执行流程

```
1. parse config → AgentNodeData
2. render query template（变量替换）
3. connect to MCP servers（从连接池获取或新建）
4. discover tools（合并所有 MCP server 的 tools）
5. build initial messages:
   a. [可选] memory messages（复用 append_memory_messages）
   b. system: system_prompt
   c. user: rendered query
6. Tool Use Loop:
   a. call provider.chat_completion(messages, tools)
   b. accumulate usage
   c. if response.tool_calls is empty → break（LLM 决定结束）
   d. append assistant message（含 tool_calls）
   e. for each tool_call:
      - SecurityGate.check_tool_invocation(tool_name, arguments)
      - MCP client.call_tool(tool_name, arguments)
      - append tool result message
      - record to tool_call_log
   f. iteration++ < max_iterations?
      - Yes → continue loop
      - No → final call without tools（强制文本回复）
7. return NodeRunResult {
     outputs: { text, tool_calls, usage, iterations },
     llm_usage: accumulated,
   }
```

### 6.3 Executor 核心实现

```rust
pub struct AgentNodeExecutor {
    llm_registry: Arc<LlmProviderRegistry>,
    mcp_pool: Arc<RwLock<McpConnectionPool>>,
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

        // Provider lookup
        let provider = self.llm_registry
            .get(&cfg.model.provider)
            .ok_or_else(|| NodeError::ConfigError(
                format!("LLM provider '{}' not found", cfg.model.provider)
            ))?;

        // Discover all available tools
        let tool_defs = self.discover_all_tools(&cfg.tools, context).await?;

        // Build initial messages
        let rendered_query = render_template(&cfg.query, variable_pool)?;
        let mut messages = Vec::new();

        // Optional: memory
        if let Some(ref mem) = cfg.memory {
            if mem.enabled {
                let mem_msgs = append_memory_messages(mem, variable_pool)?;
                messages.extend(mem_msgs);
            }
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

        // Tool Use Loop
        let mut total_usage = LlmUsage::default();
        let mut tool_call_log: Vec<Value> = Vec::new();
        let mut final_text = String::new();

        for iteration in 0..cfg.max_iterations {
            let request = ChatCompletionRequest {
                model: cfg.model.name.clone(),
                messages: messages.clone(),
                tools: tool_defs.clone(),
                temperature: cfg.model.completion_params.as_ref()
                    .and_then(|p| p.temperature),
                top_p: cfg.model.completion_params.as_ref()
                    .and_then(|p| p.top_p),
                max_tokens: cfg.model.completion_params.as_ref()
                    .and_then(|p| p.max_tokens),
                stream: false,
                credentials: cfg.model.credentials.clone().unwrap_or_default(),
            };

            let response = provider.chat_completion(request).await
                .map_err(NodeError::from)?;
            total_usage.accumulate(&response.usage);

            // LLM chose to stop — return final answer
            if response.tool_calls.is_empty() {
                final_text = response.content;
                break;
            }

            // LLM wants to call tools
            messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: ChatContent::Text(response.content.clone()),
                tool_calls: response.tool_calls.clone(),
                tool_call_id: None,
            });

            for tc in &response.tool_calls {
                // Security check
                #[cfg(feature = "security")]
                self.check_tool_security(context, node_id, &tc.name, &tc.arguments).await?;

                // Execute MCP tool
                let result = self.mcp_pool.read().await
                    .call_tool(&tc.name, &tc.arguments).await;

                let (content, is_error) = match result {
                    Ok(text) => (text, false),
                    Err(e) => (e.to_string(), true),
                };

                // Record for audit
                tool_call_log.push(json!({
                    "iteration": iteration,
                    "tool": &tc.name,
                    "arguments": &tc.arguments,
                    "result": &content,
                    "is_error": is_error,
                }));

                // Append tool result to conversation
                messages.push(ChatMessage {
                    role: ChatRole::Tool,
                    content: ChatContent::Text(content),
                    tool_calls: vec![],
                    tool_call_id: Some(tc.id.clone()),
                });
            }

            // Last iteration: force text reply by removing tools
            if iteration == cfg.max_iterations - 2 {
                // Next iteration will be the last, remove tools to force conclusion
            }
        }

        // If loop ended by max_iterations (no natural stop)
        if final_text.is_empty() {
            let force_request = ChatCompletionRequest {
                model: cfg.model.name.clone(),
                messages,
                tools: vec![],  // No tools = force text reply
                stream: false,
                ..Default::default()
            };
            let response = provider.chat_completion(force_request).await
                .map_err(NodeError::from)?;
            total_usage.accumulate(&response.usage);
            final_text = response.content;
        }

        // Build output
        let iterations_used = tool_call_log.len() as i64;
        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync({
                let mut m = HashMap::new();
                m.insert("text".to_string(), Segment::String(final_text));
                m.insert("tool_calls".to_string(),
                    Segment::from_value(&Value::Array(tool_call_log)));
                m.insert("iterations".to_string(),
                    Segment::Integer(iterations_used));
                m
            }),
            llm_usage: Some(total_usage),
            ..Default::default()
        })
    }
}
```

### 6.4 DSL 示例

```yaml
# 方式 1: 内联 MCP Server
- id: research
  data:
    type: agent
    title: "Research Assistant"
    model:
      provider: openai
      name: gpt-4o
      completion_params:
        temperature: 0.2
    system_prompt: |
      You are a senior technical support engineer.
      Use the available tools to investigate the issue.
      Always search before answering. Cite sources.
    query: "{{start.question}}"
    tools:
      mcp_servers:
        - name: knowledge_base
          transport: stdio
          command: "python"
          args: ["-m", "kb_mcp_server"]
        - name: github
          transport: http
          url: "https://mcp.github.com/sse"
    max_iterations: 8
    memory:
      enabled: true
      window_size: 10
      variable_selector: ["sys", "conversation_history"]

# 方式 2: 引用 workflow 级 MCP Server
- id: analyst
  data:
    type: agent
    title: "Data Analyst"
    model:
      provider: openai
      name: gpt-4o
    system_prompt: "You analyze data using SQL queries."
    query: "Analyze sales trends for {{start.product}}"
    tools:
      mcp_server_refs: [database, dashboard]
    max_iterations: 5
```

### 6.5 下游变量访问

```
{{research.text}}        — 最终回答文本
{{research.tool_calls}}  — 工具调用记录（JSON Array）
{{research.iterations}}  — 实际 LLM 调用轮数
```

---

## 7. Workflow 级 MCP Server 声明（可选）

**文件**: `src/dsl/schema.rs`

```rust
// WorkflowSchema 新增可选字段
pub struct WorkflowSchema {
    pub version: String,
    pub nodes: Vec<NodeSchema>,
    pub edges: Vec<EdgeSchema>,
    pub environment_variables: Vec<EnvironmentVariable>,
    pub conversation_variables: Vec<ConversationVariable>,
    pub error_handler: Option<ErrorHandlerConfig>,
    /// Workflow-level MCP server declarations (referenced by name from nodes)
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}
```

DSL 示例：

```yaml
version: "0.1.0"
mcp_servers:
  knowledge_base:
    transport: stdio
    command: "python"
    args: ["-m", "kb_server"]
  github:
    transport: http
    url: "https://mcp.github.com/sse"

nodes:
  - id: agent_1
    data:
      type: agent
      tools:
        mcp_server_refs: [knowledge_base, github]
      # ...
```

---

## 8. 共享辅助函数提取

**文件**: `src/llm/executor.rs`

与 question-classifier-design.md 一致，将以下函数改为 `pub(crate) fn`：

| 函数 | 行号 | 用途 |
|------|------|------|
| `append_memory_messages` | 379 | 构建对话记忆 messages |
| `map_role` | 331 | 角色字符串 → ChatRole |
| `audit_security_event` | 305 | 安全审计日志（cfg-gated） |

新增共享函数（用于 tool 和 agent 节点的模板参数渲染）：

```rust
/// 渲染 arguments 中的模板变量
pub(crate) fn render_arguments(
    arguments: &HashMap<String, Value>,
    variable_pool: &VariablePool,
) -> Result<Value, NodeError> {
    // 遍历每个值，对 String 类型执行模板渲染
    let mut rendered = serde_json::Map::new();
    for (key, value) in arguments {
        let rendered_value = match value {
            Value::String(s) => {
                let rendered = render_template_sync(s, variable_pool)?;
                Value::String(rendered)
            }
            other => other.clone(),
        };
        rendered.insert(key.clone(), rendered_value);
    }
    Ok(Value::Object(rendered))
}
```

---

## 9. 注册与 Feature Gating

### 9.1 Feature Flag

**文件**: `Cargo.toml`

```toml
[dependencies]
rmcp = { version = "0.1", features = ["client", "transport-child-process", "transport-sse"], optional = true }

[features]
default = [
    "security", "plugin-system", "compiler", "workflow-cache",
    "builtin-template-jinja",
    "builtin-core-nodes", "builtin-transform-nodes",
    "builtin-http-node", "builtin-subgraph-nodes", "builtin-llm-node",
    # "builtin-agent-node",  # 默认不开启（需要 rmcp 依赖）
]

# Agent 节点 = LLM tool calling + MCP client（均在 src/ 内）
builtin-agent-node = ["dep:rmcp", "builtin-llm-node"]
```

`builtin-agent-node` 启用 `rmcp` 可选依赖 + 依赖 `builtin-llm-node`（需要 `LlmProviderRegistry`）。
未启用时 `src/mcp/` 模块整体不编译，零开销。

与现有模式一致：`builtin-code-node = ["builtin-sandbox-js"]` 依赖沙箱而非外部 crate。

### 9.2 Executor 注册

**文件**: `src/nodes/executor.rs`

```rust
#[cfg(feature = "builtin-agent-node")]
pub fn set_mcp_pool(&mut self, pool: Arc<RwLock<crate::mcp::pool::McpConnectionPool>>) {
    // tool 节点
    self.register("tool", Box::new(super::tool::ToolNodeExecutor::new(pool.clone())));

    // agent 节点（需要 llm_registry，必须在 set_llm_provider_registry 之后）
    if let Some(ref llm_reg) = self.llm_registry {
        self.register("agent", Box::new(super::agent::AgentNodeExecutor::new(
            llm_reg.clone(), pool,
        )));
    }
}
```

现有的 `StubExecutor("tool")` 和 `StubExecutor("agent")` 在 `with_builtins()` 中保留，当 feature 启用时 `register()` 会覆盖它们。

### 9.3 从 STUB_NODE_TYPES 移除（当 feature 启用时）

**文件**: `src/dsl/validation/known_types.rs`

```rust
#[cfg(feature = "builtin-agent-node")]
const IMPLEMENTED_AGENT_TYPES: &[&str] = &["tool", "agent"];
// 在验证逻辑中，如果 node_type in IMPLEMENTED_AGENT_TYPES 则跳过 stub 警告
```

---

## 10. Security 相关

### 10.1 MCP 工具调用安全检查

每次 MCP tool 调用前执行：

```rust
#[cfg(feature = "security")]
async fn check_tool_security(
    &self,
    context: &RuntimeContext,
    node_id: &str,
    tool_name: &str,
    arguments: &Value,
) -> Result<(), NodeError> {
    // 1. SecurityGate 前置检查
    let tool_config = json!({
        "mcp_tool": tool_name,
        "arguments": arguments,
    });
    context.security_gate()
        .check_before_node(node_id, "mcp-tool-call", &tool_config).await?;

    // 2. 审计日志
    audit_security_event(
        context,
        SecurityEventType::ToolInvocation {
            tool_name: tool_name.to_string(),
        },
        EventSeverity::Info,
        Some(node_id.to_string()),
    ).await;

    Ok(())
}
```

### 10.2 MCP Server 连接安全

- **stdio transport**: 子进程继承最小权限，不透传父进程所有环境变量
- **HTTP transport**: URL 必须通过 `NetworkPolicy` 验证（SSRF 防护）
- **Credentials**: MCP Server 的认证 token 通过 `CredentialProvider` 获取，不硬编码在 DSL 中

### 10.3 Agent 循环安全边界

| 防护 | 机制 |
|------|------|
| 无限循环 | `max_iterations` 硬上限，默认 10 |
| Token 爆炸 | 累计 `LlmUsage`，可被 `ResourceGovernor` 限制 |
| 工具滥用 | 每次调用经过 `SecurityGate` |
| 敏感数据泄露 | `tool_calls` 输出记录完整调用链，可审计 |

---

## 11. 测试策略

### 11.1 单元测试

**文件**: `src/nodes/tool.rs`, `src/nodes/agent.rs`

| 测试 | 验证内容 |
|------|---------|
| `test_tool_node_basic` | MockMcpClient + 指定工具调用 → 返回 result |
| `test_tool_node_missing_server` | 无 mcp_server 配置 → ConfigError |
| `test_tool_node_template_args` | `{{start.id}}` 在参数中正确渲染 |
| `test_agent_no_tool_calls` | LLM 直接返回文本 → 0 iterations |
| `test_agent_single_iteration` | LLM 调一次工具后停止 → 1 iteration |
| `test_agent_max_iterations` | LLM 持续调工具直到上限 → forced stop |
| `test_agent_tool_error` | 工具返回错误 → LLM 收到错误信息继续推理 |
| `test_agent_empty_tools` | 无工具配置 → 等同于 llm 节点 |

### 11.2 集成测试

使用 mock MCP server + mock LLM server：

#### Case 130: `tool_node_basic`

- mock MCP server: 暴露 `search` tool，返回固定结果
- workflow: `start → tool(search) → end`
- 验证：输出 `result` 包含 mock 数据

#### Case 131: `agent_node_basic`

- mock LLM: 第一次返回 tool_call，第二次返回 text
- mock MCP: 暴露 `search` tool
- workflow: `start → agent → end`
- 验证：输出 `text`、`tool_calls`（1 次）、`iterations`（2）

#### Case 132: `agent_node_max_iterations`

- mock LLM: 始终返回 tool_calls
- workflow: agent 配置 max_iterations=3
- 验证：强制结束，iterations=3

---

## 12. 文件变更清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `src/llm/types.rs` | 修改 | 添加 ToolDefinition, ToolCall, ToolResult, ChatRole::Tool, ChatMessage 扩展 |
| `src/llm/provider/openai.rs` | 修改 | build_payload/build_messages/parse_response 支持 tools |
| `src/llm/executor.rs` | 修改 | 3 个函数改为 `pub(crate)`（同 question-classifier） |
| `src/dsl/schema.rs` | 修改 | 添加 ToolNodeData, AgentNodeData, AgentToolsConfig；`mcp_servers` 字段引用 `crate::mcp::config::McpServerConfig` |
| `src/nodes/tool.rs` | **新建** | ToolNodeExecutor 实现 |
| `src/nodes/agent.rs` | **新建** | AgentNodeExecutor 实现 |
| `src/nodes/mod.rs` | 修改 | 添加 tool, agent 模块声明 |
| `src/nodes/executor.rs` | 修改 | 添加 set_mcp_pool 注册方法 |
| `src/mcp/mod.rs` | **新建** | MCP 模块声明（pub mod config, client, pool, tools, error） |
| `src/mcp/config.rs` | **新建** | McpServerConfig, McpTransport 类型定义 |
| `src/mcp/client.rs` | **新建** | McpClient — rmcp 封装（connect_stdio, connect_http, list_tools, call_tool） |
| `src/mcp/pool.rs` | **新建** | McpConnectionPool — 按 config hash 复用连接 |
| `src/mcp/tools.rs` | **新建** | discover_tools() — MCP tool → ToolDefinition 零转换 |
| `src/mcp/error.rs` | **新建** | McpError 枚举 |
| `src/lib.rs` | 修改 | 添加 `#[cfg(feature = "builtin-agent-node")] pub mod mcp;` |
| `Cargo.toml` | 修改 | 添加 `rmcp` 可选依赖 + `builtin-agent-node` feature |
| `src/dsl/validation/known_types.rs` | 修改 | feature-gate 条件下移除 tool/agent 的 stub 标记 |

---

## 13. 实施顺序

1. `Cargo.toml` — 添加 `rmcp` 可选依赖 + `builtin-agent-node` feature
2. `src/llm/types.rs` — 添加 Tool Calling 类型（基础设施，不影响现有行为）
3. `src/llm/provider/openai.rs` — Provider 支持 tools 参数
4. `src/llm/executor.rs` — 提取共享函数为 `pub(crate)`
5. `src/mcp/` — MCP Client 模块（config, client, pool, tools, error）
6. `src/lib.rs` — 添加 cfg-gated `pub mod mcp`
7. `src/dsl/schema.rs` — DSL 类型定义
8. `src/nodes/tool.rs` — Tool 节点实现
9. `src/nodes/agent.rs` — Agent 节点实现
10. `src/nodes/executor.rs` — 注册新节点
11. 单元测试 + 集成测试

---

## 14. 验证命令

```bash
# 构建含 agent 节点
cargo build --features builtin-agent-node

# 不含 agent 节点（确认 feature gate 正确，零残留）
cargo build

# 全量构建（确认不破坏现有功能）
cargo build --all-features

# 测试
cargo test --all-features --workspace --lib tool_node
cargo test --all-features --workspace --lib agent_node
cargo test --all-features --workspace --lib mcp
cargo test --all-features --workspace --test integration_tests -- 130
cargo test --all-features --workspace --test integration_tests -- 131
cargo test --all-features --workspace --test integration_tests -- 132
cargo test --all-features --workspace
cargo clippy --all-features --workspace
```

---

## 15. 未来扩展

### 15.1 LLM 节点单轮 Tool Calling（L2）

LLM 节点增加可选的 `tools` 配置，支持单轮 tool calling。当 LLM 返回 tool_calls 时执行一次并将结果追加到输出中，不循环。这是 agent 节点的"轻量版"。

### 15.2 A2A 协议支持

当 A2A Rust SDK 成熟后，可扩展：
- `agent` 节点新增 `a2a_agents` 配置项（指定外部 A2A Agent 地址）
- 将 A2A Agent 包装为 `ToolDefinition`，让 LLM 统一选择
- xworkflow 对外发布 Agent Card，可被其他 A2A Agent 发现和调用

### 15.3 MCP Server 模式

将 xworkflow 作为 MCP Server 暴露给 AI 助手：
- `run_workflow` tool — 执行工作流
- `list_workflows` tool — 列出可用工作流
- `workflow://{id}/status` resource — 查询执行状态
