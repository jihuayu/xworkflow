# LLM 节点实现设计 — Provider 架构 + OpenAI 内置实现

## Context

xworkflow 当前的 LLM 节点（`"llm"` 类型）是一个 StubExecutor 占位实现，DSL schema 中已定义了 `LlmNodeData`、`ModelConfig`、`CompletionParams`、`PromptMessage`、`LlmUsage` 等类型。需要实现真正的 LLM 节点，核心设计：

- **Provider 抽象**：`LlmProvider` trait，所有 LLM 服务商实现此 trait
- **双来源 Provider**：
  1. **内置 Rust Provider**：编译期内置，如 OpenAI
  2. **WASM 动态 Provider**：运行时通过 WASM 模块加载，利用已有的 wasmtime 基础设施
- **首个内置实现**：OpenAI Chat Completions API（含 SSE 流式）

### 已有基础设施

| 组件 | 文件 | 状态 |
|------|------|------|
| LlmNodeData / ModelConfig / CompletionParams | `src/dsl/schema.rs:476-526` | 已定义 |
| LlmUsage | `src/dsl/schema.rs:576-590` | 已定义 |
| NodeRunResult.llm_usage | `src/dsl/schema.rs:552` | 已有字段 |
| NodeRunStreamChunk 事件 | `src/core/event_bus.rs:63-70` | 已定义 |
| StubExecutor("llm") | `src/nodes/executor.rs:46` | 待替换 |
| reqwest 0.12 | `Cargo.toml:29` | 已依赖 |
| wasmtime 27 | `Cargo.toml` | 已依赖 |
| NodeExecutor trait | `src/nodes/executor.rs:12-22` | 已定义 |
| PromptMessage.role/text | `src/dsl/schema.rs:504-510` | 已定义 |

---

## 一、新增依赖

```toml
# Cargo.toml 新增
tokio-stream = "0.1"         # SSE 流式解析
eventsource-stream = "0.2"   # SSE 协议解析（配合 reqwest streaming）
```

> reqwest（已有）+ eventsource-stream 组合处理 OpenAI SSE 流。

---

## 二、模块结构

```
src/
  llm/
    mod.rs              # 模块声明 + LlmProvider trait + LlmProviderRegistry + re-exports
    types.rs            # ChatMessage, ChatCompletionRequest/Response, StreamChunk 等
    error.rs            # LlmError
    executor.rs         # LlmNodeExecutor（实现 NodeExecutor trait）
    provider/
      mod.rs            # provider 子模块声明
      openai.rs         # OpenAI 内置 provider
      wasm_provider.rs  # WasmLlmProvider — WASM 动态 provider
  lib.rs                # 添加 pub mod llm;
```

---

## 三、核心类型（`llm/types.rs`）

```rust
/// 聊天角色
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

/// 消息内容（支持纯文本和多模态）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatContent {
    Text(String),
    MultiModal(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlDetail },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrlDetail {
    pub url: String,
    pub detail: Option<String>,  // "auto" | "low" | "high"
}

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatContent,
}

/// 请求
#[derive(Debug, Clone)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i32>,
    pub stream: bool,
    /// 按需覆盖的凭证（api_key 等），优先于 provider 默认配置
    pub credentials: HashMap<String, String>,
}

/// 非流式响应
#[derive(Debug, Clone)]
pub struct ChatCompletionResponse {
    pub content: String,
    pub usage: LlmUsage,
    pub model: String,
    pub finish_reason: Option<String>,
}

/// 流式 chunk
#[derive(Debug, Clone)]
pub struct StreamChunk {
    pub delta: String,
    pub finish_reason: Option<String>,
    /// 仅最后一个 chunk 包含 usage
    pub usage: Option<LlmUsage>,
}

/// Provider 描述信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub max_tokens: Option<i32>,
}
```

---

## 四、LlmProvider Trait（`llm/mod.rs`）

```rust
/// LLM 服务提供者抽象
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 提供者唯一标识（如 "openai", "anthropic"）
    fn id(&self) -> &str;

    /// 提供者信息
    fn info(&self) -> ProviderInfo;

    /// 非流式 Chat Completion
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, LlmError>;

    /// 流式 Chat Completion — 通过 channel 发送 StreamChunk
    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
        chunk_tx: mpsc::Sender<StreamChunk>,
    ) -> Result<ChatCompletionResponse, LlmError>;
}
```

