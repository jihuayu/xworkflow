# 并行节点执行设计

## 1. 问题分析

### 当前执行模型

当前 `WorkflowDispatcher::run()` 采用严格串行模型（`src/core/dispatcher.rs:886-1040`）：

```
queue = [root_node]
while node_id = queue.pop():
    info = load_node_info(node_id)       // graph.read()
    pool_snapshot = pool.read().clone()   // O(1), im::HashMap
    result = execute_node(pool_snapshot)  // ← 阻塞等待
    pool.write().set_outputs(result)      // 写回池
    graph.write().advance_edges()         // 更新边状态
    downstream = collect_ready_nodes()    // 就绪节点加入队列
    queue.extend(downstream)
```

**问题**：DAG 中无依赖关系的节点（同拓扑层级、不同分支路径）被强制串行等待。例如：

```
       Start
      /     \
   Code1   Code2     ← 可以并行执行
      \     /
       End
```

Code1 和 Code2 无数据依赖，但当前实现中 Code2 必须等 Code1 完成后才能开始执行。

### 性能影响

- 工作流中最常见的 I/O 操作：LLM API 调用（500ms-30s）、HTTP 请求（50ms-5s）
- 对于包含 N 个可并行 I/O 节点的工作流，串行执行时间 = Σ(T_i)，并行执行时间 = max(T_i)
- **预期收益**：I/O 密集型工作流 wall time 减少 30-70%

---

## 2. 设计方案

### 2.1 核心思路

将 `queue.pop() → execute → push` 的串行循环改为基于 `tokio::JoinSet` 的并发调度：所有就绪节点同时启动执行，任一节点完成后立即处理结果并启动新的就绪节点。

### 2.2 并行调度器架构

```
┌─────────────────────────────────────────────┐
│           ParallelDispatcher                │
│                                             │
│  ┌─────────────┐   ┌─────────────────────┐ │
│  │ ready_queue  │──▶│ JoinSet<NodeResult> │ │
│  │ (就绪节点)   │   │ (并发执行中的节点)    │ │
│  └─────────────┘   └─────────────────────┘ │
│        ▲                    │               │
│        │                    ▼               │
│  ┌─────────────────────────────────────┐   │
│  │  handle_completion()                │   │
│  │  1. pool.write() → 写入输出         │   │
│  │  2. graph.write() → 更新边状态      │   │
│  │  3. collect_ready() → 加入就绪队列   │   │
│  └─────────────────────────────────────┘   │
│                                             │
│  共享状态:                                   │
│  - graph: Arc<RwLock<Graph>>                │
│  - pool:  Arc<RwLock<VariablePool>>         │
│  - registry: Arc<NodeExecutorRegistry>      │
└─────────────────────────────────────────────┘
```

### 2.3 执行流程

```
1. 初始化: ready_queue = [root_node]
2. 循环:
   a. 将 ready_queue 中所有节点 spawn 到 JoinSet
   b. 等待任一节点完成: join_set.join_next().await
   c. 处理完成节点:
      - 成功: 写入 pool → 更新 graph → 收集新就绪节点
      - 失败: 根据 error_strategy 处理（重试/跳过/终止）
   d. 新就绪节点加入 ready_queue
   e. 如果 JoinSet 为空 && ready_queue 为空 → 结束
```

### 2.4 关键接口变更

#### dispatcher.rs — `run()` 方法重构

当前：
```rust
pub async fn run(&mut self) -> WorkflowResult<HashMap<String, Value>> {
    let mut queue: Vec<String> = vec![root_id];
    while let Some(node_id) = queue.pop() {
        // ... 串行执行
    }
}
```

改为：
```rust
pub async fn run(&mut self) -> WorkflowResult<HashMap<String, Value>> {
    let mut join_set: JoinSet<NodeExecOutcome> = JoinSet::new();
    let mut ready: Vec<String> = vec![root_id];

    loop {
        // 1. 启动所有就绪节点
        for node_id in ready.drain(..) {
            let task = self.spawn_node_execution(node_id);
            join_set.spawn(task);
        }

        // 2. 等待任一完成
        let Some(outcome) = join_set.join_next().await else {
            break; // 所有节点完成
        };

        // 3. 处理结果，收集新就绪节点
        let new_ready = self.handle_node_outcome(outcome?)?;
        ready.extend(new_ready);
    }
}
```

