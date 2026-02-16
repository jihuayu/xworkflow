# Memory 系统设计文档

## 1. 概述

### 1.1 背景

xworkflow 当前仅有简单的 LLM 对话窗口记忆（`MemoryConfig` + `append_memory_messages()`），通过 `VariablePool` 中的 `conversation_variables` 实现滑动窗口历史。这不足以支撑现代 AI 工作流的需求：跨轮次记忆用户偏好、长期知识积累、语义检索等。

### 1.2 行业调研

| 系统 | 记忆类型 | 存储后端 | 作用域 | 检索机制 |
|------|---------|---------|--------|---------|
| Claude Code | 过程性（CLAUDE.md）+ 情景性（auto-memory） | 本地文件系统 | 用户/项目/目录/本地 | 文件读取 |
| ChatGPT | 语义性（saved memories）+ 情景性（chat history） | 服务端 | 用户全局 | 上下文注入 |
| n8n | 短期（buffer）+ 长期（DB）+ 语义（vector） | InMemory/MongoDB/Redis/Postgres/向量库 | 工作流会话 | 窗口 + 语义搜索 |
| LangGraph | 短期（Checkpointer）+ 长期（Memory Store） | SQLite/Postgres/MongoDB | Thread + Namespace | 自动加载 + 语义过滤 |
| Dify | 对话历史 + 对话变量 + 知识库 | 内置 + 向量库 | 对话/Bot/全局 | 窗口 + 向量/全文/混合搜索 |
| CrewAI | 统一记忆（语义+情景+过程性） | LanceDB | 层级化作用域 | 浅层向量 + 深层 LLM 引导 |
| Temporal | 执行状态（事件历史） | Cassandra/MySQL/PostgreSQL | 工作流执行 | 事件回放 |

### 1.3 设计原则

遵循项目三大原则：**Security > Performance > Obviousness**。

| 原则 | 体现 |
|------|------|
| Security | 命名空间访问控制、值大小限制、审计日志、秘密泄露防护 |
| Performance | Feature gate 零开销、trait 对象无额外分配、命名空间解析 O(1) |
| Obviousness | 两种使用模式（显式节点/增强配置）语义清晰、MemoryProvider trait 方法明确 |

### 1.4 核心思路

xworkflow 作为嵌入式库，**不拥有存储后端**。提供 `MemoryProvider` trait 接口，宿主应用注入实现（Redis、向量库、SQLite 等）。内置 `InMemoryProvider` 仅用于测试和简单场景。

---

## 2. 核心 Trait 定义

### 2.1 MemoryProvider Trait

**文件**: `src/memory/provider.rs`

遵循 `CredentialProvider`（`src/security/credential.rs`）的模式：`async_trait + Send + Sync`，存储为 `Option<Arc<dyn MemoryProvider>>`。

```rust
/// 记忆操作错误类型
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("Memory not found: namespace={namespace}, key={key}")]
    NotFound { namespace: String, key: String },
    #[error("Memory access denied: namespace={namespace}")]
    AccessDenied { namespace: String },
    #[error("Memory provider error: {0}")]
    ProviderError(String),
    #[error("Memory quota exceeded: {0}")]
    QuotaExceeded(String),
}

/// 单条记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// 条目唯一标识
    pub id: String,
    /// 所属命名空间
    pub namespace: String,
    /// 命名空间内的键
    pub key: String,
    /// 存储的值
    pub value: Value,
    /// 可选元数据（时间戳、标签、分类等）
    pub metadata: Option<Value>,
    /// 可选相关性分数（0.0~1.0），由宿主的检索引擎填充
    pub score: Option<f64>,
}

/// 记忆检索查询参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryQuery {
    /// 要搜索的命名空间
    pub namespace: String,
    /// 精确键查找（Some = 精确匹配，None = 搜索模式）
    pub key: Option<String>,
    /// 语义/模糊搜索的查询文本（宿主实现具体搜索逻辑）
    pub query_text: Option<String>,
    /// 可选元数据过滤条件
    pub filter: Option<Value>,
    /// 最大返回条目数
    pub top_k: Option<usize>,
}

/// 记忆存储参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStoreParams {
    pub namespace: String,
    pub key: String,
    pub value: Value,
    pub metadata: Option<Value>,
}

/// 核心 MemoryProvider trait。宿主应用实现此 trait 以桥接存储后端。
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// 存储一条记忆。若 namespace+key 已存在则覆盖。
    async fn store(&self, params: MemoryStoreParams) -> Result<(), MemoryError>;

    /// 检索匹配查询条件的记忆条目。
    async fn recall(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// 删除指定命名空间和键的记忆条目。
    async fn delete(&self, namespace: &str, key: &str) -> Result<(), MemoryError>;

    /// 清空整个命名空间。
    async fn clear_namespace(&self, namespace: &str) -> Result<(), MemoryError>;
}
```

