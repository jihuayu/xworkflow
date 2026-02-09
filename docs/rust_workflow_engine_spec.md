# Dify 工作流引擎 Rust 替代方案 - 技术规格文档

## 1. 架构概览

### 1.1 当前 Python 架构

Dify 工作流引擎采用 **队列驱动的并行图执行架构**，核心组件：

```
WorkflowEntry (入口)
  └── GraphEngine (图执行引擎)
        ├── Graph (图结构: 节点 + 边)
        ├── GraphRuntimeState (运行时状态)
        │     ├── VariablePool (变量池)
        │     ├── GraphExecution (执行状态聚合)
        │     └── ResponseCoordinator (响应流协调)
        ├── WorkerPool (工作线程池)
        │     └── Worker[] (工作线程)
        ├── Dispatcher (事件分发器)
        ├── GraphStateManager (状态管理器)
        ├── EdgeProcessor (边处理器)
        ├── EventManager (事件管理器)
        ├── ErrorHandler (错误处理器)
        ├── CommandProcessor (命令处理器)
        └── Layer[] (扩展层: Debug/Limits/Observability)
```

### 1.2 执行流程总览

```
1. WorkflowEntry 初始化 GraphEngine（图、运行时状态、配置）
2. GraphEngine.run() 启动执行，返回事件生成器 (Generator[GraphEngineEvent])
3. 根节点入队 ready_queue
4. WorkerPool 中的 Worker 线程从 ready_queue 取节点执行
5. Worker 执行节点的 run() 方法，产生事件放入 event_queue
6. Dispatcher 线程从 event_queue 取事件，分发给 EventHandler
7. EventHandler 更新状态、处理边、将下游就绪节点入队
8. 循环直到 ready_queue 为空且无正在执行的节点
9. 发出最终事件（成功/失败/中止/暂停）
```

### 1.3 关键设计特点

- **并行执行**: 多个无依赖节点可同时执行（线程池）
- **事件驱动**: 所有状态变更通过事件传播
- **流式输出**: LLM 节点支持 streaming chunk 事件
- **可暂停/恢复**: 支持 human-input 暂停和恢复
- **错误策略**: 支持 fail-branch、default-value、retry
- **外部命令**: 支持 abort、pause、update-variables 命令

---

## 2. 核心数据结构

### 2.1 图结构 (Graph)

```rust
struct Graph {
    nodes: HashMap<String, Node>,        // node_id -> Node
    edges: HashMap<String, Edge>,        // edge_id -> Edge
    in_edges: HashMap<String, Vec<String>>,  // node_id -> [edge_id]
    out_edges: HashMap<String, Vec<String>>, // node_id -> [edge_id]
    root_node: Node,                     // 起始节点
}

### 2.2 节点 (Node)

```rust
struct Node {
    id: String,
    node_type: NodeType,
    title: String,
    config: serde_json::Value,  // 节点配置 JSON
    version: String,            // 默认 "1"
    execution_type: NodeExecutionType,
    state: NodeState,           // UNKNOWN / TAKEN / SKIPPED
}
```

### 2.3 边 (Edge)

```rust
struct Edge {
    id: String,
    source_node_id: String,
    target_node_id: String,
    source_handle: Option<String>,  // 分支出口标识 (如 "true"/"false", "success-branch"/"fail-branch")
    state: NodeState,               // UNKNOWN / TAKEN / SKIPPED
}
```

### 2.4 节点类型枚举 (NodeType)

```rust
enum NodeType {
    Start,                  // "start" - 起始节点
    End,                    // "end" - 结束节点
    Answer,                 // "answer" - 回答节点(聊天模式)
    LLM,                    // "llm" - 大语言模型调用
    KnowledgeRetrieval,     // "knowledge-retrieval" - 知识库检索
    KnowledgeIndex,         // "knowledge-index"
    IfElse,                 // "if-else" - 条件分支
    Code,                   // "code" - 代码执行
    TemplateTransform,      // "template-transform" - Jinja2模板
    QuestionClassifier,     // "question-classifier" - 问题分类
    HttpRequest,            // "http-request" - HTTP请求
    Tool,                   // "tool" - 工具调用
    Datasource,             // "datasource"
    VariableAggregator,     // "variable-aggregator" - 变量聚合
    LegacyVariableAggregator, // "variable-assigner" (旧版)
    Loop,                   // "loop" - 循环
    LoopStart,              // "loop-start"
    LoopEnd,                // "loop-end"
    Iteration,              // "iteration" - 迭代
    IterationStart,         // "iteration-start"
    ParameterExtractor,     // "parameter-extractor" - 参数提取
    VariableAssigner,       // "assigner" - 变量赋值
    DocumentExtractor,      // "document-extractor" - 文档提取
    ListOperator,           // "list-operator" - 列表操作
    Agent,                  // "agent" - 智能体
    TriggerWebhook,         // "trigger-webhook"
    TriggerSchedule,        // "trigger-schedule"
    TriggerPlugin,          // "trigger-plugin"
    HumanInput,             // "human-input" - 人工输入
}
```

### 2.5 节点执行类型 (NodeExecutionType)

```rust
enum NodeExecutionType {
    Executable,  // 常规执行节点
    Response,    // 响应流节点 (Answer, End)
    Branch,      // 分支节点 (IfElse, QuestionClassifier)
    Container,   // 容器节点 (Iteration, Loop)
    Root,        // 入口节点 (Start, Trigger*)
}
```

### 2.6 节点状态 (NodeState)

```rust
enum NodeState {
    Unknown,  // 初始状态
    Taken,    // 已执行/已选中
    Skipped,  // 已跳过
}