#### 节点执行上下文封装

每个并发任务需要携带执行所需的全部上下文，避免借用 `&mut self`：

```rust
struct NodeExecContext {
    node_id: String,
    exec_id: String,
    node_info: NodeInfo,
    pool_snapshot: VariablePool,       // 执行隔离的池快照
    executor: Arc<dyn NodeExecutor>,   // 不可变引用
    context: RuntimeContext,           // Clone
}

struct NodeExecOutcome {
    node_id: String,
    exec_id: String,
    node_info: NodeInfo,
    result: Result<NodeRunResult, NodeError>,
}
```

### 2.5 同步机制设计

#### Graph 状态

当前 `Graph` 已由 `Arc<RwLock<Graph>>` 保护。并行模式下需确保：

- **读 graph**（`is_node_ready`、`load_node_info`）：多个完成回调可并发读
- **写 graph**（`advance_graph_after_success`、`process_edges`）：必须互斥

当前 `RwLock` 已满足此需求，无需改动。关键是 `handle_node_outcome` 中的 graph 写操作必须在主循环的单一 await point 上（非并发），这在上述设计中已保证：`join_next().await` 返回后串行处理结果。

#### Variable Pool

当前 `Arc<RwLock<VariablePool>>` + `im::HashMap`：

- **读取快照**：`pool.read().clone()` — O(1)，可并发
- **写入输出**：`pool.write().set_node_outputs()` — 互斥，串行处理

并行节点执行时，多个节点同时从同一快照读取输入（安全），完成后依次写入输出（串行化）。**无需额外同步**。

#### Event Emitter

当前 `EventEmitter` 使用 `mpsc::Sender<GraphEngineEvent>`：
- `send()` 是原子操作，多个节点可并发发送事件
- 接收端按发送顺序处理（FIFO channel）
- **无需改动**，但事件时序可能不再反映执行顺序（并行节点的事件交错）

#### 事件因果序保证

并行执行后，事件流的时序语义变化：

```
串行: NodeStart(A) → NodeEnd(A) → NodeStart(B) → NodeEnd(B)
并行: NodeStart(A) → NodeStart(B) → NodeEnd(B) → NodeEnd(A)  // B 先完成
```

**处理方式**：在事件中添加 `predecessor_node_id`（已有字段，当前为 `None`）和 `parallel_group_id` 来标记并行批次，消费端可据此重建因果关系。

### 2.6 Debug Hooks 兼容

当前 debug hooks 在主循环中同步调用：

```rust
if self.debug_gate.should_pause_before(&node_id) {
    let action = self.debug_hook.before_node_execute(...).await?;
    // 暂停等待调试器指令
}
```

**并行模式适配**：
- 开启 debug 的节点不进入 JoinSet，改为串行执行（回退到当前行为）
- 或：所有节点在 spawn 前先检查 debug gate，需要暂停的放入串行队列

```rust
for node_id in ready.drain(..) {
    if self.debug_gate.should_pause_before(&node_id)
       || self.debug_gate.should_pause_after(&node_id) {
        serial_queue.push(node_id);  // 串行处理
    } else {
        join_set.spawn(self.spawn_node_execution(node_id));
    }
}
// 先处理串行队列中的 debug 节点
for node_id in serial_queue.drain(..) {
    self.execute_node_serial(node_id, &mut ready).await?;
}
```

### 2.7 错误处理与取消

**当前行为**：任一节点失败 → 调用 `handle_node_failure` → 返回 `Err`，整个工作流终止。

**并行模式**：

1. **fail-fast 策略（默认）**：
   - 节点 A 失败 → 立即 `join_set.abort_all()`，取消所有正在执行的节点
   - 等待所有任务完成（abort 后 join_next 返回 `Cancelled`）
   - 返回第一个错误

