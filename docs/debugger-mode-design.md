# 工作流调试器模式设计文档

## 1. 背景与动机

### 1.1 当前执行模式的局限性

xworkflow 的 `WorkflowDispatcher::run()`（`src/core/dispatcher.rs:76-499`）采用队列式 DAG 执行：

```
queue.pop(node_id)
  → 检查 max_steps / timeout
  → 获取 node_info
  → 跳过 Skipped 节点
  → emit NodeRunStarted
  → clone pool snapshot
  → 执行节点（含 retry）
  → 写入 VariablePool
  → emit NodeRunSucceeded
  → mark node Taken
  → process edges (branch/normal)
  → enqueue downstream ready nodes
```

一旦启动，这个循环**连续执行直到结束或失败**，中间无法暂停。虽然代码中已定义了 `Command` 枚举（`dispatcher.rs:17-23`）包含 `Pause` 和 `Abort`，但**未被集成到执行循环中**。

### 1.2 调试需求场景

| 场景 | 说明 |
|------|------|
| **单步执行** | 逐节点执行，每步暂停查看状态，确认执行路径是否符合预期 |
| **断点调试** | 在指定节点前暂停，快速跳过已验证的节点，聚焦问题区域 |
| **变量检查** | 暂停时查看 VariablePool 完整快照，确认中间变量是否正确 |
| **变量修改** | 暂停时修改变量值，测试不同输入下后续节点的行为 |
| **分支路径验证** | 在 if-else 节点前暂停，检查条件变量，预判分支选择 |
| **错误定位** | 节点执行失败时，检查该节点的输入是否符合预期 |

### 1.3 核心约束

**零开销原则**：调试器模式**不得影响**正常执行模式的性能。正常模式下不能有：
- 额外的 channel 检查
- 额外的内存分配
- 额外的虚函数调用
- 额外的条件分支开销

## 2. 方案选型

### 2.1 候选方案对比

| 方案 | 描述 | 零开销 | 代码重复 | 实现复杂度 |
|------|------|--------|---------|-----------|
| **A: 注入 command channel** | 在 `run()` 循环中每步 `try_recv` | 否（每步一次系统调用） | 无 | 低 |
| **B: 独立 DebugDispatcher** | 复制 `run()` 逻辑并加入调试功能 | 是 | **高**（~400 行重复） | 中 |
| **C: DebugHook trait + 泛型分发** | `Dispatcher<G: DebugGate, H: DebugHook>`，正常模式用 `NoopHook` | **是** | **无** | 中 |
| **D: EngineConfig flag + channel** | `if config.debug { try_recv() }` | 近似零（一次 branch/node） | 无 | 低 |

### 2.2 选定方案：C（DebugHook trait + 泛型分发）

**选择理由**：

1. **真正的零开销**：Rust 编译器对泛型做单态化（monomorphization）。当 `H = NoopHook` 时，所有 hook 调用被内联为空操作并被 dead code elimination 完全消除。编译后的正常模式二进制中**不存在任何调试代码**。

2. **无代码重复**：执行循环只有一份，调试逻辑通过 trait 方法注入，避免了方案 B 的维护负担。

3. **符合 Rust 惯例**：这是 Rust 生态中处理可选行为的标准模式（类似 `std::alloc::GlobalAlloc`、`tracing` 的 `Subscriber` 模式）。

4. **可扩展**：未来可实现不同的 hook（如性能分析 hook、日志记录 hook）而不影响核心循环。

5. **兼容现有设计**：已定义的 `Command::Pause/Abort/UpdateVariables` 自然融入 `DebugHook` 的实现中。

## 3. 核心类型定义

### 3.1 DebugGate trait（同步判断）

`DebugGate` 是一个**同步** trait，用于快速判断是否需要在某个节点暂停。将判断逻辑从异步 hook 中分离出来，是避免 `async_trait` 的 `Box<dyn Future>` 堆分配开销的关键。

```rust
// src/core/debug.rs

/// 同步调试门控 trait，判断是否需要在某个节点暂停。
///
/// 正常模式使用 NoopGate（编译器内联优化为空），
/// 调试模式使用 InteractiveDebugGate（检查断点和步进模式）。
pub trait DebugGate: Send + Sync {
    /// 节点执行前，是否需要暂停？
    fn should_pause_before(&self, node_id: &str) -> bool;
    /// 节点执行后，是否需要暂停？
    fn should_pause_after(&self, node_id: &str) -> bool;
}
```

### 3.2 DebugHook trait（异步执行）

仅当 `DebugGate` 返回 `true` 时才调用，因此 `async_trait` 的开销只出现在调试模式中：

```rust
/// 异步调试 hook trait，在需要暂停时执行交互逻辑。
///
/// 仅在 DebugGate 返回 true 时被调用。
#[async_trait::async_trait]
pub trait DebugHook: Send + Sync {
    /// 节点执行前暂停。返回 DebugAction 控制后续行为。
    async fn before_node_execute(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction>;

    /// 节点执行后暂停。传入执行结果。
    async fn after_node_execute(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        result: &NodeRunResult,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction>;
}
```

### 3.3 NoopGate / NoopHook（零开销实现）