### LlmProviderRegistry

```rust
pub struct LlmProviderRegistry {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
}

impl LlmProviderRegistry {
    pub fn new() -> Self;

    /// 注册内置 provider
    pub fn register(&mut self, provider: Arc<dyn LlmProvider>);

    /// 获取 provider
    pub fn get(&self, provider_id: &str) -> Option<Arc<dyn LlmProvider>>;

    /// 列出所有 provider
    pub fn list(&self) -> Vec<ProviderInfo>;

    /// 加载 WASM provider
    pub fn register_wasm_provider(
        &mut self,
        manifest: WasmProviderManifest,
        wasm_bytes: &[u8],
    ) -> Result<(), LlmError>;

    /// 便捷方法：创建含默认内置 provider 的 registry
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        // 注册 OpenAI（从环境变量读取配置）
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            reg.register(Arc::new(OpenAiProvider::new(OpenAiConfig {
                api_key,
                base_url: std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
                org_id: std::env::var("OPENAI_ORG_ID").ok(),
                default_model: "gpt-4o".into(),
            })));
        }
        reg
    }
}
```

---

## 五、OpenAI Provider（`llm/provider/openai.rs`）

### 5.1 配置

```rust
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub base_url: String,         // 默认 "https://api.openai.com/v1"
    pub org_id: Option<String>,
    pub default_model: String,    // 默认 "gpt-4o"
}
```

### 5.2 实现

```rust
pub struct OpenAiProvider {
    config: OpenAiConfig,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Self;

    /// 构建请求 headers（Authorization, Organization 等）
    fn build_headers(&self, credentials: &HashMap<String, String>) -> HeaderMap;

    /// 解析非流式响应 JSON
    fn parse_response(body: &Value) -> Result<ChatCompletionResponse, LlmError>;

    /// 解析 SSE data 行中的 delta
    fn parse_stream_chunk(data: &str) -> Result<Option<StreamChunk>, LlmError>;
}
```

### 5.3 API 调用流程

**非流式**：
```
POST {base_url}/chat/completions
Authorization: Bearer {api_key}
Content-Type: application/json

{
  "model": "gpt-4o",
  "messages": [...],
  "temperature": 0.7,
  "max_tokens": 4096,
  "stream": false
}

→ 解析 response.choices[0].message.content
→ 解析 response.usage → LlmUsage
```

**流式**：
```
POST {base_url}/chat/completions  (stream: true)
→ SSE 事件流
→ 每行 data: {...} 解析 choices[0].delta.content
→ 通过 chunk_tx 发送 StreamChunk
→ 遇到 data: [DONE] 结束
→ 聚合完整内容 + usage 返回 ChatCompletionResponse
```

### 5.4 凭证优先级

```
request.credentials["api_key"]  →  provider.config.api_key
request.credentials["base_url"] →  provider.config.base_url
```

节点 DSL 配置中可以通过 `model.credentials` 字段或引用环境变量来覆盖。

---

## 六、WASM 动态 Provider（`llm/provider/wasm_provider.rs`）

### 6.1 Manifest

```json
{
  "id": "anthropic",
  "name": "Anthropic Claude",
  "version": "1.0.0",
  "models": [
    { "id": "claude-3-opus", "name": "Claude 3 Opus", "max_tokens": 4096 },
    { "id": "claude-3-sonnet", "name": "Claude 3 Sonnet", "max_tokens": 4096 }
  ],
  "wasm_file": "provider.wasm",
  "config_schema": {
    "api_key": { "type": "string", "required": true, "env_var": "ANTHROPIC_API_KEY" },
    "base_url": { "type": "string", "default": "https://api.anthropic.com/v1" }
  }
}
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmProviderManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub models: Vec<ModelInfo>,
    pub wasm_file: String,
    pub config_schema: HashMap<String, WasmConfigField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmConfigField {
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub env_var: Option<String>,
}
```

### 6.2 WASM 模块导出约定

```wat
;; 内存管理（必须）
(export "alloc" (func $alloc))           ;; fn(size: i32) -> i32
(export "dealloc" (func $dealloc))       ;; fn(ptr: i32, size: i32)

;; Chat Completion（必须）
(export "chat_completion" (func))
;; fn(request_ptr: i32, request_len: i32) -> i32
;; 输入: ChatCompletionRequest JSON
;; 输出: 指向 [result_ptr, result_len] 的指针，内容为 ChatCompletionResponse JSON
;; WASM 内部通过 host function http_request 调用外部 API
```

