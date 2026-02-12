# Checkpoint 选择性持久化设计文档

## 1. 概述

本文档定义 xworkflow 的**选择性检查点**机制，使 AI 工作流能够在高成本节点（agent、human-input）完成后保存状态，在故障后从检查点恢复，以及在人工审批节点暂停/恢复。

### 设计约束

xworkflow 的原始假设是"短时运行、不做持久化、纯内存"。AI 时代引入了新需求：

| 传统工作流 | AI 工作流 |
|-----------|----------|
| 毫秒级执行 | agent 节点可能跑数分钟 |
| 失败重跑成本≈0 | agent 失败重跑花钱+结果不同 |
| 无人工介入 | human-input 等待数小时 |
| 确定性 | LLM 输出不可复现 |

但 xworkflow 不应变成 Temporal/Cadence：

| Temporal 模式 | 本设计 |
|--------------|--------|
| 每步写数据库 | 仅在关键节点存检查点 |
| 强依赖 PostgreSQL | `CheckpointStore` trait，嵌入方自定义 |
| 每个 activity 序列化开销 | 绝大多数节点零开销 |
| 运维复杂 | 不配置 store 时行为完全不变 |

遵循项目三大原则：**Security > Performance > Obviousness**。

### 已有基础设施

| 组件 | 现状 | 对检查点的意义 |
|------|------|---------------|
| `VariablePool` 使用 `im::HashMap` | Copy-on-Write | `.snapshot()` 已存在，零拷贝快照天然支持 |
| `WorkflowNodeExecutionStatus::Paused` | 枚举已定义但未使用 | 可直接用于暂停语义 |
| `Command::Pause` | 枚举已定义 | 外部暂停指令通道已存在 |
| `Graph.node_states` | `HashMap<String, EdgeTraversalState>` | 可直接序列化 |
| `EventEmitter` | 事件总线 | 可发射检查点事件 |

---

## 2. CheckpointStore Trait

**文件**: 新建 `src/core/checkpoint.rs`

### 2.1 核心 Trait

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 检查点存储接口 — 嵌入方实现，决定存在哪里
///
/// 可能的实现：
/// - `MemoryCheckpointStore` — 内存中（测试/开发）
/// - `FileCheckpointStore` — 文件系统
/// - `SqliteCheckpointStore` — SQLite（轻量持久化）
/// - `RedisCheckpointStore` — Redis（分布式场景）
/// - 用户自定义
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    /// 保存检查点（覆盖同一 workflow_id 的旧检查点）
    async fn save(
        &self,
        workflow_id: &str,
        checkpoint: &Checkpoint,
    ) -> Result<(), CheckpointError>;

    /// 加载最近的检查点（不存在时返回 None）
    async fn load(
        &self,
        workflow_id: &str,
    ) -> Result<Option<Checkpoint>, CheckpointError>;

    /// 删除检查点（workflow 完成后清理）
    async fn delete(
        &self,
        workflow_id: &str,
    ) -> Result<(), CheckpointError>;
}
```

### 2.2 Checkpoint 数据结构

```rust
/// 检查点 — 工作流在某个节点完成后的完整快照
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Checkpoint {
    // === 标识 ===
    /// 工作流实例 ID
    pub workflow_id: String,
    /// 原始执行 ID（用于审计追踪关联）
    pub execution_id: String,
    /// 检查点创建时间戳（毫秒）
    pub created_at: i64,

    // === DAG 状态 ===
    /// 最后完成的节点 ID（检查点触发点）
    pub completed_node_id: String,
    /// 每个节点的遍历状态（Pending/Taken/Skipped/Cancelled）
    pub node_states: HashMap<String, SerializableEdgeState>,
    /// 每条边的遍历状态
    pub edge_states: HashMap<String, SerializableEdgeState>,
    /// 下一步待执行的节点 ID 列表
    pub ready_queue: Vec<String>,
    /// 前驱节点映射
    pub ready_predecessor: HashMap<String, String>,

    // === 变量状态 ===
    /// VariablePool 快照（通过 pool.snapshot() 获取）
    pub variables: HashMap<String, serde_json::Value>,

    // === 执行元数据 ===
    /// 已执行步数
    pub step_count: i32,
    /// 异常计数
    pub exceptions_count: i32,
    /// 已收集的最终输出
    pub final_outputs: HashMap<String, serde_json::Value>,
    /// 已消耗时间（秒）
    pub elapsed_secs: u64,

    // === 资源消耗（恢复 ResourceGovernor 状态） ===
    /// 检查点前累计的资源消耗（仅 security feature 下有意义）
    pub consumed_resources: Option<ConsumedResources>,
}

/// 累计资源消耗摘要 — 恢复时通知 ResourceGovernor
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ConsumedResources {
    /// 累计 LLM prompt tokens
    pub total_prompt_tokens: i64,
    /// 累计 LLM completion tokens
    pub total_completion_tokens: i64,
    /// 累计 LLM 调用成本
    pub total_llm_cost: f64,
    /// 累计 MCP tool 调用次数
    pub total_tool_calls: i64,
}

/// 可序列化的 EdgeTraversalState
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SerializableEdgeState {
    Pending,
    Taken,
    Skipped,
    Cancelled,
}

impl From<EdgeTraversalState> for SerializableEdgeState {
    fn from(state: EdgeTraversalState) -> Self {
        match state {
            EdgeTraversalState::Pending => Self::Pending,
            EdgeTraversalState::Taken => Self::Taken,
            EdgeTraversalState::Skipped => Self::Skipped,
            EdgeTraversalState::Cancelled => Self::Cancelled,
        }
    }
}

impl From<SerializableEdgeState> for EdgeTraversalState {
    fn from(state: SerializableEdgeState) -> Self {
        match state {
            SerializableEdgeState::Pending => Self::Pending,
            SerializableEdgeState::Taken => Self::Taken,
            SerializableEdgeState::Skipped => Self::Skipped,
            SerializableEdgeState::Cancelled => Self::Cancelled,
        }
    }
}
```

### 2.3 CheckpointError

```rust
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("Serialization error: {0}")]
    SerializationError(String),
    #[error("Storage error: {0}")]
    StorageError(String),
    #[error("Checkpoint not found for workflow: {0}")]
    NotFound(String),
    #[error("Checkpoint corrupted: {0}")]
    Corrupted(String),
}
```

### 设计决策

| 决策 | 理由 |
|------|------|
| `variables` 为 `HashMap<String, Value>` 而非 `HashMap<String, Segment>` | Segment 中 Stream 类型不可序列化；转为 Value 后所有类型都可安全序列化 |
| 仅存最近一个检查点（覆盖写） | 工作流是 DAG 不可回退，只需最新快照；减少存储 |
| `ready_queue` + `ready_predecessor` 保存 | 恢复时直接从队列继续，无需重新遍历 DAG |
| `elapsed_secs` 保存 | 恢复后继续计时，防止超时逻辑失效 |
| Node/Edge states 使用自定义可序列化类型 | `EdgeTraversalState` 原始类型未 derive Serialize |
| `execution_id` 保存 | 审计追踪需要关联同一次执行的 checkpoint 前后阶段 |
| `consumed_resources` 保存 | 恢复后通知 `ResourceGovernor`，防止配额计算偏差 |

### RuntimeContext 不存储

`WorkflowContext` / `RuntimeGroup` 的内容**不纳入检查点**。原因：

| RuntimeContext 字段 | 不存的理由 |
|------|------|
| `time_provider: Arc<dyn TimeProvider>` | trait object 不可序列化；恢复时重新创建 |
| `id_generator: Arc<dyn IdGenerator>` | trait object 不可序列化；恢复时重新创建 |
| `event_tx: mpsc::Sender` | 活跃 channel 不可序列化；恢复时新建 |
| `node_executor_registry` | 内含 `Box<dyn NodeExecutor>` 不可序列化 |
| `llm_provider_registry` | 内含 `Arc<dyn LlmProvider>` 不可序列化 |
| `http_client_provider` | 连接池，不可序列化 |
| `credential_provider` | `Arc<dyn>` 不可序列化 |
| `resource_governor` | `Arc<dyn>` 不可序列化；已消耗量通过 `consumed_resources` 恢复 |
| `audit_logger` | `Arc<dyn>` 不可序列化 |
| `sandbox_pool` | `Arc<dyn>` 不可序列化 |
| `template_functions` | `Arc<dyn>` 不可序列化 |
| `strict_template`, `group_id`, `security_policy`, `quota` | 配置类字段，由调用方通过 builder 重新提供 |

**职责分离**：检查点负责恢复引擎执行进度（DAG 状态 + 变量），调用方负责重建运行环境（Provider、凭证、安全策略）。恢复时调用方使用相同的 `WorkflowRunnerBuilder` 配置创建新的 runner。

---

## 3. VariablePool 序列化

### 3.1 现有能力

`VariablePool::snapshot()` 已存在（`src/core/variable_pool.rs:1532`）：

```rust
pub fn snapshot(&self) -> HashMap<String, Segment> {
    self.variables
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}
```

`Segment` 已实现 `Serialize` 和 `Deserialize`。

### 3.2 Stream 处理

`Segment::Stream` 包含异步状态（`RwLock<StreamState>`），不可直接序列化。

检查点时的处理策略：

```rust
/// 将 VariablePool 快照转换为可序列化的 HashMap<String, Value>
pub fn snapshot_for_checkpoint(pool: &VariablePool) -> HashMap<String, Value> {
    pool.snapshot()
        .into_iter()
        .filter_map(|(k, seg)| {
            match &seg {
                // Stream 类型：尝试提取已完成的值，未完成则跳过
                Segment::Stream(stream) => {
                    match stream.try_snapshot() {
                        Some(completed) => Some((k, completed.to_value())),
                        None => None,  // 流尚未完成，不纳入检查点
                    }
                }
                // 其他类型：直接转为 Value
                other => {
                    Some((k, serde_json::to_value(other).unwrap_or(Value::Null)))
                }
            }
        })
        .collect()
}
```

### 3.3 从检查点恢复 VariablePool

```rust
/// 从检查点数据重建 VariablePool
pub fn restore_from_checkpoint(
    variables: &HashMap<String, Value>,
) -> VariablePool {
    let mut pool = VariablePool::new();
    for (key, value) in variables {
        // 从 pool key 解析 Selector
        if let Some(selector) = Selector::from_pool_key(key) {
            pool.set(&selector, Segment::from_value(value));
        }
    }
    pool
}
```

---

## 4. Dispatcher 集成

### 4.1 WorkflowDispatcher 新增字段

**文件**: `src/core/dispatcher.rs`

```rust
pub struct WorkflowDispatcher<G: DebugGate = NoopGate, H: DebugHook = NoopHook> {
    // ... 现有字段 ...
    graph: Arc<RwLock<Graph>>,
    variable_pool: Arc<RwLock<VariablePool>>,
    registry: Arc<NodeExecutorRegistry>,
    // ...

