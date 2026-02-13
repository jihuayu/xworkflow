# Question Classifier 节点设计文档

## 1. 文档目标

本文档定义 `question-classifier` 节点在 xworkflow 中的完整落地方案，包括：

- DSL 配置模型
- 语义校验规则
- 编译期 typed config 接入
- 运行时执行器实现
- 分支路由协议
- 安全、性能与测试策略

遵循仓库统一优先级：**Security > Performance > Obviousness**。

---

## 2. 当前状态（基线）

截至当前代码基线：

- `question-classifier` 已被识别为分支节点（`BRANCH_NODE_TYPES`）
- `NodeType::QuestionClassifier` 已在 DSL 层声明
- 执行器注册表中仍为 `StubExecutor("question-classifier")`
- 运行时尚无真实分类执行逻辑

结论：节点类型已“可识别”，但功能仍“未实现”。

---

## 3. 设计范围与非目标

### 3.1 本次范围（P0）

1. 使用 LLM 对输入文本进行**单标签分类**
2. 将分类结果映射为 `EdgeHandle::Branch(category_id)`
3. 当模型返回非法分类 ID 时，路由到 `Branch("default")`
4. 输出标准化字段：`category_id`、`class_name`
5. 提供可测试、可审计、可 feature-gate 的实现

### 3.2 非目标（P0 不做）




- 多标签分类（一次返回多个 category）
- 分类置信度回传与阈值路由
- 自动类别生成/在线学习
- 流式分类输出（分类场景无价值）
- 模型投票/多模型仲裁

---

## 4. 节点行为定义

`question-classifier` 是一个分支节点，执行后必然选择**一个**分支：

1. `category_id` 命中配置列表：走对应 `source_handle = category_id`
2. `category_id` 未命中配置列表：走 `source_handle = "default"`
3. 响应不可解析：节点执行失败（进入现有错误处理链）

这三种结果互斥，且覆盖所有运行结果。

---

## 5. DSL Schema 设计

**目标文件**：`src/dsl/schema.rs`

在现有 LLM 相关结构旁新增：

```rust
/// 单个分类定义
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ClassifierCategory {
    pub category_id: String,
    pub category_name: String,
}

/// Question Classifier 节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct QuestionClassifierNodeData {
    /// 复用现有模型配置
    pub model: ModelConfig,
    /// 待分类文本来源，例如 ["sys", "query"]
    pub query_variable_selector: VariableSelector,
    /// 预定义类别（至少 1 个）
    pub categories: Vec<ClassifierCategory>,
    /// 可选附加指令（支持模板变量）
    #[serde(default)]
    pub instruction: Option<String>,
    /// 可选记忆配置（复用 MemoryConfig）
    #[serde(default)]
    pub memory: Option<MemoryConfig>,
}
```

### 5.1 设计说明

| 决策 | 说明 |
|------|------|
| 复用 `ModelConfig` / `MemoryConfig` | 与 `llm` 节点一致，避免重复定义 |
| 不新增 `stream` 字段 | 分类必须等待完整响应并解析 JSON |
| `query_variable_selector` 必填 | 保持输入来源显式、可验证 |
| `instruction` 可选 | 默认零提示也可工作，按需增强 |

### 5.2 DSL 示例（TOML）

```toml
[[nodes]]
id = "classifier1"

[nodes.data]
type = "question-classifier"
title = "Intent Classifier"
query_variable_selector = ["sys", "query"]
instruction = "If user asks about price, classify to sales."

[nodes.data.model]
provider = "openai"
name = "gpt-4o-mini"

[[nodes.data.categories]]
category_id = "cat_support"
category_name = "Customer Support"

[[nodes.data.categories]]
category_id = "cat_sales"
category_name = "Sales"

[[edges]]
source = "classifier1"
target = "support_handler"
sourceHandle = "cat_support"

[[edges]]
source = "classifier1"
target = "sales_handler"
sourceHandle = "cat_sales"

[[edges]]
source = "classifier1"
target = "default_handler"
sourceHandle = "default"
```

---

## 6. 语义校验规则