```rust
/// 空操作 Gate，编译期完全优化掉
pub struct NoopGate;

impl DebugGate for NoopGate {
    #[inline(always)]
    fn should_pause_before(&self, _node_id: &str) -> bool { false }
    #[inline(always)]
    fn should_pause_after(&self, _node_id: &str) -> bool { false }
}

/// 空操作 Hook（仅作为泛型占位，实际不会被调用）
pub struct NoopHook;

#[async_trait::async_trait]
impl DebugHook for NoopHook {
    async fn before_node_execute(
        &self, _: &str, _: &str, _: &str, _: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        Ok(DebugAction::Continue)
    }

    async fn after_node_execute(
        &self, _: &str, _: &str, _: &str, _: &NodeRunResult, _: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        Ok(DebugAction::Continue)
    }
}
```

### 3.4 DebugAction（调试动作）

```rust
/// 调试器返回的动作指令
#[derive(Debug, Clone)]
pub enum DebugAction {
    /// 继续执行
    Continue,
    /// 中止工作流
    Abort { reason: String },
    /// 更新变量后继续
    UpdateVariables {
        variables: HashMap<String, Value>,
        then: Box<DebugAction>,
    },
    /// 跳过当前节点（仅 before_node_execute 时有效）
    SkipNode,
}
```

### 3.5 调试器配置

```rust
/// 调试器配置
#[derive(Debug, Clone, Default)]
pub struct DebugConfig {
    /// 初始断点集合（节点 ID）
    pub breakpoints: HashSet<String>,
    /// 是否从第一个节点开始暂停
    pub break_on_start: bool,
    /// 暂停时是否自动发送变量池快照
    pub auto_snapshot: bool,
}
```

### 3.6 调试命令

```rust
/// 外部调试客户端发送的命令
#[derive(Debug, Clone)]
pub enum DebugCommand {
    /// 单步执行：运行下一个节点后暂停
    Step,
    /// 步过（当前 DAG 执行模式与 Step 等价）
    StepOver,
    /// 继续运行直到下一个断点或结束
    Continue,
    /// 中止执行
    Abort { reason: Option<String> },
    /// 添加断点
    AddBreakpoint { node_id: String },
    /// 移除断点
    RemoveBreakpoint { node_id: String },
    /// 清除所有断点
    ClearBreakpoints,
    /// 查看变量池快照
    InspectVariables,
    /// 查看特定节点的变量
    InspectNodeVariables { node_id: String },
    /// 修改变量
    UpdateVariables { variables: HashMap<String, Value> },
    /// 查询当前调试状态
    QueryState,
}
```

### 3.7 调试事件

```rust
/// 调试器向外发送的事件
#[derive(Debug, Clone)]
pub enum DebugEvent {
    /// 已暂停
    Paused {
        reason: PauseReason,
        location: PauseLocation,
    },
    /// 已恢复运行
    Resumed,
    /// 断点变更
    BreakpointAdded { node_id: String },
    BreakpointRemoved { node_id: String },
    /// 变量池快照
    VariableSnapshot {
        variables: HashMap<(String, String), Segment>,
    },
    /// 节点变量快照
    NodeVariableSnapshot {
        node_id: String,
        variables: HashMap<String, Segment>,
    },
    /// 当前调试状态报告
    StateReport {
        state: DebugState,
        breakpoints: HashSet<String>,
        step_count: i32,
    },
    /// 变量已修改
    VariablesUpdated {
        updated_keys: Vec<String>,
    },
    /// 调试器错误
    Error { message: String },
}
```

### 3.8 调试状态与暂停信息

```rust
/// 调试器运行状态
#[derive(Debug, Clone)]
pub enum DebugState {
    Running,
    Paused {
        location: PauseLocation,
        variable_snapshot: Option<HashMap<(String, String), Segment>>,
    },
    Finished,
}

/// 暂停位置
#[derive(Debug, Clone)]
pub enum PauseLocation {
    /// 节点执行前
    BeforeNode {
        node_id: String,
        node_type: String,
        node_title: String,
    },
    /// 节点执行后
    AfterNode {
        node_id: String,
        node_type: String,
        node_title: String,
        result: NodeRunResult,
    },
}

/// 暂停原因
#[derive(Debug, Clone)]
pub enum PauseReason {
    /// 命中断点
    Breakpoint,
    /// 单步执行
    Step,
    /// 用户手动请求暂停
    UserRequested,
    /// 首节点自动暂停（break_on_start）
    Initial,
}

/// 步进模式
#[derive(Debug, Clone, PartialEq)]
enum StepMode {
    /// 连续运行（直到断点）
    Run,
    /// 单步（执行一个节点后暂停）
    Step,
    /// 首次启动，尚未收到任何命令
    Initial,
}
```

## 4. InteractiveDebugHook 实现

### 4.1 结构定义

```rust
/// 交互式调试 hook，通过 channel 与外部调试客户端通信
pub struct InteractiveDebugHook {
    /// 接收调试命令
    cmd_rx: tokio::sync::Mutex<mpsc::Receiver<DebugCommand>>,
    /// 发送调试事件
    event_tx: mpsc::Sender<DebugEvent>,
    /// 调试配置（断点等）—— RwLock 因为断点可动态添加
    config: Arc<tokio::sync::RwLock<DebugConfig>>,
    /// 当前步进模式
    mode: Arc<tokio::sync::RwLock<StepMode>>,
}
```

### 4.2 InteractiveDebugGate 实现