### 2.2 设计决策

| 决策 | 理由 |
|------|------|
| `query_text: Option<String>` 而非内置向量搜索 | xworkflow 不应内置嵌入模型；语义搜索由宿主的 MemoryProvider 实现 |
| `score: Option<f64>` 在 `MemoryEntry` 上 | 宿主的检索引擎可附加相关性分数，LLM 节点可据此排序/截断 |
| `MemoryError` 四种变体 | 覆盖最常见场景：未找到、权限拒绝、提供者内部错误、配额超限 |
| `delete` + `clear_namespace` 分离 | 单条删除和批量清空是不同的操作语义，不应混用 |

---

## 3. 记忆作用域模型

### 3.1 三层作用域

| 作用域 | 命名空间格式 | 生命周期 | 依赖的系统变量 | 用途 |
|--------|-------------|---------|---------------|------|
| `Conversation` | `conv:{conversation_id}` | 跨 workflow run，同一对话内 | `sys.conversation_id` | 对话历史、上下文偏好 |
| `User` | `user:{user_id}` | 跨对话，同一用户 | `sys.user_id` | 用户档案、长期偏好 |
| `Global` | `global:{app_id}` | 应用全局 | `sys.app_id`（可选，默认 `"default"`） | 共享知识库 |

### 3.2 类型定义

**文件**: `src/memory/types.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Conversation,
    User,
    Global,
}
```

### 3.3 命名空间解析

**文件**: `src/memory/resolver.rs`

```rust
pub fn resolve_namespace(
    scope: &MemoryScope,
    variable_pool: &VariablePool,
    context: &RuntimeContext,
    node_id: &str,
) -> Result<String, MemoryError> {
    match scope {
        MemoryScope::Conversation => {
            let conv_id = variable_pool
                .get(&Selector::new("sys", "conversation_id"))
                .as_string()
                .ok_or_else(|| MemoryError::ProviderError(
                    "sys.conversation_id not set; required for conversation scope".into()
                ))?;
            Ok(format!("conv:{}", conv_id))
        }
        MemoryScope::User => {
            let user_id = variable_pool
                .get(&Selector::new("sys", "user_id"))
                .as_string()
                .ok_or_else(|| MemoryError::ProviderError(
                    "sys.user_id not set; required for user scope".into()
                ))?;
            Ok(format!("user:{}", user_id))
        }
        MemoryScope::Global => {
            let app_id = variable_pool
                .get(&Selector::new("sys", "app_id"))
                .as_string()
                .unwrap_or_else(|| "default".to_string());
            Ok(format!("global:{}", app_id))
        }
    }
}
```

命名空间约定使宿主可以在存储后端中按前缀分区（如 Redis key prefix、数据库分表等）。

---

## 4. 与 LLM/Agent 节点的集成

### 4.1 两种使用模式

#### 模式 A：显式节点（精细控制）

通过 `memory-recall` 和 `memory-store` 节点显式操作记忆，LLM 节点通过模板变量引用结果。

```
start → memory-recall → llm → memory-store → end
```

LLM 节点本身不知道 Memory 的存在，只是读取 VariablePool 中的变量：

