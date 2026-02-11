# 变量类型系统梳理与设计

## Context

xworkflow 中节点之间通过 **变量池 (VariablePool)** 传递数据。当前变量类型系统以 `Segment` 枚举为核心，配合 `serde_json::Value` 在节点执行边界处做转换。本文档全面梳理现有类型系统，分析存在的问题，并给出改进方案。

---

## 一、现状梳理

### 1.1 类型表示：双类型体系

系统中存在两套并行的类型表示：

| 层 | 类型 | 用途 | 所在文件 |
|----|------|------|---------|
| 内部存储 | `Segment` | VariablePool 中的存储类型 | `src/core/variable_pool.rs` |
| 节点 I/O | `serde_json::Value` | NodeRunResult.outputs、config 传参、沙箱输入输出 | 散布在各节点执行器 |

每次节点执行时，变量要经历 `Segment → Value → (节点处理) → Value → Segment` 的往返转换。

### 1.2 Segment 枚举（当前定义）

```rust
pub enum Segment {
    None,                                      // 空值
    String(String),                            // 字符串
    Integer(i64),                              // 整数
    Float(f64),                                // 浮点数
    Boolean(bool),                             // 布尔值
    Object(HashMap<String, Segment>),          // 对象（嵌套 Segment）
    ArrayString(Vec<String>),                  // 字符串数组
    ArrayInteger(Vec<i64>),                    // 整数数组
    ArrayFloat(Vec<f64>),                      // 浮点数数组
    ArrayObject(Vec<HashMap<String, Segment>>),// 对象数组
    ArrayAny(Vec<Segment>),                    // 混合类型数组
    File(FileSegment),                         // 文件
    ArrayFile(Vec<FileSegment>),               // 文件数组
}
```

### 1.3 DSL 中声明的类型（`StartVariable.var_type`）

Start 节点的输入变量声明了类型字符串：

```yaml
variables:
  - variable: query
    type: string
  - variable: count
    type: number
  - variable: data
    type: object
  - variable: tags
    type: array[string]
  - variable: doc
    type: file
```

合法类型列表：`string`、`number`、`object`、`array[string]`、`array[number]`、`array[object]`、`file`、`array[file]`。

### 1.4 VariablePool 存储模型

```rust
pub struct VariablePool {
    variables: HashMap<(String, String), Segment>,
}
```

- **Key**：`(node_id, variable_name)` 二元组
- **Value**：`Segment`
- **特殊命名空间**：`"sys"`（系统变量）、`"env"`（环境变量）、`"conversation"`（对话变量）、`"__scope__"`（作用域变量）

### 1.5 变量在节点间的流转

```
                     ┌─────────────────────────────────────────┐
                     │           VariablePool                  │
                     │  key: (node_id, var_name) → Segment     │
                     └──────┬────────────────────────┬─────────┘
                            │ read                   │ write
                            ▼                        │
                   ┌─────────────────┐               │
                   │  pool.get()     │               │
                   │  → Segment      │               │
                   └────────┬────────┘               │
                            │ .to_value()            │
                            ▼                        │
┌───────────────────────────────────────────┐        │
│           Node Executor                   │        │
│  输入: &Value (config) + &VariablePool    │        │
│  输出: NodeRunResult {                    │        │
│     outputs: HashMap<String, Value>       │        │
│  }                                        │        │
└────────────────────┬──────────────────────┘        │
                     │                               │
                     ▼                               │
            ┌─────────────────┐                      │
            │  Dispatcher     │                      │
            │  Value →        │ Segment::from_value()│
            │  set_node_      ├──────────────────────┘
            │  outputs()      │
            └─────────────────┘
```

### 1.6 各节点类型的变量输入/输出

| 节点类型 | 输入变量 | 处理方式 | 输出变量 |
|---------|---------|---------|---------|
| **start** | 用户输入（预设到 pool） | 直接读取，Segment→Value | 各 StartVariable + sys.query + sys.files |
| **end** | 通过 value_selector 从 pool 读取 | Segment→Value 直接透传 | 各 OutputVariable（Value） |
| **answer** | 通过 `{{#node.var#}}` 模板引用 | Segment→`to_display_string()`→字符串拼接 | answer: String |
| **if-else** | 通过 variable_selector 从 pool 读取 | Segment 直接用于条件评估 | selected_case: String |
| **template-transform** | 通过 VariableMapping 读取 | Segment→Value→Jinja2 渲染 | output: String |
| **variable-aggregator** | 多个 selector，取第一个非 null | Segment→Value 透传 | output: Value（保留原类型） |
| **variable-assigner** | input_variable_selector 读取 | Segment→Value，由 dispatcher 执行写入 | output + write_mode + assigned_selector |
| **code** | VariableMapping 或 inputs map | Segment→Value→沙箱（JS/WASM） | 沙箱返回的 Value（任意类型） |
| **http-request** | URL/headers/body 模板引用 | Segment→`to_display_string()`→模板替换 | status_code: Number, body: String, headers: String |
| **iteration** | iterator_selector 读取数组 | Segment→Value→子图（item+index） | output_variable: Array |
| **loop** | initial_vars 初始化 | Value→子图循环执行 | output_variable: Object, _iterations: Number |
| **list-operator** | input_selector 读取数组 | Value 上执行 filter/map/sort 等 | output_variable: Value |

---

## 二、现存问题分析

### 问题 1：Segment 变体膨胀，大量变体无运行时意义（严重）

当前 Segment 有 13 个变体，但代码分析表明，其中 5 个变体是**死代码**：

| 变体 | 定义处以外的引用 | `from_value()` 创建 | 任何节点/评估器特殊处理 | 结论 |
|------|-----------------|---------------------|----------------------|------|
| `ArrayInteger` | 无 | ❌ 从不创建 | ❌ | **死代码** |
| `ArrayFloat` | 无 | ❌ 从不创建 | ❌ | **死代码** |
| `ArrayObject` | 无 | ❌ 从不创建 | ❌ | **死代码** |
| `File` | 无 | ❌ 从不创建 | ❌ | **死代码** |
| `ArrayFile` | 无 | ❌ 从不创建 | ❌ | **死代码** |
| `ArrayString` | condition.rs (eval_contains, eval_all_of), llm/executor.rs | ❌ 从不创建 | ✅ 有专用路径 | **保留** |

**根本原因**：`from_value()` 对数组始终返回 `ArrayAny`，对 Object 无法识别 File。这意味着变量只要经过一次节点处理（Segment→Value→Segment 往返），类型信息就丢失。

**对策**：与其在 `from_value()` 中添加复杂的类型推断逻辑来"修复"这些变体，更合理的做法是**直接移除它们**——它们从未在运行时提供过价值。

### 问题 2：`File`/`ArrayFile` 本质上是 Object 的类型别名（严重）

`FileSegment` 只是一个约定了字段结构的结构体（id, url, filename, mime_type 等），代码分析表明：

- **没有任何节点执行器**对 File 做特殊处理
- **条件评估器**不处理 File
- **模板引擎**不处理 File
- **LLM executor** 的 `extract_image_urls()` 对 File 返回空数组（不处理）
- **往返转换已损坏**：`File → to_value() → Value::Object → from_value() → Object`
- **没有任何测试**用到 File/ArrayFile
- 相关功能（Document Extractor、Vision、Tool 文件收集）均**未实现**

File 类型在 Segment 层面完全可以用 Object 替代。文件语义通过 DSL 层 `var_type: "file"` 声明即可，不需要运行时类型区分。

### 问题 3：DSL 声明类型与运行时类型完全脱节（中等）

`StartVariable.var_type` 是一个自由字符串（如 `"string"`、`"number"`），但：
- 运行时**从未校验**传入的值是否匹配声明类型
- `Segment` 枚举中区分 `Integer` 和 `Float`，但 DSL 中只有 `number`
- 没有机制从 `var_type` 字符串推断或强制约束 `Segment` 变体

### 问题 4：条件评估中类型不匹配静默返回 false（中等）

```rust
// src/evaluator/condition.rs:66-70
ComparisonOperator::Equal => {
    match (actual.as_f64(), value_to_f64(expected)) {
        (Some(a), Some(b)) => (a - b).abs() < f64::EPSILON,
        _ => false,  // ← 类型不兼容时静默返回 false
    }
}
```

**影响**：
- `Integer(42)` 和 `String("42")` 用 `Equal` 比较 → true（都转为 f64）
- `String("hello")` 和 `Number(42)` 用 `Equal` 比较 → false（String 无法 parse 为 f64）
- `Boolean(true)` 和 `Number(1)` 用 `Equal` 比较 → false（Boolean 的 `as_f64()` 返回 None）
- 用户不会收到任何警告，条件分支走向可能不符合预期

### 问题 5：`Is` 运算符全部退化为字符串比较（低）

```rust
ComparisonOperator::Is => {
    actual.to_display_string() == value_to_string(expected)
}
```

- `Integer(42)` Is `"42"` → true
- `Boolean(true)` Is `"true"` → true
- `Float(3.14)` Is `"3.14"` → true（但 `Float(0.1 + 0.2)` Is `"0.3"` → false）

这是 Dify 的兼容行为，本身不一定需要改，但应在文档中明确。

