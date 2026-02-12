# Human Input 节点设计文档

## 背景

xworkflow 当前的执行模型是**全自动一次性执行**：`WorkflowDispatcher::run()` 从 Start 节点出发沿 DAG 拓扑逐节点执行，直到所有路径抵达 End/Answer 节点。**没有任何机制允许工作流在中途暂停并等待外部人工输入**。

DSL schema 中已声明 `NodeType::HumanInput`（`src/dsl/schema.rs:213`），但执行器注册表中只是 `StubExecutor`（`src/nodes/executor.rs:112`），返回 "not implemented"。

### 需求场景

| 场景 | 说明 |
|------|------|
| **审批流** | AI 生成内容后，需要人工审核批准/拒绝才能继续 |
| **表单补充** | 工作流执行中发现缺少必要信息，暂停等待用户补充 |
| **人工校验** | LLM 输出需要人工确认/修改后再传给下游 |
| **多阶段交互** | 复杂流程需要在中间环节征求用户意见或选择 |
| **工具调用审批** | Agent 节点执行敏感工具前，先获得人工授权 |

### 业界参考

- **n8n**（最成熟）: Wait Node + Form 模式，12+ 字段类型、Webhook 恢复、超时处理、审批分支
- **Dify**: 仅支持工作流起始的 User Input，中途暂停为计划功能（未实现）
- **Coze**: 无专门的人工输入节点

---

## 整体架构

```
┌─────────────────────────────────────────────────────┐
│              WorkflowDispatcher                      │
│                                                      │
│  queue.pop() → HumanInputExecutor.execute()          │
│       │                                              │
│       ▼  返回 status: Paused                          │
│  handle_node_paused()                                │
│       │                                              │
│       ├─→ emit HumanInputRequested 事件               │
│       │      (form_schema, resume_token, prompt)     │
│       │                                              │
│       └─→ tokio::select! {                           │
│              cmd_rx.recv() → ResumeHumanInput ──┐    │
│              timeout ──────────────────────────┐│    │
│           }                                   ││    │
│                                               ▼▼    │
│           验证输入 → 写入 VariablePool → 继续执行      │
└─────────────────────────────────────────────────────┘
        ▲ Command channel
        │
  应用层 API: workflow_handle.resume_human_input(token, data)
```

**核心思路**: HumanInput Executor 返回 `Paused` 状态 + 表单 schema，Dispatcher 识别后进入等待模式，通过 Command channel 接收外部恢复信号（携带人工输入数据），然后继续执行下游节点。

---

## 一、DSL Schema 设计

### 1.1 节点配置（HumanInputNodeData）

新增于 `src/dsl/schema.rs`：

```rust
pub struct HumanInputNodeData {
    pub resume_mode: HumanInputResumeMode,    // 恢复模式
    pub form_fields: Vec<FormFieldDefinition>, // 表单字段
    pub timeout_secs: Option<u64>,             // 超时（秒），None=无限等待
    pub timeout_action: HumanInputTimeoutAction, // 超时策略
    pub timeout_default_values: Option<HashMap<String, Value>>, // 超时默认值
    pub notification: Option<NotificationConfig>, // 通知配置
    pub prompt_text: Option<String>,           // 提示文本（支持模板变量）
    pub prompt_variables: Vec<VariableMapping>, // 提示文本中的变量引用
}
```

### 1.2 恢复模式

```rust
pub enum HumanInputResumeMode {
    Form,      // 表单提交：等待用户填写表单后恢复
    Approval,  // 审批模式：等待 approve/reject 决策，可附带表单
    Webhook,   // Webhook 回调：等待外部系统通过 Webhook 提交数据
}
```

### 1.3 表单字段定义