2. **error_strategy: continue_on_error**：
   - 节点 A 失败 → 跳过该节点的下游（propagate_skip）
   - 其他并行节点继续执行
   - 最终返回异常计数

3. **超时处理**：
   - 每个节点已有 `timeout_secs` 配置
   - 在 spawn 时用 `tokio::time::timeout()` 包裹
   - 超时视为节点失败，走 error_strategy

### 2.8 Gather 汇聚节点

#### 问题

当前 DAG 中节点的汇聚是隐式的——任何有多条入边的节点自动 "wait-all"（等待所有上游分支 resolve）。这在串行模式下够用，但并行执行后出现新需求：

```
       Start
      / | \
   LLM1 LLM2 LLM3     ← 3 个 LLM 并行调用
      \ | /
       ???              ← 等 2 个完成就继续？还是等全部？
        |
       End
```

现有的 `VariableAggregator` 只做值选择（选第一个非空），**不是分支同步器**——它仍然要等所有入边 resolve 后才能执行。

#### 新节点类型：`Gather`

新增 `Gather` 节点类型，作为显式的并行分支汇聚点，支持多种汇聚策略：

```rust
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct GatherConfig {
    /// 汇聚模式
    pub join_mode: JoinMode,
    /// 汇聚满足后，是否取消仍在执行的分支
    pub cancel_remaining: bool,       // 默认: true
    /// 超时时间（秒）。None = 无超时（等到满足条件或所有分支结束）
    pub timeout_secs: Option<u64>,
    /// 超时策略
    pub timeout_strategy: TimeoutStrategy,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum JoinMode {
    /// 等待所有分支完成（同当前隐式行为，但显式表达）
    All,
    /// 等待任意一条分支完成（race 模式）
    Any,
    /// 等待 N 条分支完成
    NOfM { n: usize },
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum TimeoutStrategy {
    /// 超时后用已完成的分支结果继续（partial result）
    ProceedWithAvailable,
    /// 超时后报错
    Fail,
}
```

#### DSL 表示

```yaml
nodes:
  - id: gather_1
    type: gather
    data:
      join_mode:
        type: all          # 或 "any"，或 { type: "n_of_m", n: 2 }
      cancel_remaining: true
      timeout_secs: 30
      timeout_strategy: proceed_with_available
```

#### EdgeTraversalState 扩展

新增 `Cancelled` 状态，表示"Gather 触发后，该分支被取消"：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeTraversalState {
    Pending,
    Taken,
    Skipped,     // if-else 未选中的分支
    Cancelled,   // 新增：Gather 触发后取消的分支
}
```

`Cancelled` 与 `Skipped` 的区别：
- `Skipped`：分支决策导致，发生在分支节点执行时（静态的，上游未执行）
- `Cancelled`：运行时取消，发生在 Gather 触发后（动态的，上游可能正在执行）

两者都视为"已 resolve"，用于下游 `is_node_ready()` 判断。

#### 修改 `is_node_ready()`

当目标节点是 Gather 类型时，使用阈值判断而非 "all resolved"：

```rust
impl Graph {
    pub fn is_node_ready(&self, node_id: &str) -> bool {
        let edge_ids = match self.topology.in_edges.get(node_id) {
            Some(ids) if !ids.is_empty() => ids,
            _ => return node_id == self.root_node_id(),
        };

        // 获取节点类型，判断是否为 Gather
        let node = self.topology.nodes.get(node_id);
        let gather_config = node
            .filter(|n| n.node_type == "gather")
            .and_then(|n| serde_json::from_value::<GatherConfig>(n.config.clone()).ok());

        match gather_config {
            Some(config) => self.is_gather_ready(edge_ids, &config),
            None => self.is_standard_ready(edge_ids),
        }
    }

    /// 标准就绪检查：所有入边 resolved + 至少一条 Taken
    fn is_standard_ready(&self, edge_ids: &[String]) -> bool {
        let (mut all_resolved, mut any_taken) = (true, false);
        for eid in edge_ids {
            match self.edge_state(eid) {
                Some(EdgeTraversalState::Pending) => { all_resolved = false; break; }
                Some(EdgeTraversalState::Taken) => { any_taken = true; }
                _ => {}
            }
        }
        all_resolved && any_taken
    }

