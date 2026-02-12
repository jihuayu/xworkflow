# 消除 Value↔Segment 双重转换设计

## 1. 问题分析

### 当前转换链路

每个节点执行时，变量经历以下转换往返：

```
读取阶段:
  VariablePool (Segment) → pool.get() → Segment
  → executor 内部调用 .to_value() / .into_value() → Value
  → executor 使用 Value 进行业务处理

写回阶段:
  executor 返回 NodeRunResult { outputs: HashMap<String, Value> }
  → dispatcher 调用 pool.set_node_outputs()
  → Segment::from_value(&value) 递归转换每个 Value → Segment
  → im::HashMap::insert()
```

**核心问题**：节点输入输出强制使用 `serde_json::Value`，而池存储使用 `Segment`，导致每个变量至少经历 2 次类型转换。

### 转换成本分析

`Segment::from_value()` 实现（`src/core/variable_pool.rs:854-890`）：

```rust
pub fn from_value(v: &Value) -> Self {
    match v {
        Value::Null => Segment::None,
        Value::Bool(b) => Segment::Boolean(*b),          // 零成本
        Value::Number(n) => { /* int/float 分支判断 */ }, // 极低成本
        Value::String(s) => Segment::String(s.clone()),   // String clone
        Value::Array(arr) => {
            // 递归遍历整个数组，类型推断 (all strings? → ArrayString)
            // 每个元素递归调用 from_value
            // Arc::new() 包装
        },
        Value::Object(map) => {
            // 递归遍历整个 map
            // 每个 key/value 递归调用
            // Arc::new() 包装
        },
    }
}
```

**耗时估算**：
- 原始类型（bool/int/float）：~5ns — 可忽略
- String：~20-100ns — 取决于长度（需 clone）
- 小对象（3-5 字段）：~200-500ns — 递归 + Arc 分配
- 大嵌套对象（HTTP 响应体）：~1-100μs — 递归深遍历

### 各节点的转换模式

| 节点类型 | 读 (Segment→Value) | 写 (Value→Segment) | 备注 |
|---------|----|----|------|
| Start | `val.to_value()` | 全部 outputs 转换 | 输出 sys.query 等 |
| End | `val.to_value()` | 全部 outputs 转换 | 收集最终输出 |
| Answer | `val.to_display_string()` | outputs 转换 | 模板渲染，可能含流式 |
| Code | `val.into_value()` | sandbox 返回 Value | **双重转换最严重** |
| HTTP | `val.into_value()` | 响应 body/status/headers | 大对象转换 |
| Template | `val.into_value()` | `Value::String(rendered)` | 单输出 |
| If-Else | condition eval | `selected_case` Value | 输出简单 |
| Aggregator | `val.to_value()` | 转发值 | 1 个输出 |

### 发现：已有但未使用的方法

`src/core/variable_pool.rs:1167-1173` 已实现 `set_node_segment_outputs()`：

```rust
pub fn set_node_segment_outputs(&mut self, node_id: &str, outputs: &HashMap<String, Segment>) {
    for (key, val) in outputs {
        self.variables.insert(Self::make_key(node_id, key), val.clone());
    }
}
```

但 dispatcher 从未调用此方法，始终使用 `set_node_outputs()` 的 Value 版本。

---

## 2. 设计方案

### 2.1 核心思路

**目标**：将整条数据通路从 Value 统一为 Segment，彻底消除双重转换。

**策略**：既然项目尚未发布，直接修改 `NodeOutputs`、`NodeRunResult`、`NodeExecutor` trait，将输出类型从 `Value` 改为 `Segment`。一步到位，无需兼容层。

### 2.2 NodeOutputs 统一为 Segment

当前定义（`src/dsl/schema.rs`）：

```rust
pub enum NodeOutputs {
    Sync(HashMap<String, Value>),
    Stream {
        ready: HashMap<String, Value>,
        streams: HashMap<String, SegmentStream>,
    },
}
```

直接改为：

```rust
pub enum NodeOutputs {
    Sync(HashMap<String, Segment>),
    Stream {
        ready: HashMap<String, Segment>,
        streams: HashMap<String, SegmentStream>,
    },
}
```

**只有 2 个 variant，无枚举膨胀**。Value 从数据通路中完全消失。

### 2.3 NodeRunResult 适配

```rust
pub struct NodeRunResult {
    pub status: WorkflowNodeExecutionStatus,
    pub inputs: HashMap<String, Segment>,    // 改 Value → Segment
    pub outputs: NodeOutputs,                // 已改为 Segment
    pub metadata: HashMap<String, Value>,    // 保留 Value（元数据无需频繁转换）
    pub llm_usage: Option<LlmUsage>,
    pub edge_source_handle: EdgeHandle,
    pub error: Option<NodeErrorInfo>,
    pub retry_index: i32,
}
```