### 问题 6：`Segment` 未实现 `PartialEq`（低）

无法直接比较两个 Segment 是否相等。当前条件评估器通过各种转换间接实现比较，但如果要做变量变更检测（如 Loop 的终止条件）就不方便。

### 问题 7：Append 类型提升逻辑复杂且不可预测（低）

```rust
// append() 的行为取决于已有值的类型：
ArrayAny + any       → push to ArrayAny
ArrayString + String → push to ArrayString
ArrayString + other  → 静默丢弃（不 push）  ← 意外行为！
String + any         → 字符串拼接
other + any          → 转为 ArrayAny([old, new])
```

当 `ArrayString` 接收到非 String 类型的 append 时，**值被静默丢弃**（不报错、不转型）。

### 问题 8：`Object.is_empty()` 返回 false（低）

```rust
pub fn is_empty(&self) -> bool {
    match self {
        Segment::None => true,
        Segment::String(s) => s.is_empty(),
        // ... 各种 Array
        _ => false,  // Object 和 File 永远不 empty
    }
}
```

空 Object `{}` 不被认为是 empty。

### 问题 9：`__scope__` 命名空间冲突风险（低）

子图执行中使用 `"__scope__"` 作为 node_id 前缀来存储作用域变量（如 `item`、`index`）。如果用户的工作流中恰好有 ID 为 `__scope__` 的节点，会产生冲突。

---

## 三、改进方案

### 结论：精简 Segment 枚举，移除无运行时意义的变体

核心架构（Segment 枚举 + VariablePool + 双类型体系）是合理的。问题在于 Segment 变体过多，大量变体从未在运行时被创建或特殊处理。通过精简枚举并修复相关逻辑即可解决。

### 3.1 精简 Segment 枚举

**改动前**（13 个变体）：

```rust
pub enum Segment {
    None, String, Integer, Float, Boolean, Object,
    ArrayString, ArrayInteger, ArrayFloat, ArrayObject, ArrayAny,
    File, ArrayFile,
}
```

**改动后**（9 个变体）：

```rust
pub enum Segment {
    None,                             // 空值
    String(String),                   // 字符串
    Integer(i64),                     // 整数
    Float(f64),                       // 浮点数
    Boolean(bool),                    // 布尔值
    Object(HashMap<String, Segment>), // 对象
    ArrayString(Vec<String>),         // 字符串数组（保留：条件评估器有专用路径）
    Array(Vec<Segment>),              // 通用数组（原 ArrayAny 重命名）
    Stream(SegmentStream),            // 异步流（用于 LLM 等节点的流式返回）
}
```

**移除的变体及理由**：

| 移除的变体 | 理由 |
|-----------|------|
| `File(FileSegment)` | 无运行时特殊处理，用 Object 表示即可，文件语义在 DSL 层通过 `var_type: "file"` 声明 |
| `ArrayFile(Vec<FileSegment>)` | 同上，用 Array 包含 Object 元素即可 |
| `ArrayObject(Vec<HashMap<String, Segment>>)` | `from_value()` 从不创建，无条件评估器或节点的特殊处理，用 Array 包含 Object 元素完全等价 |
| `ArrayInteger(Vec<i64>)` | `from_value()` 从不创建，无运行时特殊处理，用 Array 包含 Integer 元素即可 |
| `ArrayFloat(Vec<f64>)` | `from_value()` 从不创建，无运行时特殊处理，用 Array 包含 Float 元素即可 |

**保留 `ArrayString` 的理由**：

- 条件评估器 `eval_contains()` 对 `ArrayString` 有专用高效路径（直接字符串比较）
- 条件评估器 `eval_all_of()` 对 `ArrayString` 有专用路径
- LLM executor `extract_image_urls()` 对 `ArrayString` 有专用路径
- `append()` 对 `ArrayString` 有类型保持逻辑

**重命名 `ArrayAny` → `Array`**：作为唯一的通用数组类型，不再需要 "Any" 后缀来区分。

### 3.2 `FileSegment` 降级为辅助类型

`FileSegment` struct 保留，但不再是 Segment 的变体。改为提供辅助方法：

```rust
/// 文件元数据（辅助类型，不是 Segment 变体）
/// 用于构建/解析文件类型的 Object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSegment {
    pub id: Option<String>,
    pub tenant_id: String,
    pub transfer_method: String,
    pub url: Option<String>,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub extension: Option<String>,
    pub size: Option<i64>,
}

impl FileSegment {
    /// 将文件元数据转为 Segment::Object
    pub fn to_segment(&self) -> Segment {
        let val = serde_json::to_value(self).unwrap_or(Value::Object(Default::default()));
        Segment::from_value(&val)
    }

    /// 尝试从 Segment::Object 解析文件元数据
    pub fn from_segment(seg: &Segment) -> Option<Self> {
        let val = seg.to_value();
        serde_json::from_value::<FileSegment>(val).ok()
    }
}
```

这样：
- 需要创建文件变量时，用 `FileSegment::new(...).to_segment()` → 得到 `Segment::Object`
- 需要读取文件元数据时，用 `FileSegment::from_segment(&seg)` → 尝试解析
- DSL 层 `var_type: "file"` / `"array[file]"` 声明不变（用于前端 UI 和输入校验）

### 3.3 简化 `from_value()` 转换

移除变体后，`from_value()` 只需要处理 `ArrayString` 的推断：

```rust
pub fn from_value(v: &Value) -> Self {
    match v {
        Value::Null => Segment::None,
        Value::Bool(b) => Segment::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Segment::Integer(i)
            } else {
                Segment::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => Segment::String(s.clone()),
        Value::Array(arr) => {
            // 尝试推断 ArrayString（因为条件评估器有专用路径）
            let all_string = !arr.is_empty() && arr.iter().all(|v| v.is_string());
            if all_string {
                Segment::ArrayString(
                    arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                )
            } else {
                Segment::Array(arr.iter().map(Segment::from_value).collect())
            }
        }
        Value::Object(map) => {
            let m: HashMap<String, Segment> = map
                .iter()
                .map(|(k, v)| (k.clone(), Segment::from_value(v)))
                .collect();
            Segment::Object(m)
        }
    }
}
```

比原方案简单得多：不需要检测 File 特征字段，不需要推断 ArrayInteger/ArrayFloat/ArrayObject。

### 3.4 增加 `SegmentType` 枚举，对齐 DSL 声明类型

`SegmentType` 是面向 DSL 用户的类型系统，与 Segment（面向引擎内部）解耦。
SegmentType 保留 File 等变体，因为它们对应 DSL 的 `var_type` 声明，前端需要知道"这个输入是文件上传"。

```rust
/// 变量类型标记，对应 DSL 中 StartVariable 的 var_type 字符串
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentType {
    String,
    Number,         // 统一 Integer 和 Float
    Boolean,
    Object,
    File,           // DSL 声明用，运行时映射到 Object
    ArrayString,
    ArrayNumber,    // DSL 声明用，运行时映射到 Array
    ArrayObject,    // DSL 声明用，运行时映射到 Array
    ArrayFile,      // DSL 声明用，运行时映射到 Array
    Array,          // 通用数组
    Any,            // 未知或混合类型
}

impl SegmentType {
    /// 从 DSL var_type 字符串解析
    pub fn from_dsl_type(s: &str) -> Option<Self> {
        match s {
            "string" => Some(SegmentType::String),
            "number" => Some(SegmentType::Number),
            "boolean" => Some(SegmentType::Boolean),
            "object" => Some(SegmentType::Object),
            "array[string]" => Some(SegmentType::ArrayString),
            "array[number]" => Some(SegmentType::ArrayNumber),
            "array[object]" => Some(SegmentType::ArrayObject),
            "file" => Some(SegmentType::File),
            "array[file]" => Some(SegmentType::ArrayFile),
            _ => None,
        }
    }
}

impl Segment {
    /// 获取当前值的类型标记
    pub fn segment_type(&self) -> SegmentType {
        match self {
            Segment::None => SegmentType::Any,
            Segment::String(_) => SegmentType::String,
            Segment::Integer(_) | Segment::Float(_) => SegmentType::Number,
            Segment::Boolean(_) => SegmentType::Boolean,
            Segment::Object(_) => SegmentType::Object,
            Segment::ArrayString(_) => SegmentType::ArrayString,
            Segment::Array(_) => SegmentType::Array,
        }
    }

    /// 检查值是否匹配指定类型（宽松匹配）
    pub fn matches_type(&self, expected: &SegmentType) -> bool {
        match expected {
            SegmentType::Any => true,
            SegmentType::File => matches!(self, Segment::Object(_)),    // File 在运行时是 Object
            SegmentType::ArrayNumber | SegmentType::ArrayObject |
            SegmentType::ArrayFile | SegmentType::Array => {
                matches!(self, Segment::Array(_) | Segment::ArrayString(_))
            }
            _ => self.segment_type() == *expected,
        }
    }
}
```

### 3.5 为 Segment 实现 `PartialEq`