```yaml
- id: recall
  data:
    type: memory-recall
    scope: conversation
    query_selector: ["start", "query"]
    top_k: 5

- id: llm1
  data:
    type: llm
    prompt_template:
      - role: system
        text: "Context: {{#recall.results#}}"
      - role: user
        text: "{{#start.query#}}"
```

#### 模式 B：增强 MemoryConfig（自动注入）

LLM/Agent 节点内置 `enhanced_memory` 配置，节点执行时自动从 MemoryProvider 检索并注入 prompt。

```yaml
- id: llm1
  data:
    type: llm
    prompt_template:
      - role: system
        text: "You are helpful."
      - role: user
        text: "{{#start.query#}}"
    enhanced_memory:
      enabled: true
      scope: conversation
      query_selector: ["start", "query"]
      top_k: 5
      injection_position: after_system
```

两种模式可同时使用，互不干扰。

### 4.2 增强 MemoryConfig 类型定义

**文件**: `src/memory/types.rs`

```rust
/// LLM/Agent 节点的增强记忆配置。
/// 使用 MemoryProvider 自动检索并注入上下文到 prompt。
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EnhancedMemoryConfig {
    pub enabled: bool,
    /// 检索的记忆作用域
    #[serde(default = "default_memory_scope")]
    pub scope: MemoryScope,
    /// 用于检索的查询文本来源（变量选择器）
    #[serde(default)]
    pub query_selector: Option<VariableSelector>,
    /// 最大返回条目数
    #[serde(default = "default_top_k")]
    pub top_k: Option<usize>,
    /// 元数据过滤条件
    #[serde(default)]
    pub filter: Option<Value>,
    /// 注入位置
    #[serde(default)]
    pub injection_position: MemoryInjectionPosition,
    /// 条目格式化模板（Jinja2），可选
    #[serde(default)]
    pub format_template: Option<String>,
}

fn default_memory_scope() -> MemoryScope { MemoryScope::Conversation }
fn default_top_k() -> Option<usize> { Some(10) }

/// 记忆上下文在 prompt messages 中的注入位置
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryInjectionPosition {
    /// 在 system 消息之前
    BeforeSystem,
    /// 在 system 消息之后（默认）
    #[default]
    AfterSystem,
    /// 在 user 消息之前
    BeforeUser,
}
```

### 4.3 LLM Executor 集成

**文件**: `src/llm/executor.rs`

在 `execute()` 方法中，现有 `append_memory_messages()` 之后，新增 feature-gated 处理：

```rust
// 现有逻辑（不变）
if let Some(ref mem) = data.memory {
    append_memory_messages(&mut messages, mem, variable_pool).await?;
}

// 新增：enhanced memory 注入
#[cfg(feature = "memory")]
if let Some(ref enhanced_mem) = data.enhanced_memory {
    if enhanced_mem.enabled {
        if let Some(provider) = context.memory_provider() {
            let namespace = resolve_namespace(
                &enhanced_mem.scope, variable_pool, context, node_id
            ).map_err(|e| NodeError::ExecutionError(e.to_string()))?;

            let query_text = enhanced_mem.query_selector.as_ref()
                .and_then(|sel| variable_pool.get(sel).as_string());

            let query = MemoryQuery {
                namespace,
                key: None,
                query_text,
                filter: enhanced_mem.filter.clone(),
                top_k: enhanced_mem.top_k,
            };

            let entries = provider.recall(query).await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

            if !entries.is_empty() {
                let memory_text = format_memory_entries(
                    &entries, enhanced_mem.format_template.as_deref(),
                );
                inject_memory_context(
                    &mut messages, &memory_text, &enhanced_mem.injection_position,
                );
            }
        }
    }
}
```

注入后的 messages 结构示例：

```
[0] system: "You are helpful."
[1] system: "[Memory Context]\n- key1: value1\n- key2: value2"  ← 自动注入
[2] user: (对话历史 - 来自窗口 memory)
[3] assistant: (对话历史)
...
[N] user: "用户当前的问题"
```

