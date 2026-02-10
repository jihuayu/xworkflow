# 运行期性能优化设计文档

## 一、概述

### 1.1 背景

xworkflow 引擎在设计上已通过 `#[cfg(feature = "plugin-system")]` 实现了编译期零成本抽象——当插件系统 feature 未启用时，相关代码不参与编译。但在**运行期**，仍存在大量"功能未使用但逻辑仍执行"的场景，违反了零成本抽象原则：

> **你不为你不用的东西付费。**

本文档系统梳理所有运行期性能问题，提出优化方案，并按优先级排序。

### 1.2 核心原则

1. **运行期零成本**：可选功能在运行期未启用时，不应产生任何额外开销（含内存分配、CPU 计算、锁竞争）
2. **延迟求值**：数据只在确认被需要时才构建
3. **最小化克隆**：热路径上的 `.clone()` 必须有正当理由
4. **按需初始化**：重量级资源仅在首次使用时创建

### 1.3 涉及文件

| 文件 | 角色 |
|------|------|
| `src/core/dispatcher.rs` | 工作流调度主循环（最核心热路径） |
| `src/core/variable_pool.rs` | 变量池（高频读写） |
| `src/core/event_bus.rs` | 事件系统 |
| `src/template/engine.rs` | 模板引擎 |
| `src/nodes/executor.rs` | 节点执行器注册表 |
| `src/nodes/data_transform.rs` | 模板变换 / Code 节点 |
| `src/scheduler.rs` | 工作流 Runner 构建 |
| `src/sandbox/manager.rs` | 沙箱管理器 |
| `src/llm/executor.rs` | LLM 节点执行器 |

---

## 二、问题清单

### P0-01：Regex 每次调用重新编译

**文件**：`src/template/engine.rs:7, 19`

**现状**：

```rust
pub fn render_template(template: &str, pool: &VariablePool) -> String {
    let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();  // 每次调用编译
    // ...
}

pub async fn render_template_async(template: &str, pool: &VariablePool) -> String {
    let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();  // 每次调用编译
    // ...
}
```

`Regex::new()` 涉及正则 NFA/DFA 编译，开销约 5-50μs。在 LLM 流式节点中，`render_template_async` 可能被调用数百次。

**方案**：使用 `std::sync::LazyLock`（Rust 1.80+ 标准库）将 Regex 提升为模块级静态量：

```rust
use std::sync::LazyLock;

static TEMPLATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{#([^#]+)#\}\}").unwrap()
});
```

两个函数直接引用 `&*TEMPLATE_RE`，零额外开销。

---

### P0-02：Hook Payload 先构建后判空

**文件**：`src/core/dispatcher.rs`

**现状**（新插件系统，共 6 处调用点）：

```rust
// 行 237-243 — 典型模式
#[cfg(feature = "plugin-system")]
{
    let payload = serde_json::json!({           // ← 无条件分配
        "event": "before_workflow_run",
    });
    let _ = self.execute_hooks(HookPoint::BeforeWorkflowRun, payload).await?;
}
```

而 `execute_hooks` 内部（行 139-178）：

```rust
async fn execute_hooks(&self, hook_point: HookPoint, data: Value) -> ... {
    let mut results = Vec::new();
    if let Some(reg) = &self.plugin_registry {
        let mut handlers = reg.hooks(&hook_point);
        if handlers.is_empty() {
            return Ok(results);          // ← 此时 payload 已构建完毕
        }
        let pool_snapshot = self.variable_pool.read().await.clone();  // ← Pool 也已克隆
        // ...
    }
    Ok(results)
}
```

**问题链条**：
1. 调用方无条件构建 `serde_json::json!()` payload（堆分配 + 字符串构造）
2. `execute_hooks` 进入后才检查 `plugin_registry` 是否为 `Some`
3. 然后才检查该 HookPoint 是否有 handler
4. 即使 handler 为空，Pool 克隆在判空**之前**已完成

**受影响的 6 个调用点及其多余开销**：