```rust
impl PartialEq for Segment {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Segment::None, Segment::None) => true,
            (Segment::String(a), Segment::String(b)) => a == b,
            (Segment::Integer(a), Segment::Integer(b)) => a == b,
            (Segment::Float(a), Segment::Float(b)) => (a - b).abs() < 1e-10,
            (Segment::Integer(a), Segment::Float(b)) | (Segment::Float(b), Segment::Integer(a)) => {
                (*a as f64 - b).abs() < 1e-10
            }
            (Segment::Boolean(a), Segment::Boolean(b)) => a == b,
            (Segment::ArrayString(a), Segment::ArrayString(b)) => a == b,
            // Object、Array 走 to_value() 比较
            _ => self.to_value() == other.to_value(),
        }
    }
}
```

### 3.6 修复 Append 类型安全

```rust
pub fn append(&mut self, selector: &[String], value: Segment) {
    // ...
    match existing {
        Segment::Array(arr) => arr.push(value),
        Segment::ArrayString(arr) => {
            if let Segment::String(s) = value {
                arr.push(s);
            } else {
                // 升级为 Array 而非静默丢弃
                let mut new_arr: Vec<Segment> = arr.drain(..).map(Segment::String).collect();
                new_arr.push(value);
                *existing = Segment::Array(new_arr);
            }
        }
        Segment::String(s) => {
            *s += &value.to_display_string();
        }
        _ => {
            let old = std::mem::replace(existing, Segment::None);
            *existing = Segment::Array(vec![old, value]);
        }
    }
}
```

### 3.7 增加 `Object.is_empty()` 支持

```rust
pub fn is_empty(&self) -> bool {
    match self {
        Segment::None => true,
        Segment::String(s) => s.is_empty(),
        Segment::Object(m) => m.is_empty(),
        Segment::ArrayString(v) => v.is_empty(),
        Segment::Array(v) => v.is_empty(),
        _ => false,
    }
}
```

### 3.8 新增 `Stream` 类型

#### 3.8.1 背景与动机

当前 LLM 节点的流式处理方式是：
1. 执行器内部通过 `mpsc` channel 接收 `StreamChunk`
2. 后台任务将每个 chunk 转发为 `GraphEngineEvent::NodeRunStreamChunk`
3. **节点执行完毕后**，将完整累积文本写入 `NodeRunResult.outputs`
4. 调度器将完整结果写入 VariablePool，下游节点才能读取

**问题**：流式数据只通过事件总线对外暴露，对工作流内部的下游节点不可见。下游节点（如 Answer）必须等待 LLM 全部完成才能开始工作。这导致：
- 无法实现流式 Answer 渲染（逐 token 推送给用户）
- 无法让下游节点在上游流进行中就开始处理
- 流是"旁路"数据，不在变量类型系统中，无法被 variable_selector 引用

**目标**：将 Stream 作为一等变量类型（Segment 的一个变体），使下游节点能拿着流变量异步读取数据块，直到流结束或报错。

**泛型性**：Stream 不限于文本流。每个 chunk 本身是 `Segment`，因此可以 emit 任意类型的数据——String、Integer、Float、Boolean、Object、Array 等。LLM 文本流只是最典型的场景，其他场景如：批量数据处理节点逐条 emit Object、实时传感器节点 emit Float 数值等。

#### 3.8.2 核心类型设计

```rust
/// 流中的事件
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 数据块，可以是任意 Segment 类型（String、Integer、Object 等）
    Chunk(Segment),
    /// 流正常结束，附带最终完整值（类型取决于具体场景）
    End(Segment),
    /// 流出错终止
    Error(String),
}

/// 流的当前状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStatus {
    /// 正在产生数据
    Running,
    /// 正常结束
    Completed,
    /// 出错终止
    Failed,
}

/// 共享的流内部状态
struct StreamState {
    /// 已接收的所有数据块
    chunks: Vec<Segment>,
    /// 流状态
    status: StreamStatus,
    /// 最终完整值（End 时设置）
    final_value: Option<Segment>,
    /// 错误信息（Error 时设置）
    error: Option<String>,
}

/// 异步流句柄（可安全 Clone、跨任务共享）
///
/// 类似 JS 的 AsyncGenerator：消费者可以异步逐个读取值，
/// 多个消费者共享同一份流数据（每个消费者维护独立的读取游标）。
#[derive(Clone)]
pub struct SegmentStream {
    /// 共享的流状态（Arc 使得 VariablePool clone 时共享同一个流）
    state: Arc<RwLock<StreamState>>,
    /// 新数据通知（生产者 emit 后唤醒所有等待中的消费者）
    notify: Arc<tokio::sync::Notify>,
}
```

**关键设计决策**：

| 决策 | 选择 | 理由 |
|------|------|------|
| 共享模型 | `Arc<RwLock<StreamState>>` | VariablePool 是 Clone 的，pool 快照需要共享同一个流实例 |
| 多消费者 | 每个消费者独立游标 | 分支后多个下游节点可以各自从头消费同一个流 |
| 通知机制 | `tokio::sync::Notify` | 轻量、适合一对多唤醒 |
| chunk 类型 | `Segment`（任意类型） | 不限于文本，支持 emit String/Integer/Float/Boolean/Object/Array 等任意 Segment |
| chunk 存储 | `Vec<Segment>` 追加 | 支持新消费者从头回放，不丢失历史数据 |
| 最终值 | `Option<Segment>` | 流结束时提供完整累积值，兼容非流式场景 |

#### 3.8.3 生产者 API（节点执行器使用）

```rust
/// 流的写入端（不可 Clone，确保单一生产者）
pub struct StreamWriter {
    state: Arc<RwLock<StreamState>>,
    notify: Arc<tokio::sync::Notify>,
}

impl SegmentStream {
    /// 创建一对 (stream, writer)
    /// stream 放入 VariablePool，writer 留在节点执行器中
    pub fn channel() -> (SegmentStream, StreamWriter) {
        let state = Arc::new(RwLock::new(StreamState {
            chunks: Vec::new(),
            status: StreamStatus::Running,
            final_value: None,
            error: None,
        }));
        let notify = Arc::new(tokio::sync::Notify::new());
        (
            SegmentStream { state: state.clone(), notify: notify.clone() },
            StreamWriter { state, notify },
        )
    }
}

impl StreamWriter {
    /// 发送一个数据块
    pub async fn emit(&self, chunk: Segment) {
        let mut state = self.state.write().await;
        if state.status != StreamStatus::Running {
            return; // 流已结束，忽略
        }
        state.chunks.push(chunk);
        drop(state);
        self.notify.notify_waiters();
    }

    /// 正常结束流，附带最终完整值
    pub async fn end(self, final_value: Segment) {
        let mut state = self.state.write().await;
        state.status = StreamStatus::Completed;
        state.final_value = Some(final_value);
        drop(state);
        self.notify.notify_waiters();
    }

    /// 错误终止流
    pub async fn error(self, err: String) {
        let mut state = self.state.write().await;
        state.status = StreamStatus::Failed;
        state.error = Some(err);
        drop(state);
        self.notify.notify_waiters();
    }
}
```

#### 3.8.4 消费者 API（下游节点使用）

```rust
/// 流的读取游标（每个消费者创建自己的 Reader）
pub struct StreamReader {
    stream: SegmentStream,
    /// 当前读到第几个 chunk
    cursor: usize,
}

impl SegmentStream {
    /// 创建一个读取游标（从头开始）
    pub fn reader(&self) -> StreamReader {
        StreamReader { stream: self.clone(), cursor: 0 }
    }

    /// 流是否已结束（完成或出错）
    pub async fn is_done(&self) -> bool {
        let state = self.state.read().await;
        state.status != StreamStatus::Running
    }

    /// 阻塞等待流结束，返回最终完整值
    /// 等价于 JS 中的 `let result = []; for await (const chunk of stream) result.push(chunk);`
    pub async fn collect(&self) -> Result<Segment, String> {
        loop {
            {
                let state = self.state.read().await;
                match &state.status {
                    StreamStatus::Completed => {
                        return Ok(state.final_value.clone().unwrap_or(Segment::None));
                    }
                    StreamStatus::Failed => {
                        return Err(state.error.clone().unwrap_or_default());
                    }
                    StreamStatus::Running => {}
                }
            }
            self.notify.notified().await;
        }
    }

    /// 获取已接收的所有块（非阻塞快照）
    pub async fn chunks(&self) -> Vec<Segment> {
        self.state.read().await.chunks.clone()
    }

    /// 获取流状态
    pub async fn status(&self) -> StreamStatus {
        self.state.read().await.status.clone()
    }
}

impl StreamReader {
    /// 异步读取下一个数据块（阻塞直到有新数据或流结束）
    /// 返回 None 表示流已结束（正常或出错）
    pub async fn next(&mut self) -> Option<StreamEvent> {
        loop {
            {
                let state = self.stream.state.read().await;
                // 有未读的 chunk
                if self.cursor < state.chunks.len() {
                    let chunk = state.chunks[self.cursor].clone();
                    self.cursor += 1;
                    return Some(StreamEvent::Chunk(chunk));
                }
                // 没有更多 chunk 了，检查是否结束
                match &state.status {
                    StreamStatus::Completed => {
                        return Some(StreamEvent::End(
                            state.final_value.clone().unwrap_or(Segment::None)
                        ));
                    }
                    StreamStatus::Failed => {
                        return Some(StreamEvent::Error(
                            state.error.clone().unwrap_or_default()
                        ));
                    }
                    StreamStatus::Running => {
                        // 等待新数据
                    }
                }
            }
            self.stream.notify.notified().await;
        }
    }
}
```

