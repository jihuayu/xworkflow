# Question Classifier 节点设计文档

## 1. 概述

`question-classifier` 是一个**分支节点**，利用 LLM 语义理解将输入文本分类到预定义类别中，然后通过 `EdgeHandle::Branch(category_id)` 路由到不同的下游分支。

当前状态：`StubExecutor`（`src/nodes/executor.rs:107`），已在 `BRANCH_NODE_TYPES` 中注册（`src/dsl/validation/known_types.rs:7`）。

参考 Dify 的问题分类器设计，遵循本项目三大原则：**Security > Performance > Obviousness**。

---

## 2. DSL Schema 类型

**文件**: `src/dsl/schema.rs`

在 `LlmNodeData` 之后添加：

```rust
/// 分类类别定义
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ClassifierCategory {
    pub category_id: String,
    pub category_name: String,
}

/// Question Classifier 节点配置
///
/// 使用 LLM 将输入文本分类到恰好一个类别中，
/// 然后通过 EdgeHandle::Branch(category_id) 路由到对应的下游分支。
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct QuestionClassifierNodeData {
    /// LLM 模型配置（复用已有 ModelConfig）
    pub model: ModelConfig,
    /// 待分类的输入文本变量选择器（如 ["sys", "query"]）
    pub query_variable_selector: VariableSelector,
    /// 预定义的分类类别列表
    pub categories: Vec<ClassifierCategory>,
    /// 可选：补充分类指令（支持 {{#node.var#}} 变量引用）
    #[serde(default)]
    pub instruction: Option<String>,
    /// 可选：对话记忆配置（复用已有 MemoryConfig）
    #[serde(default)]
    pub memory: Option<MemoryConfig>,
}
```

### 设计决策

| 决策 | 理由 |
|------|------|
| 复用 `ModelConfig`、`MemoryConfig` | 与 LLM 节点保持一致，零冗余类型 |
| 不设 `stream` 字段 | 分类必须等完整 JSON 响应才能解析路由，流式无意义 |
| `query_variable_selector` 而非硬编码 | 输入可以来自任意上游节点，不限于 `sys.query` |
| `instruction` 支持模板渲染 | 允许动态注入上下文信息到分类指令中 |

### DSL 示例

```toml
[[nodes]]
id = "classifier1"

[nodes.data]
type = "question-classifier"
title = "Intent Classifier"
query_variable_selector = ["sys", "query"]
instruction = "If the user mentions pricing or discounts, classify as Sales."

[nodes.data.model]
provider = "openai"
name = "gpt-4o-mini"

[[nodes.data.categories]]
category_id = "cat_support"
category_name = "Customer Support"

[[nodes.data.categories]]
category_id = "cat_sales"
category_name = "Sales Inquiry"

[[nodes.data.categories]]
category_id = "cat_tech"
category_name = "Technical Question"

# 分支边
[[edges]]
source = "classifier1"
target = "handle_support"
sourceHandle = "cat_support"

[[edges]]
source = "classifier1"
target = "handle_sales"
sourceHandle = "cat_sales"

[[edges]]
source = "classifier1"
target = "handle_tech"
sourceHandle = "cat_tech"
```

---

## 3. Executor 实现

### 3.1 文件位置

**新建文件**: `src/llm/question_classifier.rs`

放在 `llm/` 模块中（非 `nodes/`），因为：
- 依赖 `Arc<LlmProviderRegistry>`，与 `LlmNodeExecutor` 结构一致
- 复用 `llm/types.rs` 中的 `ChatCompletionRequest`、`ChatMessage` 等类型
- 依赖关系清晰可见

### 3.2 结构体

```rust
pub struct QuestionClassifierExecutor {
    registry: Arc<LlmProviderRegistry>,
}
```

### 3.3 执行流程

```
1. parse config → QuestionClassifierNodeData
2. validate: categories 非空
3. resolve input text: variable_pool.get_resolved(&data.query_variable_selector)
4. build messages:
   a. [可选] memory messages（复用 append_memory_messages）
   b. system prompt（列出所有 categories + 可选 instruction）
   c. user message（input text）
5. call provider.chat_completion(request)    ← 非流式，强制 stream=false
6. parse JSON response → extract category_id
7. validate: category_id ∈ configured categories
8. return NodeRunResult {
     edge_source_handle: Branch(category_id),
     outputs: { class_name, category_id },
     llm_usage: Some(response.usage),
   }
```

### 3.4 关键参数默认值