    /// 可选的检查点存储（None = 无检查点，零开销）
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    /// Workflow ID（检查点的 key）
    workflow_id: String,
}
```

### 4.2 检查点策略 — 哪些节点触发

```rust
impl<G: DebugGate, H: DebugHook> WorkflowDispatcher<G, H> {
    /// 判断节点执行完成后是否需要保存检查点
    fn should_checkpoint_after(&self, node_type: &str, node_config: &Value) -> bool {
        // 没有 store → 永远不存
        if self.checkpoint_store.is_none() {
            return false;
        }

        match node_type {
            // agent 节点：总是存（高成本、非确定性）
            "agent" => true,
            // human-input 节点：执行前存（需要暂停等待）
            // 注意：human-input 是 before 策略，在 should_checkpoint_before 中处理
            "human-input" => false,
            // 其他节点：看配置中是否显式声明
            _ => node_config.get("checkpoint")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }
    }

    /// 判断节点执行前是否需要保存检查点
    fn should_checkpoint_before(&self, node_type: &str) -> bool {
        if self.checkpoint_store.is_none() {
            return false;
        }
        // human-input 节点执行前存检查点（因为即将暂停）
        node_type == "human-input"
    }
}
```

### 4.3 run() 方法修改

对 `run()` 的改动最小化。在现有循环中插入两处检查点逻辑：

```rust
pub async fn run(&mut self) -> WorkflowResult<HashMap<String, Value>> {
    self.event_emitter.emit(GraphEngineEvent::GraphRunStarted).await;
    self.emit_before_workflow_hooks().await?;

    // ===== 新增：尝试从检查点恢复 =====
    let (mut ready, mut ready_predecessor, mut step_count, start_time) =
        if let Some(resumed) = self.try_resume_from_checkpoint().await? {
            resumed
        } else {
            // 正常启动
            let root_id = self.graph.read().root_node_id().to_string();
            (vec![root_id], HashMap::new(), 0i32, self.context.time_provider.now_timestamp())
        };

    let mut join_set: JoinSet<NodeExecOutcome> = JoinSet::new();
    let mut running: HashMap<String, AbortHandle> = HashMap::new();
    let mut gather_wait_started: HashMap<String, i64> = HashMap::new();

    let (max_steps, max_exec_time) = self.effective_limits();
    // ... 现有 max_concurrency 逻辑 ...

    loop {
        // ... 现有 gather timeout 逻辑 ...
        // ... 现有 debug_index 逻辑 ...
        // ... 现有 parallel spawn 逻辑 ...
        // ... 现有 join_next 逻辑 ...

        match run_result {
            Ok(result) => {
                // ... 现有 handle_node_success 逻辑 ...

                // ===== 新增：节点成功后检查点 =====
                if self.should_checkpoint_after(
                    &outcome.info.node_type, &outcome.info.node_config
                ) {
                    self.save_checkpoint(
                        &outcome.node_id,
                        &ready,
                        &ready_predecessor,
                        step_count,
                        start_time,
                    ).await?;
                }
            }
            Err(e) => {
                // ... 现有错误处理 ...
            }
        }
    }

    // ===== 新增：完成后删除检查点 =====
    self.delete_checkpoint().await;

    // ... 现有 event emission ...
    self.emit_after_workflow_hooks().await?;
    Ok(self.final_outputs.clone())
}
```

### 4.4 检查点保存与恢复

```rust
impl<G: DebugGate, H: DebugHook> WorkflowDispatcher<G, H> {
    /// 保存检查点
    async fn save_checkpoint(
        &self,
        completed_node_id: &str,
        ready: &[String],
        ready_predecessor: &HashMap<String, String>,
        step_count: i32,
        start_time: i64,
    ) -> WorkflowResult<()> {
        let Some(store) = &self.checkpoint_store else {
            return Ok(());
        };

        let pool = self.variable_pool.read();
        let graph = self.graph.read();

        let checkpoint = Checkpoint {
            workflow_id: self.workflow_id.clone(),
            created_at: self.context.time_provider.now_millis(),

            completed_node_id: completed_node_id.to_string(),
            node_states: graph.node_states.iter()
                .map(|(k, v)| (k.clone(), (*v).into()))
                .collect(),
            edge_states: graph.edge_states.iter()
                .map(|(k, v)| (k.clone(), (*v).into()))
                .collect(),
            ready_queue: ready.to_vec(),
            ready_predecessor: ready_predecessor.clone(),

            variables: snapshot_for_checkpoint(&pool),

            step_count,
            exceptions_count: self.exceptions_count,
            final_outputs: self.final_outputs.clone(),
            elapsed_secs: self.context.time_provider.elapsed_secs(start_time),
        };

        store.save(&self.workflow_id, &checkpoint).await
            .map_err(|e| WorkflowError::InternalError(
                format!("Checkpoint save failed: {}", e)
            ))?;

        self.event_emitter.emit(GraphEngineEvent::CheckpointSaved {
            node_id: completed_node_id.to_string(),
        }).await;

        Ok(())
    }

    /// 尝试从检查点恢复
    async fn try_resume_from_checkpoint(
        &mut self,
    ) -> WorkflowResult<Option<(Vec<String>, HashMap<String, String>, i32, i64)>> {
        let Some(store) = &self.checkpoint_store else {
            return Ok(None);
        };

        let Some(cp) = store.load(&self.workflow_id).await
            .map_err(|e| WorkflowError::InternalError(
                format!("Checkpoint load failed: {}", e)
            ))?
        else {
            return Ok(None);
        };

        // 恢复 Graph 状态
        {
            let mut graph = self.graph.write();
            for (node_id, state) in &cp.node_states {
                graph.set_node_state(node_id, (*state).into());
            }
            for (edge_id, state) in &cp.edge_states {
                graph.set_edge_state(edge_id, (*state).into());
            }
        }

        // 恢复 VariablePool
        {
            let mut pool = self.variable_pool.write();
            *pool = restore_from_checkpoint(&cp.variables);
        }

        // 恢复执行元数据
        self.exceptions_count = cp.exceptions_count;
        self.final_outputs = cp.final_outputs;

        // 调整 start_time 以补偿已消耗时间
        let adjusted_start = self.context.time_provider.now_timestamp()
            - cp.elapsed_secs as i64;

        self.event_emitter.emit(GraphEngineEvent::CheckpointResumed {
            node_id: cp.completed_node_id.clone(),
        }).await;

        Ok(Some((
            cp.ready_queue,
            cp.ready_predecessor,
            cp.step_count,
            adjusted_start,
        )))
    }

