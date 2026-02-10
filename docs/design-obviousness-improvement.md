# 设计显然性改进方案

> **目标**：消除代码中的隐知识（implicit knowledge），使设计如想象一样——不会产生误解，不需要"记住规则"才能正确使用。

---

## 一、高严重度问题

### 1. `Segment::from_value` 数组魔法推断

**现状**（`src/core/variable_pool.rs:398-412`）：

```rust
pub fn from_value(v: &Value) -> Self {
    Value::Array(arr) => {
        let all_strings = arr.iter().all(|v| v.is_string());
        if all_strings {
            Segment::ArrayString(items)  // 全是字符串 → ArrayString
        } else {
            Segment::Array(segs)         // 否则 → Array
        }
    }
}
```

**问题**：
- 所有节点输出都经过 `set_node_outputs → Segment::from_value` 隐式转换
- 输出 `["a", "b"]` 得到 `ArrayString`，输出 `["a", 1]` 得到 `Array`——仅凭内容决定类型
- 下游节点用 `pool.get()` 获取到的 Segment 变体取决于上游输出的**具体值**，而非声明的类型
- 如果上游节点在某次运行中输出 `["hello"]`（ArrayString），另一次输出 `["hello", 42]`（Array），下游使用 `match` 的代码可能在不同运行中走不同分支

**方案：保留 `ArrayString` 变体，但 `from_value` 不再自动推断**

`ArrayString(Vec<String>)` 相比 `Array(Vec<Segment::String>)` 每元素省 2.3 倍内存（24B vs 56B），且 clone、比较操作更高效。因此保留该变体，但禁止隐式构造。

**改动 1：`from_value` 统一走 `Array`**

```rust
pub fn from_value(v: &Value) -> Self {
    Value::Array(arr) => {
        if arr.is_empty() {
            Segment::Array(Vec::new())
        } else {
            // 不再猜测类型，统一为 Array
            Segment::Array(arr.iter().map(Segment::from_value).collect())
        }
    }
}
```

**改动 2：提供显式构造方法**

```rust
impl Segment {
    /// 显式构造字符串数组。用于已知类型为 ArrayString 的场景（如 DSL 声明、节点内部逻辑）。
    /// 比 Array(Vec<Segment::String>) 节省 ~2.3x 内存。
    pub fn string_array(items: Vec<String>) -> Self {
        Segment::ArrayString(items)
    }
}
```

**改动 3：DSL 反序列化时根据 schema 声明的类型构造**

在 DSL 解析层（start 节点读取输入变量、variable-aggregator 合并变量等），如果 schema 声明了 `type: array[string]`，则调用 `Segment::string_array()` 而非 `Segment::from_value()`。这样类型由声明决定，不由内容猜测。

**理由**：
- `from_value` 是通用转换，不应承担类型推断职责
- `ArrayString` 的构造权交给**知道类型信息的调用方**（DSL schema、节点内部逻辑）
- 保留性能优势：纯字符串数组仍享受紧凑存储

**影响范围**：
- `Segment::from_value`：删除 `all_strings` 推断逻辑
- 需审查所有依赖 `from_value` 自动产生 `ArrayString` 的路径，改为显式构造
- `condition.rs` 中 `eval_contains` 和 `eval_all_of`：保留 `ArrayString` 分支（该变体仍存在）
- `PartialEq for Segment`：保留 `ArrayString` 专用分支

---

### 2. Selector 隐式补全 `__scope__` + Pool key 用 `\0` 分隔

**现状**（`src/nodes/utils.rs:30-36` + `src/core/variable_pool.rs:524`）：

```rust
// utils.rs — 隐式补全
pub fn normalize_selector(parts: Vec<String>) -> Vec<String> {
    if parts.len() < 2 {
        vec![SCOPE_NODE_ID.to_string(), parts[0].clone()]
    } else { parts }
}

// variable_pool.rs — \0 分隔 key
fn make_key(node_id: &str, name: &str) -> String {
    format!("{}\0{}", node_id, name)
}
```