### 4.4 多 LLM/Agent 节点场景

#### 场景 1：多节点共享同一份检索结果

```yaml
start → recall → llm1 → llm2 → store → end
```

`recall.results` 是 VariablePool 中的普通变量，`llm1` 和 `llm2` 都可以通过 `{{#recall.results#}}` 引用。

#### 场景 2：不同节点检索不同作用域

```yaml
start → recall_prefs(scope:user) → recall_history(scope:conversation) → llm → end
```

```yaml
- id: llm1
  data:
    type: llm
    prompt_template:
      - role: system
        text: |
          User preferences: {{#recall_prefs.results#}}
          History: {{#recall_history.results#}}
```

每个 recall 节点独立配置 scope，下游 LLM 节点按需引用。

#### 场景 3：中间存储，后续节点立即可见

```yaml
start → llm1 → store1 → llm2(enhanced_memory) → store2 → end
```

`store1` 调用 `MemoryProvider.store()` 即时持久化到外部存储。`llm2` 的 `enhanced_memory` 调用 `MemoryProvider.recall()` 时能看到 `store1` 刚写入的数据，因为 MemoryProvider 是共享的外部状态，不受 VariablePool 的 copy-on-write 隔离影响。

#### 场景 4：Agent 节点的双层记忆

```yaml
- id: agent1
  data:
    type: agent
    memory:           # 现有：Agent 内部 tool-use 循环的对话窗口
      enabled: true
      window_size: 20
      variable_selector: ["conversation", "history"]
    enhanced_memory:  # 新增：启动时注入外部长期记忆
      enabled: true
      scope: user
      query_selector: ["start", "query"]
      top_k: 3
```

- `memory`（现有 `MemoryConfig`）：Agent 内部多轮 tool-use 循环的对话窗口，从 VariablePool 读
- `enhanced_memory`（新增）：启动时从 MemoryProvider 检索长期上下文，注入初始 prompt

### 4.5 与现有 MemoryConfig 的关系

| | 现有 `MemoryConfig` | 新增 `EnhancedMemoryConfig` |
|---|---|---|
| 数据源 | VariablePool（conversation_variables） | MemoryProvider（外部存储） |
| 检索方式 | 滑动窗口（window_size） | 语义/精确查询（query_text/key） |
| 生命周期 | 单次 workflow run 内 | 跨 run 持久化 |
| 格式 | PromptMessage 数组（role+text） | 自由格式 Value，可自定义模板 |
| 向后兼容 | 完全不变 | 新增字段，feature-gated |

---

## 5. Memory 节点执行器

### 5.1 memory-recall 节点

**文件**: `src/nodes/memory.rs`

DSL Schema：

```rust
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MemoryRecallNodeData {
    /// 检索的记忆作用域
    pub scope: MemoryScope,
    /// 精确键查找（None = 使用 query_text 搜索）
    #[serde(default)]
    pub key: Option<String>,
    /// 搜索查询文本来源
    #[serde(default)]
    pub query_selector: Option<VariableSelector>,
    /// 最大返回数
    #[serde(default = "default_recall_top_k")]
    pub top_k: usize,
    /// 过滤条件
    #[serde(default)]
    pub filter: Option<Value>,
}

fn default_recall_top_k() -> usize { 5 }
```

执行流程：

```
1. 解析 config → MemoryRecallNodeData
2. 从 context.memory_provider() 获取 provider（无则报错）
3. resolve_namespace(scope, pool, context, node_id) → 命名空间字符串
4. 从 variable_pool 读取 query_selector 指定的值作为 query_text
5. 构建 MemoryQuery，调用 provider.recall(query)
6. 审计日志（security feature）
7. 输出：
   - results: Segment::from_value(entries)  // JSON Array
   - count: Segment::Integer(entries.len())
```

DSL 示例：

```yaml
- id: recall_context
  data:
    type: memory-recall
    title: Recall Context
    scope: conversation
    query_selector: ["start", "query"]
    top_k: 5
    filter: { "category": "important" }
```

### 5.2 memory-store 节点