| 调用点 | 行号 | 多余 payload 内容 |
|--------|------|-------------------|
| `BeforeWorkflowRun` | 239 | 简单 json |
| `BeforeNodeExecute` | 358 | 含 `node_config.clone()` |
| `AfterNodeExecute` | 525 | 含 `result.outputs.clone()` |
| `BeforeVariableWrite` | 574 | **循环内**，每输出变量 1 次 |
| `BeforeVariableWrite` (assigner) | 627 | 含 `output_val.clone()` |
| `AfterWorkflowRun` | 803 | 含 `self.final_outputs.clone()` |

**方案**：将判空逻辑前置到调用方，使用 `has_hooks()` 快速检查：

```rust
// 方案 A：在 execute_hooks 内延迟构建
// execute_hooks 改为接收闭包，仅在确认有 handler 时才调用闭包构建 payload
async fn execute_hooks<F>(
    &self,
    hook_point: HookPoint,
    payload_fn: F,
) -> WorkflowResult<Vec<Value>>
where
    F: FnOnce() -> Value,
{
    if let Some(reg) = &self.plugin_registry {
        let handlers = reg.hooks(&hook_point);
        if handlers.is_empty() {
            return Ok(Vec::new());
        }
        let data = payload_fn();                // ← 确认有 handler 后才构建
        let pool_snapshot = self.variable_pool.read().await.clone();
        // ... 执行 handlers
    }
    Ok(Vec::new())
}

// 调用方
self.execute_hooks(HookPoint::BeforeNodeExecute, || {
    serde_json::json!({
        "event": "before_node_execute",
        "node_id": node_id.clone(),
        // ...
    })
}).await?;
```

```rust
// 方案 B：调用方先判断（更轻量，无需改签名）
#[cfg(feature = "plugin-system")]
{
    if self.has_hooks(HookPoint::BeforeNodeExecute) {
        let payload = serde_json::json!({...});
        let _ = self.execute_hooks(HookPoint::BeforeNodeExecute, payload).await?;
    }
}
```

**推荐方案 A**，因为它不会遗漏判空，且闭包在无 handler 时零开销（不会被调用）。

---

### P0-03：Event 构建无条件执行

**文件**：`src/core/dispatcher.rs` — 所有 `event_tx.send()` 调用点

**现状**：

```rust
// 行 681-686 — 每个节点成功都触发
let _ = self.event_tx.send(GraphEngineEvent::NodeRunSucceeded {
    id: exec_id.clone(),                  // String clone
    node_id: node_id.clone(),             // String clone
    node_type: node_type.clone(),         // String clone
    node_run_result: result.clone(),      // NodeRunResult 深拷贝（含 HashMap<String, Value>）
}).await;
```

**问题**：
- `event_tx` 始终存在（构造函数要求传入 `mpsc::Sender`）
- 即使调用者不关心事件，Event payload 仍被完整构建
- `result.clone()` 是最大的开销——包含 outputs（`HashMap<String, Value>`）和 metadata 的完整深拷贝
- 每个节点执行至少产生 2 个 Event（Started + Succeeded/Failed），每个都含多次 String clone

**每个节点的 Event clone 开销**：

| Event | 行号 | clone 次数 |
|-------|------|-----------|
| `NodeRunStarted` | 328-334 | 4x String |
| `NodeRunRetry` | 430-437 | 5x String（含 error） |
| `NodeRunSucceeded` | 681-686 | 3x String + NodeRunResult 深拷贝 |
| `NodeRunException` | 672-678 | 3x String + NodeRunResult 深拷贝 + error |
| `NodeRunFailed` | 749-755 | 3x String + NodeRunResult + error |
| `GraphRunSucceeded` | 778-781 | final_outputs 整体克隆 |
| `GraphRunPartialSucceeded` | 773-776 | final_outputs 整体克隆 |

**方案**：引入可选 Event 发送器，无监听者时短路：