```rust
pub struct FormFieldDefinition {
    pub variable: String,                      // 字段名（写入 VariablePool 的 key）
    pub label: String,                         // 显示标签
    pub field_type: FormFieldType,             // 字段类型
    pub required: bool,                        // 是否必填
    pub default_value: Option<Value>,          // 静态默认值
    pub default_value_selector: Option<VariableSelector>, // 从 Pool 读取默认值
    pub placeholder: Option<String>,           // 占位提示
    pub description: Option<String>,           // 描述/帮助文本
    pub validation: Option<FieldValidation>,   // 验证规则
    pub options: Option<Vec<FieldOption>>,      // 下拉/单选/多选选项
}

pub enum FormFieldType {
    Text,        // 单行文本
    Textarea,    // 多行文本
    Number,      // 数字
    Checkbox,    // 布尔
    Radio,       // 单选
    Dropdown,    // 下拉选择
    MultiSelect, // 多选
    Date,        // 日期（ISO 8601）
    Email,       // 邮箱
    Json,        // 结构化 JSON
    File,        // 文件上传
    Hidden,      // 隐藏字段（传递上下文）
}

pub struct FieldValidation {
    pub min_length: Option<i32>,   // 文本最小长度
    pub max_length: Option<i32>,   // 文本最大长度
    pub min_value: Option<f64>,    // 数字最小值
    pub max_value: Option<f64>,    // 数字最大值
    pub pattern: Option<String>,   // 正则验证
    pub error_message: Option<String>, // 自定义错误提示
}

pub struct FieldOption {
    pub value: String, // 选项值
    pub label: String, // 显示标签
}
```

### 1.4 超时策略

```rust
pub enum HumanInputTimeoutAction {
    Fail,         // 超时节点失败（走 error_strategy）
    DefaultValue, // 使用 timeout_default_values 继续
    AutoApprove,  // 自动批准（仅 Approval 模式）
    AutoReject,   // 自动拒绝（仅 Approval 模式）
}
```

### 1.5 通知配置

```rust
pub struct NotificationConfig {
    pub notification_type: NotificationType,
    pub target: Option<String>,              // URL / channel / 用户 ID
    pub params: HashMap<String, Value>,       // 附加参数
}

pub enum NotificationType {
    Webhook, // Webhook 回调通知
    Event,   // 通过 EventEmitter 发出，应用层处理
    None,    // 无通知（应用层轮询）
}
```

### 1.6 DSL YAML 示例

**表单模式**：
```yaml
- id: collect_info
  data:
    type: human-input
    title: "补充信息"
    resume_mode: form
    prompt_text: "请补充以下信息以继续处理"
    timeout_secs: 3600
    timeout_action: fail
    form_fields:
      - variable: phone
        label: "联系电话"
        field_type: text
        required: true
        validation:
          pattern: "^1[3-9]\\d{9}$"
          error_message: "请输入有效手机号"
      - variable: address
        label: "详细地址"
        field_type: textarea
        required: true
        validation:
          max_length: 500
```

**审批模式**：
```yaml
- id: human_approve
  data:
    type: human-input
    title: "人工审批"
    resume_mode: approval
    prompt_text: "请审批以下申请：{{#llm_review.summary#}}"
    timeout_secs: 86400
    timeout_action: auto_reject
    form_fields:
      - variable: comment
        label: "审批意见"
        field_type: textarea
        required: false
    notification:
      type: webhook
      target: "https://hooks.example.com/approval"

edges:
  - source: human_approve
    target: approved_handler
    sourceHandle: approve
  - source: human_approve
    target: rejected_handler
    sourceHandle: reject
```

---

## 二、执行流程设计

### 2.1 暂停-恢复时序

```
t0  Dispatcher 执行到 human-input 节点
t1  HumanInputExecutor.execute() → NodeRunResult { status: Paused, metadata: {resume_token, form_schema, ...} }
t2  Dispatcher.handle_node_paused():
    ├── 发出 HumanInputRequested 事件
    ├── 发送 WaitingForInput 状态
    └── tokio::select! 等待 cmd_rx 或超时

    [... 应用层收到事件，展示表单给操作者 ...]

t3  操作者填写表单 / 做出审批决策
t4  应用层调用 handle.resume_human_input(token, decision, data)
t5  Dispatcher cmd_rx 收到 ResumeHumanInput 命令
t6  验证输入数据（类型检查、必填、验证规则）
t7  将表单数据 + 决策写入 VariablePool
t8  确定 EdgeHandle（Form→Default, Approval→Branch("approve"/"reject")）
t9  发出 HumanInputReceived + NodeRunSucceeded 事件
t10 enqueue 下游节点，继续正常执行
```

### 2.2 HumanInput Executor 逻辑

Executor 本身的 `execute()` **不做等待**，只负责：
1. 解析 `HumanInputNodeData` 配置
2. 渲染 `prompt_text` 中的模板变量
3. 为 hidden 字段和 `default_value_selector` 从 Pool 读取默认值
4. 生成 `resume_token`（UUID，通过 `context.id_generator`）
5. 计算超时时间戳
6. 将所有信息放入 `metadata`，返回 `status: Paused`