**注意**：`metadata` 保留 `Value` 类型——元数据是低频访问的描述性信息（如耗时、模型名称），不进入变量池，无转换开销。

### 2.4 Dispatcher 简化

`src/core/dispatcher.rs:362-416` 的 `handle_node_success` 大幅简化：

```rust
// 当前代码（Value → Segment 转换）:
let (mut outputs_for_write, stream_outputs) = result.outputs.clone().into_parts();
pool.set_node_outputs(node_id, &outputs_for_write);  // 内部调用 Segment::from_value

// 改为（直接写入，零转换）:
let (outputs, stream_outputs) = result.outputs.clone().into_parts();
pool.set_node_segment_outputs(node_id, &outputs);  // 已有方法，直接 Segment clone
for (key, stream) in stream_outputs {
    let selector = Selector::new(node_id.to_string(), key);
    pool.set(&selector, Segment::Stream(stream));
}
```

可以删除 `set_node_outputs(&self, ..., &HashMap<String, Value>)` 方法，只保留 `set_node_segment_outputs`。

### 2.5 before_variable_write_hooks 改造

当前 hooks 接收 `&mut Value`：

```rust
async fn apply_before_variable_write_hooks(
    &self, node_id: &str, selector: &Selector, value: &mut Value
) -> WorkflowResult<()>
```

直接改为接收 `&mut Segment`：

```rust
async fn apply_before_variable_write_hooks(
    &self, node_id: &str, selector: &Selector, value: &mut Segment
) -> WorkflowResult<()>
```

Hook 实现内部如果需要 Value（如 JSON 序列化），自行调用 `segment.to_value()`。这将转换从热路径推迟到按需调用。

### 2.6 Event 回调适配

当前事件包含完整 `NodeRunResult`：

```rust
GraphEngineEvent::NodeRunSucceeded {
    node_run_result: result.clone(),  // 包含 outputs
}
```

outputs 现在是 `HashMap<String, Segment>`。事件消费端如果需要 JSON 序列化，使用工具方法：

```rust
impl NodeOutputs {
    /// 按需转为 Value（仅序列化/调试时使用）
    pub fn to_value_map(&self) -> HashMap<String, Value> {
        match self {
            NodeOutputs::Sync(m) => {
                m.iter().map(|(k, v)| (k.clone(), v.to_value())).collect()
            }
            NodeOutputs::Stream { ready, .. } => {
                ready.iter().map(|(k, v)| (k.clone(), v.to_value())).collect()
            }
        }
    }
}
```

### 2.7 各节点改造

所有内置节点一次性迁移。按改动大小分类：

#### 改动极小的节点（直接输出 Segment）

| 节点 | 改动 |
|------|------|
| **Start** | `pool.get()` 已返回 Segment → 直接放入 outputs，删除 `.to_value()` |
| **End** | `pool.get_resolved()` 已返回 Segment → 直接放入 outputs |
| **Aggregator** | `pool.get()` 返回 Segment → 直接传递 |
| **If-Else** | 条件求值直接操作 Segment，`selected_case` 改为 `Segment::String` |
| **Assigner** | 输出 `Segment`，`write_mode`/`assigned_variable_selector` 改为 Segment |

这些节点当前的模式是 `pool.get() → .to_value() → outputs.insert()`，改为 `pool.get() → outputs.insert()`，删一行代码即可。

#### 改动中等的节点

| 节点 | 改动 |
|------|------|
| **Template** | 渲染结果 `String` → `Segment::String(rendered)` 直接输出 |
| **HTTP** | 响应 body `String` → `Segment::String`，status `i64` → `Segment::Integer`，headers → `Segment::Object` |
| **Answer** | 模板渲染输出 → `Segment::String`，流式部分已经是 `SegmentStream` |

#### 改动最大的节点

| 节点 | 改动 |
|------|------|
| **Code** | sandbox 通过 `SandboxResult` 返回 `Value`，需要在 executor 内部转换一次：`Segment::from_value(&sandbox_result.output)`。但这只是**单点转换**，不再有回程的 `from_value` |

**Code 节点的转换不可完全消除**——JS/WASM sandbox 的 FFI 边界天然使用 JSON Value。但优化后从 2 次转换减为 1 次（sandbox → Segment，不再经过 dispatcher 的 Value → Segment）。

### 2.8 NodeExecutor Trait 改造

```rust
/// 改造前
#[async_trait]
pub trait NodeExecutor: Send + Sync {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,            // config 保持 Value（DSL 配置数据）
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError>;
}
```

trait 签名本身不需要改——`NodeRunResult` 的内部类型变了，但函数签名不变。config 参数保持 `&Value`，因为节点配置来自 DSL JSON 解析，使用 Value 是合理的。