### 2.7 变量池 (VariablePool)

变量池是节点间数据传递的核心机制。

```rust
struct VariablePool {
    // 变量存储: (node_id, variable_name) -> Segment
    // 特殊前缀:
    //   "sys" -> 系统变量 (query, files, conversation_id, user_id 等)
    //   "env" -> 环境变量
    //   节点ID -> 该节点的输出变量
    variables: HashMap<(String, String), Segment>,
}

// 变量选择器: 如 ["node_id", "output_name"] 或 ["sys", "query"]
type VariableSelector = Vec<String>;
```

**变量寻址规则**:
- `["sys", "query"]` → 系统变量 query
- `["sys", "files"]` → 系统变量 files
- `["node_abc123", "text"]` → 节点 node_abc123 的 text 输出
- `["env", "API_KEY"]` → 环境变量 API_KEY

### 2.8 变量类型 (Segment)

```rust
enum Segment {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Object(HashMap<String, Segment>),
    ArrayString(Vec<String>),
    ArrayInteger(Vec<i64>),
    ArrayFloat(Vec<f64>),
    ArrayObject(Vec<HashMap<String, Segment>>),
    ArrayAny(Vec<Segment>),
    File(FileSegment),
    ArrayFile(Vec<FileSegment>),
    None,
}

struct FileSegment {
    id: Option<String>,
    tenant_id: String,
    transfer_method: String,  // "local_file" | "remote_url" | "tool_file"
    url: Option<String>,
    filename: Option<String>,
    mime_type: Option<String>,
    extension: Option<String>,
    size: Option<i64>,
}
```

### 2.9 节点执行结果 (NodeRunResult)

```rust
struct NodeRunResult {
    status: WorkflowNodeExecutionStatus,
    inputs: HashMap<String, serde_json::Value>,
    process_data: HashMap<String, serde_json::Value>,
    outputs: HashMap<String, serde_json::Value>,
    metadata: HashMap<String, serde_json::Value>,
    llm_usage: LLMUsage,
    edge_source_handle: String,  // 默认 "source", 分支节点用于指示选中的分支
    error: String,
    error_type: String,
    retry_index: i32,
}

enum WorkflowNodeExecutionStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Exception,  // 错误策略处理后的状态
    Stopped,
    Paused,
    Retry,
}

struct LLMUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    prompt_price: f64,
    completion_price: f64,
    total_price: f64,
    currency: String,
    latency: f64,
}

---

## 3. 事件系统

### 3.1 事件类型层次

所有事件继承自 `GraphEngineEvent`。Rust 实现中用 enum 表示：

```rust
enum GraphEngineEvent {
    // === 图级别事件 ===
    GraphRunStarted,
    GraphRunSucceeded {
        outputs: HashMap<String, serde_json::Value>,
    },
    GraphRunFailed {
        error: String,
        exceptions_count: i32,
    },
    GraphRunPartialSucceeded {
        exceptions_count: i32,
        outputs: HashMap<String, serde_json::Value>,
    },
    GraphRunAborted {
        reason: Option<String>,
        outputs: HashMap<String, serde_json::Value>,
    },
    GraphRunPaused {
        reasons: Vec<PauseReason>,
        outputs: HashMap<String, serde_json::Value>,
    },

    // === 节点级别事件 ===
    NodeRunStarted {
        id: String,              // 节点执行ID
        node_id: String,
        node_type: NodeType,
        node_title: String,
        node_version: String,
        predecessor_node_id: Option<String>,
        in_iteration_id: Option<String>,
        in_loop_id: Option<String>,
        start_at: DateTime,
    },
    NodeRunSucceeded {
        id: String,
        node_id: String,
        node_type: NodeType,
        node_version: String,
        node_run_result: NodeRunResult,
        in_iteration_id: Option<String>,
        in_loop_id: Option<String>,
        start_at: DateTime,
    },
    NodeRunFailed {
        id: String,
        node_id: String,
        node_type: NodeType,
        node_version: String,
        node_run_result: NodeRunResult,
        error: String,
        in_iteration_id: Option<String>,
        in_loop_id: Option<String>,
        start_at: DateTime,
    },
    NodeRunException {
        id: String,
        node_id: String,
        node_type: NodeType,
        node_version: String,
        node_run_result: NodeRunResult,
        error: String,
        in_iteration_id: Option<String>,
        in_loop_id: Option<String>,
        start_at: DateTime,
    },
    NodeRunStreamChunk {
        id: String,
        node_id: String,
        node_type: NodeType,
        selector: Vec<String>,
        chunk: String,
        is_final: bool,
    },
    NodeRunRetry {
        id: String,
        node_id: String,
        node_type: NodeType,
        node_title: String,
        error: String,
        retry_index: i32,
        start_at: DateTime,
    },
    NodeRunPauseRequested {
        id: String,
        node_id: String,
        node_type: NodeType,
        reason: PauseReason,
    },
    NodeRunRetrieverResource {
        id: String,
        node_id: String,
        node_type: NodeType,
        retriever_resources: Vec<RetrievalSourceMetadata>,
        context: String,
    },