| 参数 | 默认值 | 理由 |
|------|--------|------|
| `temperature` | `0.0`（未显式设置时） | 分类需要确定性，降低随机性 |
| `max_tokens` | `256`（未显式设置时） | 分类 JSON 响应极短，节省 token |
| `stream` | `false`（强制） | 必须等完整响应才能解析 |

---

## 4. Prompt 构建策略

程序化构建，**不使用 Jinja2 模板**（安全 + 可预测）。

### 系统 Prompt 模板

```
You are a text classification engine. Classify the input text into exactly one category.

### Categories
- category_id: "cat_support", category_name: "Customer Support"
- category_id: "cat_sales", category_name: "Sales Inquiry"
- category_id: "cat_tech", category_name: "Technical Question"

### Instructions                          ← 仅当 instruction 有值时出现
If the user mentions pricing, classify as Sales.

### Output format
Respond ONLY with a JSON object: {"category_id": "<id>", "category_name": "<name>"}
Do not include any other text or markdown formatting.
```

### Messages 数组

```
[memory messages]?                        ← 可选，来自 MemoryConfig
+ system: <上述系统 prompt>
+ user: <input text>
```

### 安全考量

- `instruction` 通过 `render_template_async_with_config` 渲染，支持变量引用但在 system prompt 结构内
- Prompt 结构本身不可被用户输入篡改（instruction 是节点配置，非运行时输入）
- 用户输入（input text）仅出现在 user message 中

---

## 5. JSON 响应解析

```rust
fn parse_classifier_response(
    response: &str,
    valid_ids: &[&str],
) -> Result<(String, String), NodeError>
```

### 三层解析尝试

1. **直接 JSON 解析** → 提取 `category_id` 字段
2. **Strip markdown code block** （` ```json ... ``` `）后再次 JSON 解析
3. **裸字符串匹配** → 检查原文是否直接等于某个 `category_id`

### 验证

解析得到的 `category_id` 必须 ∈ 配置中的 `valid_ids`。不在列表中则返回 `NodeError::ExecutionError`。

### 错误处理策略

**不做静默 fallback**。原因：

- **Security**: 静默 fallback 可能将敏感查询路由到非预期分支
- **Obviousness**: 错误就是错误，不应被隐藏
- 用户可通过节点级 `error_strategy` 配置处理：
  - `FailBranch` → 路由到错误处理子图
  - `DefaultValue` → 使用默认输出值
  - `retry_config` → 自动重试

---

## 6. 输出定义

### NodeRunResult

```rust
NodeRunResult {
    status: Succeeded,
    outputs: Sync({
        "class_name": "Customer Support",   // 分类名称
        "category_id": "cat_support",       // 分类 ID
    }),
    metadata: {
        "model": "gpt-4o-mini",
        "provider": "openai",
    },
    llm_usage: Some(LlmUsage { ... }),
    edge_source_handle: Branch("cat_support"),   // 路由决策
}
```

### 下游变量访问

下游节点可通过 `["classifier1", "class_name"]` 和 `["classifier1", "category_id"]` 访问分类结果。

---

## 7. 共享辅助函数提取

**文件**: `src/llm/executor.rs`

将以下函数可见性从 `fn` 改为 `pub(crate) fn`：

| 函数 | 行号 | 用途 |
|------|------|------|
| `append_memory_messages` | 377 | 构建对话记忆 messages |
| `map_role` | 331 | 角色字符串 → ChatRole |
| `audit_security_event` | 305 | 安全审计日志（cfg-gated） |

这样 `question_classifier.rs` 可直接调用，零代码重复。

---

## 8. 注册与 Feature Gating

### 8.1 Feature Flag

使用已有的 `builtin-llm-node`，不新建 feature flag。分类器本质上是 LLM 功能。

### 8.2 Executor 注册

**文件**: `src/nodes/executor.rs`

在 `set_llm_provider_registry`（line 147）中同时注册：

```rust
#[cfg(feature = "builtin-llm-node")]
pub fn set_llm_provider_registry(&mut self, registry: Arc<LlmProviderRegistry>) {
    self.register("llm", Box::new(LlmNodeExecutor::new(registry.clone())));
    self.register("question-classifier", Box::new(QuestionClassifierExecutor::new(registry)));
}
```

`with_builtins()` 中的 `StubExecutor("question-classifier")` 保留不变 —— `register()` 会覆盖它。当 `builtin-llm-node` 未启用时，stub 仍然存在作为占位。

### 8.3 从 STUB_NODE_TYPES 移除

**文件**: `src/dsl/validation/known_types.rs`

