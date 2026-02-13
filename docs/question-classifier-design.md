# Question Classifier 节点设计文档

## 1. 概述

`question-classifier` 是一个**分支节点**，利用 LLM 语义理解将输入文本分类到预定义类别中，然后通过 `EdgeHandle::Branch(category_id)` 路由到不同的下游分支。

当模型返回的 `category_id` 不在配置列表中时，节点会显式路由到 `EdgeHandle::Branch("default")`。因此工作流需要提供 `sourceHandle = "default"` 的兜底出边。

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

[[edges]]
source = "classifier1"
target = "handle_default"
sourceHandle = "default"
```

### 分支边约束

- 每个 `categories[].category_id` 都必须有同名 `sourceHandle`
- 必须存在 `sourceHandle = "default"` 兜底边
- 不允许出现未知 `sourceHandle`

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
6. parse response → extract candidate category_id
7. route decision:
   a. candidate_id ∈ configured categories
      -> edge_source_handle = Branch(candidate_id)
      -> outputs.category_id = candidate_id
      -> outputs.class_name = config_map[candidate_id]
   b. candidate_id ∉ configured categories
      -> edge_source_handle = Branch("default")
      -> outputs.category_id = "default"
      -> outputs.class_name = "default"
   c. response parse failed
      -> return NodeError::ExecutionError
8. return NodeRunResult { ... }
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
Respond ONLY with a JSON object: {"category_id": "<id>"}
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
fn parse_classifier_response(response: &str) -> Result<String, NodeError>
```

### 三层解析尝试

1. **直接 JSON 解析** → 提取 `category_id` 字段
2. **Strip markdown code block** （` ```json ... ``` `）后再次 JSON 解析
3. **裸字符串匹配** → 检查原文是否直接等于某个 `category_id`

### 验证

解析得到的 `category_id` 分两种处理：

- 若 ∈ 配置中的 `valid_ids`：正常路由到对应类别分支
- 若 ∉ 配置中的 `valid_ids`：路由到 `default` 分支（`EdgeHandle::Branch("default")`）

### 错误处理策略

**仅对“非法分类 ID”做显式兜底，不对“解析失败”做静默 fallback**。原因：

- **Security**: 解析失败代表模型输出严重偏离结构约束，应中止并暴露错误
- **Obviousness**: “解析失败”与“分类不在白名单”是两类不同问题，不应混淆
- **Robustness**: 对非法分类 ID 兜底到 `default`，减少轻微漂移导致的整体失败
- 解析失败场景仍由节点级 `error_strategy`/`retry_config` 处理

> 备注：`default` 仅在“响应可解析但分类 ID 不合法”时触发。

---

## 6. 输出定义

### NodeRunResult