**问题**：
- Selector 在整个系统中以 `Vec<String>` 传递，没有类型约束
- 任何人都可以构造非法 selector（空 Vec、3 元素 Vec）
- `__scope__` 补全是纯约定，没有文档
- `\0` 分隔是泄漏的实现细节，如果序列化/调试/日志 pool 内容会看到不可见字符

**方案：引入 `Selector` newtype**

```rust
/// 变量选择器：定位 VariablePool 中的一个变量。
///
/// 格式为 (node_id, variable_name)，例如 ("start", "query") 对应 start 节点的 query 输出。
/// 特殊 node_id `__scope__` 表示当前子图的作用域变量。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Selector {
    node_id: String,
    variable_name: String,
}

impl Selector {
    /// 从 DSL 值解析 selector。
    /// - `["node_id", "var"]` → Selector { node_id, var }
    /// - `["var"]` → Selector { __scope__, var }（子图作用域快捷写法）
    /// - `"node_id.var"` → Selector { node_id, var }
    pub fn parse(value: &Value) -> Option<Self> { ... }

    /// 内部存储 key（仅 VariablePool 使用）
    pub(crate) fn pool_key(&self) -> String {
        format!("{}:{}", self.node_id, self.variable_name)
    }
}
```

**设计要点**：
- 构造只能通过 `Selector::parse()` 或 `Selector::new(node_id, var_name)`，不能直接访问内部字段
- `normalize` 逻辑内聚在 `parse()` 中，调用者看到的已经是完整的 Selector
- Pool key 格式改为 `:` 分隔（可打印字符），且通过 `pub(crate)` 限制访问
- `VariablePool::get/set` 签名从 `&[String]` 改为 `&Selector`

**影响范围**：
- `VariablePool` 的 `get`、`set`、`set_node_outputs`、`has` 等方法签名变更
- `Condition.variable_selector` 从 `Vec<String>` 改为 `Selector`
- 所有节点内部的 `pool.get(&["node", "var"])` 调用改为 `pool.get(&Selector::new("node", "var"))`
- DSL schema 中所有 `value_selector: Vec<String>` 改为 `value_selector: Selector`（实现 Deserialize）
- `nodes/utils.rs` 可精简甚至删除

---

### 3. `outputs` vs `stream_outputs` 无契约

**现状**（`src/dsl/schema.rs:613-627`）：

```rust
pub struct NodeRunResult {
    pub outputs: HashMap<String, Value>,              // 普通输出
    pub stream_outputs: HashMap<String, SegmentStream>, // 流式输出
    // ...
}
```

**问题**：
- 两个字段并列存在，没有文档说明什么时候用哪个
- 同一个 key 能否同时出现在两个 map 中？不清楚
- Dispatcher 分别处理（`outputs` 走 `set_node_outputs`，`stream_outputs` 走 `pool.set` + `Segment::Stream`），但这个分工只在 dispatcher.rs:950-1074 能看到
- 新节点开发者无法从类型签名推断正确用法

**实际使用模式**（通过代码审查确认）：
- **Answer 节点**：同时用 `outputs`（空或 placeholder）+ `stream_outputs`（流式回答）
- **Template-transform**：流式输入时用 `stream_outputs`，静态输入时用 `outputs`
- **LLM 节点**：流式模式用 `stream_outputs`，非流式用 `outputs`
- **其他节点**：只用 `outputs`

**方案：用 enum 统一输出模型**

```rust
pub struct NodeRunResult {
    pub status: WorkflowNodeExecutionStatus,
    pub inputs: HashMap<String, Value>,
    pub outputs: NodeOutputs,  // ← 替代两个字段
    pub metadata: NodeMetadata,
    pub edge_source_handle: EdgeHandle,  // ← 见 #4
    pub error: Option<NodeErrorInfo>,    // ← 合并 error/error_type/error_detail
    pub retry_index: i32,
}

/// 节点输出。一个节点要么返回同步结果，要么返回流式结果，不会混合。
pub enum NodeOutputs {
    /// 同步输出：所有值已就绪
    Sync(HashMap<String, Value>),
    /// 流式输出：部分或全部值是异步流
    /// HashMap 中 Value 是已就绪的值，SegmentStream 是流式值
    Stream {
        ready: HashMap<String, Value>,
        streams: HashMap<String, SegmentStream>,
    },
}
```