    // === 迭代事件 ===
    IterationStarted { node_id: String, node_title: String, start_at: DateTime, inputs: HashMap<String, serde_json::Value> },
    IterationNext { node_id: String, node_title: String, index: i32 },
    IterationSucceeded { node_id: String, node_title: String, outputs: HashMap<String, serde_json::Value>, steps: i32 },
    IterationFailed { node_id: String, node_title: String, error: String, steps: i32 },

    // === 循环事件 ===
    LoopStarted { node_id: String, node_title: String, start_at: DateTime, inputs: HashMap<String, serde_json::Value> },
    LoopNext { node_id: String, node_title: String, index: i32 },
    LoopSucceeded { node_id: String, node_title: String, outputs: HashMap<String, serde_json::Value>, steps: i32 },
    LoopFailed { node_id: String, node_title: String, error: String, steps: i32 },

    // === Agent 事件 ===
    AgentLog { node_id: String, message_id: String, label: String, status: String, data: HashMap<String, serde_json::Value> },
}
```

---

## 4. 执行引擎核心逻辑

### 4.1 图初始化

**输入**: graph_config JSON (包含 nodes 和 edges 数组)

**步骤**:
1. 解析节点配置，创建 Node 实例
2. 解析边配置，创建 Edge 实例，构建 in_edges/out_edges 映射
3. 找到根节点（START 类型或无入边的节点）
4. 标记不活跃的根分支为 SKIPPED（递归传播）
5. 验证图结构（无环、连通性等）

**关键**: 分支节点（IfElse, QuestionClassifier）的出边通过 `source_handle` 区分不同分支。

### 4.2 执行循环（核心算法）

```
fn execute(graph, runtime_state):
    ready_queue.push(root_node)

    while !ready_queue.is_empty() || executing_count > 0:
        // 1. 处理外部命令 (abort/pause)
        process_commands()

        // 2. 从队列取节点
        if let Some(node_id) = ready_queue.pop():
            // 3. 在工作线程中执行节点
            spawn_worker(node_id, |events| {
                for event in node.run():
                    event_queue.push(event)
            })

        // 4. 处理事件
        while let Some(event) = event_queue.pop():
            match event:
                NodeRunSucceeded { node_id, result } => {
                    // 存储输出到变量池
                    variable_pool.set(node_id, result.outputs)

                    // 处理边
                    if node.is_branch():
                        process_branch_edges(node_id, result.edge_source_handle)
                    else:
                        process_normal_edges(node_id)

                    // 检查下游节点就绪状态
                    for downstream in get_downstream_nodes(node_id):
                        if is_node_ready(downstream):
                            ready_queue.push(downstream)
                }
                NodeRunFailed { node_id, error } => {
                    handle_error(node_id, error)
                }
                // ... 其他事件

    // 发出最终事件
    emit_final_event()
```

### 4.3 节点就绪判定

一个节点就绪的条件：
- **所有入边**的状态都不是 UNKNOWN（即都已被处理）
- **至少一条入边**的状态是 TAKEN

```rust
fn is_node_ready(node_id: &str) -> bool {
    let in_edges = graph.in_edges[node_id];
    let all_resolved = in_edges.iter().all(|e| e.state != NodeState::Unknown);
    let any_taken = in_edges.iter().any(|e| e.state == NodeState::Taken);
    all_resolved && any_taken
}
```

### 4.4 分支边处理

分支节点（IfElse, QuestionClassifier）执行后返回 `edge_source_handle` 指示选中的分支：

```rust
fn process_branch_edges(node_id: &str, selected_handle: &str) {
    for edge in graph.out_edges[node_id]:
        if edge.source_handle == selected_handle:
            edge.state = NodeState::Taken
        else:
            edge.state = NodeState::Skipped
            // 递归跳过未选中分支的所有下游节点
            propagate_skip(edge.target_node_id)
}

fn propagate_skip(node_id: &str) {
    node.state = NodeState::Skipped
    for edge in graph.out_edges[node_id]:
        edge.state = NodeState::Skipped
        // 仅当目标节点的所有入边都是 SKIPPED 时才传播
        if all_incoming_skipped(edge.target_node_id):
            propagate_skip(edge.target_node_id)
}
```

### 4.5 错误处理策略

每个节点可配置错误策略：

```rust
enum ErrorStrategy {
    None,           // 默认: 中止整个工作流
    FailBranch,     // 走失败分支
    DefaultValue,   // 使用默认值继续
}