#### 3.8.5 与 VariablePool 的集成

**Clone 行为**：`SegmentStream` 内部是 `Arc`，clone 后指向同一个流实例。当调度器 clone VariablePool 快照给下游节点时，下游节点拿到的是**同一个流的句柄**，可以实时读到上游正在产生的数据。

**Serialize / Deserialize**：Stream 是运行时概念，无法序列化。

```rust
// Segment 的 serde 需要特殊处理 Stream 变体：
impl Serialize for Segment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Segment::Stream(s) => {
                // 序列化为当前已累积的值（尽力而为）
                // 如果流已结束，序列化最终值
                // 如果流正在运行，序列化已收到的 chunks 拼接
                // 这里用同步方式获取（try_read），避免在 serialize 中 await
                todo!("同步读取 StreamState")
            }
            other => { /* 其他变体正常序列化 */ }
        }
    }
}

// from_value() 不可能重建 Stream，返回 None 或 String
```

**to_value() 行为**：

| 流状态 | `to_value()` 结果 | 说明 |
|--------|------------------|------|
| Completed | `final_value.to_value()` | 流已完成，返回最终值 |
| Failed | `Value::Null` | 流出错，无有效值 |
| Running | `Value::Array(chunks.map(to_value))` | 尽力而为的快照，将已接收的 chunks 转为 Array |

**to_display_string() 行为**：

| 流状态 | 结果 | 说明 |
|--------|------|------|
| Completed | `final_value.to_display_string()` | 最终值的显示字符串 |
| Failed | `"[stream error: {msg}]"` | 错误提示 |
| Running | 各 chunk `to_display_string()` 拼接 | 尽力而为，适用于文本流场景；非文本 chunk 会 JSON 序列化 |

#### 3.8.6 LLM 节点集成示例

```rust
// LlmNodeExecutor::execute() 改造后的流程：
async fn execute(&self, node_id: &str, config: &Value,
    variable_pool: &VariablePool, context: &RuntimeContext
) -> Result<NodeRunResult, NodeError> {

    let request = self.build_request(config, variable_pool)?;

    if request.stream {
        // 创建流
        let (stream, writer) = SegmentStream::channel();

        // 后台任务：驱动 LLM 流式调用，逐 chunk 写入
        let provider = self.provider.clone();
        let event_tx = context.event_tx.clone();
        let node_id_clone = node_id.to_string();
        tokio::spawn(async move {
            let result = provider.chat_completion_stream_to_writer(
                request, &writer, event_tx.as_ref(), &node_id_clone
            ).await;
            match result {
                Ok(final_text) => {
                    writer.end(Segment::String(final_text)).await;
                }
                Err(e) => {
                    writer.error(e.to_string()).await;
                }
            }
        });

        // 立即返回，outputs 中包含 Stream
        let mut outputs = HashMap::new();
        // 注意：这里需要通过 stream_outputs 而非 Value-based outputs
        return Ok(NodeRunResult {
            stream_outputs: {
                let mut m = HashMap::new();
                m.insert("text".to_string(), stream);
                m
            },
            ..Default::default()
        });
    }

    // 非流式：原有逻辑，等待完成后返回完整文本
    // ...
}
```

#### 3.8.7 下游节点消费示例

**Answer 节点流式渲染**：

```rust
// AnswerNodeExecutor 检测到模板引用的变量是 Stream 时
let val = variable_pool.get(&selector);
match val {
    Segment::Stream(stream) => {
        // 逐 chunk 渲染，每收到新 chunk 就推送增量
        let mut reader = stream.reader();
        let mut accumulated = String::new();
        loop {
            match reader.next().await {
                Some(StreamEvent::Chunk(seg)) => {
                    let delta = seg.to_display_string();
                    accumulated.push_str(&delta);
                    // 通过事件总线发送增量
                    emit_answer_chunk(&event_tx, &accumulated, &delta).await;
                }
                Some(StreamEvent::End(_)) => break,
                Some(StreamEvent::Error(e)) => return Err(NodeError::from(e)),
                None => break,
            }
        }
        // 最终 answer 是完整文本
        outputs.insert("answer".to_string(), Value::String(accumulated));
    }
    other => {
        // 非流式：原有逻辑
        let rendered = render_template(template, variable_pool);
        outputs.insert("answer".to_string(), Value::String(rendered));
    }
}
```

**普通节点（如 End 节点）自动 collect**：

```rust
// 大多数节点不需要流式消费，自动等待流结束
let val = variable_pool.get(&selector);
let resolved = match val {
    Segment::Stream(s) => s.collect().await.unwrap_or(Segment::None),
    other => other,
};
```

**非文本流示例（批量数据处理）**：

```rust
// 某个自定义节点逐条 emit Object
let (stream, writer) = SegmentStream::channel();
tokio::spawn(async move {
    for record in records {
        // 每条记录是一个 Object
        writer.emit(Segment::Object(record)).await;
    }
    // 最终值是完整数组
    writer.end(Segment::Array(all_records)).await;
});

// 下游节点可以逐条处理，也可以 collect 等待全部完成
let mut reader = stream.reader();
while let Some(event) = reader.next().await {
    match event {
        StreamEvent::Chunk(Segment::Object(record)) => {
            // 逐条处理
        }
        StreamEvent::End(_) => break,
        _ => {}
    }
}
```

#### 3.8.8 NodeRunResult 改造

当前 `NodeRunResult.outputs` 是 `HashMap<String, Value>`，无法存放 Stream。新增一个字段：

```rust
#[derive(Debug, Clone)]
pub struct NodeRunResult {
    pub status: WorkflowNodeExecutionStatus,
    pub inputs: HashMap<String, Value>,
    pub process_data: HashMap<String, Value>,
    pub outputs: HashMap<String, Value>,          // 非流式输出（不变）
    pub stream_outputs: HashMap<String, SegmentStream>, // 流式输出（新增）
    pub metadata: HashMap<String, Value>,
    pub llm_usage: Option<LlmUsage>,
    pub edge_source_handle: String,
    pub error: Option<String>,
    pub error_type: Option<String>,
    pub retry_index: i32,
}
```

#### 3.8.9 调度器改造

调度器写入变量池时，需要同时处理两种输出：

```rust
// 写入普通输出
pool.set_node_outputs(&node_id, &outputs_for_write);

// 写入流式输出（直接作为 Segment::Stream 存储）
for (key, stream) in &result.stream_outputs {
    pool.set(
        &[node_id.clone(), key.clone()],
        Segment::Stream(stream.clone()),
    );
}
```

**调度器对流式节点的执行时序变化**：

```
当前（非流式）：                      改造后（流式）：

Node execute                          Node execute
  ├─ 等待完成                           ├─ 创建 Stream
  └─ 返回 NodeRunResult                 ├─ 立即返回 NodeRunResult（含 stream_outputs）
       ↓                               ├─ 后台任务持续写入 stream
Dispatcher 写入 pool                    │      ↓
       ↓                          Dispatcher 写入 pool（含 Segment::Stream）
调度下游节点                              ↓
                                  调度下游节点（可以立即开始消费流）
                                       │
                                  下游节点 reader.next().await
                                       ↓
                                  后台任务继续 emit → 下游节点持续消费
```

#### 3.8.10 SegmentType 映射

Stream 不需要在 DSL 层声明（用户不会声明 `type: stream` 的输入变量），它是运行时产生的类型。

```rust
impl Segment {
    pub fn segment_type(&self) -> SegmentType {
        match self {
            // ...
            Segment::Stream(_) => SegmentType::Any, // Stream 不对应 DSL 类型
        }
    }
}
```

#### 3.8.11 条件评估器和 Stream

条件评估器中如果遇到 `Segment::Stream`：

```rust
// evaluate_condition() 中
let actual = pool.get(&cond.variable_selector);
let actual = match actual {
    Segment::Stream(s) => {
        // 自动 collect，阻塞等待流完成
        s.collect().await.unwrap_or(Segment::None)
    }
    other => other,
};
```

这需要 `evaluate_condition` 变为 async。如果不希望改变条件评估器的同步签名，可以在调用前预先 resolve 所有 Stream 变量。

### 3.9 流感知节点执行模式

Stream 作为 Segment 的一等变体，下游节点被调度器立即调度后即可拿到 `Segment::Stream` 句柄。但 Code 节点和 Template Transform 节点当前的变量消费路径是同步的（`pool.get() → to_value()` 一次性转换），无法增量消费流数据。本节设计这两类节点的流感知执行模式，使它们在输入 Stream 尚未结束时就能开始处理并向下游输出自己的 Stream。

#### 3.9.1 通用：流感知检测

节点执行时，在构建输入变量阶段检查是否存在正在运行的 Stream：