### 6.3 Host Functions（WASM 可调用）

```wat
(import "xworkflow" "http_request" (func (param i32 i32) (result i32 i32)))
;; 发起 HTTP 请求（宿主代理执行）
;; 输入: JSON {"method":"POST","url":"...","headers":{"Authorization":"Bearer ..."},"body":"..."}
;; 输出: JSON {"status":200,"headers":{},"body":"..."}

(import "xworkflow" "log" (func (param i32 i32 i32)))
;; 日志输出
```

### 6.4 WasmLlmProvider 结构

```rust
pub struct WasmLlmProvider {
    manifest: WasmProviderManifest,
    engine: wasmtime::Engine,
    module: wasmtime::Module,
    config: HashMap<String, String>,  // 解析后的配置
}

#[async_trait]
impl LlmProvider for WasmLlmProvider {
    fn id(&self) -> &str { &self.manifest.id }
    fn info(&self) -> ProviderInfo { /* from manifest */ }

    async fn chat_completion(&self, request: ChatCompletionRequest)
        -> Result<ChatCompletionResponse, LlmError>
    {
        // 1. 将 request + config 序列化为 JSON
        // 2. spawn_blocking 中执行 WASM
        // 3. WASM 内部通过 http_request host function 调用外部 API
        // 4. 反序列化返回结果
    }

    async fn chat_completion_stream(&self, request: ChatCompletionRequest, chunk_tx: ...)
        -> Result<ChatCompletionResponse, LlmError>
    {
        // WASM provider 暂不支持真正的流式，退化为非流式
        // 完成后发送一个完整 chunk
        let resp = self.chat_completion(request).await?;
        chunk_tx.send(StreamChunk {
            delta: resp.content.clone(),
            finish_reason: resp.finish_reason.clone(),
            usage: Some(resp.usage.clone()),
        }).await.ok();
        Ok(resp)
    }
}
```

> 注意：WASM provider 由于 wasmtime 的同步特性，流式输出需要特殊处理。初始实现中 WASM provider 的流式模式退化为非流式（完成后一次性输出），后续可通过 WASM host callback 机制实现真正的流式。

---

## 七、LlmNodeExecutor（`llm/executor.rs`）

### 7.1 结构

```rust
pub struct LlmNodeExecutor {
    registry: Arc<LlmProviderRegistry>,
}
```

### 7.2 执行流程

```
┌──────────────────────────────────────────────────────────┐
│ LlmNodeExecutor::execute(node_id, config, pool, context) │
│                                                           │
│ 1. 反序列化 config → LlmNodeData                         │
│    - model: { provider, name, completion_params }         │
│    - prompt_template: [{ role, text }]                    │
│    - context / vision (可选)                              │
│                                                           │
│ 2. 解析 prompt_template，替换变量                          │
│    - text 中的 {{#node_id.var_name#}} → 从 pool 获取值     │
│    - 构建 Vec<ChatMessage>                                │
│                                                           │
│ 3. 如果 context.enabled，注入上下文到 system message       │
│    - 从 context.variable_selector 获取上下文内容           │
│    - 追加到 system prompt 末尾                             │
│                                                           │
│ 4. 如果 vision.enabled，处理图片内容                       │
│    - 从 vision.variable_selector 获取图片 URL/数据         │
│    - 构建 MultiModal content                              │
│                                                           │
│ 5. 从 registry 获取 provider                              │
│    - provider = registry.get(model.provider)              │
│                                                           │
│ 6. 构建 ChatCompletionRequest                             │
│    - model, messages, temperature, top_p, max_tokens      │
│    - credentials: 从 env vars / node config 提取          │
│                                                           │
│ 7. 判断是否流式                                           │
│    - 如果 config 中有 stream: true 且 context 有 event_tx  │
│    → 调用 chat_completion_stream                          │
│    → 每个 chunk 通过 event_tx 发送 NodeRunStreamChunk      │
│    - 否则调用 chat_completion                             │
│                                                           │
│ 8. 构建 NodeRunResult                                     │
│    - outputs: { "text": content, "usage": usage_json }    │
│    - llm_usage: Some(usage)                               │
│    - metadata: { "model": model_name, "provider": id }    │
└──────────────────────────────────────────────────────────┘
```

### 7.3 变量替换