**目标文件**：`src/dsl/validation/layer3_semantic.rs`

在分支边校验阶段为 `question-classifier` 增加专用规则（独立于 `if-else`）：

### 6.1 配置有效性

1. `categories` 不能为空
2. `categories[].category_id` 必须唯一
3. `categories[].category_id` 不允许为 `"default"`
4. `category_id` / `category_name` 不允许空字符串

### 6.2 出边一致性（严格）

给定节点 `N`：

1. 必须存在 `source_handle = "default"` 出边
2. 每个 `category_id` 必须存在同名 `source_handle`
3. 不允许出现不在 `{all category_id} ∪ {"default"}` 的 `source_handle`

### 6.3 与通用规则关系

- `question-classifier` 仍属于 `BRANCH_NODE_TYPES`
- 非分支节点 `E303` 逻辑不受影响
- `if-else` 仍沿用现有 `false + case_id` 规则

---

## 7. 编译期 Typed Config 接入

### 7.1 编译产物扩展

**目标文件**：`src/compiler/compiled_workflow.rs`

为 `CompiledNodeConfig` 增加分支：

```rust
QuestionClassifier(CompiledConfig<QuestionClassifierNodeData>)
```

并在 `as_value()` 中补齐匹配分支。

### 7.2 编译器接入

**目标文件**：`src/compiler/compiler.rs`

在 `compile_node_config()` 中增加：

```rust
"question-classifier" => Self::compile_typed(raw, CompiledNodeConfig::QuestionClassifier)
```

收益：

- 运行期避免重复反序列化
- 配置错误更早暴露
- 与 `llm`、`if-else` 等 typed 节点路径一致

---

## 8. 执行器实现

### 8.1 模块位置

**新建文件**：`src/llm/question_classifier.rs`

放入 `llm/` 模块（非 `nodes/`）的原因：

1. 直接依赖 `LlmProviderRegistry`
2. 复用 `llm/types.rs` DTO
3. 与 `LlmNodeExecutor` 共享辅助函数

### 8.2 结构体

```rust
pub struct QuestionClassifierExecutor {
    registry: Arc<LlmProviderRegistry>,
}
```

### 8.3 核心执行流程

```text
1) 读取/解析 QuestionClassifierNodeData
2) 读取输入文本 variable_pool.get_resolved(query_variable_selector)
3) 构造 messages：memory(可选) + system + user
4) 发起非流式 chat_completion(stream=false)
5) 解析响应得到 candidate_category_id
6) 路由决策：
   - 命中 categories: Branch(candidate_id)
   - 未命中: Branch("default")
   - 响应不可解析: return NodeError::ExecutionError
7) 组装 NodeRunResult（outputs/metadata/usage/edge）
```

### 8.4 默认参数策略

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `temperature` | `Some(0.0)`（当未配置） | 分类追求稳定性 |
| `max_tokens` | `Some(256)`（当未配置） | 响应体极短 |
| `stream` | `false`（强制） | 必须等完整响应 |

---

## 9. Prompt 生成策略

采用程序化拼接，避免隐藏逻辑。

### 9.1 System Prompt 模板

```text
You are a text classification engine.
Classify the input into exactly one category.

Categories:
- category_id: "cat_support", category_name: "Customer Support"
- category_id: "cat_sales", category_name: "Sales"

Instruction:
If user asks about pricing, choose cat_sales.

Output format:
Return ONLY JSON: {"category_id":"<id>"}
Do not output markdown or explanations.
```

### 9.2 Instruction 渲染

- 若配置了 `instruction`，使用 `render_template_async_with_config(...)` 渲染
- 仅渲染文本内容，不改变 prompt 结构
- `strict_template` 行为继承运行时上下文设定

### 9.3 Message 组成

```text
[memory messages ...] (optional)
system: <classifier system prompt>
user: <resolved query text>
```

---

## 10. 响应解析与路由协议

### 10.1 解析函数

```rust
fn parse_classifier_response(response: &str, valid_ids: &HashSet<String>) -> Result<ClassifierDecision, NodeError>
```

其中：