```rust
/// 包装 event_tx，提供零成本短路
struct EventEmitter {
    tx: mpsc::Sender<GraphEngineEvent>,
    /// receiver 是否仍然存活。当 receiver drop 后，此标记置 false。
    /// 使用 Arc<AtomicBool> 由 receiver 侧在 drop 时设置。
    active: Arc<AtomicBool>,
}

impl EventEmitter {
    #[inline(always)]
    fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    async fn emit(&self, event: GraphEngineEvent) {
        if self.is_active() {
            let _ = self.tx.send(event).await;
        }
    }
}
```

调用方改为：

```rust
if self.event_emitter.is_active() {
    self.event_emitter.emit(GraphEngineEvent::NodeRunSucceeded {
        // 仅在 active 时才构建
    }).await;
}
```

当无监听者时，`is_active()` 返回 `false`，跳过全部 clone 和构造逻辑。开销仅为一次 `AtomicBool` 的 Relaxed load（约 1ns）。

---

### P1-01：execute_hooks 内 Pool 克隆在 handler 判空之前

**文件**：`src/core/dispatcher.rs:153`

**现状**：

```rust
async fn execute_hooks(&self, hook_point: HookPoint, data: Value) -> ... {
    if let Some(reg) = &self.plugin_registry {
        let mut handlers = reg.hooks(&hook_point);
        if handlers.is_empty() {
            return Ok(results);              // ← 空了就返回
        }
        handlers.sort_by_key(|h| h.priority());
        let pool_snapshot = self.variable_pool.read().await.clone();  // ← 在非空之后，OK
```

实际上新版 `execute_hooks` 的 Pool 克隆在 `handlers.is_empty()` 检查之**后**，这一点没问题。但 **Pool 仍然是整体深克隆**，即使 handler 只需要读取其中一两个变量。

**方案**：将 `HookPayload.variable_pool` 改为 `Arc<VariablePool>` 的共享引用（只读快照），避免深克隆：

```rust
// 当前
pub struct HookPayload {
    pub variable_pool: Option<Arc<VariablePool>>,  // Arc 内是克隆出的新 Pool
}

// 优化后：直接共享 dispatcher 的 RwLock read guard 产出的引用
// 方案：在 dispatcher 中维护一个 Arc<VariablePool> 快照缓存
// 每次 pool 写入后标记脏，下次 hook 调用时才重新 snapshot
struct PoolSnapshotCache {
    snapshot: Option<Arc<VariablePool>>,
    dirty: bool,
}
```

或者更简单的做法——Hook handler 如果需要访问 Pool，传入 `&VariablePool` 引用（持有 read guard 期间调用 handler）：

```rust
async fn execute_hooks(&self, hook_point: HookPoint, data: Value) -> ... {
    // ...
    let pool_guard = self.variable_pool.read().await;
    let payload = HookPayload {
        hook_point,
        data,
        variable_pool: &*pool_guard,  // 借用，非克隆
        event_tx: Some(self.event_tx.clone()),
    };
    for handler in handlers {
        handler.handle(&payload).await?;
    }
    drop(pool_guard);
}
```

这需要调整 `HookPayload` 的生命周期，但完全消除了 Pool 克隆。

---

### P1-02：旧插件系统循环内 Pool 重复克隆

**文件**：`src/core/dispatcher.rs:540-562`

**现状**：

```rust
#[cfg(not(feature = "plugin-system"))]
if let Some(pm) = &self.plugin_manager {
    let pool_snapshot = self.variable_pool.read().await.clone();  // 克隆 1 次
    for (key, value) in outputs_for_write.iter_mut() {
        let payload = serde_json::json!({...});
        pm.execute_hooks(
            PluginHookType::BeforeVariableWrite,
            &payload,
            Arc::new(std::sync::RwLock::new(pool_snapshot.clone())),  // ← 循环内再克隆 N 次!
            Some(self.event_tx.clone()),                              // ← Sender 也克隆 N 次
        ).await?;
    }
}
```

**问题**：对于有 N 个输出的节点，Pool 被克隆 N+1 次，`event_tx` 被克隆 N 次。