    /// 删除检查点（workflow 正常完成后调用）
    async fn delete_checkpoint(&self) {
        if let Some(store) = &self.checkpoint_store {
            let _ = store.delete(&self.workflow_id).await;
        }
    }
}
```

---

## 5. WorkflowHandle 暂停/恢复

### 5.1 ExecutionStatus 扩展

**文件**: `src/scheduler.rs`

```rust
#[derive(Debug, Clone)]
pub enum ExecutionStatus {
    Running,
    Completed(HashMap<String, Value>),
    Failed(String),
    FailedWithRecovery {
        original_error: String,
        recovered_outputs: HashMap<String, Value>,
    },
    /// 工作流在 human-input 节点暂停，等待外部输入
    Paused {
        /// 暂停在哪个节点
        node_id: String,
        /// 节点标题（用于 UI 展示）
        node_title: String,
        /// 提示信息（告诉用户需要提供什么）
        prompt: String,
    },
}
```

### 5.2 WorkflowHandle 新增方法

```rust
impl WorkflowHandle {
    // ... 现有 status(), wait(), events() ...

    /// 等待直到 workflow 到达终态或暂停
    pub async fn wait_or_paused(&self) -> ExecutionStatus {
        let mut rx = self.status_rx.clone();
        loop {
            let status = rx.borrow().clone();
            match status {
                ExecutionStatus::Running => {
                    if rx.changed().await.is_err() {
                        return rx.borrow().clone();
                    }
                }
                // Paused 也是可以等到的状态
                _ => return status,
            }
        }
    }

    /// 向暂停的 workflow 提交 human input，恢复执行
    pub async fn resume_with_input(
        &self,
        input: HashMap<String, Value>,
    ) -> Result<(), WorkflowError> {
        self.command_tx.send(Command::ResumeWithInput { input })
            .await
            .map_err(|_| WorkflowError::InternalError(
                "Workflow already terminated".to_string()
            ))
    }
}
```

### 5.3 Command 扩展

**文件**: `src/core/dispatcher.rs`

```rust
#[derive(Debug, Clone)]
pub enum Command {
    Abort { reason: Option<String> },
    Pause,
    UpdateVariables { variables: HashMap<String, Value> },
    /// 恢复暂停的 workflow，注入 human input
    ResumeWithInput { input: HashMap<String, Value> },
    /// 安全停止：保存检查点后终止
    SafeStop,
}
```

### 5.4 ExecutionStatus 扩展

```rust
#[derive(Debug, Clone)]
pub enum ExecutionStatus {
    Running,
    Completed(HashMap<String, Value>),
    Failed(String),
    FailedWithRecovery { original_error: String, recovered_outputs: HashMap<String, Value> },
    Paused { node_id: String, node_title: String, prompt: String },
    /// 安全停止完成 — 所有进行中的工作已保存检查点
    SafeStopped {
        /// 最后完成的节点 ID
        last_completed_node: Option<String>,
        /// 被中断的节点 ID 列表（正在执行但未完成）
        interrupted_nodes: Vec<String>,
        /// 检查点保存是否成功
        checkpoint_saved: bool,
    },
}
```

### 5.5 SafeStopSignal — 跨 workflow 广播

**文件**: `src/core/checkpoint.rs`

```rust
use tokio_util::sync::CancellationToken;

/// 安全停止信号 — 可在多个 workflow 之间共享
///
/// 调用 `trigger()` 后，所有持有该 signal clone 的 workflow
/// 会在当前节点完成后保存检查点并终止。
#[derive(Clone)]
pub struct SafeStopSignal {
    token: CancellationToken,
    /// 等待正在执行的节点完成的超时时间
    timeout: Arc<AtomicU64>,
}

impl SafeStopSignal {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            timeout: Arc::new(AtomicU64::new(30)),
        }
    }

    /// 触发安全停止
    ///
    /// `timeout_secs`: 等待正在执行的节点完成的最长时间。
    /// 超时后仍未完成的节点会被中断（结果丢失，从上一个检查点恢复）。
    pub fn trigger(&self, timeout_secs: u64) {
        self.timeout.store(timeout_secs, Ordering::Relaxed);
        self.token.cancel();
    }

    /// 是否已触发
    pub fn is_triggered(&self) -> bool {
        self.token.is_cancelled()
    }

    /// 获取 CancellationToken 用于 select!
    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    /// 获取超时秒数
    pub fn timeout_secs(&self) -> u64 {
        self.timeout.load(Ordering::Relaxed)
    }
}
```

### 5.6 WorkflowRunnerBuilder 新增方法

```rust
impl WorkflowRunnerBuilder {
    // ... 现有方法 ...

    /// 设置检查点存储（可选）
    pub fn checkpoint_store(mut self, store: Arc<dyn CheckpointStore>) -> Self {
        self.checkpoint_store = Some(store);
        self
    }

    /// 设置 Workflow ID（检查点的 key；不设则自动生成）
    pub fn workflow_id(mut self, id: String) -> Self {
        self.workflow_id = Some(id);
        self
    }

    /// 设置安全停止信号（可选，多个 workflow 可共享同一个 signal）
    pub fn safe_stop_signal(mut self, signal: SafeStopSignal) -> Self {
        self.safe_stop_signal = Some(signal);
        self
    }
}
```

---

## 6. Human-Input 节点

### 6.1 Executor

**文件**: 新建 `src/nodes/human_input.rs`

```rust
/// Human-Input 节点 — 暂停工作流等待人工输入
pub struct HumanInputExecutor;