    /// Gather 就绪检查：根据 join_mode 判断
    fn is_gather_ready(&self, edge_ids: &[String], config: &GatherConfig) -> bool {
        let mut taken_count = 0;
        let mut resolved_count = 0;    // Taken + Skipped + Cancelled
        let total = edge_ids.len();

        for eid in edge_ids {
            match self.edge_state(eid) {
                Some(EdgeTraversalState::Taken) => {
                    taken_count += 1;
                    resolved_count += 1;
                }
                Some(EdgeTraversalState::Skipped | EdgeTraversalState::Cancelled) => {
                    resolved_count += 1;
                }
                _ => {} // Pending
            }
        }

        match &config.join_mode {
            JoinMode::All => {
                // 所有入边 resolved + 至少一条 Taken
                resolved_count == total && taken_count > 0
            }
            JoinMode::Any => {
                // 任意一条 Taken
                taken_count >= 1
            }
            JoinMode::NOfM { n } => {
                // N 条 Taken
                taken_count >= *n
            }
        }
    }
}
```

**关键**：Gather 的就绪检查**不要求所有入边 resolved**——只要 Taken 数量达到阈值就触发。这意味着某些入边可能还是 Pending 状态。

#### Gather 触发后的处理

Gather 变为 ready 并执行后，dispatcher 需要额外处理未完成的分支：

```
Gather 触发后:
  1. 标记 Gather 所有仍为 Pending 的入边为 Cancelled
  2. 如果 cancel_remaining = true:
     a. 找到这些 Cancelled 入边的来源节点及其上游正在执行的任务
     b. 从 JoinSet 中 abort 这些任务
  3. 从 Cancelled 入边的来源节点开始，向下游 propagate_cancel()
     - 类似 propagate_skip()，标记下游节点和边为 Cancelled
     - 但注意：如果一个下游节点有其他非 Cancelled 的入边，不应传播
```

```rust
impl Graph {
    /// Gather 触发后，处理未完成的入边
    pub fn finalize_gather(&mut self, gather_node_id: &str) {
        let in_edges = self.topology.in_edges.get(gather_node_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        let mut cancelled_sources = Vec::new();
        for eid in &in_edges {
            if matches!(self.edge_state(eid), Some(EdgeTraversalState::Pending)) {
                self.set_edge_state(eid, EdgeTraversalState::Cancelled);
                if let Some(edge) = self.topology.edges.get(eid) {
                    cancelled_sources.push(edge.source_node_id.clone());
                }
            }
        }

        // 向上游传播取消（取消正在执行但结果不再需要的节点）
        // 注意：只取消"唯一通向此 Gather"的上游路径
        for source in cancelled_sources {
            self.propagate_cancel_upstream(&source, gather_node_id);
        }
    }

    /// 向上游传播取消
    fn propagate_cancel_upstream(&mut self, node_id: &str, gather_id: &str) {
        // 检查该节点的所有出边是否都指向 gather（或已 cancelled 的路径）
        // 如果是，标记该节点为 Cancelled
        // 如果否（该节点还有其他非 cancelled 的下游），不取消
        let out_edges = self.topology.out_edges.get(node_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        let all_cancelled_or_gather = out_edges.iter().all(|eid| {
            let edge = self.topology.edges.get(eid);
            let target = edge.map(|e| e.target_node_id.as_str());
            target == Some(gather_id)
                || matches!(self.edge_state(eid),
                    Some(EdgeTraversalState::Cancelled | EdgeTraversalState::Skipped))
        });

        if all_cancelled_or_gather {
            self.set_node_state(node_id, EdgeTraversalState::Cancelled);
            // 继续向上游传播...
        }
    }
}
```

#### Gather 节点执行器

```rust
#[async_trait]
impl NodeExecutor for GatherExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let gather_config: GatherConfig = serde_json::from_value(config.clone())?;

        // 收集所有已完成分支的输出
        let input_selectors: Vec<Selector> = gather_config.input_variables
            .iter()
            .map(|v| v.into())
            .collect();

        let mut results = Vec::new();
        let mut completed_branches = Vec::new();

        for selector in &input_selectors {
            let val = variable_pool.get(selector);
            if !val.is_none() {
                results.push(val.clone());
                completed_branches.push(selector.node_id.clone());
            }
        }

        let mut outputs = HashMap::new();
        // 所有已完成分支的结果数组
        outputs.insert("results".to_string(),
            Segment::Array(Arc::new(results)));
        // 完成数量
        outputs.insert("completed_count".to_string(),
            Segment::Integer(completed_branches.len() as i64));
        // 第一个完成的结果（便捷访问）
        if let Some(first) = results.first() {
            outputs.insert("first_result".to_string(), first.clone());
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            ..Default::default()
        })
    }
}
```

#### 与并行调度器的集成

在 `handle_node_outcome()` 中，增加 Gather 完成后的特殊处理：

```
handle_node_outcome(outcome):
  node_id = outcome.node_id
  result = outcome.result

  // 正常处理：写入 pool、更新 graph
  pool.write().set_outputs(node_id, result.outputs)
  graph.write().process_normal_edges(node_id)

  // Gather 专用后处理
  if node_type(node_id) == "gather" {
      graph.write().finalize_gather(node_id)

      if gather_config.cancel_remaining {
          // 获取需要取消的上游节点
          cancelled_nodes = graph.read().cancelled_running_nodes()
          for task in join_set.tasks() {
              if cancelled_nodes.contains(task.node_id) {
                  task.abort()
              }
          }
      }
  }

  // 收集新就绪节点
  collect_ready_nodes(node_id)