```rust
NodeRunResult {
    status: Succeeded,
    outputs: Sync({
        "class_name": "Customer Support",   // 分类名称（来自配置映射）
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

### default 兜底分支输出

```rust
NodeRunResult {
    status: Succeeded,
    outputs: Sync({
        "class_name": "default",
        "category_id": "default",
    }),
    edge_source_handle: Branch("default"),
    ...
}
```

### 下游变量访问

下游节点可通过 `["classifier1", "class_name"]` 和 `["classifier1", "category_id"]` 访问分类结果。

---

## 7. 共享辅助函数提取

**文件**: `src/llm/executor.rs`

复用以下已存在的 `pub(crate)` 共享函数（无需重复实现）：

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

### 8.3 STUB_NODE_TYPES 的 feature 条件处理

**文件**: `src/dsl/validation/known_types.rs`

- 保留 `STUB_NODE_TYPES` 常量结构
- 在 `is_stub_node_type()` 中按 feature 条件处理：
  - `builtin-llm-node` 启用时：`question-classifier` 不视为 stub（不报 W202）
  - `builtin-llm-node` 未启用时：`question-classifier` 仍视为 stub（保留 W202）
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

- `categories` 非空 + `category_id` 唯一性检查
- `model.provider` 必须在 registry 中存在
- 解析后的 `category_id` 若不匹配配置列表，路由到 `default`
- DSL 语义校验要求分支边严格匹配：每个 `category_id` 与 `default` 均需对应 `source_handle`

---

## 10. 测试策略

### 10.1 单元测试

**文件**: `src/llm/question_classifier.rs` 底部

| 测试 | 验证内容 |
|------|---------|
| `test_parse_valid_json` | `{"category_id":"a"}` → Ok("a") |
| `test_parse_markdown_block` | ` ```json{"category_id":"a"}``` ` → Ok |
| `test_parse_bare_id` | `"a"` → Ok("a") |
| `test_executor_invalid_category_routes_default` | JSON 有效但 id 不在列表 → `Branch("default")` |
| `test_parse_malformed` | 乱文本 → Err |
| `test_build_prompt` | 验证 prompt 包含所有 categories 和 instruction |
| `test_executor_basic` | MockProvider + 完整执行 → Branch("cat_x") |
| `test_executor_empty_categories` | → ConfigError |
| `test_executor_missing_provider` | → ExecutionError |
| `test_executor_class_name_from_config` | 模型返回 category_name 与配置不一致时，输出仍取配置值 |

### 10.2 集成测试

使用现有 mock server 机制（`state.toml` 中的 `mock_server` + `llm_providers`）。

#### Case 168: `question_classifier_basic`

- mock server: `POST /v1/chat/completions` 返回 `{"category_id":"cat_support","category_name":"Support"}`
- workflow: `start → classifier → end_support` (sourceHandle=cat_support) / `end_sales` (sourceHandle=cat_sales) / `end_default` (sourceHandle=default)
- 验证：路由到 cat_support 分支，且 `class_name` 等于配置中的 `category_name`

#### Case 169: `question_classifier_invalid_id_fallback_default`

- mock server 返回 `{"category_id":"cat_unknown"}`
- workflow 包含 `sourceHandle = "default"` 出边
- 验证：路由到 default 分支，输出 `category_id = "default"`、`class_name = "default"`

#### Case 170: `question_classifier_invalid_response`

- mock server 返回无法解析的文本 `"I think this is support"`
- workflow 无 error_strategy
- 验证：`status = "failed"`, `error_contains = "Failed to parse"`

---

## 11. 文件变更清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `src/dsl/schema.rs` | 修改 | 添加 `ClassifierCategory` + `QuestionClassifierNodeData` |
| `src/compiler/compiled_workflow.rs` | 修改 | 增加 `CompiledNodeConfig::QuestionClassifier` |
| `src/compiler/compiler.rs` | 修改 | 在 `compile_node_config` 中支持 `question-classifier` typed compile |
| `src/llm/question_classifier.rs` | **新建** | Executor + prompt 构建 + JSON 解析 + 单元测试 |
| `src/llm/mod.rs` | 修改 | 添加 `pub mod question_classifier` + re-export |
| `src/llm/executor.rs` | 复用 | 调用已存在 `pub(crate)` 共享函数（memory/role/audit） |
| `src/nodes/executor.rs` | 修改 | `set_llm_provider_registry` 注册 question-classifier |
| `src/dsl/validation/layer3_semantic.rs` | 修改 | 增加 question-classifier 语义校验（严格分支 + default） |
| `src/dsl/validation/known_types.rs` | 修改 | `is_stub_node_type` 按 feature 条件处理 + 更新测试 |
| `tests/integration_tests.rs` | 修改 | 注册新增 case |
| `tests/integration/cases/168_*/` | **新建** | 基本分类集成测试 |
| `tests/integration/cases/169_*/` | **新建** | 非法分类 ID 路由 default 集成测试 |
| `tests/integration/cases/170_*/` | **新建** | 解析失败集成测试 |

---

## 12. 实施顺序

1. `src/dsl/schema.rs` — 添加类型定义
2. `src/compiler/compiled_workflow.rs` + `src/compiler/compiler.rs` — 编译期 typed config 接入
3. `src/llm/question_classifier.rs` — 核心实现 + 单元测试
4. `src/llm/mod.rs` — 模块声明
5. `src/nodes/executor.rs` — 注册 executor
6. `src/dsl/validation/layer3_semantic.rs` — 语义校验补齐（严格分支 + default）
7. `src/dsl/validation/known_types.rs` — stub 判定 feature 条件化
8. `tests/integration_tests.rs` + `tests/integration/cases/168~170_*` — 集成测试

---

## 13. 验证命令

```bash
cargo build --all-features
cargo test --all-features --workspace --lib question_classifier
cargo test --all-features --workspace --test integration_tests -- 168
cargo test --all-features --workspace --test integration_tests -- 169
cargo test --all-features --workspace --test integration_tests -- 170
cargo test --all-features --workspace dsl::validation
cargo test --all-features --workspace
cargo clippy --all-features --workspace
```