```rust
pub struct InteractiveDebugGate {
    config: Arc<tokio::sync::RwLock<DebugConfig>>,
    mode: Arc<tokio::sync::RwLock<StepMode>>,
}

impl DebugGate for InteractiveDebugGate {
    fn should_pause_before(&self, node_id: &str) -> bool {
        // 使用 try_read 避免阻塞；如果锁被占用返回 false
        let mode = self.mode.try_read();
        let config = self.config.try_read();

        match (mode, config) {
            (Ok(mode), Ok(config)) => {
                *mode == StepMode::Step
                || (*mode == StepMode::Initial && config.break_on_start)
                || config.breakpoints.contains(node_id)
            }
            _ => false,
        }
    }

    fn should_pause_after(&self, _node_id: &str) -> bool {
        self.mode
            .try_read()
            .map(|m| *m == StepMode::Step)
            .unwrap_or(false)
    }
}
```

### 4.3 Hook 核心逻辑

```rust
#[async_trait::async_trait]
impl DebugHook for InteractiveDebugHook {
    async fn before_node_execute(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        // 确定暂停原因
        let reason = {
            let config = self.config.read().await;
            let mode = self.mode.read().await;
            if config.breakpoints.contains(node_id) {
                PauseReason::Breakpoint
            } else if *mode == StepMode::Step {
                PauseReason::Step
            } else {
                PauseReason::Initial
            }
        };

        // 发送暂停事件
        let _ = self.event_tx.send(DebugEvent::Paused {
            reason,
            location: PauseLocation::BeforeNode {
                node_id: node_id.to_string(),
                node_type: node_type.to_string(),
                node_title: node_title.to_string(),
            },
        }).await;

        // 如果配置了 auto_snapshot，自动发送变量快照
        if self.config.read().await.auto_snapshot {
            let _ = self.event_tx.send(DebugEvent::VariableSnapshot {
                variables: variable_pool.snapshot(),
            }).await;
        }

        // 等待命令
        self.wait_for_command(variable_pool).await
    }

    async fn after_node_execute(
        &self,
        node_id: &str,
        node_type: &str,
        node_title: &str,
        result: &NodeRunResult,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        let _ = self.event_tx.send(DebugEvent::Paused {
            reason: PauseReason::Step,
            location: PauseLocation::AfterNode {
                node_id: node_id.to_string(),
                node_type: node_type.to_string(),
                node_title: node_title.to_string(),
                result: result.clone(),
            },
        }).await;

        self.wait_for_command(variable_pool).await
    }
}
```

### 4.4 命令处理循环

```rust
impl InteractiveDebugHook {
    /// 等待并处理调试命令，直到收到继续/步进/中止指令
    async fn wait_for_command(
        &self,
        variable_pool: &VariablePool,
    ) -> WorkflowResult<DebugAction> {
        loop {
            let cmd = {
                let mut rx = self.cmd_rx.lock().await;
                rx.recv().await
            };

            match cmd {
                // === 继续执行类命令 ===
                Some(DebugCommand::Step) | Some(DebugCommand::StepOver) => {
                    *self.mode.write().await = StepMode::Step;
                    let _ = self.event_tx.send(DebugEvent::Resumed).await;
                    return Ok(DebugAction::Continue);
                }
                Some(DebugCommand::Continue) => {
                    *self.mode.write().await = StepMode::Run;
                    let _ = self.event_tx.send(DebugEvent::Resumed).await;
                    return Ok(DebugAction::Continue);
                }
                Some(DebugCommand::Abort { reason }) => {
                    return Ok(DebugAction::Abort {
                        reason: reason.unwrap_or_else(|| "User aborted".into()),
                    });
                }

                // === 断点管理命令（不返回，继续等待） ===
                Some(DebugCommand::AddBreakpoint { node_id }) => {
                    self.config.write().await.breakpoints.insert(node_id.clone());
                    let _ = self.event_tx
                        .send(DebugEvent::BreakpointAdded { node_id })
                        .await;
                }
                Some(DebugCommand::RemoveBreakpoint { node_id }) => {
                    self.config.write().await.breakpoints.remove(&node_id);
                    let _ = self.event_tx
                        .send(DebugEvent::BreakpointRemoved { node_id })
                        .await;
                }
                Some(DebugCommand::ClearBreakpoints) => {
                    self.config.write().await.breakpoints.clear();
                }

                // === 检查命令（不返回，继续等待） ===
                Some(DebugCommand::InspectVariables) => {
                    let _ = self.event_tx.send(DebugEvent::VariableSnapshot {
                        variables: variable_pool.snapshot(),
                    }).await;
                }
                Some(DebugCommand::InspectNodeVariables { node_id }) => {
                    let vars = variable_pool.get_node_variables(&node_id);
                    let _ = self.event_tx.send(DebugEvent::NodeVariableSnapshot {
                        node_id,
                        variables: vars,
                    }).await;
                    
                }

                // === 变量修改命令（返回 UpdateVariables action） ===
                Some(DebugCommand::UpdateVariables { variables }) => {
                    let keys: Vec<String> = variables.keys().cloned().collect();
                    let _ = self.event_tx.send(DebugEvent::VariablesUpdated {
                        updated_keys: keys,
                    }).await;
                    return Ok(DebugAction::UpdateVariables {
                        variables,
                        then: Box::new(DebugAction::Continue),
                    });
                }

                // === 状态查询命令（不返回，继续等待） ===
                Some(DebugCommand::QueryState) => {
                    let config = self.config.read().await;
                    let _ = self.event_tx.send(DebugEvent::StateReport {
                        state: DebugState::Paused {
                            location: PauseLocation::BeforeNode {
                                node_id: String::new(),
                                node_type: String::new(),
                                node_title: String::new(),
                            },
                            variable_snapshot: None,
                        },
                        breakpoints: config.breakpoints.clone(),
                        step_count: 0,
                    }).await;
                }

                // === Channel 关闭 ===
                None => {
                    return Ok(DebugAction::Abort {
                        reason: "Debug channel closed".into(),
                    });
                }
            }
        }
    }
}
```