```

#### 超时处理

Gather 节点的超时与普通节点不同——它是在**等待上游分支完成**期间的超时，而不是执行期间的超时：

```
方案：Gather 超时由 dispatcher 管理

spawn_gather_timeout(gather_node_id, timeout_secs):
  1. 在 JoinSet 中添加一个 timer 任务
  2. 计时器到期时，检查 Gather 是否已 ready
  3. 如果未 ready:
     - timeout_strategy == Fail → 返回错误
     - timeout_strategy == ProceedWithAvailable →
       强制将所有 Pending 入边标记为 Cancelled
       重新检查 is_gather_ready()（此时应返回 true，如果有至少 1 个 Taken）
       触发 Gather 执行
```

#### 使用场景示例

**场景 1：多 LLM 竞速（取最快）**

```yaml
nodes:
  - id: start
    type: start
  - id: llm_gpt4
    type: llm
    data: { model: gpt-4 }
  - id: llm_claude
    type: llm
    data: { model: claude-3-opus }
  - id: gather
    type: gather
    data:
      join_mode: { type: any }
      cancel_remaining: true
  - id: end
    type: end
edges:
  - source: start, target: llm_gpt4
  - source: start, target: llm_claude
  - source: llm_gpt4, target: gather
  - source: llm_claude, target: gather
  - source: gather, target: end
```

**场景 2：多数投票（3 选 2）**

```yaml
- id: gather
  type: gather
  data:
    join_mode: { type: n_of_m, n: 2 }
    cancel_remaining: true
    timeout_secs: 30
    timeout_strategy: proceed_with_available
```

**场景 3：并行采集后全量汇总**

```yaml
- id: gather
  type: gather
  data:
    join_mode: { type: all }
    cancel_remaining: false    # 等待所有分支