```
构建输入变量时:
  any_stream_running = false
  stream_inputs  = []   // (var_name, SegmentStream)
  static_inputs  = {}   // var_name → Value

  for each variable mapping (var_name, selector):
    seg = pool.get(selector)
    match seg:
      Stream(s) if s.status() == Running →
        any_stream_running = true
        stream_inputs.push((var_name, s))
      Stream(s) if s.status() == Completed →
        // 已完成的 stream 透明降级为 final_value
        static_inputs[var_name] = s.collect().to_value()
      other →
        static_inputs[var_name] = seg.to_value()

  if any_stream_running:
    进入 **流式执行模式**
  else:
    正常执行（所有输入都是 Value，行为不变）
```

**向后兼容**：如果所有 Stream 都已 Completed 或没有 Stream 输入，节点以正常模式执行，行为完全不变。

#### 3.9.2 Code 节点流式执行

##### JS API：StreamProxy 对象

在 JS 沙箱中，Running 状态的 Stream 变量被表示为 `StreamProxy` 对象（而非普通 JSON Value）：

```javascript
function main(inputs) {
    // inputs.prefix = "AI says: "     ← 普通变量，正常 JSON Value
    // inputs.llm_text                 ← StreamProxy 对象

    // 注册 chunk 回调
    inputs.llm_text.on_chunk(function(chunk) {
        // chunk: 每个流数据块的值（string/number/object 等，取决于上游 emit 的类型）
        // return 的值作为输出流的一个 chunk emit 出去
        return { text: inputs.prefix + chunk };
    });

    // 注册结束回调（可选）
    inputs.llm_text.on_end(function(final_value) {
        // final_value: 流结束时的完整值
        // return 的值作为输出流的 final_value
        return { text: "COMPLETE: " + final_value };
    });

    // 注册错误回调（可选）
    inputs.llm_text.on_error(function(error_message) {
        // error_message: 错误信息字符串
        return { text: "ERROR: " + error_message };
    });

    // main 的返回值作为节点的 initial outputs（写入普通 outputs）
    return {};
}
```

**API 特性**：
- `on_chunk(fn)` / `on_end(fn)` / `on_error(fn)` 返回 `this`，支持链式调用
- 如果未注册 `on_chunk`，该 Stream 变量退化为 auto-collect（等待完成后用 final_value）
- `on_end` 和 `on_error` 可选；未注册时使用默认行为
- 回调函数中可以通过闭包访问 `inputs` 中的其他变量（包括其他 StreamProxy 的累积状态）

##### Rust 侧 StreamProxy 实现

```rust
/// JS 沙箱中 Stream 变量的代理对象
/// 在 boa_engine 中注册为自定义 JS 对象，带有 on_chunk/on_end/on_error 方法
struct StreamProxy {
    variable_name: String,
    /// 用户注册的回调（boa JsFunction 引用，保持在 Context 中存活）
    on_chunk_fn: Option<JsFunction>,
    on_end_fn: Option<JsFunction>,
    on_error_fn: Option<JsFunction>,
}
```

当 JS 代码调用 `inputs.llm_text.on_chunk(fn)` 时：
1. boa 侧将 `fn` 存储为 `JsFunction` 引用
2. 返回 StreamProxy 自身（支持链式调用）
3. `main()` 返回后，Rust 侧提取所有 StreamProxy 上注册的回调

##### 执行流程

```
CodeNodeExecutor::execute()
│
├─ 1. 检测输入变量中的 Stream（见 3.9.1）
│     stream_inputs: Vec<(var_name, SegmentStream)>
│     static_inputs: Map<String, Value>
│
├─ 2. 构建 JS 输入对象
│     对于 static_inputs → 正常 JSON value
│     对于 stream_inputs → 创建 StreamProxy JS 对象
│
├─ 3. 在 boa Context 中调用 main(inputs)
│     - 用户代码注册 on_chunk/on_end/on_error 回调
│     - main() 返回 initial outputs
│     - 从各 StreamProxy 提取注册的回调函数
│
├─ 4. 检查回调注册情况
│     if 没有任何 stream 变量注册了 on_chunk:
│       → 退化：auto-collect 所有 stream，用 final_value 替换对应输入
│       → 在同一 Context 中重新调用 main(resolved_inputs)
│       → 返回普通 NodeRunResult（行为等价于等流结束后执行）
│
├─ 5. 有回调 → 创建输出流，立即返回
│     let (output_stream, writer) = SegmentStream::channel()
│     返回 NodeRunResult {
│       outputs: main() 的返回值（initial outputs）,
│       stream_outputs: { "output": output_stream },
│     }
│
└─ 6. tokio::spawn 后台任务（保持 boa Context 存活）
      │
      ├─ 按 stream_inputs 声明顺序，逐个消费 stream：
      │   for (var_name, stream) in stream_inputs:
      │     let callback = stream_proxies[var_name].on_chunk_fn
      │     let reader = stream.reader()
      │     loop:
      │       match reader.next().await:
      │         Chunk(seg) →
      │           // 在保持存活的 boa Context 中调用回调
      │           chunk_val = seg.to_value()
      │           result = context.call(callback, chunk_val)  // spawn_blocking
      │           writer.send(Segment::from_value(&result)).await
      │
      │         End(final_seg) →
      │           if on_end_fn registered:
      │             result = context.call(on_end_fn, final_seg.to_value())
      │             // on_end 的 result 作为这个 stream 阶段的终结 chunk
      │             writer.send(Segment::from_value(&result)).await
      │           break → 进入下一个 stream
      │
      │         Error(msg) →
      │           if on_error_fn registered:
      │             result = context.call(on_error_fn, msg)
      │             writer.send(Segment::from_value(&result)).await
      │           else:
      │             writer.error(msg).await
      │           break
      │
      └─ 所有 stream 消费完毕
          // 最后一个 on_end 的返回值（或最后一个 chunk 的返回值）作为 final_value
          writer.end(last_result).await
          // 销毁 boa Context，释放资源
```

##### 沙箱上下文保持（方案 A）

选择保持 boa Context 存活而非无状态回调，原因：

| 方面 | 保持 Context 存活 | 无状态回调 |
|------|-----------------|----------|
| 回调间状态共享 | ✅ 闭包变量天然共享 | ❌ 每次新 Context，无法共享 |
| 使用场景 | 累加器、计数器、缓冲区 | 纯转换（如 toUpperCase） |
| 内存 | 需要手动销毁 | 自动释放 |
| 实现复杂度 | 中等（需管理生命周期） | 低 |

**选择**：保持 Context 存活。流处理中保持状态是常见需求（如"将前 3 个 chunk 聚合后再 emit"），无状态模式限制太大。

**生命周期管理**：
- boa Context 在 `tokio::task::spawn_blocking` 中创建
- 后台任务持有 Context 的所有权
- 所有 stream 消费完毕后，任务结束，Context 自动 drop
- 超时保护：通过 `tokio::time::timeout` 包装整个后台任务

```rust
// 示例：带状态的流处理
function main(inputs) {
    var count = 0;
    var buffer = [];

    inputs.llm_text.on_chunk(function(chunk) {
        count++;
        buffer.push(chunk);

        // 每 3 个 chunk 聚合一次 emit
        if (buffer.length >= 3) {
            var merged = buffer.join("");
            buffer = [];
            return { text: merged, chunk_count: count };
        }
        // 返回 null/undefined → 不 emit（跳过这个 chunk）
        return null;
    });

    inputs.llm_text.on_end(function(final_value) {
        // flush 剩余 buffer
        if (buffer.length > 0) {
            return { text: buffer.join(""), chunk_count: count };
        }
        return null;
    });

    return {};
}
```

**on_chunk 返回 null/undefined 的行为**：不 emit 到输出流（跳过），这允许用户实现 filter/batch 等模式。

##### 多 Stream 输入处理

```
stream_inputs 按 variables config 中的声明顺序排列:
  stream_inputs = [(var_a, stream_a), (var_b, stream_b)]

时间线:
──────────────────────────────────────────────────────────►

stream_a:    [chunk] [chunk] [chunk] [end]
stream_b:    [chunk] [chunk] [chunk] [chunk] [chunk] [end]
                                                ↑
                                      后台自动缓冲中

消费阶段 1（stream_a 活跃）:
  - stream_a: 实时逐 chunk 消费（reader.next().await）
  - stream_b: 上游持续写入，chunks 自动缓存在 Arc<RwLock<Vec>> 中
  - 每个 stream_a chunk → 调用 var_a 的 on_chunk 回调

消费阶段 2（stream_a 结束，轮到 stream_b）:
  - 创建 stream_b 的 reader（cursor = 0）
  - reader.next() 立即返回已缓冲的 chunks（快速追赶）
  - 追完缓冲后，如果 stream_b 还在 Running，继续等待新 chunks
  - 每个 stream_b chunk → 调用 var_b 的 on_chunk 回调
  - 效果：先一口气输入所有已缓冲数据，然后切换为实时消费
```

这个行为是 `StreamReader` cursor 机制的自然结果——已有的 chunks 在 `Vec<Segment>` 中，新 reader 从 0 开始读，全部是即时返回。

##### 每个 stream 可以有自己的回调

```javascript
function main(inputs) {
    // 两个不同 LLM 的流，分别注册不同的回调
    inputs.llm1_text.on_chunk(function(chunk) {
        return { source: "llm1", text: chunk };
    });

    inputs.llm2_text.on_chunk(function(chunk) {
        return { source: "llm2", text: chunk };
    });

    return {};
}
```