## 5. Dispatcher 集成

### 5.1 泛型化 WorkflowDispatcher

在 `src/core/dispatcher.rs` 中将 `WorkflowDispatcher` 改为泛型结构，使用默认类型参数保证向后兼容：

```rust
pub struct WorkflowDispatcher<G: DebugGate = NoopGate, H: DebugHook = NoopHook> {
    graph: Arc<RwLock<Graph>>,
    variable_pool: Arc<RwLock<VariablePool>>,
    registry: Arc<NodeExecutorRegistry>,
    event_tx: mpsc::Sender<GraphEngineEvent>,
    config: EngineConfig,
    exceptions_count: i32,
    context: Arc<RuntimeContext>,
    plugin_manager: Option<Arc<PluginManager>>,
    // 新增
    debug_gate: G,
    debug_hook: H,
}
```

### 5.2 构造函数

现有 `new()` 签名**完全不变**，利用默认泛型参数：

```rust
impl WorkflowDispatcher<NoopGate, NoopHook> {
    pub fn new(
        graph: Graph,
        variable_pool: VariablePool,
        registry: NodeExecutorRegistry,
        event_tx: mpsc::Sender<GraphEngineEvent>,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
        plugin_manager: Option<Arc<PluginManager>>,
    ) -> Self {
        // ... 现有代码不变 ...
        // debug_gate: NoopGate,
        // debug_hook: NoopHook,
    }
}
```

新增带调试功能的构造函数：

```rust
impl<G: DebugGate, H: DebugHook> WorkflowDispatcher<G, H> {
    pub fn new_with_debug(
        graph: Graph,
        variable_pool: VariablePool,
        registry: NodeExecutorRegistry,
        event_tx: mpsc::Sender<GraphEngineEvent>,
        config: EngineConfig,
        context: Arc<RuntimeContext>,
        plugin_manager: Option<Arc<PluginManager>>,
        debug_gate: G,
        debug_hook: H,
    ) -> Self { ... }
}
```

### 5.3 在 run() 循环中插入 hook 调用

在 `run()` 方法的两个关键位置插入 hook 调用（用 `// [DEBUG]` 标记）：

```rust
pub async fn run(&mut self) -> WorkflowResult<HashMap<String, Value>> {
    // ... 现有初始化代码 ...

    while let Some(node_id) = queue.pop() {
        // ... 现有 step/time 检查 ...
        // ... 现有 node_info 获取 ...
        // ... 现有 skipped 检查 ...

        // ========================================
        // [DEBUG] Hook 调用点 1: 节点执行前
        // ========================================
        if self.debug_gate.should_pause_before(&node_id) {
            let pool = self.variable_pool.read().await;
            let action = self.debug_hook
                .before_node_execute(&node_id, &node_type, &node_title, &pool)
                .await?;
            drop(pool);

            match self.apply_debug_action(action).await? {
                DebugActionResult::Continue => {}
                DebugActionResult::Abort(reason) => {
                    return Err(WorkflowError::Aborted(reason));
                }
                DebugActionResult::SkipNode => continue,
            }
        }

        // ... 现有 emit NodeRunStarted ...
        // ... 现有 plugin before_node_execute hook ...
        // ... 现有 node execution (含 retry) ...

        match run_result {
            Ok(result) => {
                // ... 现有 store outputs, emit events, process edges ...

                // ========================================
                // [DEBUG] Hook 调用点 2: 节点执行后
                // ========================================
                if self.debug_gate.should_pause_after(&node_id) {
                    let pool = self.variable_pool.read().await;
                    let action = self.debug_hook
                        .after_node_execute(
                            &node_id, &node_type, &node_title,
                            &result, &pool,
                        )
                        .await?;
                    drop(pool);

                    match self.apply_debug_action(action).await? {
                        DebugActionResult::Continue => {}
                        DebugActionResult::Abort(reason) => {
                            return Err(WorkflowError::Aborted(reason));
                        }
                        _ => {}
                    }
                }

                // ... 现有 enqueue downstream ...
            }
            Err(e) => { /* 现有错误处理不变 */ }
        }
    }

    // ... 现有终结逻辑 ...
}
```

### 5.4 apply_debug_action 辅助方法