```

### 2.9 限流与并发度控制

新增配置参数：

```rust
pub struct ParallelConfig {
    /// 最大并行节点数，0 = 无限制
    pub max_concurrency: usize,
    /// 是否启用并行执行（默认 true）
    pub enabled: bool,
}
```

当 `max_concurrency > 0` 时，JoinSet 中活跃任务数不超过此值：

```rust
if join_set.len() >= max_concurrency {
    // 等待一个完成后再启动新节点
    let outcome = join_set.join_next().await.unwrap()?;
    let new_ready = self.handle_node_outcome(outcome)?;
    ready.extend(new_ready);
}
```

---

## 3. 迁移策略

### 阶段 1：重构 run() 为异步调度循环
- 将节点执行逻辑抽取为独立异步函数（不引用 `&mut self`）
- 引入 `JoinSet`，但默认 `max_concurrency = 1`（等同串行）
- 确保所有现有测试通过

### 阶段 2：启用并行
- 设置 `max_concurrency` 为默认无限制
- 添加并行执行的集成测试（多分支 DAG、菱形汇聚等）
- 验证事件顺序的兼容性

### 阶段 3：Gather 汇聚节点
1. 新增 `EdgeTraversalState::Cancelled`
2. 实现 `is_gather_ready()` 阈值判断
3. 实现 `finalize_gather()` 取消传播
4. 实现 `GatherExecutor`
5. dispatcher 中增加 Gather 触发后的取消逻辑
6. 测试：all/any/n_of_m 三种模式 + 超时 + 取消

### 阶段 4：Debug 兼容
- 实现 debug 节点的串行回退
- 添加 debug + 并行混合场景测试

---

## 4. 预期收益

| 场景 | 串行耗时 | 并行耗时 | 提升 |
|------|---------|---------|------|
| 2 个并行 LLM 调用（各 2s） | 4s | 2s | 50% |
| 3 个并行 HTTP 请求（各 500ms） | 1.5s | 500ms | 67% |
| 5 个并行 Code 节点（各 50ms） | 250ms | 50ms | 80% |
| 线性链路（无并行机会） | T | T | 0% |
| 3 个 LLM 竞速 + Gather(any) | 6s（串行总和） | 2s（最快一个） | 67% |
| 3 选 2 + Gather(n_of_m, n=2) | 6s | 4s（第二快完成时） | 33% |

**对纯计算型节点（Code、Template）影响较小**（单节点执行本身很快），**对 I/O 密集型工作流（LLM、HTTP）影响显著**。

**Gather 额外收益**：`any` 模式实现"最快响应"语义（同时调用多个 LLM，用最快返回的结果），在对延迟敏感的场景中可大幅降低 P99 延迟。

---

## 5. 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| 事件时序变化 | 依赖严格顺序的消费端可能出错 | 添加 parallel_group_id 字段，文档说明 |
| 并发写 pool 竞争 | 理论上不存在（串行处理结果） | 完成回调在主循环中串行执行 |
| 资源压力 | 大量并行节点消耗过多内存/连接 | max_concurrency 限流 |
| Debug 体验 | 并行节点调试复杂 | Debug 节点自动回退串行 |
| 错误传播复杂度 | 多个节点同时失败 | fail-fast 策略 + abort_all |
| Gather 取消传播 | Cancelled 边的上游可能有复杂拓扑 | propagate_cancel_upstream 只取消"唯一通向 Gather"的路径 |
| Gather 部分结果 | any/n_of_m 模式下结果不完整 | outputs 包含 completed_count，用户自行判断 |
| Gather 超时 | 慢分支导致 Gather 长时间等待 | timeout_secs + timeout_strategy 配置 |
| cancel_remaining 的任务取消 | abort 可能导致资源泄漏 | tokio abort 触发 Drop，Rust RAII 保证资源释放 |

---

## 6. 关键文件

| 文件 | 改动 |
|------|------|
| `src/core/dispatcher.rs` | 重构 `run()` 为并行调度循环 + Gather 触发后取消逻辑 |
| `src/graph/types.rs` | 新增 `Cancelled` 边状态 + `is_gather_ready()` + `finalize_gather()` |
| `src/core/variable_pool.rs` | 无需改动（im::HashMap 天然支持） |
| `src/core/event_bus.rs` | 可选：添加 parallel_group_id |
| `src/dsl/schema.rs` | 新增 `Gather` 节点类型 + `GatherConfig` + `JoinMode` + `ParallelConfig` |
| `src/nodes/gather.rs`（新文件） | `GatherExecutor` 实现 |
| `src/nodes/mod.rs` | 注册 GatherExecutor |
| `src/dsl/validation/known_types.rs` | 添加 gather 节点类型 |