DSL Schema：

```rust
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MemoryStoreNodeData {
    /// 存储的目标作用域
    pub scope: MemoryScope,
    /// 存储键（支持模板变量）
    pub key: String,
    /// 要存储的值来源
    pub value_selector: VariableSelector,
    /// 静态元数据
    #[serde(default)]
    pub metadata: Option<HashMap<String, Value>>,
    /// 从变量池读取的动态元数据
    #[serde(default)]
    pub metadata_selectors: Option<HashMap<String, VariableSelector>>,
}
```

执行流程：

```
1. 解析 config → MemoryStoreNodeData
2. 从 context.memory_provider() 获取 provider
3. resolve_namespace(scope, pool, context, node_id) → 命名空间
4. 渲染 key 中的模板变量
5. 从 variable_pool 读取 value_selector 指定的值
6. 合并静态 metadata 和动态 metadata_selectors
7. 安全检查：值大小限制（security feature）
8. 调用 provider.store(params)
9. 审计日志
10. 输出：
    - stored: Segment::Boolean(true)
    - key: Segment::String(rendered_key)
```

DSL 示例：

```yaml
- id: store_exchange
  data:
    type: memory-store
    title: Store Exchange
    scope: conversation
    key: "exchange_{{#sys.execution_id#}}"
    value_selector: ["llm1", "text"]
    metadata:
      type: "qa_exchange"
    metadata_selectors:
      timestamp: ["sys", "timestamp"]
```

### 5.3 注册

**文件**: `src/nodes/executor.rs`，在 `with_builtins()` 中：

```rust
#[cfg(feature = "builtin-memory-nodes")]
{
    registry.register("memory-recall", Box::new(super::memory::MemoryRecallExecutor));
    registry.register("memory-store", Box::new(super::memory::MemoryStoreExecutor));
}
```

---

## 6. 内置 InMemoryProvider

**文件**: `src/memory/in_memory.rs`

用于测试和简单单进程场景。**不支持语义搜索**（无 query_text 时返回全部条目）。

```rust
pub struct InMemoryProvider {
    // namespace → (key → MemoryEntry)
    store: parking_lot::RwLock<HashMap<String, HashMap<String, MemoryEntry>>>,
}

impl InMemoryProvider {
    pub fn new() -> Self { ... }
}

#[async_trait]
impl MemoryProvider for InMemoryProvider {
    async fn store(&self, params: MemoryStoreParams) -> Result<(), MemoryError> {
        // 写入 store[namespace][key]
    }

    async fn recall(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError> {
        // key 精确查找 或 返回全部（截断到 top_k）
    }

    async fn delete(&self, namespace: &str, key: &str) -> Result<(), MemoryError> { ... }
    async fn clear_namespace(&self, namespace: &str) -> Result<(), MemoryError> { ... }
}
```

---

## 7. 基础设施集成

### 7.1 RuntimeGroup

**文件**: `src/core/runtime_group.rs`

新增字段，遵循 `credential_provider` 的 cfg-gate 模式：

```rust
pub struct RuntimeGroup {
    // ... 现有字段 ...
    #[cfg(feature = "memory")]
    pub memory_provider: Option<Arc<dyn crate::memory::MemoryProvider>>,
}
```

`RuntimeGroupBuilder` 新增：

```rust
#[cfg(feature = "memory")]
pub fn memory_provider(mut self, provider: Arc<dyn crate::memory::MemoryProvider>) -> Self {
    self.group.memory_provider = Some(provider);
    self
}
```

### 7.2 WorkflowContext

**文件**: `src/core/workflow_context.rs`

新增访问器和 setter：

```rust
#[cfg(feature = "memory")]
pub fn memory_provider(&self) -> Option<&Arc<dyn crate::memory::MemoryProvider>> {
    self.runtime_group.memory_provider.as_ref()
}

#[cfg(feature = "memory")]
pub fn set_memory_provider(&mut self, provider: Arc<dyn crate::memory::MemoryProvider>) {
    self.update_runtime_group(|group| {
        group.memory_provider = Some(provider);
    });
}
```