```rust
/// 调试动作处理结果
enum DebugActionResult {
    Continue,
    Abort(String),
    SkipNode,
}

impl<G: DebugGate, H: DebugHook> WorkflowDispatcher<G, H> {
    async fn apply_debug_action(
        &self,
        action: DebugAction,
    ) -> WorkflowResult<DebugActionResult> {
        match action {
            DebugAction::Continue => Ok(DebugActionResult::Continue),
            DebugAction::Abort { reason } => Ok(DebugActionResult::Abort(reason)),
            DebugAction::SkipNode => Ok(DebugActionResult::SkipNode),
            DebugAction::UpdateVariables { variables, then } => {
                // 应用变量修改到 VariablePool
                let mut pool = self.variable_pool.write().await;
                for (key, value) in &variables {
                    // key 格式: "node_id.var_name"
                    let parts: Vec<&str> = key.splitn(2, '.').collect();
                    if parts.len() == 2 {
                        pool.set(
                            &[parts[0].to_string(), parts[1].to_string()],
                            Segment::from_value(value),
                        );
                    }
                }
                drop(pool);
                // 递归处理后续动作
                self.apply_debug_action(*then).await
            }
        }
    }
}
```

### 5.5 零开销证明

当 `G = NoopGate` 时，编译器的优化过程：

```
源代码:
  if self.debug_gate.should_pause_before(&node_id) { ... }

单态化后:
  if NoopGate::should_pause_before(&self.debug_gate, &node_id) { ... }

内联后:
  if false { ... }

Dead code elimination 后:
  (空)
```

最终编译结果中：
- 无 `should_pause_before` 函数调用
- 无 `before_node_execute` 函数调用
- 无 `VariablePool::read()` 获取锁的操作
- 无 `DebugAction` 匹配分支
- `NoopHook` 的 `before_node_execute` / `after_node_execute` 方法不会出现在二进制中

## 6. 事件系统扩展

### 6.1 GraphEngineEvent 新增事件

在 `src/core/event_bus.rs` 的 `GraphEngineEvent` 中新增：

```rust
pub enum GraphEngineEvent {
    // ... 现有事件不变 ...

    // === Debug events ===
    DebugPaused {
        reason: String,     // "breakpoint" | "step" | "initial" | "user_requested"
        node_id: String,
        node_type: String,
        node_title: String,
        phase: String,      // "before" | "after"
    },
    DebugResumed,
    DebugBreakpointChanged {
        action: String,     // "added" | "removed" | "cleared"
        node_id: Option<String>,
    },
    DebugVariableSnapshot {
        data: Value,        // 序列化后的变量池快照
    },
}
```

### 6.2 to_json 序列化

```rust
GraphEngineEvent::DebugPaused { reason, node_id, node_type, node_title, phase } => json!({
    "event": "debug_paused",
    "reason": reason,
    "node_id": node_id,
    "node_type": node_type,
    "node_title": node_title,
    "phase": phase,
}),
GraphEngineEvent::DebugResumed => json!({
    "event": "debug_resumed",
}),
GraphEngineEvent::DebugBreakpointChanged { action, node_id } => json!({
    "event": "debug_breakpoint_changed",
    "action": action,
    "node_id": node_id,
}),
GraphEngineEvent::DebugVariableSnapshot { data } => json!({
    "event": "debug_variable_snapshot",
    "data": data,
}),
```

### 6.3 调试模式事件时序

```
正常执行事件流:
  GraphRunStarted
  NodeRunStarted { node_id: "start" }
  NodeRunSucceeded { node_id: "start" }
  NodeRunStarted { node_id: "code_1" }
  NodeRunSucceeded { node_id: "code_1" }
  ...
  GraphRunSucceeded

调试模式事件流 (break_on_start=true, breakpoints=["code_1"]):
  GraphRunStarted

  DebugPaused { reason: "initial", node_id: "start", phase: "before" }
  -- 用户发送 Step 命令 --
  DebugResumed

  NodeRunStarted { node_id: "start" }
  NodeRunSucceeded { node_id: "start" }

  DebugPaused { reason: "step", node_id: "start", phase: "after" }
  -- 用户发送 Continue 命令 --
  DebugResumed

  DebugPaused { reason: "breakpoint", node_id: "code_1", phase: "before" }
  -- 用户 InspectVariables --
  DebugVariableSnapshot { data: {...} }
  -- 用户发送 Continue 命令 --
  DebugResumed

  NodeRunStarted { node_id: "code_1" }
  NodeRunSucceeded { node_id: "code_1" }
  ...
  GraphRunSucceeded
```

> **注意**：调试事件（`DebugPaused` / `DebugResumed` 等）通过主工作流的 `event_tx` 发送，与节点执行事件共用同一个 channel，保证外部监听者可以在统一的事件流中看到完整的调试 + 执行事件交错。

## 7. DebugHandle 外部控制 API

### 7.1 结构定义

```rust
/// 调试器外部控制句柄，调试模式启动时返回
pub struct DebugHandle {
    /// 发送调试命令
    cmd_tx: mpsc::Sender<DebugCommand>,
    /// 接收调试事件
    event_rx: tokio::sync::Mutex<mpsc::Receiver<DebugEvent>>,
}
```

### 7.2 公开 API