```rust
// 核心返回
Ok(NodeRunResult {
    status: WorkflowNodeExecutionStatus::Paused,
    inputs: HashMap::new(),
    outputs: NodeOutputs::Sync(HashMap::new()), // 恢复时填充
    metadata, // resume_token, form_schema, prompt_text, timeout_secs, timeout_action, notification
    edge_source_handle: EdgeHandle::Default,     // 恢复时重设
    ..Default::default()
})
```

### 2.3 Dispatcher 暂停等待（handle_node_paused）

在 `run()` 主循环（`src/core/dispatcher.rs:1026`）中，匹配 `result.status == Paused` 时调用 `handle_node_paused`：

```rust
match run_result {
    Ok(result) if result.status == WorkflowNodeExecutionStatus::Paused => {
        self.handle_node_paused(&exec_id, &node_id, &info, result, &mut queue).await?;
    }
    Ok(result) => {
        self.handle_node_success(&exec_id, &node_id, &info, result, &mut queue).await?;
    }
    Err(e) => { ... }
}
```

`handle_node_paused` 的核心逻辑：

1. 从 metadata 提取 resume_token、form_schema、timeout 配置
2. 发出 `HumanInputRequested` 事件
3. 通过 `status_tx` 发送 `WaitingForInput` 状态
4. `tokio::select!` 等待 `cmd_rx` 收到 `ResumeHumanInput` 或超时
5. **收到恢复**：验证数据 → 写入 Pool → 确定 EdgeHandle → 发出事件 → `handle_node_success`
6. **超时**：根据 `timeout_action` 处理（Fail/DefaultValue/AutoApprove/AutoReject）

### 2.4 超时处理

| timeout_action | 行为 |
|----------------|------|
| `Fail` | 返回 `NodeError::Timeout`，走节点 `error_strategy` |
| `DefaultValue` | 使用 `timeout_default_values` 填充 outputs，继续 |
| `AutoApprove` | decision=approve，走 approve 分支 |
| `AutoReject` | decision=reject，走 reject 分支 |

---

## 三、引擎变更

### 3.1 Command 扩展

`src/core/dispatcher.rs:62-68`：

```rust
pub enum Command {
    Abort { reason: Option<String> },
    Pause,
    UpdateVariables { variables: HashMap<String, Value> },
    // 新增
    ResumeHumanInput {
        node_id: String,
        resume_token: String,
        decision: Option<HumanInputDecision>,  // Approve / Reject
        form_data: HashMap<String, Value>,
    },
}

pub enum HumanInputDecision {
    Approve,
    Reject,
}
```

### 3.2 Dispatcher 新增 cmd_rx 字段

`src/core/dispatcher.rs:101-114`：

```rust
pub struct WorkflowDispatcher<G, H> {
    // ... 现有字段 ...
    cmd_rx: Option<mpsc::Receiver<Command>>,  // 新增：外部命令接收端
}
```

构造函数增加 `cmd_rx` 参数（可选，不影响不需要 HumanInput 的场景）。

### 3.3 新增事件

`src/core/event_bus.rs`，在 `GraphEngineEvent` 枚举中添加：

```rust
// === Human Input 事件 ===
HumanInputRequested {
    node_id: String,
    node_type: String,
    node_title: String,
    resume_token: String,          // 唯一恢复令牌
    resume_mode: String,           // "form" | "approval" | "webhook"
    form_schema: Value,            // 表单字段 JSON
    prompt_text: Option<String>,   // 已渲染的提示文本
    timeout_at: Option<i64>,       // 超时 Unix 时间戳
},

HumanInputReceived {
    node_id: String,
    resume_token: String,
    decision: Option<String>,      // "approve" | "reject"
    form_data: HashMap<String, Value>,
},

HumanInputTimeout {
    node_id: String,
    resume_token: String,
    timeout_action: String,        // "fail" | "default_value" | "auto_approve" | "auto_reject"
},
```

### 3.4 ExecutionStatus 扩展

`src/scheduler.rs:38-47`：

```rust
pub enum ExecutionStatus {
    Running,
    Completed(HashMap<String, Value>),
    Failed(String),
    FailedWithRecovery { ... },
    // 新增
    WaitingForInput {
        node_id: String,
        resume_token: String,
        resume_mode: String,
        form_schema: Value,
        prompt_text: Option<String>,
        timeout_at: Option<i64>,
    },
}
```

### 3.5 WorkflowHandle 扩展

`src/scheduler.rs:53-93`，增加命令发送能力：