- 从 `STUB_NODE_TYPES` 数组移除 `"question-classifier"`
- 更新 `test_stub_node_types` 测试
- 保留在 `BRANCH_NODE_TYPES` 中（已在）

---

## 9. Security 相关

### 9.1 Credential 注入

与 `LlmNodeExecutor` 完全一致的 `#[cfg(feature = "security")]` 模式：

```rust
#[cfg(feature = "security")]
if let (Some(provider), Some(group)) = (context.credential_provider(), context.resource_group()) {
    match provider.get_credentials(&group.group_id, &data.model.provider).await {
        Ok(creds) => { /* audit success + merge */ }
        Err(err) => { /* audit failure + return error */ }
    }
}
```

### 9.2 审计日志

分类执行记录 `SecurityEventType::CredentialAccess`。

### 9.3 输入验证

- `categories` 非空检查
- `model.provider` 必须在 registry 中存在
- 解析后的 `category_id` 必须匹配配置列表

---

## 10. 测试策略

### 10.1 单元测试

**文件**: `src/llm/question_classifier.rs` 底部

| 测试 | 验证内容 |
|------|---------|
| `test_parse_valid_json` | `{"category_id":"a","category_name":"A"}` → Ok("a", "A") |
| `test_parse_markdown_block` | ` ```json{"category_id":"a"...}``` ` → Ok |
| `test_parse_bare_id` | `"a"` → Ok("a", "") |
| `test_parse_invalid_category` | JSON 有效但 id 不在列表 → Err |
| `test_parse_malformed` | 乱文本 → Err |
| `test_build_prompt` | 验证 prompt 包含所有 categories 和 instruction |
| `test_executor_basic` | MockProvider + 完整执行 → Branch("cat_x") |
| `test_executor_empty_categories` | → ConfigError |
| `test_executor_missing_provider` | → ExecutionError |

### 10.2 集成测试

使用现有 mock server 机制（`state.toml` 中的 `mock_server` + `llm_providers`）。

#### Case 121: `question_classifier_basic`

- mock server: `POST /v1/chat/completions` 返回 `{"category_id":"cat_support","category_name":"Support"}`
- workflow: `start → classifier → end_support` (sourceHandle=cat_support) / `end_sales` (sourceHandle=cat_sales)
- 验证：路由到 cat_support 分支，输出 `class_name = "Support"`

#### Case 122: `question_classifier_invalid_response`

- mock server 返回无法解析的文本 `"I think this is support"`
- workflow 无 error_strategy
- 验证：`status = "failed"`, `error_contains = "Failed to parse"`

#### Case 123: `question_classifier_markdown_response`

- mock server 返回 `` ```json\n{"category_id":"cat_sales","category_name":"Sales"}\n``` ``
- 验证：正确 strip markdown → 路由到 cat_sales

---

## 11. 文件变更清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `src/dsl/schema.rs` | 修改 | 添加 `ClassifierCategory` + `QuestionClassifierNodeData` |
| `src/llm/question_classifier.rs` | **新建** | Executor + prompt 构建 + JSON 解析 + 单元测试 |
| `src/llm/mod.rs` | 修改 | 添加 `pub mod question_classifier` + re-export |
| `src/llm/executor.rs` | 修改 | 3 个函数改为 `pub(crate)` |
| `src/nodes/executor.rs` | 修改 | `set_llm_provider_registry` 注册 question-classifier |
| `src/dsl/validation/known_types.rs` | 修改 | 从 `STUB_NODE_TYPES` 移除 + 更新测试 |
| `tests/integration/cases/121_*/` | **新建** | 基本分类集成测试 |
| `tests/integration/cases/122_*/` | **新建** | 无效响应集成测试 |
| `tests/integration/cases/123_*/` | **新建** | Markdown code block 解析集成测试 |

---

## 12. 实施顺序

1. `src/dsl/schema.rs` — 添加类型定义
2. `src/llm/executor.rs` — 提取共享函数为 `pub(crate)`
3. `src/llm/question_classifier.rs` — 核心实现 + 单元测试
4. `src/llm/mod.rs` — 模块声明
5. `src/nodes/executor.rs` — 注册 executor
6. `src/dsl/validation/known_types.rs` — 移除 stub 标记
7. 集成测试用例

---

## 13. 验证命令

```bash
cargo build --all-features
cargo test --all-features --workspace --lib question_classifier
cargo test --all-features --workspace --test integration_tests -- 121
cargo test --all-features --workspace --test integration_tests -- 122
cargo test --all-features --workspace --test integration_tests -- 123
cargo test --all-features --workspace
cargo clippy --all-features --workspace
```