消费顺序：先消费 `llm1_text` 的所有 chunks，再消费 `llm2_text` 的所有 chunks。两个回调在同一个 boa Context 中执行，可以共享闭包状态。

#### 3.9.3 Template Transform 节点流式执行

Template 节点没有 JS 回调机制。当检测到输入变量包含 Running Stream 时，Rust 侧自动进入流式渲染模式。

##### 执行流程

```
TemplateTransformExecutor::execute()
│
├─ 1. 检测输入变量中的 Stream（见 3.9.1）
│     stream_vars: Vec<(var_name, SegmentStream)>
│     static_vars: HashMap<String, Value>
│
├─ 2. 无 Running Stream → 正常执行（行为不变）
│
├─ 3. 有 Running Stream → 创建输出流，立即返回
│     let (output_stream, writer) = SegmentStream::channel()
│     返回 NodeRunResult {
│       stream_outputs: { "output": output_stream },
│     }
│
└─ 4. tokio::spawn 后台任务
      │
      ├─ accumulated: HashMap<String, String>  // 每个 stream 变量的已累积显示字符串
      ├─ last_rendered: String = ""             // 上次完整渲染结果（用于计算增量）
      │
      ├─ 按 stream_vars 声明顺序，逐个消费 stream：
      │   for (var_name, stream) in stream_vars:
      │     let reader = stream.reader()
      │     loop:
      │       match reader.next().await:
      │         Chunk(seg) →
      │           // 累积 stream 变量的显示值
      │           accumulated[var_name] += seg.to_display_string()
      │
      │           // 构建完整的 jinja_vars（静态变量 + 所有 stream 的当前累积值）
      │           let mut jinja_vars = static_vars.clone()
      │           for (name, acc) in &accumulated:
      │             jinja_vars[name] = Value::String(acc.clone())
      │
      │           // 渲染完整模板
      │           let rendered = render_jinja2(template, &jinja_vars)?
      │
      │           // 计算增量并 emit
      │           if rendered.len() > last_rendered.len()
      │              && rendered.starts_with(&last_rendered):
      │             // 简单后缀追加（常见场景优化）
      │             let delta = &rendered[last_rendered.len()..]
      │             writer.send(Segment::String(delta)).await
      │           else if rendered != last_rendered:
      │             // 模板结构变化，发送全量
      │             writer.send(Segment::String(rendered.clone())).await
      │
      │           last_rendered = rendered
      │
      │         End(final_seg) →
      │           accumulated[var_name] = final_seg.to_display_string()
      │           // 最终渲染（同上逻辑）
      │           ...
      │           break → 进入下一个 stream
      │
      │         Error(msg) →
      │           writer.error(msg).await
      │           return
      │
      └─ 所有 stream 消费完毕
          writer.end(Segment::String(last_rendered)).await
```

##### 增量计算

模板增量基于一个常见假设：**流式文本追加时，模板输出也是追加的**。

```
模板: "AI says: {{ llm_text }}"

Chunk 1: "Hello"
  accumulated["llm_text"] = "Hello"
  rendered = "AI says: Hello"
  delta = "AI says: Hello"          (首次，全量)
  emit: "AI says: Hello"

Chunk 2: " World"
  accumulated["llm_text"] = "Hello World"
  rendered = "AI says: Hello World"
  delta = " World"                  (后缀追加)
  emit: " World"

最终 stream output final_value = "AI says: Hello World"
```

**边界情况：模板变量在中间**

```
模板: ">> {{ llm_text }} <<"

Chunk 1: "Hi"
  rendered = ">> Hi <<"
  emit: ">> Hi <<"                  (全量)

Chunk 2: " there"
  rendered = ">> Hi there <<"
  starts_with(">> Hi <<") = false   (后缀不匹配！)
  → 发送全量 ">> Hi there <<"
```

对于这种情况，每个 chunk 都发送全量渲染结果。消费端需自行处理（替换而非追加）。实际场景中，绝大多数流式模板是 `前缀 + {{ stream_var }}` 形式，后缀追加模式可以覆盖。

#### 3.9.4 Answer 节点（补充多 Stream 场景）

Answer 节点使用 `{{#node.var#}}` 语法引用变量。单个 Stream 的消费已在 3.8.7 设计（StreamReader 逐 chunk 消费）。

**多 Stream 补充**：当模板引用多个流变量时（如 `{{#llm1.text#}} vs {{#llm2.text#}}`），适用与 Template 节点相同的规则：
- 按模板中出现的顺序串行消费
- 先消费的流实时推送，后消费的流后台缓冲
- 增量计算逻辑相同（累积 + 重渲染 + diff）

#### 3.9.5 节点输出类型更新

当节点检测到 Running Stream 输入并进入流式模式时，输出从普通 Value 变为 Stream：

| 节点类型 | 普通模式输出 | 流式模式输出 |
|---------|------------|------------|
| code | `outputs: { key: Value }` | `outputs: main() 返回值` + `stream_outputs: { "output": Stream }` |
| template-transform | `outputs: { "output": String }` | `stream_outputs: { "output": Stream }` |
| answer | `outputs: { "answer": String }` | `stream_outputs: { "answer": Stream }` |

流式模式下，`stream_outputs` 中的 Stream 在流结束后 `final_value` 等于普通模式下的完整输出值。

#### 3.9.6 向后兼容

| 场景 | 行为 | 理由 |
|------|------|------|
| 无 Stream 输入 | 完全不变 | 不触发流式检测 |
| Stream 已 Completed | 完全不变 | `snapshot_segment()` 返回 final_value，走正常 `to_value()` 路径 |
| Code 有 Running Stream 但未注册 on_chunk | 等价于等流结束 | auto-collect 所有 stream，用 final_value 重新调 main() |
| Template 有 Running Stream | 自动进入流式渲染 | 对用户透明，模板写法不变，最终结果相同 |
| 下游节点不支持 Stream | 自动 collect | 下游节点 `to_value()` 调用 `snapshot_segment()`，Completed 的流返回 final_value |

---

## 四、各场景的变量类型总览

### 4.1 节点输出类型表

| 节点类型 | 输出变量 | Segment 类型 | 说明 |
|---------|---------|-------------|------|
| start | `{var_name}` | 取决于用户输入 | 由 DSL 声明 var_type 但不强制 |
| start | `sys.query` | String | 系统变量 |
| start | `sys.files` | Array（内含 Object） | 系统变量，每个 Object 是文件元数据 |
| end | `{variable}` | 透传上游类型 | 通过 value_selector 引用 |
| answer | `answer` | String | 模板渲染后的字符串 |
| answer (流式模式) | `answer` | **Stream** | 输入含 Running Stream 时，逐 chunk 渲染推送（3.9.4） |
| if-else | `selected_case` | String | case_id 或 "false" |
| template-transform | `output` | String | Jinja2 渲染结果 |
| template-transform (流式模式) | `output` | **Stream** | 输入含 Running Stream 时，逐 chunk 增量渲染（3.9.3） |
| variable-aggregator | `output` | 透传上游类型 | 第一个非 null 值 |
| variable-assigner | `output` | 透传上游类型 | 加 write_mode 和 selector 元数据 |
| code (JS) | `{key}` | 任意 | 沙箱代码返回的 JSON |
| code (JS 流式模式) | `output` | **Stream** | 输入含 Running Stream 且注册了 on_chunk 回调（3.9.2） |
| code (WASM) | `{key}` | 任意 | WASM 执行结果 |
| http-request | `status_code` | Integer | HTTP 状态码 |
| http-request | `body` | String | 响应体文本 |
| http-request | `headers` | String | Debug 格式的头字符串 |
| iteration | `{output_variable}` | Array | 子图输出的数组（每项为 Object） |
| loop | `{output_variable}` | Object | 循环最终状态 |
| loop | `_iterations` | Integer | 循环次数 |
| list-operator | `{output_variable}` | 取决于操作 | filter/map→Array, first/last→Any, length→Integer |
| llm (非流式) | `text` | String | 完整生成文本 |
| llm (流式) | `text` | **Stream** | 流式返回，消费者异步逐 chunk 读取 |

### 4.2 系统/环境/对话变量

| 命名空间 | 变量名 | 类型 | 设置时机 |
|----------|--------|------|---------|
| `sys` | `query` | String | 工作流执行前由调用方设置 |
| `sys` | `files` | Array（内含文件 Object） | 工作流执行前由调用方设置（可选） |
| `env` | `{name}` | String | 从 WorkflowSchema.environment_variables 加载 |
| `conversation` | `{name}` | 取决于 default 值 | 从 WorkflowSchema.conversation_variables 加载 |

### 4.3 子图作用域变量

| 容器节点 | 变量名 | 类型 | 作用域 |
|---------|--------|------|--------|
| iteration | `item` | 任意（数组元素） | `__scope__.item` |
| iteration | `index` | Integer | `__scope__.index` |
| loop | `loop.{var}` | 取决于 initial_vars | `loop.{var}` |
| loop | `_iteration` | Integer | `__scope__._iteration` |
| list-operator (filter/map) | `item` | 任意（数组元素） | `__scope__.item` |
| list-operator (filter/map) | `index` | Integer | `__scope__.index` |
| list-operator (reduce) | `accumulator` | 任意 | `__scope__.accumulator` |