struct RetryConfig {
    max_retries: i32,
    retry_interval: i32,  // 毫秒
}
```

**处理流程**:
1. 节点执行失败
2. 检查是否有重试配置 → 重试
3. 检查错误策略:
   - `FailBranch`: 将 edge_source_handle 设为 "fail-branch"，走失败分支
   - `DefaultValue`: 使用预配置的默认输出值，继续正常流程
   - `None`: 中止整个工作流

### 4.6 暂停与恢复

**暂停触发**: HumanInput 节点发出 `NodeRunPauseRequestedEvent`

**暂停流程**:
1. 记录暂停节点到 `paused_nodes`
2. 停止 Worker 和 Dispatcher
3. 序列化 GraphRuntimeState（包括变量池、执行状态、队列）
4. 发出 `GraphRunPausedEvent`

**恢复流程**:
1. 反序列化 GraphRuntimeState
2. 将 paused_nodes 中的节点重新入队
3. 重新启动执行循环

---

## 5. 节点类型概览

> **详细的节点执行逻辑（配置结构、伪代码、外部依赖）请参见 [`rust_node_execution_spec.md`](./rust_node_execution_spec.md)**

| 节点类型 | 执行类型 | 分支 | 流式 | 外部依赖 | 说明 |
|----------|---------|------|------|---------|------|
| Start | Root | 否 | 否 | 无 | 收集用户输入变量 |
| End | Response | 否 | 否 | 无 | 收集工作流最终输出 |
| Answer | Response | 否 | 是 | 无 | 渲染模板，流式输出回答 |
| LLM | Executable | 否 | 是 | LLM API | 调用大语言模型 |
| IfElse | Branch | 是 | 否 | 无 | 条件分支路由 |
| Code | Executable | 否 | 否 | 代码沙箱 | 执行 Python/JS 代码 |
| TemplateTransform | Executable | 否 | 否 | 代码沙箱 | Jinja2 模板渲染 |
| HttpRequest | Executable | 否 | 否 | HTTP 客户端 | 发送 HTTP 请求 |
| Tool | Executable | 否 | 可选 | 工具/插件系统 | 调用外部工具 |
| KnowledgeRetrieval | Executable | 否 | 否 | 向量数据库 | 知识库检索 |
| QuestionClassifier | Branch | 是 | 否 | LLM API | LLM 问题分类 |
| ParameterExtractor | Executable | 否 | 否 | LLM API | LLM 参数提取 |
| VariableAggregator | Executable | 否 | 否 | 无 | 合并分支变量 |
| VariableAssigner | Executable | 否 | 否 | 无 | 动态赋值变量 |
| Iteration | Container | 否 | 否 | 子图引擎 | 数组迭代执行子图 |
| Loop | Container | 否 | 否 | 子图引擎 | 条件循环执行子图 |
| DocumentExtractor | Executable | 否 | 否 | 文档解析库 | 提取文档文本 |
| ListOperator | Executable | 否 | 否 | 无 | 数组过滤/排序/切片 |
| HumanInput | Executable | 否 | 否 | 暂停/恢复机制 | 等待人工输入 |
| Agent | Executable | 否 | 是 | LLM API + 工具 | 多轮 LLM+工具调用 |

### 关键外部依赖说明

1. **LLM API**: 需要 HTTP 客户端调用 OpenAI/Anthropic/Azure 等 API，支持 SSE 流式响应
2. **代码沙箱**: HTTP POST 到 `{CODE_EXECUTION_ENDPOINT}/v1/sandbox/run`，执行 Python3/JavaScript/Jinja2
3. **向量数据库**: 知识库检索需要调用向量搜索服务
4. **工具/插件系统**: Tool 节点需要调用 Dify 的工具注册表和插件系统
5. **文档解析**: DocumentExtractor 需要 PDF/DOCX/XLSX 等格式的解析库

---

## 6. 兼容方案

### 6.1 替代边界

```
┌─────────────────────────────────────────────────────┐
│                    Python 层                         │
│  API → WorkflowAppRunner → [事件消费/持久化/SSE推送] │
│                      ↕ 事件流                        │
│  ┌─────────────────────────────────────────────┐    │
│  │           Rust 工作流引擎 (替代范围)          │    │
│  │  GraphEngine + Graph + VariablePool          │    │
│  │  + 执行循环 + 边处理 + 状态管理              │    │
│  └─────────────────────────────────────────────┘    │
│                      ↕ FFI/gRPC                      │
│  节点执行器 (可选: Python 回调 或 Rust 原生实现)     │
└─────────────────────────────────────────────────────┘
```

### 6.2 接口协议

Rust 引擎需要暴露以下接口给 Python 层：

```rust
// 1. 创建引擎
fn create_engine(config: EngineConfig) -> EngineHandle;

// 2. 执行工作流 (返回事件流)
fn run_workflow(
    handle: EngineHandle,
    graph_config: &str,       // JSON
    user_inputs: &str,        // JSON
    system_variables: &str,   // JSON
) -> EventStream;

// 3. 发送命令
fn send_command(handle: EngineHandle, command: Command);

// 4. 获取下一个事件
fn next_event(stream: EventStream) -> Option<GraphEngineEvent>;