```rust
pub struct WorkflowHandle {
    status_rx: watch::Receiver<ExecutionStatus>,
    events: Option<Arc<Mutex<Vec<GraphEngineEvent>>>>,
    event_active: Arc<AtomicBool>,
    cmd_tx: Option<mpsc::Sender<Command>>,   // 新增
}

impl WorkflowHandle {
    // 新增：恢复人工输入
    pub async fn resume_human_input(
        &self,
        node_id: &str,
        resume_token: &str,
        decision: Option<HumanInputDecision>,
        form_data: HashMap<String, Value>,
    ) -> Result<(), WorkflowError> { ... }
}
```

---

## 四、表单字段类型与 Segment 映射

| FormFieldType | JSON 输入类型 | Segment 类型 | SegmentType |
|---------------|-------------|-------------|-------------|
| Text | String | String | String |
| Textarea | String | String | String |
| Number | Number | Integer / Float | Number |
| Checkbox | Boolean | Boolean | Boolean |
| Radio | String | String | String |
| Dropdown | String | String | String |
| MultiSelect | Array\<String\> | ArrayString | ArrayString |
| Date | String (ISO 8601) | String | String |
| Email | String | String | String |
| Json | Object | Object | Object |
| File | Object | Object | File |
| Hidden | any | from_value | Any |

下游节点通过 `["human_input_node_id", "field_variable"]` 选择器引用输入数据。

---

## 五、审批模式（Approval Pattern）

### 5.1 分支路由

Approval 模式下，节点有两个出边（通过 `sourceHandle` 区分）：
- `sourceHandle: "approve"` → 批准路径
- `sourceHandle: "reject"` → 拒绝路径

图构建器已通过检查出边的 `source_handle` 自动识别分支节点（`Graph::is_branch_node()`），无需硬编码。

### 5.2 决策 + 表单组合

审批者在做出决定的同时可填写附加表单：

```json
// ResumeHumanInput payload
{
    "node_id": "human_approve",
    "resume_token": "token_xxx",
    "decision": "approve",
    "form_data": {
        "comment": "审核通过，可以发布",
        "priority": "high"
    }
}
```

写入 VariablePool：
- `(human_approve, comment)` = Segment::String("审核通过，可以发布")
- `(human_approve, priority)` = Segment::String("high")
- `(human_approve, __decision)` = Segment::String("approve")

### 5.3 与 IfElse 对比

| 特性 | IfElse | HumanInput (Approval) |
|------|--------|----------------------|
| 分支决策 | 自动条件评估 | 人工决策 |
| 执行模式 | 同步即时 | 异步等待恢复 |
| 附加数据 | 无 | 可附带表单 |
| 超时处理 | 不需要 | auto_approve / auto_reject |

---

## 六、输入验证

`validate_form_input(fields, data)` 函数负责验证恢复时提交的数据：

1. **必填检查** — required 字段不能缺失或为 null
2. **类型检查** — 值类型匹配 field_type（如 Number 要求 JSON number）
3. **验证规则** — min_length/max_length/min_value/max_value/pattern
4. **选项检查** — Dropdown/Radio/MultiSelect 的值必须在 options 中
5. **Approval 决策检查** — Approval 模式必须包含 decision（approve/reject）

验证失败返回 `NodeError::InputValidationError`，由应用层决定是否允许重新提交。

---

## 七、DSL 验证变更

### Layer 1（结构验证）`src/dsl/validation/layer1_structure.rs`
- `resume_mode` 必须是合法枚举值
- `form_fields` 中 `variable` 不能重复
- `field_type` 必须是合法枚举值
- Approval 模式必须有 `approve` 和 `reject` 两个出边
- `timeout_action` 为 `auto_approve`/`auto_reject` 时 `resume_mode` 必须为 `approval`
- `timeout_action` 为 `default_value` 时 `timeout_default_values` 不能为空

### Layer 3（语义验证）`src/dsl/validation/layer3_semantic.rs`
- `default_value_selector` 引用的变量必须存在于上游节点输出
- `prompt_variables` 中的变量引用必须有效

---

## 八、等待策略与资源消耗

### 设计决策：内存等待

采用**内存等待**方案 — Dispatcher 的 `run()` async 函数在 `tokio::select!` 处挂起等待。

**资源分析**：
- **CPU**：Tokio task 被 park，**不消耗 CPU**
- **内存**：`WorkflowDispatcher` 及其 `Arc` 引用（Graph、VariablePool、Registry、Context）驻留内存
  - 典型工作流约占 **几十 KB ~ 几 MB**（取决于 Pool 中变量数据量）
  - 对于分钟级等待完全可接受