---

## 五、类型转换与强制转型规则

### 5.1 Segment → Value（`to_value()`）

| Segment 变体 | Value 结果 | 无损 |
|-------------|-----------|------|
| None | Null | ✅ |
| String(s) | String(s) | ✅ |
| Integer(i) | Number(i) | ✅ |
| Float(f) | Number(f) | ✅ |
| Boolean(b) | Bool(b) | ✅ |
| Object(m) | Object(递归) | ✅ |
| ArrayString(v) | Array([String...]) | ✅ |
| Array(v) | Array(递归) | ✅ |
| Stream(s) | 取决于流状态（见 3.8.5） | ❌ Completed→final_value, Running→Array(chunks), Failed→Null |

### 5.2 Value → Segment（`from_value()`，改进后）

| Value 类型 | Segment 结果 | 说明 |
|-----------|-------------|------|
| Null | None | ✅ |
| Bool(b) | Boolean(b) | ✅ |
| Number(整数) | Integer(i) | ✅ |
| Number(浮点) | Float(f) | ✅ |
| String(s) | String(s) | ✅ |
| Array（全 String） | **ArrayString** | ✅ 推断为 ArrayString |
| Array（其他） | **Array** | ✅ 通用数组 |
| Object({...}) | Object(递归) | ✅ 文件 Object 也是普通 Object |

> **注意**：`from_value()` 不可能重建 `Stream`。Stream 只能通过 `SegmentStream::channel()` 创建，无法从反序列化的 Value 恢复。

### 5.3 显示转换（`to_display_string()`）

| Segment 变体 | 显示结果 |
|-------------|---------|
| None | `""` |
| String(s) | `s` |
| Integer(i) | `"42"` |
| Float(f) | `"3.14"` |
| Boolean(b) | `"true"` / `"false"` |
| Stream(s) | Completed→final_value 的显示字符串, Running→各 chunk 显示字符串拼接, Failed→`"[stream error]"` |
| 其他 | JSON 序列化字符串 |

### 5.4 数值转换（`as_f64()`）

| Segment 变体 | 结果 |
|-------------|------|
| Integer(i) | Some(i as f64) |
| Float(f) | Some(f) |
| String(s) | s.parse::<f64>().ok() |
| 其他 | None |

### 5.5 条件评估器的隐式类型强转

| 运算符类别 | 转换策略 | 类型不匹配行为 |
|-----------|---------|---------------|
| Is / IsNot | 双方都 `to_display_string()` → 字符串比较 | 总是可比较 |
| Contains / StartWith / EndWith | actual `to_display_string()`，expected `value_to_string()` | 非 String/Array 返回 false |
| Equal / GT / LT / GE / LE | 双方 `as_f64()` → 浮点比较 | 无法转数值返回 false |
| Empty / NotEmpty | 调用 `is_none()` + `is_empty()` | N/A |
| Null / NotNull | 调用 `is_none()` | N/A |
| In / NotIn | actual `to_display_string()`，expected 转字符串数组 | 总是可比较 |
| AllOf | actual 需为数组类型，expected 转字符串数组 | 非数组返回 false |

---

## 六、完整改动列表

### 修改文件

| 文件 | 改动 |
|------|------|
| `src/core/variable_pool.rs` | 1. 精简 Segment 枚举：移除 5 个变体，ArrayAny→Array，新增 Stream（3.1, 3.8） |
| | 2. FileSegment 降级为辅助类型，新增 `to_segment()`、`from_segment()` 方法（3.2） |
| | 3. 简化 `from_value()`：只推断 ArrayString（3.3） |
| | 4. 简化 `to_value()`：移除已删变体的 match arm，新增 Stream 处理（3.8.5） |
| | 5. 新增 `SegmentType` 枚举（3.4） |
| | 6. 实现 `PartialEq for Segment`（3.5） |
| | 7. 修复 `append()` 类型安全（3.6） |
| | 8. `is_empty()` 支持空 Object（3.7） |
| | 9. 新增 `SegmentStream`, `StreamWriter`, `StreamReader`, `StreamEvent`, `StreamStatus` 等类型（3.8） |
| `src/core/mod.rs` | 更新导出：移除 `FileSegment` 从 Segment 相关导出（FileSegment 仍导出但作为辅助类型），新增导出 Stream 相关类型 |
| `src/core/dispatcher.rs` | 节点执行完毕后，除写入 `outputs` 外，还需写入 `stream_outputs`（3.8.9） |
| `src/dsl/schema.rs` | `NodeRunResult` 新增 `stream_outputs: HashMap<String, SegmentStream>` 字段（3.8.8） |
| `src/evaluator/condition.rs` | `eval_contains` 和 `eval_all_of` 中 ArrayString 路径不变；移除对已删变体的隐式匹配；新增 Stream auto-collect 预处理（3.8.11） |
| `src/llm/executor.rs` | `extract_image_urls` 中 ArrayString 路径不变；流式模式改为创建 `SegmentStream` 并通过 `stream_outputs` 返回（3.8.6） |
| `src/nodes/data_transform.rs` | CodeNodeExecutor：新增流式检测、StreamProxy 构建、boa Context 保持、后台 chunk 回调执行（3.9.2）；TemplateTransformExecutor：新增流式检测、后台增量渲染（3.9.3） |
| `src/nodes/control_flow.rs` | AnswerNodeExecutor：新增多 Stream 串行消费（3.9.4） |
| `src/sandbox/builtin.rs` | 新增 StreamProxy JS 对象注册、回调函数提取、Context 保持模式的 API（3.9.2） |
| `src/lib.rs` | 新增导出 `SegmentType` |

### 不修改的部分

| 部分 | 原因 |
|------|------|
| `VariablePool` 存储结构 | `(node_id, var_name) → Segment` 模型合理，Stream 作为 Segment 变体自然融入 |
| 条件评估器的比较逻辑 | 隐式转换行为与 Dify 兼容，不宜改变（Stream 通过预处理 auto-collect 适配） |
| 模板引擎 (`src/template/engine.rs`) | `render_template()` 和 `render_jinja2()` 本身不变；流式渲染在节点执行器层处理，模板引擎仍是无状态的纯函数调用 |
| DSL 的 `var_type` 字符串列表 | `"file"` 和 `"array[file]"` 在 DSL 层仍有效；Stream 不需要 DSL 声明 |

---

## 七、测试策略

### 7.1 `from_value()` 类型推断

```rust
// 纯字符串数组 → ArrayString
assert!(matches!(
    Segment::from_value(&json!(["a", "b", "c"])),
    Segment::ArrayString(_)
));

// 纯整数数组 → Array（内含 Integer）
let seg = Segment::from_value(&json!([1, 2, 3]));
assert!(matches!(seg, Segment::Array(_)));
if let Segment::Array(v) = seg {
    assert!(matches!(v[0], Segment::Integer(1)));
}

// 混合类型 → Array
assert!(matches!(
    Segment::from_value(&json!([1, "a"])),
    Segment::Array(_)
));

// 空数组 → Array
assert!(matches!(
    Segment::from_value(&json!([])),
    Segment::Array(_)
));

// 文件 Object → 普通 Object（不是 File 变体）
let file_val = json!({"transfer_method": "local", "url": "/path", "mime_type": "text/plain"});
assert!(matches!(
    Segment::from_value(&file_val),
    Segment::Object(_)
));
```

### 7.2 往返一致性

```rust
// ArrayString 往返测试
let orig = Segment::ArrayString(vec!["a".into(), "b".into()]);
let roundtrip = Segment::from_value(&orig.to_value());
assert_eq!(orig, roundtrip);

// Array 往返测试
let orig = Segment::Array(vec![Segment::Integer(1), Segment::Integer(2)]);
let roundtrip = Segment::from_value(&orig.to_value());
assert_eq!(orig, roundtrip);

// Object 往返测试（包括文件 Object）
let file_obj = FileSegment { url: Some("/path".into()), ..Default::default() };
let seg = file_obj.to_segment();
assert!(matches!(seg, Segment::Object(_)));
let roundtrip = Segment::from_value(&seg.to_value());
assert!(matches!(roundtrip, Segment::Object(_)));
// 仍可解析出 FileSegment
assert!(FileSegment::from_segment(&roundtrip).is_some());
```

### 7.3 SegmentType 测试

```rust
assert_eq!(SegmentType::from_dsl_type("string"), Some(SegmentType::String));
assert_eq!(SegmentType::from_dsl_type("number"), Some(SegmentType::Number));
assert_eq!(SegmentType::from_dsl_type("file"), Some(SegmentType::File));
assert_eq!(SegmentType::from_dsl_type("invalid"), None);

assert!(Segment::Integer(42).matches_type(&SegmentType::Number));
assert!(Segment::Float(3.14).matches_type(&SegmentType::Number));
assert!(!Segment::String("42".into()).matches_type(&SegmentType::Number));

// File 类型匹配：Object 匹配 File
let file_seg = FileSegment::default().to_segment();
assert!(file_seg.matches_type(&SegmentType::File));
assert!(file_seg.matches_type(&SegmentType::Object));
```

### 7.4 Append 类型安全