**设计要点**：
- 类型强制二选一：同步或流式，编译期不可能混淆
- `Stream` 变体同时带 `ready` 和 `streams`，因为 LLM 节点可能既有 `text`（流）又有 `usage`（同步）
- Dispatcher 用 `match` 处理，两个分支逻辑清晰

**影响范围**：
- 所有 `NodeExecutor::execute` 实现需要返回 `NodeOutputs::Sync(...)` 或 `NodeOutputs::Stream { ... }`
- Dispatcher 的输出写入逻辑用 match 替代当前的顺序处理
- 现有的 answer、template-transform、LLM 节点的 stream_outputs 用法迁移到 `NodeOutputs::Stream`

---

### 4. `edge_source_handle` 魔术字符串决定分支

**现状**（`src/dsl/schema.rs:622` + `src/graph/types.rs:86-107`）：

```rust
// NodeRunResult 中
pub edge_source_handle: String,  // default: "source"

// Graph 中分支处理
pub fn process_branch_edges(&mut self, node_id: &str, selected_handle: &str) {
    let handle = edge.source_handle.as_deref().unwrap_or("source");
    if handle == selected_handle { Taken } else { Skipped }
}
```

**问题**：
- `"source"` 是个魔术字符串，分散在 default impl 和 graph 代码中
- If-else 节点返回 `case_id`（如 `"case1"`），dispatcher 用字符串匹配 edge 的 `source_handle`
- 如果 `case_id` 和 `source_handle` 拼写不一致，边被静默跳过（无报错）
- Else 分支用 `"false"` 字符串（`evaluator/condition.rs:13`），又是一个魔术值
- `is_branch_node` 硬编码判断 `"if-else" || "question-classifier"`（`graph/types.rs:160-162`）

**方案：类型化 + 校验**

```rust
/// 节点出边标识。
/// 非分支节点使用 Default（所有出边都走），分支节点使用 Branch 指定走哪条。
pub enum EdgeHandle {
    /// 非分支节点的默认出边（all edges taken）
    Default,
    /// 分支节点选中的 case（匹配 edge.source_handle）
    Branch(String),
}

impl Default for EdgeHandle {
    fn default() -> Self { EdgeHandle::Default }
}
```

**在 Dispatcher 中：**

```rust
match &result.edge_source_handle {
    EdgeHandle::Default => graph.process_normal_edges(&node_id),
    EdgeHandle::Branch(handle) => {
        // 校验：handle 必须匹配至少一条出边的 source_handle
        let valid = graph.out_edges.get(&node_id)
            .map(|eids| eids.iter().any(|eid|
                graph.edges.get(eid)
                    .and_then(|e| e.source_handle.as_deref())
                    == Some(handle.as_str())
            ))
            .unwrap_or(false);
        if !valid {
            return Err(WorkflowError::GraphValidationError(
                format!("Node {} returned branch handle '{}' but no matching edge found", node_id, handle)
            ));
        }
        graph.process_branch_edges(&node_id, handle);
    }
}
```

**设计要点**：
- 非分支节点不需要关心 handle，用 `EdgeHandle::Default`
- 分支节点显式返回 `EdgeHandle::Branch("case1")`
- Dispatcher 在运行时校验 handle 是否匹配出边，不再静默跳过
- `is_branch_node` 可以改为判断 `EdgeHandle` 类型，不再硬编码节点类型名

**附带清理**：
- 删除 `evaluate_cases` 返回的 `"false"` 魔术字符串，改为 `Option<String>`（None 表示 else 分支）
- 或者保持 `"false"` 但定义常量 `const ELSE_BRANCH_HANDLE: &str = "false"`

**影响范围**：
- `NodeRunResult.edge_source_handle` 类型变更
- 所有 `NodeExecutor::execute` 实现中设置 handle 的地方
- `Graph::process_branch_edges` 签名不变（内部仍接收 `&str`），但 Dispatcher 层做校验
- `evaluator/condition.rs` 的 `evaluate_cases` 返回值考虑改为 `Option<String>`

---

## 二、中严重度问题