- **进程重启**：等待中的工作流状态会丢失

**适用场景**：等待时间在分钟到小时级别。如果未来需要支持天级等待或进程重启后恢复，可以增加持久化层（序列化 WorkflowState → DB，恢复时反序列化重建 Dispatcher），但这不在当前设计范围内。

### 并发处理
- 每个 WorkflowRunner 独立的 Dispatcher + Command channel，多工作流可同时 WaitingForInput
- 同一工作流中多个 HumanInput 按 DAG 拓扑序逐个暂停恢复（Dispatcher 顺序执行）
- `resume_token` 通过 `RuntimeContext.id_generator` 保证全局唯一

### 安全考量
- `resume_token` 使用加密安全随机 ID（UUID v4），防止猜测
- 应用层验证 token 与 node_id 对应关系
- 所有人工输入经过表单验证（类型、必填、长度、正则）
- JSON 输入有大小限制，File 输入受 ResourceGroup 约束
- 权限控制（谁可以审批）由应用层负责，不在引擎层实现

---

## 九、需要修改的文件

| 文件 | 变更类型 | 内容 |
|------|---------|------|
| `src/dsl/schema.rs` | **新增类型** | HumanInputNodeData, FormFieldDefinition, FormFieldType, FieldValidation, FieldOption, HumanInputResumeMode, HumanInputTimeoutAction, NotificationConfig |
| `src/core/dispatcher.rs` | **核心变更** | Command 扩展 ResumeHumanInput, 新增 cmd_rx 字段, handle_node_paused 方法, run() 循环 Paused 分支 |
| `src/core/event_bus.rs` | **扩展枚举** | HumanInputRequested / HumanInputReceived / HumanInputTimeout 事件 |
| `src/nodes/control_flow.rs` | **新增** | HumanInputNodeExecutor 实现 |
| `src/nodes/executor.rs` | **替换注册** | StubExecutor → HumanInputNodeExecutor |
| `src/scheduler.rs` | **扩展** | ExecutionStatus::WaitingForInput, WorkflowHandle 增加 cmd_tx + resume_human_input() |
| `src/dsl/validation/layer1_structure.rs` | **新增规则** | human-input 配置验证 |
| `src/dsl/validation/layer3_semantic.rs` | **新增规则** | 变量引用和出边验证 |
| `src/dsl/validation/known_types.rs` | **移除** | 从 STUB_NODE_TYPES 移除 "human-input" |

---

## 十、实现步骤

### Phase 1: 核心基础设施
1. 扩展 `Command` 枚举，添加 `ResumeHumanInput` + `HumanInputDecision`
2. `WorkflowDispatcher` 增加 `cmd_rx` 字段，更新构造函数
3. `GraphEngineEvent` 增加 3 个 HumanInput 事件
4. `ExecutionStatus` 增加 `WaitingForInput`
5. `WorkflowHandle` 增加 `cmd_tx` + `resume_human_input()` 方法

### Phase 2: 节点实现
6. `src/dsl/schema.rs` 添加所有新类型定义
7. 实现 `HumanInputNodeExecutor`（放在 `control_flow.rs`）
8. 实现 `handle_node_paused` 方法（Dispatcher 核心等待逻辑）
9. 实现 `validate_form_input` 验证函数

### Phase 3: 集成
10. 替换 StubExecutor 注册
11. WorkflowRunner/Builder 传递 Command channel
12. 添加 DSL 验证规则
13. 从 STUB_NODE_TYPES 移除 "human-input"

### Phase 4: 测试
14. 单元测试：Executor 返回 Paused、表单验证各规则、超时各策略
15. 集成测试：Form 模式端到端、Approval 模式分支路由、超时 DefaultValue/AutoReject
16. 并发测试：多工作流同时 WaitingForInput

---

## 十一、验证方式

1. **单元测试** — 验证 HumanInputExecutor 返回 Paused 状态、validate_form_input 各种情况
2. **集成测试** — 新增测试用例（如 `031_human_input_form`、`032_human_input_approval`）：
   - 创建包含 human-input 节点的 workflow.json
   - 使用 WorkflowRunner 启动工作流
   - 检查 WorkflowHandle.status() 为 WaitingForInput
   - 调用 resume_human_input() 提交数据
   - 验证最终输出包含人工输入的数据
3. **超时测试** — 设置短超时（1秒），验证各 timeout_action 的行为
4. **审批分支测试** — Approval 模式下分别提交 approve/reject，验证走不同分支