```rust
impl DebugHandle {
    /// 单步执行：运行下一个节点后暂停
    pub async fn step(&self) -> Result<(), DebugError> {
        self.cmd_tx.send(DebugCommand::Step).await
            .map_err(|_| DebugError::ChannelClosed)
    }

    /// 继续运行至下一个断点或结束
    pub async fn continue_run(&self) -> Result<(), DebugError> {
        self.cmd_tx.send(DebugCommand::Continue).await
            .map_err(|_| DebugError::ChannelClosed)
    }

    /// 中止执行
    pub async fn abort(&self, reason: Option<String>) -> Result<(), DebugError> {
        self.cmd_tx.send(DebugCommand::Abort { reason }).await
            .map_err(|_| DebugError::ChannelClosed)
    }

    /// 添加断点
    pub async fn add_breakpoint(&self, node_id: &str) -> Result<(), DebugError> {
        self.cmd_tx.send(DebugCommand::AddBreakpoint {
            node_id: node_id.to_string(),
        }).await.map_err(|_| DebugError::ChannelClosed)
    }

    /// 移除断点
    pub async fn remove_breakpoint(&self, node_id: &str) -> Result<(), DebugError> {
        self.cmd_tx.send(DebugCommand::RemoveBreakpoint {
            node_id: node_id.to_string(),
        }).await.map_err(|_| DebugError::ChannelClosed)
    }

    /// 检查变量池
    pub async fn inspect_variables(&self) -> Result<(), DebugError> {
        self.cmd_tx.send(DebugCommand::InspectVariables).await
            .map_err(|_| DebugError::ChannelClosed)
    }

    /// 检查特定节点的变量
    pub async fn inspect_node(&self, node_id: &str) -> Result<(), DebugError> {
        self.cmd_tx.send(DebugCommand::InspectNodeVariables {
            node_id: node_id.to_string(),
        }).await.map_err(|_| DebugError::ChannelClosed)
    }

    /// 修改变量
    pub async fn update_variables(
        &self,
        variables: HashMap<String, Value>,
    ) -> Result<(), DebugError> {
        self.cmd_tx.send(DebugCommand::UpdateVariables { variables }).await
            .map_err(|_| DebugError::ChannelClosed)
    }

    /// 等待下一个调试事件
    pub async fn next_event(&self) -> Option<DebugEvent> {
        self.event_rx.lock().await.recv().await
    }

    /// 便捷方法：等待暂停事件
    pub async fn wait_for_pause(&self) -> Result<DebugEvent, DebugError> {
        loop {
            match self.next_event().await {
                Some(evt @ DebugEvent::Paused { .. }) => return Ok(evt),
                Some(_) => continue,
                None => return Err(DebugError::ChannelClosed),
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DebugError {
    #[error("Debug channel closed")]
    ChannelClosed,
    #[error("Debug timeout")]
    Timeout,
}
```

## 8. WorkflowRunner 集成

### 8.1 WorkflowRunnerBuilder 扩展

在 `src/scheduler.rs` 的 builder 中新增调试相关字段和方法：

```rust
pub struct WorkflowRunnerBuilder {
    // ... 现有字段不变 ...
    // 新增
    debug_config: Option<DebugConfig>,
}

impl WorkflowRunnerBuilder {
    // ... 现有方法不变 ...

    /// 启用调试模式
    pub fn debug(mut self, config: DebugConfig) -> Self {
        self.debug_config = Some(config);
        self
    }

    /// 现有 run() 方法完全不变（使用 NoopGate + NoopHook）
    pub async fn run(self) -> Result<WorkflowHandle, WorkflowError> {
        // ... 现有实现不变 ...
    }

    /// 以调试模式运行，返回 (WorkflowHandle, DebugHandle)
    pub async fn run_debug(
        self,
    ) -> Result<(WorkflowHandle, DebugHandle), WorkflowError> {
        let debug_config = self.debug_config.clone().unwrap_or_default();

        validate_workflow_schema(&self.schema)?;
        let graph = build_graph(&self.schema)?;

        // ... 构建 pool, registry（与 run() 相同）...

        // 创建调试通道
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (debug_evt_tx, debug_evt_rx) = mpsc::channel(256);

        // 构建共享状态
        let config_arc = Arc::new(tokio::sync::RwLock::new(debug_config.clone()));
        let mode_arc = Arc::new(tokio::sync::RwLock::new(
            if debug_config.break_on_start {
                StepMode::Initial
            } else {
                StepMode::Run
            },
        ));

        let gate = InteractiveDebugGate {
            config: config_arc.clone(),
            mode: mode_arc.clone(),
        };
        let hook = InteractiveDebugHook {
            cmd_rx: tokio::sync::Mutex::new(cmd_rx),
            event_tx: debug_evt_tx,
            config: config_arc,
            mode: mode_arc,
        };

        let (tx, mut rx) = mpsc::channel(256);
        // ... event collector spawn（同 run()）...

        // Spawn 调试模式的工作流执行
        let status_exec = status.clone();
        tokio::spawn(async move {
            let mut dispatcher = WorkflowDispatcher::new_with_debug(
                graph, pool, registry, tx, config, context,
                plugin_manager, gate, hook,
            );
            match dispatcher.run().await {
                Ok(outputs) => {
                    *status_exec.lock().await = ExecutionStatus::Completed(outputs);
                }
                Err(e) => {
                    *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
                }
            }
        });

        let workflow_handle = WorkflowHandle { status, events };
        let debug_handle = DebugHandle {
            cmd_tx,
            event_rx: tokio::sync::Mutex::new(debug_evt_rx),
        };

        Ok((workflow_handle, debug_handle))
    }
}
```

## 9. 使用示例

### 9.1 单步调试完整流程