### 5. Pool key `\0` 分隔

已在 **#2 Selector newtype** 方案中合并解决。`Selector::pool_key()` 使用 `:` 分隔，且为 `pub(crate)` 可见性。

---

### 6. `Segment ↔ Value` 有损转换

**现状**：`Segment::Stream` 调用 `to_value()` 会快照化，丢失流状态。

**问题**：这个转换不可逆，但没有任何标记。开发者可能以为 `to_value().pipe(from_value)` 是恒等操作。

**方案：区分 `to_value` 和 `snapshot_to_value`**

```rust
impl Segment {
    /// 将 Segment 转为 Value。非流式类型无损转换。
    /// 对 Stream 类型，会 panic——请使用 `snapshot_to_value()` 显式快照。
    pub fn to_value(&self) -> Value {
        match self {
            Segment::Stream(_) => panic!(
                "Cannot call to_value() on Stream. Use snapshot_to_value() for explicit lossy conversion."
            ),
            // ... 其他变体不变
        }
    }

    /// 将 Segment 转为 Value，流式数据会被快照化（有损）。
    pub fn snapshot_to_value(&self) -> Value {
        match self {
            Segment::Stream(stream) => { /* 现有的快照逻辑 */ },
            other => other.to_value(),
        }
    }
}
```

**设计要点**：
- `to_value()` 对 Stream panic，强制开发者意识到这是有损操作
- 所有需要处理 Stream 的地方显式调用 `snapshot_to_value()`
- 或者更柔和的方案：`to_value()` 返回 `Result<Value, StreamNotResolved>`，但这会改动所有调用点

**更柔和的替代方案**：保持 `to_value()` 行为不变，但在 Stream 分支添加 `tracing::warn!`，开发期提醒。

**影响范围**：
- 所有 `segment.to_value()` 调用点需审查是否可能遇到 Stream
- Dispatcher 中的输出转换、事件发送等处需改为 `snapshot_to_value()`

---

### 7. 条件比较类型不兼容时静默返回 false

**现状**（`src/evaluator/condition.rs:79-114`）：

```rust
ComparisonOperator::GreaterThan => {
    match (actual.as_f64(), value_to_f64(expected)) {
        (Some(a), Some(b)) => a > b,
        _ => false,  // 字符串和数字比较？静默 false
    }
}
```

**问题**：
- 用户在 DSL 中配置 `if x > 5`，如果 `x` 是字符串 `"abc"`，条件静默返回 false
- 用户不知道原因，可能花很长时间调试"为什么走了错误分支"
- 这是"静默错误比报错更危险"的经典案例

**方案：返回 `Result`，Dispatcher 层决定策略**

```rust
/// 条件评估结果
pub enum ConditionResult {
    /// 条件成立
    True,
    /// 条件不成立
    False,
    /// 类型不兼容，无法比较
    TypeMismatch {
        actual_type: &'static str,
        expected_type: &'static str,
        operator: ComparisonOperator,
    },
}

pub async fn evaluate_condition(cond: &Condition, pool: &VariablePool) -> ConditionResult {
    // 数值比较
    ComparisonOperator::GreaterThan => {
        match (actual.as_f64(), value_to_f64(expected)) {
            (Some(a), Some(b)) => if a > b { True } else { False },
            _ => TypeMismatch {
                actual_type: actual.type_name(),
                expected_type: "number",
                operator: ComparisonOperator::GreaterThan,
            },
        }
    }
}
```

**在 If-else 节点中：**
- `TypeMismatch` 默认视为 false（保持兼容）
- 但通过事件总线发送 warning event，日志可见
- 如果开启 strict 模式（security feature），TypeMismatch 直接报错

**影响范围**：
- `evaluate_condition` 返回类型变更
- `evaluate_case` / `evaluate_cases` 需处理 `ConditionResult`
- If-else 节点的条件评估调用需适配

---

### 8. `process_data` 字段用途不明

**现状**（`src/dsl/schema.rs:617`）：

```rust
pub process_data: HashMap<String, Value>,
```

**审查发现**：通过全局搜索，`process_data` **从未被任何节点实际写入**。仅在 `NodeRunResult` 定义和 Default impl 中出现。