### 7.3 WorkflowRunnerBuilder

**文件**: `src/scheduler.rs`

新增 builder 方法：

```rust
#[cfg(feature = "memory")]
pub fn memory_provider(mut self, provider: Arc<dyn crate::memory::MemoryProvider>) -> Self {
    self.context.set_memory_provider(provider);
    self
}
```

### 7.4 DSL Schema 扩展

**文件**: `src/dsl/schema.rs`

`LlmNodeData` 新增字段：

```rust
pub struct LlmNodeData {
    // ... 现有字段（包括 memory: Option<MemoryConfig>，完全不变）...
    #[cfg(feature = "memory")]
    #[serde(default)]
    pub enhanced_memory: Option<EnhancedMemoryConfig>,
}
```

`NodeType` 枚举新增：

```rust
#[cfg(feature = "memory")]
MemoryRecall,
#[cfg(feature = "memory")]
MemoryStore,
```

---

## 8. 插件系统集成

### 8.1 PluginContext

**文件**: `src/plugin_system/context.rs`

允许插件注册 MemoryProvider：

```rust
#[cfg(feature = "memory")]
pub fn register_memory_provider(
    &mut self,
    provider: Arc<dyn crate::memory::MemoryProvider>,
) -> Result<(), PluginError> {
    self.ensure_phase(PluginPhase::Normal)?;
    self.registry_inner.memory_provider = Some(provider);
    Ok(())
}
```

### 8.2 PluginRegistryInner

**文件**: `src/plugin_system/registry.rs`

```rust
pub(crate) struct PluginRegistryInner {
    // ... 现有字段 ...
    #[cfg(feature = "memory")]
    pub(crate) memory_provider: Option<Arc<dyn crate::memory::MemoryProvider>>,
}
```

---

## 9. 安全集成

### 9.1 审计日志

**文件**: `src/security/audit.rs`

新增审计事件类型（双 feature gate：`security` + `memory`）：

```rust
#[cfg(feature = "memory")]
MemoryAccess {
    namespace: String,
    operation: String,  // "recall", "store", "delete", "clear"
    node_id: String,
    success: bool,
},
```

每次 MemoryProvider 操作前后均记录审计事件。

### 9.2 记忆安全策略

**文件**: `src/security/policy.rs`

```rust
#[cfg(feature = "memory")]
pub memory: Option<MemorySecurityPolicy>,
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySecurityPolicy {
    /// 允许的命名空间前缀
    pub allowed_namespace_prefixes: Vec<String>,
    /// 每个命名空间最大条目数
    pub max_entries_per_namespace: Option<usize>,
    /// 单条值最大字节数
    pub max_value_bytes: Option<usize>,
}
```

`resolve_namespace()` 在返回前检查此策略。不在允许前缀列表中的命名空间返回 `MemoryError::AccessDenied`。

### 9.3 值大小检查

`memory-store` 节点在调用 `provider.store()` 之前，检查值的序列化大小是否超过 `max_value_bytes`：

```rust
#[cfg(feature = "security")]
fn check_value_safety(value: &Value, policy: &MemorySecurityPolicy) -> Result<(), MemoryError> {
    if let Some(max_bytes) = policy.max_value_bytes {
        let size = serde_json::to_string(value)
            .map_err(|e| MemoryError::ProviderError(e.to_string()))?
            .len();
        if size > max_bytes {
            return Err(MemoryError::QuotaExceeded(
                format!("Value size {} exceeds max {}", size, max_bytes)
            ));
        }
    }
    Ok(())
}
```

---

## 10. Feature Gating

### 10.1 Feature Flag

**文件**: `Cargo.toml`

```toml
[features]
memory = []
builtin-memory-nodes = ["memory"]

default = [
    # ... 现有 ...
    "memory",
    "builtin-memory-nodes",
]
```

`memory` 启用核心 trait 和基础设施集成。`builtin-memory-nodes` 启用 `memory-recall`/`memory-store` 节点执行器注册。