```rust
use xworkflow::*;
use xworkflow::core::debug::{DebugConfig, DebugEvent};
use std::collections::{HashMap, HashSet};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let yaml = r#"
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
  - id: code_1
    data:
      type: code
      title: Process
      language: javascript
      code: |
        return { result: args.input + " processed" };
      variables:
        - variable: input
          value_selector: ["start", "query"]
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["code_1", "result"]
edges:
  - source: start
    target: code_1
  - source: code_1
    target: end
"#;

    let schema = parse_dsl(yaml, DslFormat::Yaml)?;

    let mut inputs = HashMap::new();
    inputs.insert("query".to_string(), serde_json::json!("hello"));

    // 启动调试模式
    let (handle, debug) = WorkflowRunner::builder(schema)
        .user_inputs(inputs)
        .debug(DebugConfig {
            breakpoints: HashSet::new(),
            break_on_start: true,    // 从第一个节点开始暂停
            auto_snapshot: true,     // 自动发送变量快照
        })
        .run_debug()
        .await?;

    // 等待首次暂停（start 节点前）
    let event = debug.wait_for_pause().await?;
    println!("[DEBUG] Paused: {:?}", event);

    // 单步执行 start 节点
    debug.step().await?;
    let event = debug.wait_for_pause().await?;
    println!("[DEBUG] After start: {:?}", event);

    // 在 code_1 前添加断点并继续
    debug.add_breakpoint("code_1").await?;
    debug.continue_run().await?;
    let event = debug.wait_for_pause().await?;
    println!("[DEBUG] Hit breakpoint at code_1: {:?}", event);

    // 检查变量
    debug.inspect_variables().await?;
    if let Some(DebugEvent::VariableSnapshot { variables }) = debug.next_event().await {
        println!("[DEBUG] Variables: {:?}", variables);
    }

    // 修改变量后继续
    let mut vars = HashMap::new();
    vars.insert(
        "start.query".to_string(),
        serde_json::json!("modified input"),
    );
    debug.update_variables(vars).await?;

    // 继续到结束
    debug.continue_run().await?;

    // 等待工作流完成
    let status = handle.wait().await;
    println!("Final status: {:?}", status);

    Ok(())
}
```

### 9.2 正常模式（零开销，无任何变化）

```rust
// 现有代码完全不变
let handle = WorkflowRunner::builder(schema)
    .user_inputs(inputs)
    .run()   // 使用 NoopGate + NoopHook，零开销
    .await?;
let status = handle.wait().await;
```

## 10. VariablePool 辅助方法

### 10.1 新增方法

在 `src/core/variable_pool.rs` 中新增两个辅助方法，供调试器使用：

```rust
impl VariablePool {
    // ... 现有方法不变 ...

    /// 返回变量池的完整快照
    pub fn snapshot(&self) -> HashMap<(String, String), Segment> {
        self.variables.clone()
    }

    /// 返回指定节点的所有输出变量
    pub fn get_node_variables(&self, node_id: &str) -> HashMap<String, Segment> {
        self.variables
            .iter()
            .filter(|((nid, _), _)| nid == node_id)
            .map(|((_, var_name), seg)| (var_name.clone(), seg.clone()))
            .collect()
    }
}
```

## 11. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src/core/debug.rs` | **新增** | 全部调试类型定义：`DebugGate`, `DebugHook`, `NoopGate`, `NoopHook`, `InteractiveDebugGate`, `InteractiveDebugHook`, `DebugHandle`, `DebugConfig`, `DebugCommand`, `DebugEvent`, `DebugAction`, `DebugState`, `PauseLocation`, `PauseReason`, `DebugError` |
| `src/core/mod.rs` | 修改 | 新增 `pub mod debug;` |
| `src/core/dispatcher.rs` | 修改 | (1) `WorkflowDispatcher` 增加泛型参数 `<G: DebugGate = NoopGate, H: DebugHook = NoopHook>`；(2) `new()` 保持不变；(3) 新增 `new_with_debug()`；(4) `run()` 循环中插入两处 hook 调用；(5) 新增 `apply_debug_action()` |
| `src/core/event_bus.rs` | 修改 | `GraphEngineEvent` 新增 `DebugPaused`, `DebugResumed`, `DebugBreakpointChanged`, `DebugVariableSnapshot`；`to_json()` 新增对应分支 |
| `src/core/variable_pool.rs` | 修改 | 新增 `snapshot()` 和 `get_node_variables()` 辅助方法 |
| `src/scheduler.rs` | 修改 | `WorkflowRunnerBuilder` 新增 `debug_config` 字段、`debug()` builder 方法、`run_debug()` 方法 |
| `src/lib.rs` | 修改 | re-export 调试类型 |

## 12. 实施阶段

| 阶段 | 内容 | 依赖 |
|------|------|------|
| **1** | 定义 `src/core/debug.rs`：所有类型（trait, enum, struct） | 无 |
| **2** | 实现 `NoopGate` + `NoopHook` | 阶段 1 |
| **3** | 泛型化 `WorkflowDispatcher`，保持现有 `new()` 兼容 | 阶段 2 |
| **4** | 在 `run()` 循环中插入 hook 调用点 | 阶段 3 |
| **5** | 运行现有全部测试，确认无回归 | 阶段 4 |
| **6** | 实现 `InteractiveDebugHook` + `InteractiveDebugGate` | 阶段 1 |
| **7** | 实现 `DebugHandle` | 阶段 6 |
| **8** | 扩展 `WorkflowRunnerBuilder`：`.debug()` + `run_debug()` | 阶段 3, 7 |
| **9** | 扩展 `GraphEngineEvent` 新增调试事件 | 阶段 1 |
| **10** | 新增 `VariablePool::snapshot()` / `get_node_variables()` | 无 |
| **11** | 编写调试模式集成测试 | 阶段 8, 10 |
| **12** | 性能回归 benchmark | 阶段 5 |