prompt_template 中的 `text` 字段使用 Dify 模板语法 `{{#node_id.var_name#}}`，复用已有的 `template::engine::render_template()`。

### 7.4 流式事件集成

为了让 LlmNodeExecutor 能发送流式事件，需要扩展 `RuntimeContext`：

```rust
// src/core/runtime_context.rs — 新增字段
pub struct RuntimeContext {
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,  // 新增
}
```

Dispatcher 在创建 RuntimeContext 时注入 event_tx，LlmNodeExecutor 在执行时从 context 中取出使用。

---

## 八、错误类型（`llm/error.rs`）

```rust
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    #[error("Model not supported: {0}")]
    ModelNotSupported(String),

    #[error("Authentication error: {0}")]
    AuthenticationError(String),

    #[error("Rate limit exceeded: retry after {retry_after:?}s")]
    RateLimitExceeded { retry_after: Option<u64> },

    #[error("API error ({status}): {message}")]
    ApiError { status: u16, message: String },

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Timeout")]
    Timeout,

    #[error("Stream error: {0}")]
    StreamError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("WASM provider error: {0}")]
    WasmError(String),
}

impl From<LlmError> for NodeError {
    fn from(e: LlmError) -> Self {
        NodeError::ExecutionError(e.to_string())
    }
}
```

---

## 九、引擎集成

### 9.1 NodeExecutorRegistry 改动

`src/nodes/executor.rs`：替换 `StubExecutor("llm")` → `LlmNodeExecutor`

```rust
// NodeExecutorRegistry::new() 中
// 旧: registry.register("llm", Box::new(StubExecutor("llm")));
// 新: 不在 new() 中注册 LLM，而是通过 builder 注入

// 新增方法
impl NodeExecutorRegistry {
    pub fn set_llm_provider_registry(&mut self, registry: Arc<LlmProviderRegistry>) {
        self.executors.insert(
            "llm".to_string(),
            Box::new(LlmNodeExecutor::new(registry)),
        );
    }
}
```

### 9.2 WorkflowRunnerBuilder 改动

`src/scheduler.rs`：

```rust
pub struct WorkflowRunnerBuilder {
    // ... 已有字段
    llm_provider_registry: Option<Arc<LlmProviderRegistry>>,  // 新增
}

impl WorkflowRunnerBuilder {
    pub fn llm_providers(mut self, registry: Arc<LlmProviderRegistry>) -> Self {
        self.llm_provider_registry = Some(registry);
        self
    }
}
```

在 `run()` 方法中：
```rust
let mut registry = NodeExecutorRegistry::new();
if let Some(llm_reg) = &self.llm_provider_registry {
    registry.set_llm_provider_registry(llm_reg.clone());
}
```

### 9.3 RuntimeContext 改动

`src/core/runtime_context.rs`：新增 `event_tx` 字段。

`src/core/dispatcher.rs`：创建 context 时注入 event_tx（或在执行节点前临时设置）。

### 9.4 DSL Schema 补充

`src/dsl/schema.rs` 中 LlmNodeData 新增可选字段：

```rust
pub struct LlmNodeData {
    pub model: ModelConfig,
    pub prompt_template: Vec<PromptMessage>,
    pub context: Option<ContextConfig>,
    pub vision: Option<VisionConfig>,
    pub memory: Option<MemoryConfig>,      // 新增：对话历史
    pub stream: Option<bool>,               // 新增：是否流式
}

// 新增：对话历史配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub window_size: Option<i32>,          // 保留最近 N 轮对话
    pub variable_selector: Option<VariableSelector>,  // 历史消息来源
}

// ModelConfig 新增 credentials
pub struct ModelConfig {
    pub provider: String,
    pub name: String,
    pub mode: Option<String>,
    pub completion_params: Option<CompletionParams>,
    pub credentials: Option<HashMap<String, String>>,  // 新增
}
```

---

## 十、两种 Provider 来源对比

| | 内置 Rust Provider | WASM 动态 Provider |
|---|---|---|
| 示例 | OpenAI, Anthropic | 任意第三方 |
| 编译 | 随 xworkflow 编译 | 运行时加载 .wasm |
| 流式支持 | 完整 SSE 流式 | 退化为非流式（初期） |
| 性能 | 原生性能 | WASM 开销（主要在 HTTP 代理） |
| HTTP | 直接使用 reqwest | 通过 host function 代理 |
| 扩展方式 | 修改代码重新编译 | 放置 .wasm + manifest.json |
| 安全性 | 完全信任 | WASM 沙箱隔离 |