### 10.2 Feature Gate 范围

| 文件 | Gate 条件 | 影响 |
|------|----------|------|
| `src/memory/` 整个模块 | `#[cfg(feature = "memory")]` | 零编译 |
| `RuntimeGroup.memory_provider` | `#[cfg(feature = "memory")]` | 无字段 |
| `WorkflowContext.memory_provider()` | `#[cfg(feature = "memory")]` | 无方法 |
| `LlmNodeData.enhanced_memory` | `#[cfg(feature = "memory")]` | 无字段 |
| `NodeType::MemoryRecall/MemoryStore` | `#[cfg(feature = "memory")]` | 无变体 |
| 节点注册 | `#[cfg(feature = "builtin-memory-nodes")]` | 不注册 |
| 安全策略 | `#[cfg(all(feature = "security", feature = "memory"))]` | 无策略 |

未启用时零运行时开销、零编译产物。

---

## 11. 模块结构

```
src/
  memory/
    mod.rs           # 模块声明、统一 re-export
    provider.rs      # MemoryProvider trait、MemoryError、MemoryEntry、MemoryQuery、MemoryStoreParams
    types.rs         # MemoryScope、EnhancedMemoryConfig、MemoryInjectionPosition、节点 DSL 类型
    resolver.rs      # resolve_namespace() 辅助函数
    in_memory.rs     # InMemoryProvider 内置实现
  nodes/
    memory.rs        # MemoryRecallExecutor、MemoryStoreExecutor（新建）
```

变更的现有文件：

| 文件 | 变更内容 |
|------|---------|
| `Cargo.toml` | 新增 `memory`、`builtin-memory-nodes` features |
| `src/lib.rs` | 新增 `#[cfg(feature = "memory")] pub mod memory;` |
| `src/core/runtime_group.rs` | 新增 `memory_provider` 字段 + builder 方法 |
| `src/core/workflow_context.rs` | 新增 accessor + setter |
| `src/scheduler.rs` | 新增 `.memory_provider()` builder 方法 |
| `src/dsl/schema.rs` | 新增 `enhanced_memory` 字段、`NodeType` 变体 |
| `src/dsl/validation/known_types.rs` | 注册 `memory-recall`、`memory-store` |
| `src/nodes/mod.rs` | 新增 `mod memory` |
| `src/nodes/executor.rs` | 注册 memory 节点 |
| `src/llm/executor.rs` | enhanced memory 注入逻辑 |
| `src/plugin_system/context.rs` | `register_memory_provider()` |
| `src/plugin_system/registry.rs` | `memory_provider` 字段 |
| `src/security/audit.rs` | `MemoryAccess` 事件类型 |
| `src/security/policy.rs` | `MemorySecurityPolicy` |

---

## 12. 完整 DSL 示例

```yaml
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

  - id: recall_context
    data:
      type: memory-recall
      title: Recall Relevant Context
      scope: conversation
      query_selector: ["start", "query"]
      top_k: 5

  - id: llm1
    data:
      type: llm
      title: Generate Response
      model:
        provider: openai
        name: gpt-4
      prompt_template:
        - role: system
          text: "You are a helpful assistant. Context:\n{{#recall_context.results#}}"
        - role: user
          text: "{{#start.query#}}"
      memory:
        enabled: true
        variable_selector: ["conversation", "history"]
        window_size: 10

  - id: store_exchange
    data:
      type: memory-store
      title: Store Exchange
      scope: conversation
      key: "exchange_{{#sys.execution_id#}}"
      value_selector: ["llm1", "text"]
      metadata:
        type: "qa_exchange"

  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: answer
          value_selector: ["llm1", "text"]

edges:
  - { id: e1, source: start, target: recall_context }
  - { id: e2, source: recall_context, target: llm1 }
  - { id: e3, source: llm1, target: store_exchange }
  - { id: e4, source: store_exchange, target: end }
```

## 13. Rust API 示例