**方案**：循环外用 `Arc` 包装一次，循环内只 clone `Arc`（仅增加引用计数，约 1ns）：

```rust
if let Some(pm) = &self.plugin_manager {
    let pool_snapshot = Arc::new(std::sync::RwLock::new(
        self.variable_pool.read().await.clone()  // 仅克隆 1 次
    ));
    let event_tx = Some(self.event_tx.clone());  // 仅克隆 1 次
    for (key, value) in outputs_for_write.iter_mut() {
        let payload = serde_json::json!({...});
        pm.execute_hooks(
            PluginHookType::BeforeVariableWrite,
            &payload,
            pool_snapshot.clone(),  // ← Arc clone，约 1ns
            event_tx.clone(),       // ← Sender clone
        ).await?;
    }
}
```

---

### P1-03：minijinja Environment 每次调用重建 + 模板重新解析

**文件**：`src/template/engine.rs:47-79`

**现状**：

```rust
pub fn render_jinja2_with_functions(
    template: &str,
    variables: &HashMap<String, Value>,
    functions: Option<&TemplateFunctionMap>,
) -> Result<String, String> {
    let mut env = minijinja::Environment::new();       // ← 每次新建 Environment
    // ... 注册 functions（如果有）
    env.add_template("tpl", template)?;                // ← 每次重新解析模板
    let tmpl = env.get_template("tpl")?;
    tmpl.render(ctx)
}
```

**问题**：在流式模板渲染中（`data_transform.rs:112, 141`），每个 stream chunk 都调用此函数。100 个 chunk = 100 次 Environment 创建 + 100 次模板解析。

**方案**：为流式场景提供支持模板预编译的版本：

```rust
/// 预编译模板，返回可复用的渲染上下文
pub struct CompiledTemplate {
    env: minijinja::Environment<'static>,
}

impl CompiledTemplate {
    pub fn new(
        template: &str,
        functions: Option<&TemplateFunctionMap>,
    ) -> Result<Self, String> {
        let mut env = minijinja::Environment::new();
        // 注册 functions...
        env.add_template("tpl", template.to_owned())
            .map_err(|e| format!("Template parse error: {}", e))?;
        Ok(Self { env })
    }

    pub fn render(&self, variables: &HashMap<String, Value>) -> Result<String, String> {
        let tmpl = self.env.get_template("tpl")
            .map_err(|e| format!("Template not found: {}", e))?;
        let ctx = minijinja::Value::from_serialize(variables);
        tmpl.render(ctx)
            .map_err(|e| format!("Template render error: {}", e))
    }
}
```

在 `data_transform.rs` 的流式循环外创建 `CompiledTemplate`，循环内只调用 `.render()`。

---

### P1-04：NodeExecutorRegistry 全量注册

**文件**：`src/nodes/executor.rs:33-60`

**现状**：每次工作流执行都调用 `NodeExecutorRegistry::new()`，注册全部 19 个执行器。其中 `CodeNodeExecutor::new()` 触发 `SandboxManager::new()`，内部初始化 JS 引擎（boa_engine）和 WASM 引擎（wasmtime）。

**方案**：懒注册——仅在首次 `get()` 对应类型时才初始化执行器：

```rust
pub struct NodeExecutorRegistry {
    executors: HashMap<String, Box<dyn NodeExecutor>>,
    /// 懒初始化工厂
    factories: HashMap<String, Box<dyn Fn() -> Box<dyn NodeExecutor> + Send + Sync>>,
}

impl NodeExecutorRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            executors: HashMap::new(),
            factories: HashMap::new(),
        };
        // 轻量执行器直接注册
        registry.register("start", Box::new(StartNodeExecutor));
        registry.register("end", Box::new(EndNodeExecutor));
        // ...

        // 重量级执行器延迟注册
        registry.register_lazy("code", Box::new(|| {
            Box::new(CodeNodeExecutor::new())
        }));
        registry
    }

    pub fn get(&mut self, node_type: &str) -> Option<&dyn NodeExecutor> {
        if !self.executors.contains_key(node_type) {
            if let Some(factory) = self.factories.remove(node_type) {
                self.executors.insert(node_type.to_string(), factory());
            }
        }
        self.executors.get(node_type).map(|e| e.as_ref())
    }
}
```