**方案：删除**

直接从 `NodeRunResult` 中删除 `process_data` 字段。

**理由**：
- 没有发布过版本，没有兼容性负担
- 字段从未被使用，属于 Dify 结构的残留
- 如果未来需要"中间处理数据"语义，可以明确定义后再添加

**影响范围**：
- `NodeRunResult` 结构体
- `Default for NodeRunResult` 实现
- 可能有的序列化测试

---

### 9. 两套插件系统共存

**现状**：
- `src/plugin/`：旧系统，基于 WASM manifest，`PluginManager`
- `src/plugin_system/`：新系统，trait 基础，两阶段加载
- 通过 `#[cfg(feature = "plugin-system")]` 条件编译切换
- Dispatcher 中约 10+ 处 `#[cfg]` 分支

**方案：删除旧插件系统**

1. 删除 `src/plugin/` 目录
2. 将 `plugin-system` feature 改为默认启用，或完全移除条件编译
3. 清理 Dispatcher 中的 `#[cfg(not(feature = "plugin-system"))]` 分支
4. 清理 `NodeExecutorRegistry::new_with_plugins` 方法

**理由**：
- 没有发布过版本
- 两套系统共存只会增加理解成本
- 新系统功能覆盖旧系统

**影响范围**：
- `src/plugin/` 目录删除
- `src/core/dispatcher.rs` 中所有 `#[cfg(not(feature = "plugin-system"))]` 分支删除
- `src/nodes/executor.rs` 中 `new_with_plugins` 删除
- `Cargo.toml` 中 feature 定义调整
- 相关测试代码

---

## 三、低严重度问题

### 10. `NodeState` 实为 `EdgeState`

**现状**（`src/graph/types.rs:8-13`）：

```rust
pub enum NodeState {
    Unknown, Taken, Skipped,
}
```

这个枚举同时用于 `GraphNode.state` 和 `GraphEdge.state`，但语义是**边的遍历状态**（一条边被"走过"或"跳过"），不是节点的执行状态。

**方案：重命名**

```rust
pub enum EdgeTraversalState {
    /// 尚未决定（执行未到达此边/节点）
    Pending,
    /// 被选中走过
    Taken,
    /// 被跳过（分支未选中）
    Skipped,
}
```

- 同时重命名 `GraphNode.state` 和 `GraphEdge.state` 字段类型
- `Unknown` → `Pending` 更准确表达"尚未处理"

**影响范围**：所有引用 `NodeState` 的代码（types.rs、dispatcher.rs 中的状态判断）。

---

### 11. `NodeRunResult::default()` 默认是 `Succeeded`

**现状**：

```rust
impl Default for NodeRunResult {
    fn default() -> Self {
        NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,  // ← 默认成功
            ...
        }
    }
}
```

**问题**：如果节点实现者用 `..Default::default()` 且忘记设置 status，会默认报告成功。

**方案：改默认值为 `Pending`**

```rust
status: WorkflowNodeExecutionStatus::Pending,
```

**理由**：
- `Pending` 是中性状态，不会误导
- 如果节点忘记设置 status，Dispatcher 可以检测到"节点返回了 Pending 状态"并报错
- 现有节点已经显式设置 `Succeeded`，不受影响

---

### 12. StubExecutor 静默成功

**现状**（`src/nodes/executor.rs:183-208`）：

```rust
impl NodeExecutor for StubExecutor {
    async fn execute(...) -> Result<NodeRunResult, NodeError> {
        Ok(NodeRunResult {
            outputs: { m.insert("text", "[Stub: ... not implemented]"); m },
            ..Default::default()  // status = Succeeded
        })
    }
}
```

**问题**：未实现的节点类型在生产环境中静默"成功"，输出占位文本。

**方案：返回 `Exception` 状态**

```rust
Ok(NodeRunResult {
    status: WorkflowNodeExecutionStatus::Exception,
    outputs: { ... },
    error: Some(format!("Node type '{}' is not implemented", self.0)),
    ..Default::default()
})
```

或者更激进：直接返回 `Err(NodeError::ExecutionError(...))`。但 Exception 更温和，允许 error_strategy 处理。