```rust
enum ClassifierDecision {
    Matched(String),     // 命中配置 category_id
    FallbackDefault,     // 解析成功但 ID 非法
}
```

### 10.2 三层解析顺序

1. 直接按 JSON 解析并提取 `category_id`
2. 若为 Markdown code fence，剥离后再次 JSON 解析
3. 允许裸 ID（响应正文直接是某个字符串）

### 10.3 行为边界

- **解析失败**：返回错误，不隐式兜底
- **解析成功但 ID 非法**：显式兜底到 `default`

这是关键可观测性边界：

- 非法 ID = 模型偏移，系统可恢复
- 解析失败 = 协议失配，必须暴露

---

## 11. 输出契约

### 11.1 成功命中类别

```rust
outputs = {
    "category_id": "cat_support",
    "class_name": "Customer Support",
}
edge_source_handle = Branch("cat_support")
```

### 11.2 default 兜底

```rust
outputs = {
    "category_id": "default",
    "class_name": "default",
}
edge_source_handle = Branch("default")
```

### 11.3 metadata / usage

- `metadata.model` = 实际返回模型名
- `metadata.provider` = `data.model.provider`
- `llm_usage` 透传 provider usage

---

## 12. 错误处理与重试语义

| 场景 | 行为 |
|------|------|
| provider 不存在 | `NodeError::ExecutionError` |
| categories 空/重复 | `NodeError::ConfigError` |
| 输入 selector 取值失败 | 按变量解析错误返回 |
| LLM 请求失败 | 透传 provider 错误 |
| 响应解析失败 | `NodeError::ExecutionError` |
| 分类 ID 非法 | 成功返回 + 路由 `default` |

节点级 `retry_config`、`error_strategy` 沿用现有 dispatcher 机制，不新增隐藏分支。

---

## 13. 安全设计

### 13.1 凭证读取

与 `LlmNodeExecutor` 完全同构：

- 在 `security` feature 下从 `CredentialProvider` 拉取 provider 凭证
- 成功/失败均写审计日志

### 13.2 审计事件

- 记录 `SecurityEventType::CredentialAccess`
- `node_id` 使用当前节点 ID

### 13.3 Prompt 注入面控制

- 运行时用户输入仅进入 `user` message
- 分类规则与类别列表固定在 system message
- `instruction` 来源于 DSL 配置，不由终端用户直接控制

---

## 14. 性能设计

1. 强制非流式，避免不必要 stream 管线开销
2. 低 token 上限（默认 256）
3. 使用编译期 typed config 减少运行期反序列化
4. 默认温度 0 提升输出稳定性，减少重试概率

---

## 15. 注册与 Feature Gating

### 15.1 模块导出

**目标文件**：`src/llm/mod.rs`

- `pub mod question_classifier;`
- `pub use question_classifier::QuestionClassifierExecutor;`

### 15.2 执行器注册

**目标文件**：`src/nodes/executor.rs`

在 `set_llm_provider_registry(...)` 中：

```rust
self.register("llm", Box::new(LlmNodeExecutor::new(registry.clone())));
self.register("question-classifier", Box::new(QuestionClassifierExecutor::new(registry)));
```

说明：

- `with_builtins()` 中保留 stub 注册
- 启用 `builtin-llm-node` 时真实执行器覆盖 stub
- 未启用时维持 stub 行为

### 15.3 stub 判定细化

**目标文件**：`src/dsl/validation/known_types.rs`

`is_stub_node_type("question-classifier")` 需要随 `builtin-llm-node` 动态变化：

- 开启：返回 `false`
- 关闭：返回 `true`

避免“实现已启用但仍报告 stub 警告”。

---

## 16. 测试设计

### 16.1 单元测试（`src/llm/question_classifier.rs`）

| 测试名 | 目的 |
|--------|------|
| `parse_valid_json` | 直接 JSON 解析成功 |
| `parse_markdown_json` | code fence JSON 解析成功 |
| `parse_bare_id` | 裸 ID 识别 |
| `parse_malformed_fails` | 非法文本返回错误 |
| `invalid_id_routes_default` | 非法 ID 走 default |
| `empty_categories_config_error` | 空类别配置报错 |
| `missing_provider_error` | provider 缺失报错 |
| `class_name_from_config` | 输出 class_name 以配置为准 |