注意：这要求 `get()` 接收 `&mut self`。如果这个改动过大，替代方案是使用 `OnceCell` 做内部可变性：

```rust
pub struct NodeExecutorRegistry {
    executors: HashMap<String, OnceCell<Box<dyn NodeExecutor>>>,
    factories: HashMap<String, Box<dyn Fn() -> Box<dyn NodeExecutor> + Send + Sync>>,
}
```

**最小改动方案**：不改 Registry 接口，只在 `new()` 中避免注册 `CodeNodeExecutor`，改为在 `scheduler.rs` 中按需注册：

```rust
// scheduler.rs — 构建 registry 时
let mut registry = NodeExecutorRegistry::new();  // 不含 code 执行器

// 仅当 DSL 中存在 code 节点时才注册
if graph_contains_node_type(&graph, "code") {
    registry.register("code", Box::new(CodeNodeExecutor::new()));
}
```

---

### P2-01：流式模板 base_vars 每 chunk 全量克隆

**文件**：`src/nodes/data_transform.rs:112-118, 141-147`

**现状**：

```rust
// 对每个 chunk:
let mut vars = base_vars.clone();                     // HashMap 整体克隆
for (k, v) in &accumulated {
    vars.insert(k.clone(), Value::String(v.clone())); // 逐个 clone
}
render_jinja2_with_functions(&template_str, &vars, funcs_ref)?;
```

**方案**：在循环外预分配 `vars`，每次 chunk 只更新变化部分：

```rust
let mut vars = base_vars.clone();  // 循环外克隆一次

// 对每个 chunk:
// 仅更新变化的 accumulated 变量，base_vars 部分不变
for (k, v) in &accumulated {
    vars.insert(k.clone(), Value::String(v.clone()));
}
compiled_template.render(&vars)?;
// 无需 clear，下一次 insert 会覆盖相同 key
```

---

### P2-02：Event 收集器始终 spawn

**文件**：`src/scheduler.rs:459-469`

**现状**：

```rust
let events = Arc::new(Mutex::new(Vec::new()));
let events_clone = events.clone();
tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
        events_clone.lock().await.push(event);
    }
});
```

即使调用者只需要最终输出而不关心事件，仍然创建 `Arc<Mutex<Vec<_>>>`、spawn tokio task、以及 256 容量的 mpsc channel。

**方案**：提供两种 runner 模式——有事件收集和无事件收集：

```rust
// 方案：EventCollector 可选
enum EventSink {
    /// 不收集事件，receiver 立即 drop，sender 侧 send 失败但被忽略
    Discard,
    /// 收集到 Vec
    Collect(Arc<Mutex<Vec<GraphEngineEvent>>>),
}
```

当用户不需要事件时使用 `Discard`，此时 receiver 直接 drop。结合 P0-03 的 `EventEmitter.is_active()` 机制，sender 侧检测到 receiver 已 drop 后短路全部 Event 构建。

---

### P2-03：StreamReader::next() 全量 snapshot

**文件**：`src/core/variable_pool.rs:246-256`

**现状**：

```rust
pub async fn next(&mut self) -> Option<StreamEvent> {
    loop {
        let snapshot = self.stream.state.read().await;
        let status = snapshot.status.clone();
        let chunks = snapshot.chunks.clone();          // ← 克隆全部 chunks!
        let final_value = snapshot.final_value.clone();
        let error = snapshot.error.clone();
        drop(snapshot);
        if self.cursor < chunks.len() {
            let item = chunks[self.cursor].clone();    // ← 只需要这一个
            self.cursor += 1;
            return Some(StreamEvent::Chunk(item));
        }
```