```rust
use std::sync::Arc;
use xworkflow::memory::InMemoryProvider;
use xworkflow::scheduler::WorkflowRunner;

// 创建共享的 MemoryProvider（跨多次 workflow 运行）
let memory = Arc::new(InMemoryProvider::new());

// 第一次运行 — 存储对话记忆
let handle = WorkflowRunner::builder(schema.clone())
    .user_inputs(inputs1)
    .system_vars(hashmap!{
        "conversation_id" => "conv-123",
        "user_id" => "user-456",
    })
    .memory_provider(memory.clone())
    .run()
    .await?;
let result = handle.wait().await;

// 第二次运行 — recall 节点能检索到第一次存储的记忆
let handle = WorkflowRunner::builder(schema)
    .user_inputs(inputs2)
    .system_vars(hashmap!{
        "conversation_id" => "conv-123",
        "user_id" => "user-456",
    })
    .memory_provider(memory.clone())
    .run()
    .await?;
```

---

## 14. 测试策略

### 14.1 单元测试

| 模块 | 测试内容 |
|------|---------|
| `provider.rs` | `MemoryError` 变体、`MemoryEntry` 序列化 roundtrip、`MemoryQuery` 构造 |
| `resolver.rs` | 各 scope 命名空间解析、缺少系统变量报错、Global scope 默认 app_id |
| `in_memory.rs` | CRUD 全流程、命名空间隔离、top_k 截断、clear_namespace |
| `types.rs` | `MemoryScope` serde roundtrip、`MemoryInjectionPosition` 默认值 |
| `nodes/memory.rs` | Mock MemoryProvider + 节点输出格式、无 provider 报错、key 精确查找 |

### 14.2 集成测试

| 测试 | 内容 |
|------|------|
| 完整流程 | `start → memory-recall → llm → memory-store → end` 端到端 |
| 跨 run 持久化 | 两次 `WorkflowRunner::builder().run()` 共享 `InMemoryProvider` |
| 插件注册 | 通过 Plugin trait 注册 MemoryProvider |
| 多节点共享 | 多个 LLM 节点引用同一 recall 结果 |

已落地用例（`tests/memory_feature_integration.rs`）：

- `test_memory_store_and_recall_e2e`
- `test_memory_full_flow_recall_llm_store_end`
- `test_memory_provider_registered_by_plugin`
- `test_multiple_llm_nodes_share_same_recall_results`

### 14.3 Feature Gate 测试

```bash
cargo build --no-default-features           # 零 memory 代码
cargo build --features memory               # 仅核心 trait
cargo build --features builtin-memory-nodes # trait + 节点
cargo build --all-features                  # 全量
```

---

## 15. 实施顺序

| 阶段 | 内容 | 文件数 |
|------|------|--------|
| Phase 1 | 核心 Trait 和类型 | 4 新建 |
| Phase 2 | 内置 InMemoryProvider | 1 新建 |
| Phase 3 | 基础设施集成（RuntimeGroup、WorkflowContext、Scheduler） | 5 修改 |
| Phase 4 | DSL Schema 扩展 | 2 修改 |
| Phase 5 | 节点执行器 | 1 新建 + 2 修改 |
| Phase 6 | LLM 集成 | 1 修改 |
| Phase 7 | 插件集成 | 2 修改 |
| Phase 8 | 安全集成 | 2 修改 |
| Phase 9 | 测试 | 新建测试文件 |

---

## 16. 验证命令

```bash
# 编译验证
cargo build --all-features
cargo build --no-default-features
cargo build --features memory

# 严格零 warning 验证
RUSTFLAGS='-D warnings' cargo check --all-features --workspace --all-targets
RUSTFLAGS='-D warnings' cargo check --no-default-features --workspace --all-targets
RUSTFLAGS='-D warnings' cargo check --features memory --workspace --all-targets

# 测试
cargo test --all-features --workspace --lib memory
cargo test --all-features --workspace --lib memory_recall
cargo test --all-features --workspace --lib memory_store
cargo test --all-features --workspace --test memory_feature_integration
cargo test --all-features --workspace
cargo clippy --all-features --workspace
```