struct EngineConfig {
    tenant_id: String,
    app_id: String,
    workflow_id: String,
    user_id: String,
    user_from: String,       // "account" | "end-user"
    invoke_from: String,     // "service-api" | "web-app" | "explore" | "debugger"
    call_depth: i32,         // 嵌套调用深度
    max_steps: i32,          // 最大执行步数
    max_execution_time: i32, // 最大执行时间(秒)
}

enum Command {
    Abort { reason: Option<String> },
    Pause,
    UpdateVariables { variables: HashMap<String, serde_json::Value> },
}
```

### 6.3 节点执行器接口

由于节点执行涉及外部服务调用（LLM、HTTP、工具等），建议采用回调模式：

```rust
trait NodeExecutor: Send + Sync {
    /// 执行节点，返回事件流
    fn execute(
        &self,
        node_id: &str,
        node_type: NodeType,
        node_config: &serde_json::Value,
        inputs: HashMap<String, Segment>,
    ) -> Pin<Box<dyn Stream<Item = NodeEvent> + Send>>;
}

enum NodeEvent {
    Started,
    StreamChunk { selector: Vec<String>, chunk: String, is_final: bool },
    Succeeded { result: NodeRunResult },
    Failed { error: String },
    PauseRequested { reason: PauseReason },
}
```

### 6.4 集成方式选择

| 方式 | 优点 | 缺点 |
|------|------|------|
| **PyO3 (推荐)** | 零拷贝、低延迟、直接 Python 集成 | 编译复杂度高 |
| **gRPC** | 语言无关、可独立部署 | 序列化开销、网络延迟 |
| **FFI + C ABI** | 通用、稳定 | 手动内存管理 |
| **子进程 + stdio** | 简单、隔离性好 | 序列化开销大 |

**推荐方案**: PyO3 用于紧密集成，gRPC 用于独立部署。

### 6.5 事件序列化格式

事件需要序列化为 JSON 以便 Python 层消费：

```json
{
    "type": "node_run_succeeded",
    "data": {
        "id": "exec_001",
        "node_id": "node_abc",
        "node_type": "llm",
        "node_version": "1",
        "node_run_result": {
            "status": "succeeded",
            "inputs": {"prompt": "Hello"},
            "outputs": {"text": "Hi there!"},
            "metadata": {"total_tokens": 100},
            "llm_usage": {
                "prompt_tokens": 50,
                "completion_tokens": 50,
                "total_tokens": 100
            },
            "edge_source_handle": "source"
        },
        "start_at": "2024-01-01T00:00:00Z"
    }
}
```

### 6.6 不替代的部分

以下部分保留在 Python 层，不需要 Rust 实现：

1. **持久化层**: WorkflowRun、NodeExecution 的数据库写入
2. **SSE 推送**: 事件到 SSE 的转换和推送
3. **文件存储**: 文件上传、下载、存储管理
4. **权限校验**: 用户权限、配额检查

> **注意**: 所有节点执行逻辑（LLM 调用、HTTP 请求、代码沙箱、知识库检索等）均需在 Rust 中实现，与 Python 版本行为一致。

---

## 7. Rust 实现要点

### 7.1 并发模型

Python 版本使用线程池。Rust 建议使用 **tokio async runtime**：

```rust
// 使用 tokio 的 mpsc channel 替代 Python 的 Queue
use tokio::sync::mpsc;

struct GraphEngine {
    graph: Graph,
    runtime_state: GraphRuntimeState,
    ready_tx: mpsc::Sender<String>,
    ready_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<GraphEngineEvent>,
    event_rx: mpsc::Receiver<GraphEngineEvent>,
}
```

### 7.2 状态管理

使用 `Arc<RwLock<>>` 保护共享状态：

```rust
struct GraphRuntimeState {
    variable_pool: Arc<RwLock<VariablePool>>,
    node_states: Arc<RwLock<HashMap<String, NodeState>>>,
    edge_states: Arc<RwLock<HashMap<String, NodeState>>>,
    executing_nodes: Arc<RwLock<HashSet<String>>>,
    total_tokens: AtomicI64,
    node_run_steps: AtomicI32,
}
```

### 7.3 关键实现注意事项

1. **边处理的原子性**: 边状态更新和节点就绪检查必须在同一个锁内完成，避免竞态条件
2. **Skip 传播**: 分支跳过传播必须是递归的，且只在所有入边都是 SKIPPED 时才传播
3. **容器节点**: Iteration/Loop 节点内部创建子图引擎，需要正确传递变量池
4. **流式事件**: LLM 节点的 StreamChunk 事件需要低延迟传递
5. **执行限制**: 必须实现 max_steps 和 max_execution_time 限制
6. **序列化兼容**: 事件 JSON 格式必须与 Python 版本完全一致

---

## 8. 测试用例

### 8.1 图结构测试

#### TC-G-001: 线性图执行
```
Start → LLM → End
```
- **输入**: `{ "query": "Hello" }`
- **预期**: 按顺序执行 Start → LLM → End，产生事件序列:
  - GraphRunStarted
  - NodeRunStarted(Start) → NodeRunSucceeded(Start)
  - NodeRunStarted(LLM) → NodeRunSucceeded(LLM)
  - NodeRunStarted(End) → NodeRunSucceeded(End)
  - GraphRunSucceeded

#### TC-G-002: 分支图执行 (IfElse true 分支)
```
Start → IfElse → [true] → LLM_A → End
                → [false] → LLM_B → End