```rust
// ArrayString + Integer → 升级为 Array（不再静默丢弃）
let mut pool = VariablePool::new();
pool.set(&sel, Segment::ArrayString(vec!["a".into()]));
pool.append(&sel, Segment::Integer(1));
match pool.get(&sel) {
    Segment::Array(v) => assert_eq!(v.len(), 2),
    _ => panic!("Expected Array after type promotion"),
}
```

### 7.5 Stream 类型测试

```rust
// 基本生产-消费
#[tokio::test]
async fn test_stream_basic() {
    let (stream, writer) = SegmentStream::channel();

    tokio::spawn(async move {
        writer.emit(Segment::String("hello ".into())).await;
        writer.emit(Segment::String("world".into())).await;
        writer.end(Segment::String("hello world".into())).await;
    });

    let result = stream.collect().await.unwrap();
    assert_eq!(result, Segment::String("hello world".into()));
}

// 多消费者各自独立游标
#[tokio::test]
async fn test_stream_multiple_readers() {
    let (stream, writer) = SegmentStream::channel();

    tokio::spawn(async move {
        writer.emit(Segment::String("a".into())).await;
        writer.emit(Segment::String("b".into())).await;
        writer.end(Segment::String("ab".into())).await;
    });

    let mut reader1 = stream.reader();
    let mut reader2 = stream.reader();

    // 两个 reader 各自从头消费
    let e1 = reader1.next().await;
    assert!(matches!(e1, Some(StreamEvent::Chunk(_))));
    let e2 = reader2.next().await;
    assert!(matches!(e2, Some(StreamEvent::Chunk(_))));
}

// VariablePool clone 共享同一个流
#[tokio::test]
async fn test_stream_pool_clone() {
    let (stream, writer) = SegmentStream::channel();
    let mut pool = VariablePool::new();
    pool.set(&["node1".into(), "text".into()], Segment::Stream(stream));

    let pool_clone = pool.clone();

    tokio::spawn(async move {
        writer.emit(Segment::String("chunk".into())).await;
        writer.end(Segment::String("chunk".into())).await;
    });

    // 从 clone 的 pool 中读取同一个流
    if let Segment::Stream(s) = pool_clone.get(&["node1".into(), "text".into()]).unwrap() {
        let result = s.collect().await.unwrap();
        assert_eq!(result, Segment::String("chunk".into()));
    }
}

// 流错误处理
#[tokio::test]
async fn test_stream_error() {
    let (stream, writer) = SegmentStream::channel();

    tokio::spawn(async move {
        writer.emit(Segment::String("partial".into())).await;
        writer.error("timeout".into()).await;
    });

    let result = stream.collect().await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "timeout");
}

// 非文本流：emit 不同类型的 Segment
#[tokio::test]
async fn test_stream_mixed_types() {
    let (stream, writer) = SegmentStream::channel();

    tokio::spawn(async move {
        writer.emit(Segment::Integer(1)).await;
        writer.emit(Segment::Float(2.5)).await;
        writer.emit(Segment::Boolean(true)).await;
        writer.emit(Segment::Object(HashMap::from([
            ("key".into(), Segment::String("value".into())),
        ]))).await;
        writer.end(Segment::Array(vec![
            Segment::Integer(1),
            Segment::Float(2.5),
            Segment::Boolean(true),
        ])).await;
    });

    let mut reader = stream.reader();
    // 第一个 chunk 是 Integer
    match reader.next().await {
        Some(StreamEvent::Chunk(Segment::Integer(1))) => {},
        other => panic!("Expected Integer chunk, got {:?}", other),
    }
    // 第二个 chunk 是 Float
    match reader.next().await {
        Some(StreamEvent::Chunk(Segment::Float(_))) => {},
        other => panic!("Expected Float chunk, got {:?}", other),
    }
}

// Stream 的 to_value() — 已完成
#[tokio::test]
async fn test_stream_to_value_completed() {
    let (stream, writer) = SegmentStream::channel();
    writer.end(Segment::String("done".into())).await;

    let seg = Segment::Stream(stream);
    // to_value 应返回 final_value
    // 注意：实际实现需处理 async，此处为概念测试
}
```

### 7.6 流感知节点测试

```rust
// Code 节点：on_chunk 回调基本流程
#[tokio::test]
async fn test_code_node_stream_on_chunk() {
    // 构造一个 Running Stream 作为输入
    let (stream, writer) = SegmentStream::channel();
    let mut pool = VariablePool::new();
    pool.set(&["llm1".into(), "text".into()], Segment::Stream(stream));

    // Code 节点配置：注册 on_chunk 回调
    let config = json!({
        "code": r#"
            function main(inputs) {
                inputs.llm_text.on_chunk(function(chunk) {
                    return { text: chunk.toUpperCase() };
                });
                return {};
            }
        "#,
        "language": "javascript",
        "variables": [{ "variable": "llm_text", "value_selector": ["llm1", "text"] }]
    });

    // 后台 emit chunks
    tokio::spawn(async move {
        writer.send(Segment::String("hello".into())).await;
        writer.send(Segment::String(" world".into())).await;
        writer.end(Segment::String("hello world".into())).await;
    });

    let result = executor.execute("code1", &config, &pool, &ctx).await.unwrap();
    // 应返回 stream_outputs
    assert!(result.stream_outputs.contains_key("output"));

    // 消费输出流，验证每个 chunk 已被 toUpperCase 处理
    let output_stream = result.stream_outputs.get("output").unwrap();
    let mut reader = output_stream.reader();
    match reader.next().await {
        Some(StreamEvent::Chunk(seg)) => {
            let val = seg.to_value();
            assert_eq!(val["text"], "HELLO");
        }
        _ => panic!("Expected chunk"),
    }
}

// Code 节点：无 on_chunk 注册时 auto-collect 退化
#[tokio::test]
async fn test_code_node_stream_no_callback_fallback() {
    let (stream, writer) = SegmentStream::channel();
    // 立即结束流
    writer.end(Segment::String("done".into())).await;

    let mut pool = VariablePool::new();
    pool.set(&["llm1".into(), "text".into()], Segment::Stream(stream));

    let config = json!({
        "code": r#"
            function main(inputs) {
                // 没有调用 on_chunk，直接用值
                return { text: inputs.llm_text + "!" };
            }
        "#,
        "language": "javascript",
        "variables": [{ "variable": "llm_text", "value_selector": ["llm1", "text"] }]
    });

    let result = executor.execute("code1", &config, &pool, &ctx).await.unwrap();
    // 无 stream_outputs，退化为普通模式
    assert!(result.stream_outputs.is_empty());
    assert_eq!(result.outputs["text"], "done!");
}

// Code 节点：on_chunk 中保持状态（方案 A 验证）
#[tokio::test]
async fn test_code_node_stream_stateful_callback() {
    // 验证闭包中的 count 变量在多次 on_chunk 调用之间保持
    let config = json!({
        "code": r#"
            function main(inputs) {
                var count = 0;
                inputs.stream_var.on_chunk(function(chunk) {
                    count++;
                    return { text: chunk, index: count };
                });
                return {};
            }
        "#,
        // ...
    });
    // 第一个 chunk → index: 1
    // 第二个 chunk → index: 2（count 被保持了）
}

// Template 节点：流式增量渲染
#[tokio::test]
async fn test_template_stream_incremental() {
    let (stream, writer) = SegmentStream::channel();
    let mut pool = VariablePool::new();
    pool.set(&["llm1".into(), "text".into()], Segment::Stream(stream));
    pool.set(&["start".into(), "name".into()], Segment::String("Alice".into()));

    let config = json!({
        "template": "Hello {{ name }}, {{ llm_text }}",
        "variables": [
            { "variable": "name", "value_selector": ["start", "name"] },
            { "variable": "llm_text", "value_selector": ["llm1", "text"] }
        ]
    });

    tokio::spawn(async move {
        writer.send(Segment::String("how".into())).await;
        writer.send(Segment::String(" are you".into())).await;
        writer.end(Segment::String("how are you".into())).await;
    });

    let result = executor.execute("tpl1", &config, &pool, &ctx).await.unwrap();
    assert!(result.stream_outputs.contains_key("output"));

    let output_stream = result.stream_outputs.get("output").unwrap();
    let final_val = output_stream.collect().await.unwrap();
    // final_value 应为完整渲染结果
    assert_eq!(final_val.to_display_string(), "Hello Alice, how are you");
}

// Template 节点：无 stream 输入时正常执行（向后兼容）
#[tokio::test]
async fn test_template_no_stream_unchanged() {
    let mut pool = VariablePool::new();
    pool.set(&["n1".into(), "val".into()], Segment::String("world".into()));

    let config = json!({
        "template": "Hello {{ x }}!",
        "variables": [{ "variable": "x", "value_selector": ["n1", "val"] }]
    });

    let result = executor.execute("tpl1", &config, &pool, &ctx).await.unwrap();
    // 普通模式，无 stream_outputs
    assert!(result.stream_outputs.is_empty());
    assert_eq!(result.outputs["output"], "Hello world!");
}
```

### 7.7 现有 E2E 测试回归

所有 `tests/integration/cases/` 中的测试用例应通过。改动不影响已有的值转换结果——移除的变体从未在运行时被创建过，因此不会改变任何现有行为。