**问题**：每次 `next()` 调用克隆整个 `Vec<Segment>` chunks，但只使用 `chunks[cursor]` 这一个元素。

**方案**：持有 read guard 期间直接索引取值，不克隆整个 Vec：

```rust
pub async fn next(&mut self) -> Option<StreamEvent> {
    loop {
        let snapshot = self.stream.state.read().await;

        if self.cursor < snapshot.chunks.len() {
            let item = snapshot.chunks[self.cursor].clone();  // 仅克隆 1 个
            self.cursor += 1;
            return Some(StreamEvent::Chunk(item));
        }

        match snapshot.status {
            StreamStatus::Running => {
                drop(snapshot);
                self.stream.notify.notified().await;
            }
            StreamStatus::Completed => {
                let val = snapshot.final_value.clone().unwrap_or(Segment::None);
                return Some(StreamEvent::End(val));
            }
            StreamStatus::Failed => {
                let err = snapshot.error.clone().unwrap_or_else(|| "stream failed".into());
                return Some(StreamEvent::Error(err));
            }
        }
    }
}
```

---

### P2-04：VariablePool.get() 键构造开销

**文件**：`src/core/variable_pool.rs` — `get()` 方法

**现状**：

```rust
pub fn get(&self, selector: &[String]) -> Segment {
    if selector.len() < 2 { return Segment::None; }
    let key = (selector[0].clone(), selector[1].clone());  // ← 2 次 String clone
    self.variables.get(&key).cloned().unwrap_or(Segment::None)
}
```

每次变量访问都要 clone 两个 String 来构造 HashMap key。在模板渲染中，每个 `{{#x.y#}}` 触发一次。

**方案**：改用支持借用查询的 key 类型。两种选择：

```rust
// 方案 A：使用 (String, String) key 但通过 Borrow trait 支持 (&str, &str) 查询
// 需要自定义 wrapper 类型来实现 Borrow

// 方案 B（更简单）：key 改为单个拼接字符串
type PoolKey = String;  // "node_id\0var_name"

pub fn get(&self, selector: &[String]) -> Segment {
    if selector.len() < 2 { return Segment::None; }
    // 构造临时 key，无需 clone
    let mut key = String::with_capacity(selector[0].len() + 1 + selector[1].len());
    key.push_str(&selector[0]);
    key.push('\0');
    key.push_str(&selector[1]);
    self.variables.get(&key).cloned().unwrap_or(Segment::None)
}
```

方案 B 更简单，且 `String::with_capacity` + `push_str` 比 2 次 `clone()` 的内存分配更少（仅 1 次分配 vs 2 次）。

---

### P3-01：node_config 子字段二次克隆

**文件**：`src/core/dispatcher.rs:276-282, 376-381`

**现状**：

```rust
// 第一次：从 Graph 取出整个 config
let node_config = node.config.clone();  // 完整 JSON Value 克隆

// 第二次：解析子字段
node_config.get("error_strategy")
    .and_then(|v| serde_json::from_value(v.clone()).ok());  // 子字段再 clone
```

**方案**：对子字段使用 `serde_json::from_value` 的零拷贝替代——由于 `from_value` 消费 `Value`，可以 take 而非 clone：

```rust
// 如果后续不再需要 node_config 中的 error_strategy 字段：
let error_strategy: Option<ErrorStrategyConfig> = node_config
    .as_object_mut()
    .and_then(|m| m.remove("error_strategy"))  // take，不 clone
    .and_then(|v| serde_json::from_value(v).ok());
```

但由于 `node_config` 后续还要传给 executor（行 394），不能 mutate。替代方案是跳过 clone、直接用 `serde_json::from_value(v.clone())` 但仅在字段存在时才执行——当前代码已经是这样（`.get()` 返回 `None` 时 `and_then` 短路），所以实际开销仅在配置了 error_strategy 的节点上，可接受。

**结论**：此项维持现状，优先级最低。

---

## 三、优化优先级总表