```
- **输入**: `{ "number": 10 }`, 条件: `number > 5`
- **预期**: 执行 Start → IfElse → LLM_A → End
- **验证**: LLM_B 被跳过，IfElse 的 edge_source_handle 为 true 分支 ID

#### TC-G-003: 分支图执行 (IfElse false 分支)
- 同上，输入 `{ "number": 3 }`
- **预期**: 执行 Start → IfElse → LLM_B → End
- **验证**: LLM_A 被跳过

#### TC-G-004: 并行执行
```
Start → Node_A → End
      → Node_B → End
```
- **预期**: Node_A 和 Node_B 可并行执行
- **验证**: 两个节点的 NodeRunStarted 事件可以交错出现

#### TC-G-005: 汇聚节点
```
Start → Node_A → Aggregator → End
      → Node_B ↗
```
- **预期**: Aggregator 等待 Node_A 和 Node_B 都完成后才执行
- **验证**: Aggregator 的 NodeRunStarted 在 Node_A 和 Node_B 的 NodeRunSucceeded 之后

#### TC-G-006: 空图 (仅 Start → End)
```
Start → End
```
- **预期**: 正常执行，输出为空
- **验证**: GraphRunSucceeded 事件

#### TC-G-007: 复杂 DAG
```
Start → A → C → E → End
      → B → D ↗
           → E ↗
```
- **预期**: 正确处理多入边汇聚
- **验证**: E 在 C 和 D 都完成后执行

### 8.2 变量池测试

#### TC-V-001: 基本变量传递
```
Start(query="hello") → Code(input=sys.query) → End(output=code.result)
```
- **验证**: End 节点能正确获取 Code 节点的输出

#### TC-V-002: 系统变量访问
- **验证**: 节点能通过 `["sys", "query"]` 访问系统变量
- **验证**: `["sys", "user_id"]`, `["sys", "conversation_id"]` 等

#### TC-V-003: 跨分支变量隔离
```
Start → IfElse → [true] → Code_A(output=x) → Aggregator → End
               → [false] → Code_B(output=x) → Aggregator → End
```
- **验证**: Aggregator 只能获取到被执行分支的变量

#### TC-V-004: 变量类型正确性
- **验证**: String, Integer, Float, Boolean, Object, Array 类型正确传递
- **验证**: 类型不匹配时的处理

#### TC-V-005: 变量选择器解析
- `["node_abc", "text"]` → 节点 node_abc 的 text 输出
- `["sys", "query"]` → 系统变量
- `["env", "API_KEY"]` → 环境变量
- **验证**: 不存在的变量返回 None

### 8.3 节点执行测试

#### TC-N-001: Start 节点
- **输入**: 用户变量 `{ "query": "test", "files": [] }`
- **验证**: 变量正确写入变量池

#### TC-N-002: End 节点
- **输入**: variable_selector 引用上游节点输出
- **验证**: 工作流输出正确

#### TC-N-003: IfElse 条件评估
- 测试所有条件操作符:
  - `contains`, `not_contains`
  - `start_with`, `end_with`
  - `is`, `is_not`
  - `empty`, `not_empty`
  - `=`, `≠`, `>`, `<`, `≥`, `≤`
  - `in`, `not_in`
  - `regex_match`
- 测试 AND/OR 逻辑组合
- 测试多条件组（多个 elif）
- 测试 else 分支（所有条件不满足）

#### TC-N-004: IfElse 多分支
```
IfElse → [condition_1] → A
       → [condition_2] → B
       → [else] → C
```
- **验证**: 只有匹配的分支被执行

#### TC-N-005: Code 节点执行
- **输入**: Python 代码 + 输入变量
- **验证**: 代码执行结果正确返回
- **验证**: 代码执行超时处理
- **验证**: 代码执行错误处理

#### TC-N-006: TemplateTransform 节点
- **输入**: Jinja2 模板 + 变量
- **验证**: 模板渲染结果正确

#### TC-N-007: VariableAggregator 节点
- **输入**: 多个变量选择器，部分为空
- **验证**: 返回第一个非空值

#### TC-N-008: VariableAssigner 节点
- **验证**: 变量被正确赋值到变量池

### 8.4 错误处理测试

#### TC-E-001: 节点执行失败 - 无错误策略
```
Start → FailingNode → End
```
- **预期**: GraphRunFailed 事件
- **验证**: error 信息正确

#### TC-E-002: 节点执行失败 - FailBranch 策略
```
Start → FailingNode(error_strategy=fail-branch) → [success] → A → End
                                                 → [fail] → B → End