### 16.2 语义校验测试（`layer3_semantic`）

新增覆盖：

1. 缺失 `default` 边
2. 某个 `category_id` 缺失同名边
3. 存在未知 `source_handle`
4. `category_id` 重复

### 16.3 集成测试（`tests/integration/cases`）

#### Case 168: `question_classifier_basic`

- 模型返回合法 `category_id`
- 断言路由命中对应分支
- 断言输出 `category_id/class_name`

#### Case 169: `question_classifier_invalid_id_default`

- 模型返回未知 ID
- 断言路由到 `default`
- 断言输出为 default 约定

#### Case 170: `question_classifier_parse_error`

- 模型返回不可解析文本
- 断言节点失败并包含 parse 错误信息

---

## 17. 文件变更清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `src/dsl/schema.rs` | 修改 | 新增 `ClassifierCategory` / `QuestionClassifierNodeData` |
| `src/compiler/compiled_workflow.rs` | 修改 | 增加 `CompiledNodeConfig::QuestionClassifier` |
| `src/compiler/compiler.rs` | 修改 | 编译器接入 typed config |
| `src/llm/question_classifier.rs` | 新建 | 执行器、prompt、解析、单元测试 |
| `src/llm/mod.rs` | 修改 | 模块导出与 re-export |
| `src/nodes/executor.rs` | 修改 | 注册真实执行器 |
| `src/dsl/validation/layer3_semantic.rs` | 修改 | classifier 专用分支边校验 |
| `src/dsl/validation/known_types.rs` | 修改 | stub 判定 feature 化 |
| `tests/integration_tests.rs` | 修改 | 挂载新增 case |
| `tests/integration/cases/168_*` | 新建 | 基础分类案例 |
| `tests/integration/cases/169_*` | 新建 | default 兜底案例 |
| `tests/integration/cases/170_*` | 新建 | 解析失败案例 |

---

## 18. 实施顺序

1. Schema：`QuestionClassifierNodeData` 与 `ClassifierCategory`
2. Compiler：typed config 接入
3. Runtime：`question_classifier.rs` 实现
4. Registry：executor 注册与模块导出
5. Validation：layer3 语义规则补齐
6. Tests：单元 + 语义 + 集成
7. 文档同步：索引与实现状态总结

---

## 19. 验收标准（Definition of Done）

满足以下条件即视为完成：

1. `question-classifier` 不再由 stub 执行（在 `builtin-llm-node` 开启时）
2. 三种核心路径可稳定复现：命中、default、解析失败
3. 语义校验能阻止非法分支图
4. `cargo test --all-features --workspace` 通过
5. `cargo check --workspace --all-targets --all-features` 无新增告警
6. 相关设计索引与状态文档同步更新

---

## 20. 风险与回滚

### 20.1 主要风险

- 模型输出格式不稳定导致解析失败率偏高
- 业务方漏配 `default` 出边导致运行时不可达
- feature 判定遗漏导致“已实现仍被标记为 stub”

### 20.2 缓解策略

- 提升提示约束并增加解析容错（JSON/code fence/裸 ID）
- 在语义层强制 `default` 边与 category 边齐全
- 为 `is_stub_node_type` 添加 feature 组合测试

### 20.3 回滚方案

- 若线上异常，可仅撤销 `set_llm_provider_registry` 中 classifier 注册
- 回退后自动恢复 stub 行为，不影响其它节点

---

## 21. 验证命令

```bash
cargo build --all-features
cargo test --all-features --workspace --lib question_classifier
cargo test --all-features --workspace --test integration_tests -- 168
cargo test --all-features --workspace --test integration_tests -- 169
cargo test --all-features --workspace --test integration_tests -- 170
cargo test --all-features --workspace dsl::validation
cargo test --all-features --workspace
cargo check --workspace --all-targets --all-features
cargo clippy --all-features --workspace
```