| 编号 | 优先级 | 问题 | 预计收益 | 改动范围 |
|------|--------|------|----------|----------|
| P0-01 | P0 | Regex 每次编译 | 每模板渲染节省 5-50μs | `engine.rs` 2 行 |
| P0-02 | P0 | Hook payload 先构建后判空 | 每节点省 6 次 JSON 分配 + clone | `dispatcher.rs` 方法签名 + 6 处调用 |
| P0-03 | P0 | Event 构建无条件执行 | 每节点省 2-3 次 NodeRunResult 深拷贝 | `dispatcher.rs` + 新增 `EventEmitter` |
| P1-01 | P1 | execute_hooks Pool 克隆优化 | 每 hook 省 1 次整池克隆 | `dispatcher.rs` + `hooks.rs` |
| P1-02 | P1 | 旧插件循环内 Pool 重复克隆 | N 次整池克隆 → 1 次 + N 次 Arc clone | `dispatcher.rs` 3 处循环 |
| P1-03 | P1 | minijinja Env + 模板缓存 | 每 chunk 省 1 次 Env 创建 + 模板解析 | `engine.rs` + `data_transform.rs` |
| P1-04 | P1 | 执行器懒注册 | 省 SandboxManager 重量级初始化 | `executor.rs` 或 `scheduler.rs` |
| P2-01 | P2 | 流式 vars 复用 | 每 chunk 省 1 次 HashMap 克隆 | `data_transform.rs` |
| P2-02 | P2 | Event 收集器可选 | 省 1 tokio task + Mutex | `scheduler.rs` |
| P2-03 | P2 | StreamReader 只取 cursor 元素 | 每次 next() 省整个 Vec 克隆 | `variable_pool.rs` |
| P2-04 | P2 | Pool get() 键构造优化 | 每次变量访问省 2 次 String clone | `variable_pool.rs` 全局改动 |
| P3-01 | P3 | node_config 子字段二次克隆 | 仅影响配置了 error_strategy 的节点 | 维持现状 |

---

## 四、实施建议

### 4.1 分批实施

**第一批（P0，立即可做）**：
- P0-01 Regex 静态化：独立改动，无依赖，测试简单
- P0-02 Hook payload 延迟构建：改 `execute_hooks` 签名为闭包模式
- P0-03 Event 短路：新增 `EventEmitter` wrapper

**第二批（P1，中等改动）**：
- P1-01 + P1-02 可合并：统一重构 Hook 调用中的 Pool 传递方式
- P1-03 minijinja 缓存：独立改动
- P1-04 执行器懒注册：可选最小方案（按需注册 code 执行器）

**第三批（P2，优化收尾）**：
- P2-01 ~ P2-04 均为局部优化，可独立完成

### 4.2 验证方法

每项优化完成后，应通过以下方式验证：

1. **功能测试**：确保现有 e2e 测试全部通过
2. **性能对比**：使用 `docs/benchmark-design.md` 中设计的 benchmark 框架，对比优化前后的：
   - 单节点执行时间（`criterion` 微基准）
   - 端到端工作流吞吐量
   - 内存分配次数（`dhat` 或 `jemalloc` profiler）
3. **零开销验证**：在无插件 / 无事件监听 / 无 code 节点的最小工作流上，验证相关代码路径被完全跳过（可通过条件断点或计数器确认）

### 4.3 注意事项

- **P0-02 闭包方案**的 `FnOnce` 闭包不能跨 `.await` 点捕获引用，如果 payload 构建需要 `&self` 数据，闭包需要捕获 clone 的值，但这些 clone 只在闭包被调用时才发生（即有 handler 时）
- **P1-01 借用方案**需要 `HookPayload` 添加生命周期参数或改为 `Arc` 共享，需评估对 trait 签名的影响
- **P1-04 懒注册**如果使用 `OnceCell` 方案，`get()` 方法签名保持 `&self` 不变，对外接口无侵入
- **P2-04 Pool key 改动**影响范围大（所有 `set`/`get`/`remove` 调用点），建议放到统一重构中