```
- **预期**: 走 fail 分支，执行 B
- **验证**: GraphRunPartialSucceeded 事件，exceptions_count = 1

#### TC-E-003: 节点执行失败 - DefaultValue 策略
```
Start → FailingNode(error_strategy=default-value, default={"text": "fallback"}) → End
```
- **预期**: 使用默认值继续执行
- **验证**: End 节点收到默认值

#### TC-E-004: 节点重试
```
Start → FlakeyNode(max_retries=3) → End
```
- **预期**: 失败后重试最多 3 次
- **验证**: NodeRunRetry 事件正确产生
- **验证**: 重试成功后正常继续

#### TC-E-005: 节点重试耗尽后走错误策略
- **预期**: 3 次重试都失败后，执行错误策略

### 8.5 执行控制测试

#### TC-C-001: 中止执行
```
Start → LongRunningNode → End
```
- 执行中发送 Abort 命令
- **预期**: GraphRunAborted 事件
- **验证**: 正在执行的节点被中断

#### TC-C-002: 暂停与恢复
```
Start → HumanInput → End
```
- **预期**: HumanInput 触发暂停
- **验证**: GraphRunPaused 事件
- 恢复后继续执行
- **验证**: GraphRunSucceeded 事件

#### TC-C-003: 执行步数限制
- 设置 max_steps = 5
- 图中有 10 个节点
- **预期**: 执行 5 步后停止
- **验证**: GraphRunFailed 事件，错误信息包含步数限制

#### TC-C-004: 执行时间限制
- 设置 max_execution_time = 1 (秒)
- 节点执行时间 > 1 秒
- **预期**: 超时后停止

### 8.6 迭代/循环测试

#### TC-I-001: 基本迭代
```
Start → Iteration([1,2,3]) → [子图: Code(x*2)] → End
```
- **预期**: 输出 [2, 4, 6]
- **验证**: IterationStarted, IterationNext(0), IterationNext(1), IterationNext(2), IterationSucceeded

#### TC-I-002: 空数组迭代
- **输入**: 空数组 []
- **预期**: 输出空数组，正常完成

#### TC-I-003: 迭代中节点失败
- **预期**: 根据错误策略处理
- **验证**: IterationFailed 事件

#### TC-I-004: 嵌套迭代
```
Iteration → [子图: Iteration → [子图: Code]]
```
- **验证**: 正确处理嵌套

#### TC-L-001: 基本循环
```
Start → Loop(condition: x < 10, max=100) → [子图: x = x + 1] → End
```
- **预期**: 循环 10 次后退出
- **验证**: LoopNext 事件 10 次

#### TC-L-002: 循环达到最大次数
- 设置 max_iterations = 5，条件永远为 true
- **预期**: 5 次后退出
- **验证**: completed_reason 为 "max_iterations"

#### TC-L-003: 循环条件首次即不满足
- **预期**: 不执行循环体，直接输出

### 8.7 流式输出测试

#### TC-S-001: LLM 流式输出
```
Start → LLM → End
```
- **验证**: 收到多个 NodeRunStreamChunk 事件
- **验证**: chunk 拼接后等于最终输出

#### TC-S-002: 多个 Answer 节点流式输出
```
Start → Answer_1 → Answer_2 → End
```
- **验证**: 两个 Answer 节点的流式事件按顺序到达

#### TC-S-003: 分支后的流式输出
```
Start → IfElse → [true] → LLM_A(streaming) → End
               → [false] → LLM_B(streaming) → End