## 13. 测试策略

### 13.1 单元测试

| 测试 | 验证内容 |
|------|---------|
| `test_noop_gate_always_false` | `NoopGate::should_pause_before/after` 永远返回 `false` |
| `test_interactive_gate_breakpoint` | 设置断点后 `should_pause_before` 对目标节点返回 `true` |
| `test_interactive_gate_step_mode` | `StepMode::Step` 时所有节点返回 `true` |
| `test_apply_debug_action_continue` | `Continue` action 返回 `DebugActionResult::Continue` |
| `test_apply_debug_action_update_vars` | `UpdateVariables` action 正确修改 pool 并递归处理 `then` |
| `test_command_loop_step` | `Step` 命令设置 `StepMode::Step` 并返回 `Continue` |
| `test_command_loop_breakpoint_mgmt` | 添加/移除断点命令不中断等待循环 |

### 13.2 集成测试

| 测试 | 验证内容 |
|------|---------|
| `test_debug_break_on_start` | `break_on_start=true` 时在首个节点前暂停 |
| `test_debug_step_through` | 单步执行三节点工作流，验证每步暂停 |
| `test_debug_breakpoint_hit` | 设置断点，`continue` 运行后在断点处暂停 |
| `test_debug_dynamic_breakpoint` | 暂停时动态添加断点，`continue` 后命中 |
| `test_debug_inspect_variables` | 暂停时检查变量快照内容正确 |
| `test_debug_update_variables` | 修改变量后下游节点使用新值 |
| `test_debug_abort` | 暂停时发送 `Abort`，工作流正确终止 |
| `test_debug_branch_flow` | 调试 if-else 工作流，验证分支选择前可检查条件 |
| `test_normal_mode_unaffected` | 正常模式 `run()` 行为与调试引入前完全一致 |

### 13.3 性能回归测试

使用现有的 `bench_meso_dispatcher` benchmark：

1. 对比 `WorkflowDispatcher<NoopGate, NoopHook>` 与引入前的 `WorkflowDispatcher` 性能
2. 确认差异在噪音范围内（< 1%）
3. 可选：通过 `cargo asm` 检查关键循环的汇编输出，确认调试代码已被消除

## 14. 设计决策记录

### Q: 为什么用泛型而不是 `dyn DebugHook`？

`dyn DebugHook` 意味着每次 hook 调用都是虚函数调用（vtable 查找），即使是 `NoopHook` 也无法被内联优化。泛型单态化保证 `NoopHook` 的方法被编译器完全消除。

### Q: 为什么需要 DebugGate 和 DebugHook 两个 trait？

`DebugHook::before_node_execute` 使用 `async_trait`，会产生 `Box<dyn Future>` 堆分配。如果直接调用 hook，即使 `NoopHook` 也会有堆分配开销。`DebugGate` 是同步 trait，`NoopGate::should_pause_before()` 返回 `false` 后整个 `if` 块被消除，连 `async_trait` 的 `Box` 都不会触及。

### Q: 为什么不用 feature flag (`cfg(feature = "debugger")`)？

Feature flag 需要编译两个版本的二进制。泛型方案在单一二进制中同时支持正常模式和调试模式，部署更灵活。如果需要进一步减小二进制体积，可以将 `InteractiveDebugHook` 的实现放在 `cfg(feature = "debugger")` 后面作为可选优化。

### Q: Step 和 StepOver 有什么区别？

当前 xworkflow 的 DAG 执行是单线程顺序的（一次处理一个节点），因此 Step 和 StepOver 行为等价。未来如果引入并行节点执行（如 iteration 的并行模式在主循环层面），StepOver 可以跳过并行展开的子步骤，而 Step 会逐个暂停。

### Q: 为什么调试事件通过独立 channel 传递，而不是复用 event_tx？

调试事件（`DebugEvent`）和工作流事件（`GraphEngineEvent`）有不同的消费者：
- `GraphEngineEvent` 由 `WorkflowHandle::events()` 收集，面向工作流执行结果的监控
- `DebugEvent` 由 `DebugHandle` 消费，面向调试器交互

两者独立可以避免调试事件污染正常的事件流。但同时，关键的调试状态变更（Paused/Resumed）也会作为 `GraphEngineEvent::DebugPaused/DebugResumed` 发送到主事件流，确保外部监听者能看到完整的执行 + 调试事件交错。

### Q: 暂停时变量修改如何保证一致性？

变量修改通过 `DebugAction::UpdateVariables` 返回给 dispatcher，由 dispatcher 在 `apply_debug_action` 中直接写入 `VariablePool`。这保证了：
1. 修改发生在 dispatcher 的执行线程中，不存在竞态
2. 修改后的变量会被后续节点的 `pool_snapshot` 读取到
3. 已执行节点的结果不受影响（它们已经写入 pool）