---

### 13. 模板变量缺失时静默为空字符串

**现状**：Dify 风格模板 `{{#node_id.var_name#}}`，变量不存在时替换为空字符串。

**方案：保持默认行为，添加 strict 模式**

- 默认行为不变（Dify 兼容）
- 添加 `EngineConfig.strict_template: bool`
- strict 模式下，缺失变量返回 `NodeError::VariableNotFound`
- 非 strict 模式下，发送 warning event（通过事件总线）

**理由**：这是 Dify 的行为设计，改变默认行为会破坏 DSL 兼容性。但提供 strict 模式让需要严格检查的用户可以启用。

---

### 14. `RuntimeContext` 大量 `Option` 字段

**现状**（`src/core/runtime_context.rs:10-27`）：

```rust
pub struct RuntimeContext {
    pub time_provider: Arc<dyn TimeProvider>,       // 必须
    pub id_generator: Arc<dyn IdGenerator>,         // 必须
    pub event_tx: Option<...>,                      // 可选
    pub sub_graph_runner: Option<...>,              // 可选
    pub template_functions: Option<...>,            // cfg(plugin-system)
    pub resource_group: Option<...>,                // cfg(security)
    pub security_policy: Option<...>,               // cfg(security)
    pub resource_governor: Option<...>,             // cfg(security)
    pub credential_provider: Option<...>,           // cfg(security)
    pub audit_logger: Option<...>,                  // cfg(security)
}
```

**问题**：调用者不知道哪些是必须的，哪些在什么条件下存在。

**方案：分层 + Builder**

```rust
/// 核心运行时上下文（必须提供）
pub struct RuntimeContext {
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub extensions: RuntimeExtensions,
}

/// 可选扩展（按需提供）
#[derive(Default)]
pub struct RuntimeExtensions {
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    pub sub_graph_runner: Option<Arc<dyn SubGraphRunner>>,
    #[cfg(feature = "plugin-system")]
    pub template_functions: Option<Arc<HashMap<String, Arc<dyn TemplateFunction>>>>,
    #[cfg(feature = "security")]
    pub security: Option<SecurityContext>,
}

/// 安全相关上下文（要么全有要么全无）
#[cfg(feature = "security")]
pub struct SecurityContext {
    pub resource_group: ResourceGroup,
    pub security_policy: SecurityPolicy,
    pub resource_governor: Arc<dyn ResourceGovernor>,
    pub credential_provider: Option<Arc<dyn CredentialProvider>>,
    pub audit_logger: Option<Arc<dyn AuditLogger>>,
}
```

**设计要点**：
- 必需字段不是 Option（`time_provider`、`id_generator`）
- 可选扩展集中在 `RuntimeExtensions`，名字表明这些是可选的
- Security 相关字段打包成 `SecurityContext`，不再散落——如果启用安全特性，至少需要 `resource_group` 和 `security_policy`

---

## 四、实施顺序建议

| 阶段 | 改动项 | 理由 |
|------|--------|------|
| 1 | #8 删除 process_data | 最简单，零风险 |
| 1 | #9 删除旧插件系统 | 减少代码量，降低后续改动复杂度 |
| 1 | #10 重命名 NodeState | 纯重命名，机械操作 |
| 1 | #11 改 Default 为 Pending | 一行改动 |
| 1 | #12 StubExecutor 改为 Exception | 一行改动 |
| 2 | #2+#5 引入 Selector newtype | 影响面广但改动模式统一 |
| 2 | #4 类型化 EdgeHandle | 和 Selector 类似的 newtype 改造 |
| 3 | #1 移除数组自动推断 | 需要仔细验证下游影响 |
| 3 | #3 统一 NodeOutputs | 所有节点输出逻辑需调整 |
| 4 | #6 Segment::to_value 流处理 | 需审查所有调用点 |
| 4 | #7 条件比较 TypeMismatch | 需要 ConditionResult 新类型 |
| 4 | #13 strict 模板模式 | 新增功能 |
| 4 | #14 RuntimeContext 分层 | 影响所有构造 RuntimeContext 的地方 |