```
- **验证**: 只有被选中分支的 StreamChunk 事件

### 8.8 边界条件测试

#### TC-B-001: 单节点图
```
Start (同时也是 End)
```
- **验证**: 正常执行

#### TC-B-002: 大规模图 (100+ 节点)
- **验证**: 性能和正确性

#### TC-B-003: 深度嵌套调用
- call_depth 达到限制
- **预期**: 拒绝执行，返回错误

#### TC-B-004: 变量值为 null/空
- **验证**: 正确处理 None/null 变量

#### TC-B-005: 超大变量值
- 变量值为超长字符串 (>1MB)
- **验证**: 正确传递，不截断

#### TC-B-006: 并发命令
- 同时发送多个 Abort 命令
- **验证**: 幂等处理

### 8.9 序列化兼容性测试

#### TC-SER-001: 事件 JSON 格式兼容
- 对每种事件类型，验证 Rust 产生的 JSON 与 Python 版本格式一致
- **关键字段**: type, node_id, node_type, status, outputs, error

#### TC-SER-002: 变量池序列化/反序列化
- 序列化变量池 → 反序列化 → 验证数据一致
- 用于暂停/恢复场景

#### TC-SER-003: GraphRuntimeState 快照
- 完整序列化运行时状态
- 反序列化后恢复执行
- **验证**: 恢复后执行结果与不中断一致

### 8.10 集成测试 (Python-Rust 交互)

#### TC-INT-001: Python 调用 Rust 引擎
- 通过 PyO3/gRPC 创建引擎
- 传入 graph_config JSON
- 接收事件流
- **验证**: 事件格式正确

#### TC-INT-002: 节点回调到 Python
- Rust 引擎执行到 LLM 节点
- 回调 Python 的 NodeExecutor
- 接收 Python 返回的结果
- **验证**: 结果正确传递

#### TC-INT-003: 命令通道
- Python 发送 Abort 命令到 Rust 引擎
- **验证**: 引擎正确响应

---

## 9. 附录

### 9.1 graph_config JSON 格式示例

```json
{
    "nodes": [
        {
            "id": "node_start",
            "data": {
                "type": "start",
                "title": "Start",
                "variables": [
                    {
                        "variable": "query",
                        "label": "Query",
                        "type": "string",
                        "required": true
                    }
                ]
            }
        },
        {
            "id": "node_llm",
            "data": {
                "type": "llm",
                "title": "LLM",
                "model": {
                    "provider": "openai",
                    "name": "gpt-4",
                    "mode": "chat",
                    "completion_params": {
                        "temperature": 0.7
                    }
                },
                "prompt_template": [
                    {
                        "role": "system",
                        "text": "You are a helpful assistant."
                    },
                    {
                        "role": "user",
                        "text": "{{#sys.query#}}"
                    }
                ]
            }
        },
        {
            "id": "node_end",
            "data": {
                "type": "end",
                "title": "End",
                "outputs": [
                    {
                        "variable": "result",
                        "value_selector": ["node_llm", "text"]
                    }
                ]
            }
        }
    ],
    "edges": [
        {
            "id": "edge_1",
            "source": "node_start",
            "target": "node_llm",
            "sourceHandle": "source"
        },
        {
            "id": "edge_2",
            "source": "node_llm",
            "target": "node_end",
            "sourceHandle": "source"
        }
    ]
}
```

### 9.2 IfElse 条件配置示例

```json
{
    "type": "if-else",
    "conditions": [
        {
            "id": "condition_1",
            "logical_operator": "and",
            "conditions": [
                {
                    "variable_selector": ["node_start", "number"],
                    "comparison_operator": ">",
                    "value": "5"
                },
                {
                    "variable_selector": ["node_start", "number"],
                    "comparison_operator": "<",
                    "value": "100"
                }
            ]
        },
        {
            "id": "condition_2",
            "logical_operator": "or",
            "conditions": [
                {
                    "variable_selector": ["node_start", "text"],
                    "comparison_operator": "contains",
                    "value": "hello"
                }
            ]
        }
    ]
}
```

### 9.3 错误策略配置示例

```json
{
    "error_strategy": "fail-branch",
    "retry_config": {
        "max_retries": 3,
        "retry_interval": 1000
    },
    "default_value": {
        "text": "fallback value",
        "status": "error"
    }
}
```

### 9.4 Iteration 节点配置示例

```json
{
    "type": "iteration",
    "iterator_selector": ["node_start", "items"],
    "output_selector": ["iteration_node_code", "result"],
    "is_parallel": false,
    "parallel_nums": 1,
    "error_handle_mode": "terminated"
}
```

### 9.5 Python 源码关键文件索引

| 文件 | 说明 |
|------|------|
| `core/workflow/graph_engine/graph_engine.py` | 图执行引擎主类 |
| `core/workflow/graph/graph.py` | 图数据结构 |
| `core/workflow/graph/edge.py` | 边定义 |
| `core/workflow/graph/node.py` | 节点定义 |
| `core/workflow/runtime/graph_runtime_state.py` | 运行时状态 |
| `core/workflow/workflow_entry.py` | 工作流入口 |
| `core/workflow/enums.py` | 所有枚举定义 |
| `core/workflow/graph_events/` | 事件类型定义 |
| `core/workflow/node_events/base.py` | NodeRunResult 定义 |
| `core/workflow/nodes/base/` | 基础节点类 |
| `core/workflow/nodes/start/` | Start 节点 |
| `core/workflow/nodes/end/` | End 节点 |
| `core/workflow/nodes/llm/` | LLM 节点 |
| `core/workflow/nodes/if_else/` | IfElse 节点 |
| `core/workflow/nodes/code/` | Code 节点 |
| `core/workflow/nodes/iteration/` | Iteration 节点 |
| `core/workflow/nodes/loop/` | Loop 节点 |
| `core/workflow/nodes/http_request/` | HTTP 请求节点 |
| `core/workflow/nodes/tool/` | 工具节点 |
| `core/workflow/nodes/answer/` | Answer 节点 |
| `core/workflow/nodes/question_classifier/` | 问题分类节点 |
| `core/workflow/nodes/parameter_extractor/` | 参数提取节点 |
| `core/workflow/nodes/variable_aggregator/` | 变量聚合节点 |
| `core/workflow/nodes/variable_assigner/` | 变量赋值节点 |
| `core/workflow/nodes/template_transform/` | 模板转换节点 |
| `core/workflow/nodes/document_extractor/` | 文档提取节点 |
| `core/workflow/nodes/list_operator/` | 列表操作节点 |
| `core/workflow/nodes/agent/` | Agent 节点 |
| `core/workflow/graph_engine/graph_state_manager.py` | 状态管理器 |
| `core/workflow/graph_engine/event_management/` | 事件管理 |
| `core/workflow/graph_engine/graph_traversal/` | 图遍历 |
| `core/workflow/graph_engine/orchestration/` | 编排协调 |
| `core/workflow/graph_engine/worker_management/` | 工作线程管理 |
| `core/workflow/graph_engine/error_handling/` | 错误处理 |