---

## 十一、文件改动清单

### 新增文件

| 文件 | 说明 |
|------|------|
| `src/llm/mod.rs` | 模块声明 + LlmProvider trait + LlmProviderRegistry |
| `src/llm/types.rs` | ChatMessage, Request/Response, StreamChunk 等 |
| `src/llm/error.rs` | LlmError |
| `src/llm/executor.rs` | LlmNodeExecutor（NodeExecutor 实现） |
| `src/llm/provider/mod.rs` | provider 子模块声明 |
| `src/llm/provider/openai.rs` | OpenAI 内置 provider |
| `src/llm/provider/wasm_provider.rs` | WASM 动态 provider |

### 修改文件

| 文件 | 改动 |
|------|------|
| `Cargo.toml` | 添加 tokio-stream, eventsource-stream |
| `src/lib.rs` | 添加 `pub mod llm;` |
| `src/nodes/executor.rs` | 新增 `set_llm_provider_registry()` 方法 |
| `src/scheduler.rs` | WorkflowRunnerBuilder 新增 `llm_providers()` / `llm_provider_registry` 字段 |
| `src/core/runtime_context.rs` | RuntimeContext 新增 `event_tx` 字段 |
| `src/core/dispatcher.rs` | 传递 event_tx 到 RuntimeContext |
| `src/dsl/schema.rs` | LlmNodeData 新增 stream/memory 字段，ModelConfig 新增 credentials |

---

## 十二、实施步骤

### Phase 1：基础设施
1. `Cargo.toml` — 添加 tokio-stream, eventsource-stream 依赖
2. `src/llm/error.rs` — LlmError 定义
3. `src/llm/types.rs` — 核心类型定义
4. `src/llm/mod.rs` — LlmProvider trait + LlmProviderRegistry
5. `src/lib.rs` — 导出 llm 模块

### Phase 2：OpenAI Provider
6. `src/llm/provider/mod.rs` — provider 子模块
7. `src/llm/provider/openai.rs` — OpenAiProvider 完整实现
   - 非流式 chat completion
   - SSE 流式 chat completion
   - 凭证管理（环境变量 + 请求覆盖）
   - 错误处理（rate limit, auth error, API error）

### Phase 3：LLM Node Executor
8. `src/llm/executor.rs` — LlmNodeExecutor
   - 解析 LlmNodeData
   - 变量替换 prompt template
   - 上下文注入
   - 调用 provider
   - 流式事件发送
9. `src/nodes/executor.rs` — 新增 set_llm_provider_registry()
10. `src/scheduler.rs` — builder 新增 llm_providers()
11. `src/core/runtime_context.rs` — 新增 event_tx
12. `src/core/dispatcher.rs` — 注入 event_tx
13. `src/dsl/schema.rs` — 补充 LlmNodeData 字段

### Phase 4：WASM Provider
14. `src/llm/provider/wasm_provider.rs` — WasmLlmProvider
   - Manifest 解析
   - WASM 模块加载和调用
   - HTTP host function 注入
   - 注册到 LlmProviderRegistry

### Phase 5：测试
15. OpenAI provider 单元测试（mock HTTP）
16. LlmNodeExecutor 单元测试
17. WASM provider 测试
18. 端到端测试：LLM 节点在工作流中执行

---

## 十三、DSL 示例

```yaml
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
      variables:
        - variable: query
          label: Query
          type: string
          required: true
  - id: llm1
    data:
      type: llm
      title: GPT-4o
      model:
        provider: openai
        name: gpt-4o
        completion_params:
          temperature: 0.7
          max_tokens: 2048
      prompt_template:
        - role: system
          text: "You are a helpful assistant."
        - role: user
          text: "{{#start.query#}}"
      stream: true
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["llm1", "text"]
edges:
  - source: start
    target: llm1
  - source: llm1
    target: end
```

---

## 十四、验证方式

```bash
# 全量编译
cargo build

# 全量测试
cargo test

# LLM 模块测试
cargo test llm

# OpenAI provider 测试（需要 mock 或 OPENAI_API_KEY）
cargo test openai

# 端到端（需要 API key）
OPENAI_API_KEY=sk-xxx cargo test test_llm_node_e2e
```