#[async_trait]
impl NodeExecutor for HumanInputExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let cfg: HumanInputNodeData = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        // 渲染提示信息模板
        let prompt = render_template(&cfg.prompt, variable_pool)?;

        // 返回 Paused 状态 — Dispatcher 会将此状态广播到 WorkflowHandle
        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Paused,
            metadata: {
                let mut m = HashMap::new();
                m.insert("prompt".to_string(), Value::String(prompt));
                m.insert("node_title".to_string(), Value::String(cfg.title.clone()));
                m
            },
            ..Default::default()
        })
    }
}
```

### 6.2 Dispatcher 处理 Paused 状态

在 `handle_node_success` 方法中：

```rust
async fn handle_node_success(
    &mut self,
    exec_id: &str,
    node_id: &str,
    info: &NodeInfo,
    result: NodeRunResult,
    ready: &mut Vec<String>,
) -> WorkflowResult<Vec<String>> {
    // ... 现有逻辑 ...

    // 新增：处理 Paused 状态
    if result.status == WorkflowNodeExecutionStatus::Paused {
        let prompt = result.metadata.get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let title = result.metadata.get("node_title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // 保存检查点（暂停前）
        self.save_checkpoint(node_id, ready, &HashMap::new(), step_count, start_time).await?;

        // 广播 Paused 状态
        self.status_tx.send_replace(ExecutionStatus::Paused {
            node_id: node_id.to_string(),
            node_title: title,
            prompt,
        });

        // 等待 ResumeWithInput 命令
        let input = self.wait_for_resume().await?;

        // 注入 human input 到 variable pool
        {
            let mut pool = self.variable_pool.write();
            for (key, value) in input {
                let selector = Selector::new(node_id, &key);
                pool.set(&selector, Segment::from_value(&value));
            }
        }

        // 广播恢复为 Running
        self.status_tx.send_replace(ExecutionStatus::Running);

        // 继续正常流程：推进下游边
    }

    // ... 现有的 advance_graph_after_success ...
}
```

### 6.3 DSL 示例

```yaml
- id: approval
  data:
    type: human-input
    title: "Manager Approval"
    prompt: |
      Agent analysis: {{research.text}}

      Approve this action?
    variables:
      - name: approved
        type: boolean
        required: true
      - name: comments
        type: string

# 后续节点使用 human input
- id: check_approval
  data:
    type: if-else
    conditions:
      - id: approved
        variable_selector: [approval, approved]
        comparison: "eq"
        value: true
```

---

## 7. 调用方使用示例

### 7.1 基本用法（无检查点）

```rust
// 行为与现在完全一致，零开销
let handle = WorkflowRunner::builder(schema)
    .user_inputs(inputs)
    .build()
    .run()
    .await?;

let status = handle.wait().await;
```

### 7.2 带检查点

```rust
let store = Arc::new(SqliteCheckpointStore::new("checkpoints.db"));
let workflow_id = "order-12345".to_string();

let handle = WorkflowRunner::builder(schema)
    .user_inputs(inputs)
    .checkpoint_store(store.clone())
    .workflow_id(workflow_id.clone())
    .build()
    .run()
    .await?;

let status = handle.wait().await;
// agent 节点完成后自动存了检查点
// workflow 正常完成后自动删了检查点
```

### 7.3 故障恢复

```rust
// 第一次运行，crash 在 send_email 节点
let handle = WorkflowRunner::builder(schema.clone())
    .checkpoint_store(store.clone())
    .workflow_id("order-12345".to_string())
    .build()
    .run()
    .await?;
// handle.wait() → Failed("send_email timeout")
// 但 agent 节点的结果已保存在检查点中

// 第二次运行：自动从 agent 之后恢复，跳过已完成的 agent
let handle = WorkflowRunner::builder(schema)
    .checkpoint_store(store.clone())
    .workflow_id("order-12345".to_string())  // 同一个 workflow_id
    .build()
    .run()
    .await?;
let status = handle.wait().await;
// 这次从 agent 后的节点开始，不重跑 agent，省了 $3
```

### 7.4 Human-in-the-loop

```rust
let handle = WorkflowRunner::builder(schema)
    .checkpoint_store(store.clone())
    .workflow_id("ticket-67890".to_string())
    .build()
    .run()
    .await?;

loop {
    match handle.wait_or_paused().await {
        ExecutionStatus::Paused { node_id, prompt, .. } => {
            println!("Approval needed: {}", prompt);
            // 展示给用户...用户操作后...
            let input = HashMap::from([
                ("approved".to_string(), Value::Bool(true)),
                ("comments".to_string(), Value::String("LGTM".into())),
            ]);
            handle.resume_with_input(input).await?;
        }
        ExecutionStatus::Completed(outputs) => {
            println!("Done: {:?}", outputs);
            break;
        }
        ExecutionStatus::Failed(err) => {
            eprintln!("Error: {}", err);
            break;
        }
        _ => {}
    }
}
```

### 7.5 安全停止（单个 workflow）

```rust
let handle = WorkflowRunner::builder(schema)
    .checkpoint_store(store.clone())
    .workflow_id("order-123".to_string())
    .build()
    .run()
    .await?;

// 某个时刻需要停止（比如收到 SIGTERM）
handle.safe_stop().await;

match handle.wait().await {
    ExecutionStatus::SafeStopped { checkpoint_saved, .. } => {
        assert!(checkpoint_saved);
        // 下次用同一个 workflow_id 启动即可恢复
    }
    _ => {}
}
```

### 7.6 安全停止（所有 workflow，优雅关闭）

```rust
let shutdown_signal = SafeStopSignal::new();

// 多个 workflow 共享同一个 signal
let handle1 = WorkflowRunner::builder(schema1)
    .checkpoint_store(store.clone())
    .workflow_id("order-001".to_string())
    .safe_stop_signal(shutdown_signal.clone())
    .build()
    .run()
    .await?;

let handle2 = WorkflowRunner::builder(schema2)
    .checkpoint_store(store.clone())
    .workflow_id("order-002".to_string())
    .safe_stop_signal(shutdown_signal.clone())
    .build()
    .run()
    .await?;

// 监听系统信号
tokio::spawn(async move {
    tokio::signal::ctrl_c().await.unwrap();
    println!("Received SIGINT, safe stopping all workflows...");
    // 给正在执行的节点 30 秒完成
    shutdown_signal.trigger(30);
});

// 等待所有 workflow 停止
let status1 = handle1.wait().await;
let status2 = handle2.wait().await;
// 两个都是 SafeStopped，下次启动进程后可恢复
```

---

## 8. 安全停止（Safe Stop）

### 8.1 概述

安全停止允许在不丢失工作进度的情况下终止工作流执行。典型场景：
- 服务部署更新（滚动发布）
- 收到 SIGTERM/SIGINT 信号
- 资源不足需要腾出
- 运维人员手动干预

### 8.2 执行流程

```
正常执行中：
  ready: [node_C, node_D]    ← 等待执行
  running: {node_A, node_B}  ← 正在执行

收到 SafeStop 信号：

  1. 停止调度 ← ready 队列冻结，不再派发新节点
     ready: [node_C, node_D]  (冻结)
     running: {node_A, node_B} (继续)

  2. 等待正在执行的节点完成（带超时）
     ┌─ node_A 完成 → 正常处理结果、更新变量
     │  → ready 可能变为 [node_C, node_D, node_E]（但不派发）
     │
     └─ node_B 超时未完成 → 强制中断
        → node_B 的结果丢失（下次从上一个检查点恢复时重跑）

  3. 保存检查点
     checkpoint = {
       node_states: {A: Taken, B: Pending, C: Pending, D: Pending},
       ready_queue: [node_B, node_C, node_D, node_E],  // B 回到 ready
       variables: 包含 A 的输出，不含 B 的输出,
     }

  4. 广播 SafeStopped 状态 → WorkflowHandle 收到终态
```

### 8.3 Dispatcher 实现

**文件**: `src/core/dispatcher.rs`

#### run() 循环中新增安全停止检测

```rust
pub async fn run(&mut self) -> WorkflowResult<HashMap<String, Value>> {
    // ... 现有初始化 ...

    loop {
        // ===== 新增：安全停止检测 =====
        if self.is_safe_stop_triggered() {
            let outcome = self.execute_safe_stop(
                &mut ready,
                &mut ready_predecessor,
                &mut running,
                &mut join_set,
                step_count,
                start_time,
            ).await?;
            return outcome;
        }

        // ... 现有 gather timeout 逻辑 ...
        // ... 现有 dispatch/join 逻辑 ...
    }
}
```

#### 安全停止检测

```rust
fn is_safe_stop_triggered(&self) -> bool {
    // 方式 1: SafeStopSignal（跨 workflow 广播）
    if let Some(signal) = &self.safe_stop_signal {
        if signal.is_triggered() {
            return true;
        }
    }

    // 方式 2: Command 通道（单 workflow）
    if let Some(rx) = &self.command_rx {
        if let Ok(Command::SafeStop) = rx.try_recv() {
            return true;
        }
    }

    false
}
```

#### 安全停止执行

```rust
async fn execute_safe_stop(
    &mut self,
    ready: &mut Vec<String>,
    ready_predecessor: &mut HashMap<String, String>,
    running: &mut HashMap<String, AbortHandle>,
    join_set: &mut JoinSet<NodeExecOutcome>,
    step_count: i32,
    start_time: i64,
) -> WorkflowResult<HashMap<String, Value>> {
    let timeout_secs = self.safe_stop_signal
        .as_ref()
        .map(|s| s.timeout_secs())
        .unwrap_or(30);

    let mut interrupted_nodes = Vec::new();
    let mut last_completed = None;

    // 1. 等待正在执行的节点完成（带超时）
    if !join_set.is_empty() {
        let deadline = tokio::time::Instant::now()
            + tokio::time::Duration::from_secs(timeout_secs);

        loop {
            tokio::select! {
                joined = join_set.join_next() => {
                    let Some(joined) = joined else { break };

                    match joined {
                        Ok(outcome) => {
                            running.remove(&outcome.node_id);
                            last_completed = Some(outcome.node_id.clone());

                            // 正常处理完成的节点
                            match outcome.result {
                                Ok(result) => {
                                    let _ = self.handle_node_success(
                                        &outcome.exec_id,
                                        &outcome.node_id,
                                        &outcome.info,
                                        result,
                                        ready,
                                    ).await;
                                }
                                Err(_) => {
                                    // 节点失败 → 不影响安全停止，继续等其他节点
                                }
                            }
                        }
                        Err(join_error) => {
                            if !join_error.is_cancelled() {
                                // JoinError → 记录但不阻止安全停止
                            }
                        }
                    }

                    if running.is_empty() {
                        break;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    // 超时：中断所有未完成的节点
                    for (node_id, handle) in running.drain() {
                        handle.abort();
                        interrupted_nodes.push(node_id.clone());
                        // 被中断的节点回到 ready 队列（下次恢复时重跑）
                        ready.push(node_id);
                    }
                    join_set.abort_all();
                    // 消耗所有 JoinError
                    while join_set.join_next().await.is_some() {}
                    break;
                }
            }
        }
    }

    // 2. 保存检查点
    let checkpoint_saved = if self.checkpoint_store.is_some() {
        let result = self.save_checkpoint(
            last_completed.as_deref().unwrap_or("safe_stop"),
            ready,
            ready_predecessor,
            step_count,
            start_time,
        ).await;
        result.is_ok()
    } else {
        false
    };

    // 3. 发射事件
    self.event_emitter.emit(GraphEngineEvent::WorkflowSafeStopped {
        interrupted_nodes: interrupted_nodes.clone(),
        checkpoint_saved,
    }).await;

    // 4. 返回 SafeStopped 状态（通过 Err 传递，让调用方处理）
    Err(WorkflowError::SafeStopped {
        last_completed_node: last_completed,
        interrupted_nodes,
        checkpoint_saved,
    })
}
```

### 8.4 WorkflowHandle 方法

```rust
impl WorkflowHandle {
    // ... 现有方法 ...

    /// 请求安全停止（单个 workflow）
    pub async fn safe_stop(&self) -> Result<(), WorkflowError> {
        self.command_tx.send(Command::SafeStop)
            .await
            .map_err(|_| WorkflowError::InternalError(
                "Workflow already terminated".to_string()
            ))
    }
}
```

### 8.5 无 CheckpointStore 时的行为

| 有 CheckpointStore | 无 CheckpointStore |
|---|---|
| 等待节点完成 → 保存检查点 → SafeStopped | 等待节点完成 → SafeStopped |
| 下次启动可恢复 | 下次启动从头开始 |
| `checkpoint_saved: true` | `checkpoint_saved: false` |

无 CheckpointStore 时安全停止仍然有意义：给正在执行的节点时间完成，而不是直接 kill。

### 8.6 与现有 Command::Abort 的区别

| | Abort | SafeStop |
|---|---|---|
| 正在执行的节点 | 立即中断 | 等待完成（带超时） |
| 检查点 | 不保存 | 保存 |
| 可恢复 | 否 | 是 |
| 返回状态 | Failed | SafeStopped |
| 语义 | "出错了，放弃" | "需要停了，但保留进度" |

---

## 9. 内置 CheckpointStore 实现

内存和文件两种 `CheckpointStore` 作为**内置实现**，随 `checkpoint` feature 一起编译。其他存储后端（Redis、SQLite、S3 等）通过**插件系统**扩展。

```
内置（checkpoint feature）         插件扩展（plugin-system feature）
┌───────────────────────┐         ┌──────────────────────────┐
│ MemoryCheckpointStore │         │ RedisCheckpointStore     │
│ FileCheckpointStore   │         │ SqliteCheckpointStore    │
└───────────────────────┘         │ S3CheckpointStore        │
                                  │ 用户自定义...             │
                                  └──────────────────────────┘
```

### 9.1 MemoryCheckpointStore（内置 — 测试/开发）

```rust
/// 内存检查点存储 — 进程退出即丢失
pub struct MemoryCheckpointStore {
    data: tokio::sync::RwLock<HashMap<String, Checkpoint>>,
}

#[async_trait]
impl CheckpointStore for MemoryCheckpointStore {
    async fn save(&self, workflow_id: &str, cp: &Checkpoint) -> Result<(), CheckpointError> {
        self.data.write().await.insert(workflow_id.to_string(), cp.clone());
        Ok(())
    }

    async fn load(&self, workflow_id: &str) -> Result<Option<Checkpoint>, CheckpointError> {
        Ok(self.data.read().await.get(workflow_id).cloned())
    }

    async fn delete(&self, workflow_id: &str) -> Result<(), CheckpointError> {
        self.data.write().await.remove(workflow_id);
        Ok(())
    }
}
```

### 9.2 FileCheckpointStore（内置 — 轻量持久化）

```rust
/// 文件系统检查点存储 — 每个 workflow 一个 JSON 文件
pub struct FileCheckpointStore {
    dir: PathBuf,
}

#[async_trait]
impl CheckpointStore for FileCheckpointStore {
    async fn save(&self, workflow_id: &str, cp: &Checkpoint) -> Result<(), CheckpointError> {
        let path = self.dir.join(format!("{}.checkpoint.json", workflow_id));
        let data = serde_json::to_vec(cp)
            .map_err(|e| CheckpointError::SerializationError(e.to_string()))?;
        tokio::fs::write(&path, &data).await
            .map_err(|e| CheckpointError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn load(&self, workflow_id: &str) -> Result<Option<Checkpoint>, CheckpointError> {
        let path = self.dir.join(format!("{}.checkpoint.json", workflow_id));
        match tokio::fs::read(&path).await {
            Ok(data) => {
                let cp = serde_json::from_slice(&data)
                    .map_err(|e| CheckpointError::Corrupted(e.to_string()))?;
                Ok(Some(cp))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CheckpointError::StorageError(e.to_string())),
        }
    }

    async fn delete(&self, workflow_id: &str) -> Result<(), CheckpointError> {
        let path = self.dir.join(format!("{}.checkpoint.json", workflow_id));
        let _ = tokio::fs::remove_file(&path).await;
        Ok(())
    }
}
```

### 9.3 插件扩展方式

第三方 `CheckpointStore` 实现通过**插件系统注册**（Bootstrap 或 Normal 阶段均可）：

```rust
pub struct RedisCheckpointPlugin {
    metadata: PluginMetadata,
    config: RedisConfig,
}

#[async_trait]
impl Plugin for RedisCheckpointPlugin {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata  // Bootstrap 或 Normal 均可
    }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let store = RedisCheckpointStore::connect(&self.config).await?;
        ctx.provide_service::<Arc<dyn CheckpointStore>>(Arc::new(store))?;
        Ok(())
    }
}
```

**为什么不限定 Bootstrap？** CheckpointStore 仅在 `Dispatcher::run()` 时消费，而 `run()` 在所有插件阶段（Bootstrap + Normal）完成之后才开始。没有其他插件在注册期间依赖 CheckpointStore，因此无需强制提前注册。插件作者可根据自身情况选择阶段。

`WorkflowRunnerBuilder` 在所有插件阶段完成后检查是否有插件提供的 `CheckpointStore`：

```rust
// scheduler.rs 中，所有插件注册完成后
if builder.checkpoint_store.is_none() {
    if let Some(store) = plugin_registry.query_service::<Arc<dyn CheckpointStore>>() {
        builder.checkpoint_store = Some(store);
    }
}
```

这样用户可以：
- **直接注入**：`builder.checkpoint_store(Arc::new(FileCheckpointStore::new(...)))`
- **通过插件**：加载 Redis/SQLite 插件，自动注册到 builder

---

## 10. GraphEngineEvent 扩展

**文件**: `src/core/event_bus.rs`

```rust
pub enum GraphEngineEvent {
    // ... 现有事件 ...
    GraphRunStarted,
    GraphRunSucceeded { outputs },
    GraphRunPartialSucceeded { exceptions_count, outputs },
    GraphRunFailed { error },
    // ... 节点事件 ...

    /// 检查点已保存
    CheckpointSaved {
        node_id: String,
    },
    /// 从检查点恢复
    CheckpointResumed {
        node_id: String,
    },
    /// Workflow 暂停等待人工输入
    WorkflowPaused {
        node_id: String,
        prompt: String,
    },
    /// Workflow 从暂停恢复
    WorkflowResumed {
        node_id: String,
    },
    /// 恢复时检测到环境变化警告（Normal 策略下 Warning 级别）
    ResumeWarning {
        diagnostic: String,
    },
    /// 安全停止完成
    WorkflowSafeStopped {
        interrupted_nodes: Vec<String>,
        checkpoint_saved: bool,
    },
}
```

---

## 11. Feature Gating

**文件**: `Cargo.toml`

```toml
[features]
default = [
    "security", "plugin-system",
    # "checkpoint",      # 默认不开启
]

# 检查点支持
checkpoint = []
```

所有检查点相关代码使用 `#[cfg(feature = "checkpoint")]` 门控：

```rust
#[cfg(feature = "checkpoint")]
pub mod checkpoint;

// dispatcher.rs
#[cfg(feature = "checkpoint")]
checkpoint_store: Option<Arc<dyn CheckpointStore>>,

#[cfg(not(feature = "checkpoint"))]
// 不存在 checkpoint_store 字段，零开销
```

不开启 `checkpoint` feature 时：
- `WorkflowDispatcher` 没有 `checkpoint_store` 字段
- `run()` 方法中无任何检查点逻辑
- 编译后二进制零增量

---

## 12. Security 相关

### 12.1 检查点数据安全

- **变量可能包含敏感信息**（用户输入、API 密钥、LLM 输出）
- `CheckpointStore` 实现方负责加密存储（在 trait doc 中说明）
- 内置的 `FileCheckpointStore` 应在文档中警告：文件未加密

### 12.2 审计

检查点操作通过 `GraphEngineEvent` 记录：
- `CheckpointSaved` — 谁的 workflow、在哪个节点保存
- `CheckpointResumed` — 谁的 workflow、从哪个节点恢复

### 12.3 检查点有效性

恢复时需验证：
- Schema 版本是否匹配（检查点中保存的 workflow 版本与当前 schema 一致）
- 节点 ID 是否仍存在于当前 DAG 中
- 不匹配时拒绝恢复，返回 `CheckpointError::Corrupted`

---

## 13. 恢复安全策略（Resume Safety）

### 13.1 问题

检查点保存时的运行环境与恢复时可能不同。例如：

| 变化类型 | 举例 | 风险等级 |
|---------|------|---------|
| LLM Provider 变更 | 保存时用 GPT-4o，恢复时用 GPT-3.5 | ⚠️ 中 — 可能影响后续 agent 质量 |
| 安全级别降低 | 保存时 `Strict`，恢复时 `Permissive` | 🔴 高 — 检查点中的变量可能含有受限数据 |
| 安全级别提升 | 保存时 `Permissive`，恢复时 `Strict` | ✅ 安全 — 更严格总是 OK |
| 凭证变更 | 保存时 API Key A，恢复时 Key B | ⚠️ 中 — 可能导致后续调用失败 |
| 网络策略收紧 | 保存时允许外网，恢复时仅内网 | ⚠️ 中 — 后续 HTTP/MCP 节点可能失败 |
| Schema 版本变更 | 保存时 v1，恢复时 v2 | 🔴 高 — DAG 结构可能不兼容 |

### 13.2 设计：两种恢复策略

提供两种恢复策略，由调用方显式选择：

```rust
/// 恢复策略
#[derive(Debug, Clone, Copy, Default)]
pub enum ResumePolicy {
    /// 默认策略：检测环境变化，发现危险时拒绝恢复并报告具体内容
    ///
    /// 调用方收到 `ResumeRejected` 错误后可以：
    /// 1. 修复环境问题后重试
    /// 2. 切换为 `Force` 策略强制恢复
    #[default]
    Normal,

    /// 强制恢复：跳过所有安全检查，直接从检查点恢复
    ///
    /// 仅审计日志记录（如果 AuditLogger 存在），不阻止恢复。
    /// 用于：用户已知环境变化、手动确认风险后的场景。
    Force,
}
```

### 13.3 ContextFingerprint — 环境快照

保存检查点时，同步记录当前运行环境的"指纹"：

```rust
/// 运行环境指纹 — 保存在检查点中，恢复时用于比对
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContextFingerprint {
    /// 安全级别（security feature 下）
    pub security_level: Option<String>,
    /// 已注册的 LLM Provider 名称列表
    pub llm_providers: Vec<String>,
    /// 已注册的节点类型列表
    pub registered_node_types: Vec<String>,
    /// 网络策略摘要（允许的域名数、是否有黑名单等）
    pub network_policy_hash: Option<String>,
    /// Schema 的内容摘要（用于检测 DAG 结构变化）
    pub schema_hash: String,
    /// 凭证组名列表（不含凭证内容，仅名称用于比对可用性）
    pub credential_groups: Vec<String>,
    /// 引擎配置摘要
    pub engine_config_hash: String,
}
```

**采集时机**：在 `save_checkpoint()` 中，与 DAG 状态和变量一起保存。

```rust
impl ContextFingerprint {
    /// 从当前运行环境采集指纹
    pub fn capture(context: &WorkflowContext, schema: &WorkflowSchema) -> Self {
        let runtime = &context.runtime_group;
        Self {
            security_level: runtime.security_level().map(|l| format!("{:?}", l)),
            llm_providers: runtime.llm_provider_registry()
                .map(|r| r.list_providers())
                .unwrap_or_default(),
            registered_node_types: runtime.node_executor_registry()
                .list_registered_types(),
            network_policy_hash: runtime.network_policy()
                .map(|p| p.summary_hash()),
            schema_hash: schema.content_hash(),
            credential_groups: runtime.credential_provider()
                .map(|p| p.list_groups())
                .unwrap_or_default(),
            engine_config_hash: context.engine_config_hash(),
        }
    }
}
```

### 13.4 Checkpoint 结构新增字段

在 `Checkpoint` 中新增：

```rust
pub struct Checkpoint {
    // ... 现有字段 ...

    /// 保存时的运行环境指纹（用于恢复时安全比对）
    pub context_fingerprint: Option<ContextFingerprint>,
}
```

使用 `Option<ContextFingerprint>` 以兼容旧版检查点（没有指纹的检查点在 Normal 策略下会被标记为 warning）。

### 13.5 ResumeDiagnostic — 差异检测

恢复时将当前环境与检查点中的指纹比对，生成诊断报告：

```rust
/// 单项环境差异
#[derive(Debug, Clone)]
pub struct EnvironmentChange {
    /// 变化的组件
    pub component: String,
    /// 具体描述
    pub description: String,
    /// 风险等级
    pub severity: ChangeSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSeverity {
    /// 信息：变化安全或中性
    Info,
    /// 警告：可能影响后续执行
    Warning,
    /// 危险：安全降级或结构不兼容
    Danger,
}

/// 恢复诊断报告
#[derive(Debug, Clone)]
pub struct ResumeDiagnostic {
    /// 检测到的所有环境变化
    pub changes: Vec<EnvironmentChange>,
}

impl ResumeDiagnostic {
    /// 是否包含危险变化
    pub fn has_danger(&self) -> bool {
        self.changes.iter().any(|c| c.severity == ChangeSeverity::Danger)
    }

    /// 是否包含警告或危险
    pub fn has_warnings(&self) -> bool {
        self.changes.iter().any(|c| {
            matches!(c.severity, ChangeSeverity::Warning | ChangeSeverity::Danger)
        })
    }

    /// 生成人类可读的报告
    pub fn report(&self) -> String {
        let mut lines = Vec::new();
        for change in &self.changes {
            let icon = match change.severity {
                ChangeSeverity::Info => "ℹ️",
                ChangeSeverity::Warning => "⚠️",
                ChangeSeverity::Danger => "🔴",
            };
            lines.push(format!("{} [{}] {}", icon, change.component, change.description));
        }
        lines.join("\n")
    }
}
```

### 13.6 比对逻辑

```rust
/// 比对检查点指纹与当前环境
pub fn diff_fingerprints(
    saved: &ContextFingerprint,
    current: &ContextFingerprint,
) -> ResumeDiagnostic {
    let mut changes = Vec::new();

    // 1. Schema 结构变化 → Danger
    if saved.schema_hash != current.schema_hash {
        changes.push(EnvironmentChange {
            component: "schema".into(),
            description: "Workflow schema has changed since checkpoint was saved. \
                          DAG structure may be incompatible.".into(),
            severity: ChangeSeverity::Danger,
        });
    }

    // 2. 安全级别变化
    match (&saved.security_level, &current.security_level) {
        (Some(saved_level), Some(current_level)) if saved_level != current_level => {
            let severity = if is_security_downgrade(saved_level, current_level) {
                ChangeSeverity::Danger
            } else {
                ChangeSeverity::Info  // 升级是安全的
            };
            changes.push(EnvironmentChange {
                component: "security_level".into(),
                description: format!(
                    "Security level changed: {} → {}",
                    saved_level, current_level
                ),
                severity,
            });
        }
        (Some(_), None) => {
            changes.push(EnvironmentChange {
                component: "security_level".into(),
                description: "Security was enabled at checkpoint time but is now disabled.".into(),
                severity: ChangeSeverity::Danger,
            });
        }
        _ => {}
    }

    // 3. LLM Provider 变化
    let removed_providers: Vec<_> = saved.llm_providers.iter()
        .filter(|p| !current.llm_providers.contains(p))
        .collect();
    if !removed_providers.is_empty() {
        changes.push(EnvironmentChange {
            component: "llm_providers".into(),
            description: format!(
                "LLM providers removed since checkpoint: [{}]. \
                 Subsequent LLM/agent nodes using these providers will fail.",
                removed_providers.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ),
            severity: ChangeSeverity::Warning,
        });
    }

    // 4. 节点类型缺失
    let removed_types: Vec<_> = saved.registered_node_types.iter()
        .filter(|t| !current.registered_node_types.contains(t))
        .collect();
    if !removed_types.is_empty() {
        changes.push(EnvironmentChange {
            component: "node_types".into(),
            description: format!(
                "Node types removed: [{}]. Pending nodes of these types will fail.",
                removed_types.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ),
            severity: ChangeSeverity::Danger,
        });
    }

    // 5. 网络策略变化
    if saved.network_policy_hash != current.network_policy_hash {
        changes.push(EnvironmentChange {
            component: "network_policy".into(),
            description: "Network policy has changed. HTTP/MCP nodes may be \
                          blocked by new restrictions.".into(),
            severity: ChangeSeverity::Warning,
        });
    }

    // 6. 凭证组变化
    let removed_creds: Vec<_> = saved.credential_groups.iter()
        .filter(|g| !current.credential_groups.contains(g))
        .collect();
    if !removed_creds.is_empty() {
        changes.push(EnvironmentChange {
            component: "credentials".into(),
            description: format!(
                "Credential groups no longer available: [{}].",
                removed_creds.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ),
            severity: ChangeSeverity::Warning,
        });
    }

    // 7. 引擎配置变化
    if saved.engine_config_hash != current.engine_config_hash {
        changes.push(EnvironmentChange {
            component: "engine_config".into(),
            description: "Engine configuration has changed (max_steps, timeout, etc.).".into(),
            severity: ChangeSeverity::Info,
        });
    }

    ResumeDiagnostic { changes }
}

/// 判断安全级别是否为降级
fn is_security_downgrade(saved: &str, current: &str) -> bool {
    let level_order = ["Strict", "Standard", "Permissive"];
    let saved_idx = level_order.iter().position(|&l| l == saved);
    let current_idx = level_order.iter().position(|&l| l == current);
    match (saved_idx, current_idx) {
        (Some(s), Some(c)) => c > s,  // 数值越大越宽松 = 降级
        _ => false,
    }
}
```

### 13.7 恢复流程集成

修改 `try_resume_from_checkpoint()` 以支持恢复策略：

```rust
impl<G: DebugGate, H: DebugHook> WorkflowDispatcher<G, H> {
    async fn try_resume_from_checkpoint(
        &mut self,
        resume_policy: ResumePolicy,
    ) -> WorkflowResult<Option<(Vec<String>, HashMap<String, String>, i32, i64)>> {
        let Some(store) = &self.checkpoint_store else {
            return Ok(None);
        };

        let Some(cp) = store.load(&self.workflow_id).await
            .map_err(|e| WorkflowError::InternalError(
                format!("Checkpoint load failed: {}", e)
            ))?
        else {
            return Ok(None);
        };

        // === 环境安全检查 ===
        let current_fingerprint = ContextFingerprint::capture(
            &self.context, &self.schema
        );

        match resume_policy {
            ResumePolicy::Normal => {
                if let Some(saved_fp) = &cp.context_fingerprint {
                    let diagnostic = diff_fingerprints(saved_fp, &current_fingerprint);
                    if diagnostic.has_danger() {
                        // 拒绝恢复，返回详细的诊断报告
                        return Err(WorkflowError::ResumeRejected {
                            workflow_id: self.workflow_id.clone(),
                            diagnostic: diagnostic.report(),
                            changes: diagnostic.changes,
                        });
                    }
                    if diagnostic.has_warnings() {
                        // 有警告但无危险：记录审计日志，继续恢复
                        self.event_emitter.emit(GraphEngineEvent::ResumeWarning {
                            diagnostic: diagnostic.report(),
                        }).await;
                    }
                } else {
                    // 旧版检查点无指纹：记录警告，允许恢复
                    self.event_emitter.emit(GraphEngineEvent::ResumeWarning {
                        diagnostic: "Checkpoint has no context fingerprint \
                                    (legacy format). Safety check skipped.".into(),
                    }).await;
                }
            }
            ResumePolicy::Force => {
                // 强制恢复：跳过所有安全检查，仅记录审计日志
                if let Some(saved_fp) = &cp.context_fingerprint {
                    let diagnostic = diff_fingerprints(saved_fp, &current_fingerprint);
                    if diagnostic.has_warnings() {
                        // 仅写审计日志，不阻止
                        #[cfg(feature = "security")]
                        if let Some(logger) = self.context.runtime_group.audit_logger() {
                            logger.log(AuditEvent::ForceResumeWithChanges {
                                workflow_id: self.workflow_id.clone(),
                                changes: diagnostic.report(),
                            }).await;
                        }
                    }
                }
                // 无论有无差异，继续恢复
            }
        }

        // === 恢复状态（与之前相同） ===
        // 恢复 Graph、VariablePool、执行元数据 ...
        // （同 4.4 节）

        Ok(Some((cp.ready_queue, cp.ready_predecessor, cp.step_count, adjusted_start)))
    }
}
```

### 13.8 WorkflowRunnerBuilder 新增方法

```rust
impl WorkflowRunnerBuilder {
    /// 设置恢复策略（仅在有检查点时生效）
    ///
    /// - `Normal`（默认）：检测环境变化，危险时拒绝恢复并报告
    /// - `Force`：跳过安全检查，直接恢复
    pub fn resume_policy(mut self, policy: ResumePolicy) -> Self {
        self.resume_policy = policy;
        self
    }
}
```

### 13.9 CheckpointError 扩展

```rust
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    // ... 现有 variants ...

    #[error("Resume rejected for workflow '{workflow_id}':\n{diagnostic}")]
    ResumeRejected {
        workflow_id: String,
        diagnostic: String,
    },
}
```

### 13.10 调用方使用示例

#### 默认恢复（Normal — 检测危险）

```rust
let handle = WorkflowRunner::builder(schema)
    .checkpoint_store(store.clone())
    .workflow_id("order-12345".to_string())
    // resume_policy 默认为 Normal
    .build()
    .run()
    .await;

match handle {
    Err(WorkflowError::ResumeRejected { diagnostic, .. }) => {
        // 环境变化被检测到，向用户展示具体的危险内容
        eprintln!("Cannot resume safely:\n{}", diagnostic);
        // 输出示例：
        // 🔴 [security_level] Security level changed: Strict → Permissive
        // 🔴 [schema] Workflow schema has changed since checkpoint was saved.
        // ⚠️ [llm_providers] LLM providers removed: [openai].

        // 用户确认后，可以选择强制恢复：
        let handle = WorkflowRunner::builder(schema)
            .checkpoint_store(store.clone())
            .workflow_id("order-12345".to_string())
            .resume_policy(ResumePolicy::Force)
            .build()
            .run()
            .await?;
    }
    Ok(handle) => {
        // 正常恢复或无检查点，继续执行
        let status = handle.wait().await;
    }
    Err(e) => {
        eprintln!("Other error: {}", e);
    }
}
```

#### 强制恢复（Force — 跳过安全检查）

```rust
// 已知环境变化，用户手动确认风险
let handle = WorkflowRunner::builder(schema)
    .checkpoint_store(store.clone())
    .workflow_id("order-12345".to_string())
    .resume_policy(ResumePolicy::Force)  // 显式强制
    .build()
    .run()
    .await?;

// 即使安全级别降低、Provider 变更，也直接恢复
// 但所有变化会被记录到审计日志
```

### 13.11 行为总结

| 场景 | Normal 策略 | Force 策略 |
|------|------------|-----------|
| 无环境变化 | ✅ 正常恢复 | ✅ 正常恢复 |
| 安全级别升级（更严格） | ✅ 正常恢复（Info 日志） | ✅ 正常恢复 |
| 安全级别降级（更宽松） | 🔴 **拒绝恢复** + 报告详情 | ✅ 恢复 + 审计日志 |
| Security 被禁用 | 🔴 **拒绝恢复** + 报告详情 | ✅ 恢复 + 审计日志 |
| Schema 变更 | 🔴 **拒绝恢复** + 报告详情 | ✅ 恢复 + 审计日志 |
| LLM Provider 缺失 | ⚠️ 警告日志，允许恢复 | ✅ 恢复 + 审计日志 |
| 凭证组缺失 | ⚠️ 警告日志，允许恢复 | ✅ 恢复 + 审计日志 |
| 网络策略变化 | ⚠️ 警告日志，允许恢复 | ✅ 恢复 + 审计日志 |
| 引擎配置变化 | ✅ Info 日志，正常恢复 | ✅ 正常恢复 |
| 旧版检查点（无指纹） | ⚠️ 警告日志，允许恢复 | ✅ 正常恢复 |

**设计原则**：
- **Normal 策略**只在检测到 `Danger` 级别变化时拒绝恢复，`Warning` 级别仅记录日志
- **Force 策略**永远不阻止恢复，但所有变化都写审计日志（可追溯）
- 调用方收到 `ResumeRejected` 后拿到完整的诊断报告，可以向终端用户展示具体内容
- 用户看到具体风险后自行决定是否 Force 恢复 — 系统不做二次确认

---

## 14. 测试策略

### 14.1 单元测试

| 测试 | 验证内容 |
|------|---------|
| `test_checkpoint_save_load` | MemoryStore save → load → 数据一致 |
| `test_checkpoint_delete` | save → delete → load returns None |
| `test_checkpoint_overwrite` | save twice → load returns latest |
| `test_snapshot_for_checkpoint` | VariablePool 含各种 Segment 类型 → Value 转换正确 |
| `test_snapshot_skips_streams` | 含未完成 Stream → 被跳过 |
| `test_restore_from_checkpoint` | Value → VariablePool → 读取正确 |
| `test_serializable_edge_state` | 双向转换正确 |
| `test_context_fingerprint_capture` | 从 WorkflowContext 采集指纹，字段正确 |
| `test_diff_fingerprints_no_change` | 相同指纹 → 无变化 |
| `test_diff_fingerprints_security_downgrade` | 安全级别降级 → Danger |
| `test_diff_fingerprints_security_upgrade` | 安全级别升级 → Info |
| `test_diff_fingerprints_provider_removed` | LLM Provider 缺失 → Warning |
| `test_diff_fingerprints_schema_changed` | Schema 变更 → Danger |
| `test_resume_normal_rejects_danger` | Normal 策略 + Danger 变化 → ResumeRejected |
| `test_resume_force_allows_danger` | Force 策略 + Danger 变化 → 正常恢复 |

### 14.2 集成测试

#### Case 140: `checkpoint_basic`

- workflow: `start → code → agent(mock) → end`
- 配置 MemoryCheckpointStore
- 验证：agent 完成后检查点存在，workflow 完成后检查点被删除

#### Case 141: `checkpoint_resume`

- 第一次运行：agent 成功 → 下一个节点人为报错
- 第二次运行：同一 workflow_id → 从 agent 之后恢复
- 验证：agent 不重跑，最终输出正确

#### Case 142: `human_input_pause_resume`

- workflow: `start → human-input → end`
- 验证：status 变为 Paused → resume_with_input → Completed

#### Case 143: `checkpoint_file_store`

- 使用 FileCheckpointStore + 临时目录
- 验证：文件创建/读取/删除正确

#### Case 144: `resume_safety_normal_rejects`

- 第一次运行：security_level=Strict，agent 成功后保存检查点
- 第二次运行：security_level=Permissive，Normal 策略
- 验证：返回 `ResumeRejected` + diagnostic 包含 "security_level" 和 "Strict → Permissive"

#### Case 145: `resume_safety_force_allows`

- 同 Case 144 的场景，但使用 `ResumePolicy::Force`
- 验证：正常恢复，审计日志中记录了 ForceResumeWithChanges

---

## 15. 文件变更清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `src/core/checkpoint.rs` | **新建** | CheckpointStore trait, Checkpoint, CheckpointError, ContextFingerprint, ResumeDiagnostic, ResumePolicy, diff_fingerprints(), 序列化辅助函数, SafeStopSignal, 内置 Store 实现 |
| `src/core/mod.rs` | 修改 | 添加 `#[cfg(feature = "checkpoint")] pub mod checkpoint` |
| `src/core/dispatcher.rs` | 修改 | 添加 checkpoint_store 字段、save/resume/delete 方法、run() 中两处调用 |
| `src/core/event_bus.rs` | 修改 | 添加 CheckpointSaved/Resumed/WorkflowPaused/Resumed 事件 |
| `src/core/variable_pool.rs` | 修改 | 添加 `snapshot_for_checkpoint()` 和 `restore_from_checkpoint()` |
| `src/scheduler.rs` | 修改 | ExecutionStatus::Paused, WorkflowHandle::resume_with_input/wait_or_paused, Builder::checkpoint_store |
| `src/nodes/human_input.rs` | **新建** | HumanInputExecutor |
| `src/nodes/executor.rs` | 修改 | 注册 human-input（替换 stub） |
| `Cargo.toml` | 修改 | 添加 `checkpoint` feature |

---

## 16. 实施顺序

1. `src/core/checkpoint.rs` — 定义 trait 和数据结构（含 ContextFingerprint, ResumePolicy）
2. `src/core/variable_pool.rs` — 添加 `snapshot_for_checkpoint` / `restore_from_checkpoint`
3. `src/core/event_bus.rs` — 新增事件类型（含 ResumeWarning）
4. `src/core/dispatcher.rs` — 集成检查点逻辑（save/resume/delete + 恢复安全检查）
5. `src/scheduler.rs` — ExecutionStatus::Paused + WorkflowHandle 方法 + resume_policy
6. `src/nodes/human_input.rs` — HumanInputExecutor
7. MemoryCheckpointStore 内置实现
8. FileCheckpointStore 内置实现
9. SafeStopSignal + execute_safe_stop
10. 单元测试 + 集成测试（含 resume safety cases）

---

## 17. 验证命令

```bash
# 构建含检查点
cargo build --features checkpoint

# 全量构建
cargo build --all-features

# 测试
cargo test --all-features --workspace --lib checkpoint
cargo test --all-features --workspace --lib human_input
cargo test --all-features --workspace --test integration_tests -- 140
cargo test --all-features --workspace --test integration_tests -- 141
cargo test --all-features --workspace --test integration_tests -- 142
cargo test --all-features --workspace --test integration_tests -- 143
cargo test --all-features --workspace --test integration_tests -- 144
cargo test --all-features --workspace --test integration_tests -- 145
cargo test --all-features --workspace
cargo clippy --all-features --workspace
```

---

## 18. 设计总结

```
不配置 checkpoint_store 时：
  行为与现在完全一致 ← 零开销，零破坏

配置 checkpoint_store 时：
  ┌───────────────────────────────────────────┐
  │  run() 开始                               │
  │  ├─ 有检查点？ → 恢复状态，跳过已完成节点   │
  │  ├─ 无检查点？ → 正常从 start 开始         │
  │  │                                         │
  │  │  执行循环                               │
  │  │  ├─ 节点完成后                          │
  │  │  │  ├─ agent 节点？      → 自动存检查点  │
  │  │  │  ├─ human-input 节点？ → 存检查点+暂停│
  │  │  │  ├─ 用户标记 checkpoint? → 存检查点   │
  │  │  │  └─ 其他？            → 不存          │
  │  │  └─ 继续...                             │
  │  │                                         │
  │  ├─ 正常完成 → 删除检查点                   │
  │  └─ 异常退出 → 检查点保留，下次可恢复       │
  └───────────────────────────────────────────┘
```

核心设计特征：
- **选择性**：只存高价值节点，不是每步都存
- **可选的**：不配置时零开销，行为不变
- **trait 化**：存储由嵌入方决定，引擎不强依赖
- **最小侵入**：dispatcher 仅新增 ~50 行核心逻辑
- **恢复安全**：Normal 策略检测环境变化并报告具体风险，Force 策略跳过检查但记录审计日志