**插件系统影响**：`NodeExecutor` trait 的返回类型 `NodeRunResult` 内部从 `HashMap<String, Value>` 变为 `HashMap<String, Segment>`。所有实现此 trait 的插件需要适配。但由于 `Segment` 已经在 `xworkflow-types` crate 中导出，插件可以直接使用。

### 2.9 删除废弃代码

统一后可以清理的代码：

```
删除: VariablePool::set_node_outputs(&mut self, node_id: &str, outputs: &HashMap<String, Value>)
保留: VariablePool::set_node_segment_outputs() → 重命名为 set_node_outputs()
删除: NodeOutputs::into_parts() 中的 Value 相关逻辑
删除: dispatcher 中 outputs_for_write 的 Value 处理分支
```

---

## 3. 迁移策略

由于没有兼容性约束，推荐**一次性迁移**（single PR），避免中间状态：

### 步骤 1：修改核心类型
1. `NodeOutputs` 的 `Sync` / `Stream` variant 改为 `HashMap<String, Segment>`
2. `NodeRunResult.inputs` 改为 `HashMap<String, Segment>`
3. 添加 `NodeOutputs::to_value_map()` 工具方法
4. 编译——此时所有节点 executor 都会报错，这是预期的

### 步骤 2：批量修改所有节点 executor
1. `control_flow.rs`：Start、End、Answer、If-Else
2. `data_transform.rs`：Template、Aggregator、Assigner、HTTP、Code
3. `subgraph_nodes.rs`：Iteration、Loop、ListOperator
4. `document_extract.rs`：DocumentExtractor
5. `llm/executor.rs`：LLM 节点（如有）

### 步骤 3：修改 Dispatcher
1. `handle_node_success` 改用 `set_node_segment_outputs`
2. `before_variable_write_hooks` 改为接收 `&mut Segment`
3. 删除 `set_node_outputs` 的 Value 版本

### 步骤 4：修改事件系统
1. 事件消费端使用 `to_value_map()` 获取 JSON 表示
2. 确保 Segment 实现必要的 `Serialize`（如果事件需要序列化）

### 步骤 5：编译 + 测试
1. `cargo build` 确保编译通过
2. 运行全部单元测试和集成测试
3. benchmark 对比转换前后性能

---

## 4. 预期收益

### CPU 节省

| 场景 | 当前转换次数 | 优化后 | 节省 |
|------|-----------|-------|------|
| 10 节点简单工作流 | ~60 次 | ~5 次（仅 Code sandbox FFI） | ~92% |
| 20 节点含 HTTP | ~120 次 | ~10 次 | ~92% |
| 含大 JSON 对象 | ~20μs/节点 | ~1μs/节点（仅 metadata） | ~95% |

与之前"枚举扩展"方案相比，一次性迁移消除了更多转换（包括读取端的 `to_value()`），收益从 ~67% 提升到 ~92%。

### 内存节省

- 消除所有中间 `Value` 的临时分配
- 节点内部直接操作 `Segment`，无 `HashMap<String, Value>` 的中间 map
- 对大对象和长字符串效果最明显

### 代码清晰度

- 数据通路统一：Segment 从池到节点到池，无类型切换
- 删除 ~50 行转换代码（各节点的 `.to_value()` / `Segment::from_value()`）
- `set_node_outputs` 方法只有一个版本

---

## 5. 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| 一次性改动量大 | 改错导致回归 | 全量测试覆盖 + 单 PR 原子提交 |
| 事件系统序列化 | Segment 不直接 Serialize | 添加 `to_value_map()` 工具方法 |
| Hook 签名变更 | 依赖 Value 的 hook 需改造 | 统一改为 `&mut Segment` |
| 插件 API 变更 | 外部插件需适配 | 未发布，无影响；文档说明 |
| sandbox FFI 边界 | Code 节点仍需 1 次转换 | 无法消除，但从 2 次减为 1 次 |

---

## 6. 关键文件

| 文件 | 改动 |
|------|------|
| `src/dsl/schema.rs` | `NodeOutputs` / `NodeRunResult` 类型改为 Segment |
| `src/core/dispatcher.rs` | `handle_node_success` 简化，hook 签名变更 |
| `src/core/variable_pool.rs` | 删除 `set_node_outputs` Value 版本，重命名 segment 版本 |
| `src/nodes/control_flow.rs` | Start/End/Answer/If-Else 全部迁移 |
| `src/nodes/data_transform.rs` | Template/Code/HTTP/Aggregator/Assigner 全部迁移 |
| `src/nodes/subgraph_nodes.rs` | Iteration/Loop/ListOperator 迁移 |
| `src/nodes/executor.rs` | trait 签名不变，但 `StubExecutor` 输出改为 Segment |
