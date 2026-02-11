# 内存泄漏与资源释放测试设计文档

## 1. 概述

xworkflow 是基于 Tokio 异步运行时的 DAG 工作流引擎，大量使用 `Arc`、`tokio::spawn`、`mpsc::channel`、`SegmentStream` 等异步并发原语。目前项目**没有任何专门的内存泄漏或资源释放测试**。

通过对代码的系统性审查，识别出以下资源管理模式和风险：

| 资源类型 | 管理方式 | 已有 Drop | 泄漏风险 |
|---------|---------|----------|---------|
| `VariablePool` | `im::HashMap` + `Arc<RwLock<>>` | 自动 (COW) | 持久化 Arc 引用 |
| `SegmentStream` | `Arc<RwLock<StreamState>>` + `Arc<Notify>` | StreamReader 有 | **StreamWriter 无** |
| `WorkflowDispatcher` | 拥有多个 `Arc<RwLock<...>>` | 无显式 Drop | 依赖 Arc 引用计数 |
| `EventEmitter` | `mpsc::Sender` + `Arc<AtomicBool>` | 自动 | channel 未关闭时 receiver 阻塞 |
| `JsStreamRuntime` | `mpsc::Sender<RuntimeCommand>` | **无 Drop** | blocking thread 依赖显式 shutdown |
| `PluginRegistry` | `HashMap<String, Box<dyn Plugin>>` | 有 `shutdown_all()` | **从未被调用** |
| `CPluginWrapper` | `Library` + destroy_fn | 有 Drop | 正常 |
| `WasmSandbox` | `wasmtime::Engine` (共享) + 每次新建 Store | 自动 | Engine 缓存无边界 |
| Tokio spawned tasks | `tokio::spawn` fire-and-forget | N/A | **无 JoinHandle 追踪** |

## 2. 测试策略

### 2.1 检测手段

| 检测手段 | 适用场景 | 实现复杂度 | 精确度 |
|---------|---------|-----------|-------|
| `Arc::strong_count()` / `Weak::upgrade()` | 引用泄漏检测 | 低 | 高 |
| `dhat` 堆分析器 | 内存增长趋势、综合压力测试 | 中 | 高 |
| 原子计数器 Drop 验证 | 自定义类型析构确认 | 低 | 高 |
| `tokio::time::timeout` | 异步等待挂起检测 | 低 | 中 |
| Channel 关闭验证 | mpsc/watch 泄漏 | 低 | 高 |

### 2.2 核心检测原则

**原则 1：Arc 引用计数归零**

```rust
let pool = Arc::new(RwLock::new(VariablePool::new()));
let pool_weak = Arc::downgrade(&pool);
// ... 运行工作流，将 pool 传入 dispatcher ...
drop(pool);
drop(dispatcher);
assert!(pool_weak.upgrade().is_none(), "VariablePool 未被完全释放");
```

**原则 2：Drop 计数器验证**

```rust
static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);
struct TrackableDrop;
impl Drop for TrackableDrop {
    fn drop(&mut self) { DROP_COUNT.fetch_add(1, Ordering::SeqCst); }
}
```

**原则 3：超时保护**

所有涉及异步等待的测试**必须**包含 `tokio::time::timeout`，防止资源未释放导致测试永久挂起：

```rust
let result = tokio::time::timeout(Duration::from_secs(5), some_async_op()).await;
assert!(result.is_ok(), "操作超时，疑似资源未释放");
```

**原则 4：dhat 堆分析**

用于综合压力测试，检测多轮执行后内存不无限增长：

```rust
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[test]
fn test_memory_stable() {
    let _profiler = dhat::Profiler::new_heap();
    // ... 多轮执行 ...
    let stats = dhat::HeapStats::get();
    // 验证 curr_bytes 在合理范围内
}
```

## 3. 测试用例设计

### 3.1 [P0] SegmentStream 资源释放测试

**风险**：`StreamWriter`（`src/core/variable_pool.rs:322-327`）没有 `Drop` 实现。如果 writer 被 drop 但未调用 `end()`/`error()`，`StreamReader` 在 `self.stream.notify.notified().await`（第 555 行）处永远等待。

#### 3.1.1 `test_stream_writer_drop_without_end`
- **目标**：验证 StreamWriter drop 后 reader 不永久挂起
- **检测方法**：`tokio::time::timeout` 包裹 `stream.collect()`
- **预期行为**：
  - **当前**：reader 永久等待（超时 → 测试失败，确认 bug 存在）
  - **修复后**：reader 收到错误返回
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_stream_writer_drop_without_end() {
      let (stream, writer) = SegmentStream::channel();
      writer.send(Segment::String("chunk1".into())).await;
      drop(writer); // 不调用 end()

      let result = tokio::time::timeout(
          Duration::from_secs(2),
          stream.collect(),
      ).await;

      // 修复后预期：Ok(Err("stream writer dropped without end"))
      assert!(result.is_ok(), "StreamReader 不应永久等待");
      assert!(result.unwrap().is_err());
  }
  ```

#### 3.1.2 `test_stream_reader_drop_decrements_count`
- **目标**：验证 StreamReader Drop 正确递减 `readers` 计数
- **检测方法**：创建多个 reader，逐一 drop，检查计数
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_stream_reader_drop_decrements_count() {
      let (stream, writer) = SegmentStream::channel();
      let r1 = stream.reader(); // count = 1
      let r2 = stream.reader(); // count = 2
      let r3 = stream.reader(); // count = 3

      drop(r1); // count = 2
      drop(r2); // count = 1
      drop(r3); // count = 0

      // 验证：writer 结束后 stream collect 正常
      writer.end(Segment::String("done".into())).await;
      let result = stream.collect().await;
      assert!(result.is_ok());
  }
  ```

#### 3.1.3 `test_stream_arc_cleanup_after_completion`
- **目标**：验证 stream 正常结束后所有 Arc 引用完全释放
- **检测方法**：使用 `Weak` 追踪 `state` 的 Arc 引用
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_stream_arc_cleanup_after_completion() {
      let (stream, writer) = SegmentStream::channel();
      // 注意：需要通过某种方式获取内部 state 的 Weak 引用
      // 可通过为测试添加 pub(crate) fn state_weak() 方法

      writer.end(Segment::String("final".into())).await;
      let _ = stream.collect().await;

      drop(writer);
      drop(stream);

      // 验证 Arc<RwLock<StreamState>> strong_count 归零
  }
  ```

#### 3.1.4 `test_stream_multiple_readers_partial_drop`
- **目标**：多个 reader 中部分 drop 后，剩余 reader 仍正常工作
- **检测方法**：功能验证 + reader 计数检查
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_stream_multiple_readers_partial_drop() {
      let (stream, writer) = SegmentStream::channel();
      let mut r1 = stream.reader();
      let _r2 = stream.reader(); // 不使用

      writer.send(Segment::String("chunk".into())).await;
      writer.end(Segment::String("done".into())).await;

      // r1 应正常消费所有事件
      let mut events = vec![];
      while let Some(e) = r1.next().await {
          events.push(e);
          if matches!(events.last(), Some(StreamEvent::End(_))) { break; }
      }
      assert_eq!(events.len(), 2); // chunk + end
      // drop _r2 后计数应归零
  }
  ```

### 3.2 [P0] Tokio Spawned Task 泄漏测试

**风险**：
- `src/scheduler.rs:481` — 工作流执行 task 通过 `tokio::spawn` 启动，无 JoinHandle
- `src/scheduler.rs:465` — 事件收集器 task，依赖 channel 关闭退出
- `src/llm/executor.rs:201` — LLM 流式 task，无 JoinHandle

#### 3.2.1 `test_scheduler_tasks_complete_after_workflow`
- **目标**：验证 `WorkflowRunner::run()` 产生的所有 task 在工作流完成后退出
- **检测方法**：
  - 用 `Arc<AtomicBool>` 包装 task 完成标志
  - 或利用 `watch::Receiver` 的 `changed()` 返回 Err 来验证 sender 已 drop
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_scheduler_tasks_complete_after_workflow() {
      let schema = parse_dsl(&simple_yaml(), DslFormat::Yaml).unwrap();
      let handle = WorkflowRunner::builder(schema)
          .user_inputs(HashMap::new())
          .run().await.unwrap();

      let status = handle.wait().await;
      assert!(matches!(status, ExecutionStatus::Completed(_)));

      // 等待后台 task 完成
      tokio::time::sleep(Duration::from_millis(200)).await;

      // 验证 event active 标志已变为 false
      // 验证 watch sender 已 drop
  }
  ```

#### 3.2.2 `test_event_collector_exits_on_channel_close`
- **目标**：验证事件收集器 task 在 sender 端 drop 后正确退出
- **检测方法**：用 `Arc<AtomicBool>` 追踪 task 退出
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_event_collector_exits_on_channel_close() {
      let (tx, mut rx) = mpsc::channel(256);
      let exited = Arc::new(AtomicBool::new(false));
      let exited_clone = exited.clone();

      tokio::spawn(async move {
          while let Some(_event) = rx.recv().await {}
          exited_clone.store(true, Ordering::SeqCst);
      });

      // drop sender → channel 关闭 → collector 退出
      drop(tx);
      tokio::time::sleep(Duration::from_millis(100)).await;
      assert!(exited.load(Ordering::SeqCst), "collector task 应在 channel 关闭后退出");
  }
  ```

#### 3.2.3 `test_llm_stream_task_completes`
- **目标**：验证 LLM 流式 task 在流结束后退出
- **检测方法**：mock LLM provider + 流消费 + 验证 writer 已调用 end/error
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_llm_stream_task_completes() {
      // 使用 mock LLM provider，立即返回完成
      // 调用 execute_llm_node 的流式路径
      // 验证 SegmentStream collect() 正常返回
      // 等待短暂时间后验证 task 已退出
  }
  ```

### 3.3 [P0] JsStreamRuntime 阻塞线程泄漏测试

**风险**：`JsStreamRuntime`（`src/nodes/data_transform.rs:988-990`）通过 `tokio::task::spawn_blocking`（第 1055 行）启动阻塞线程，线程在 `cmd_rx.blocking_recv()` 循环（第 1147-1169 行）中等待。`JsStreamRuntime` 没有 Drop impl，如果 struct 被 drop 而未调用 shutdown，**需依赖 cmd_tx drop 导致 blocking_recv() 返回 None 来退出线程**。

#### 3.3.1 `test_js_runtime_drop_without_shutdown`
- **目标**：验证 JsStreamRuntime drop 时（未调用 shutdown），blocking thread 是否退出
- **检测方法**：用 `Arc<AtomicBool>` 追踪线程退出
- **预期行为**：当前 tx drop → `blocking_recv()` 返回 `None` → 线程退出。但需确认此路径可靠。
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_js_runtime_drop_without_shutdown() {
      let code = "function main(inputs) { return inputs; }";
      let inputs = serde_json::json!({"x": 1});

      let (_, _, runtime) = spawn_js_stream_runtime(
          code.into(), inputs, vec![]
      ).await.unwrap();

      // 不调用 shutdown，直接 drop
      drop(runtime);

      // 等待 blocking thread 退出
      tokio::time::sleep(Duration::from_millis(500)).await;
      // 验证方式：检查 blocking thread pool 没有泄漏
      // (精确验证需要为 spawn_blocking 添加退出追踪)
  }
  ```

#### 3.3.2 `test_js_runtime_explicit_shutdown`
- **目标**：验证显式 `shutdown()` 正确终止 blocking thread
- **检测方法**：shutdown 后验证 invoke 返回错误（channel 已关闭）

### 3.4 [P0] PluginRegistry shutdown 测试

**风险**：`PluginRegistry::shutdown_all()`（`src/plugin_system/registry.rs:184-196`）存在但**从未被任何代码调用**。这意味着插件的 `shutdown()` 回调永远不会被触发。

#### 3.4.1 `test_plugin_shutdown_called_on_workflow_end`
- **目标**：验证工作流完成后插件 shutdown 是否被调用
- **检测方法**：创建记录 shutdown 调用次数的 mock Plugin
- **预期行为**：**当前 shutdown 不会被调用**（测试失败 → 确认 bug）
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_plugin_shutdown_called_on_workflow_end() {
      let shutdown_count = Arc::new(AtomicUsize::new(0));
      let mock_plugin = MockPlugin::new(shutdown_count.clone());

      let schema = parse_dsl(&simple_yaml(), DslFormat::Yaml).unwrap();
      let handle = WorkflowRunner::builder(schema)
          .plugin(mock_plugin)
          .run().await.unwrap();
      let _ = handle.wait().await;

      // 当前预期失败：shutdown 未被调用
      assert!(shutdown_count.load(Ordering::SeqCst) > 0,
          "Plugin shutdown 应在工作流结束后被调用");
  }
  ```

#### 3.4.2 `test_plugin_registry_arc_release`
- **目标**：验证 `Arc<PluginRegistry>` 在工作流完成后引用计数归零
- **检测方法**：`Weak::upgrade()` 检查

### 3.5 [P0] 综合压力测试

#### 3.5.1 `test_repeated_workflow_execution_memory_stable`
- **目标**：N 次完整工作流执行后，进程堆内存增长在可接受范围内
- **检测方法**：`dhat` 全局分配器追踪
- **代码骨架**：
  ```rust
  #[global_allocator]
  static ALLOC: dhat::Alloc = dhat::Alloc;

  #[test]
  fn test_repeated_workflow_execution_memory_stable() {
      let _profiler = dhat::Profiler::new_heap();
      let rt = tokio::runtime::Builder::new_multi_thread()
          .worker_threads(2)
          .enable_all()
          .build().unwrap();

      // 预热（排除首次初始化的影响）
      for _ in 0..5 {
          rt.block_on(run_simple_workflow());
      }
      let warmup_stats = dhat::HeapStats::get();
      let baseline = warmup_stats.curr_bytes;

      // 正式执行
      for _ in 0..50 {
          rt.block_on(run_simple_workflow());
      }
      let final_stats = dhat::HeapStats::get();

      // 验证：50 次执行后内存增长不超过 baseline 的 50%
      let growth = final_stats.curr_bytes as f64 / baseline as f64;
      assert!(growth < 1.5,
          "内存增长 {:.1}x 超过阈值 1.5x (baseline={}, final={})",
          growth, baseline, final_stats.curr_bytes);
  }
  ```

#### 3.5.2 `test_concurrent_workflow_execution_memory_stable`
- **目标**：并发执行多个工作流后内存稳定
- **检测方法**：同上，但用 `tokio::spawn` 并发 10 个工作流，重复 10 轮

### 3.6 [P1] WorkflowDispatcher 资源释放测试

**风险**：`WorkflowDispatcher`（`src/core/dispatcher.rs:90-103`）持有多个 Arc 资源：
- `Arc<RwLock<Graph>>`
- `Arc<RwLock<VariablePool>>`
- `Arc<NodeExecutorRegistry>`
- `Arc<RuntimeContext>`
- `Arc<dyn PluginGate>`
- `Arc<dyn SecurityGate>`

Dispatcher 在 scheduler 的 `tokio::spawn` 中运行（`src/scheduler.rs:481`），所有 Arc 只在 task 退出后释放。

#### 3.6.1 `test_dispatcher_arc_release_after_run`
- **目标**：验证 dispatcher.run() 完成后所有 Arc 资源可释放
- **检测方法**：用 `Weak` 引用追踪 graph、pool、registry
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_dispatcher_arc_release_after_run() {
      let graph = Arc::new(RwLock::new(build_simple_graph()));
      let pool = Arc::new(RwLock::new(VariablePool::new()));
      let registry = Arc::new(NodeExecutorRegistry::new());

      let graph_weak = Arc::downgrade(&graph);
      let pool_weak = Arc::downgrade(&pool);
      let registry_weak = Arc::downgrade(&registry);

      let (tx, _rx) = mpsc::channel(16);
      let emitter = EventEmitter::new(tx, Arc::new(AtomicBool::new(false)));
      let context = Arc::new(RuntimeContext::default());

      let mut dispatcher = WorkflowDispatcher::new_with_registry(
          /* 传入 Arc clone */
      );
      let _ = dispatcher.run().await;
      drop(dispatcher);

      // drop 所有原始 Arc
      drop(graph);
      drop(pool);
      drop(registry);

      assert!(graph_weak.upgrade().is_none(), "Graph Arc 未完全释放");
      assert!(pool_weak.upgrade().is_none(), "Pool Arc 未完全释放");
      assert!(registry_weak.upgrade().is_none(), "Registry Arc 未完全释放");
  }
  ```

#### 3.6.2 `test_dispatcher_cleanup_on_error`
- **目标**：工作流执行失败时资源也正确释放
- **检测方法**：使用会触发 NodeError 的工作流，验证 Arc 释放

#### 3.6.3 `test_dispatcher_cleanup_on_timeout`
- **目标**：工作流超时时资源释放
- **检测方法**：设置极短的 `max_execution_time_secs`，验证 Weak 失效

### 3.7 [P1] SubGraphRunner 资源释放测试

**风险**：`DefaultSubGraphRunner`（`src/core/sub_graph_runner.rs:42-78`）每次执行：
1. 克隆 parent pool（第 48 行）
2. 创建 `mpsc::channel(16)` 但立即 drop receiver `_rx`（第 56 行）
3. 创建新的 dispatcher + context

#### 3.7.1 `test_subgraph_resources_release`
- **目标**：验证子图执行完成后所有临时资源释放
- **检测方法**：Weak 追踪子图的 context Arc

#### 3.7.2 `test_subgraph_pool_isolation`
- **目标**：验证子图 pool 克隆不影响父 pool 内存
- **检测方法**：比较子图执行前后父 pool 的 `estimate_total_bytes()`
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_subgraph_pool_isolation() {
      let mut parent_pool = VariablePool::new();
      parent_pool.set(&selector("start", "x"), Segment::String("hello".into()));
      let before_bytes = parent_pool.estimate_total_bytes();

      let runner = DefaultSubGraphRunner;
      let _ = runner.run_sub_graph(&sub_graph_def, &parent_pool, vars, &context).await;

      let after_bytes = parent_pool.estimate_total_bytes();
      assert_eq!(before_bytes, after_bytes, "子图不应修改父 pool");
  }
  ```

#### 3.7.3 `test_nested_subgraph_resource_release`
- **目标**：多层嵌套子图（迭代节点 → 子图 → 子图）的资源递归释放
- **检测方法**：dhat 追踪嵌套执行后堆分配不累积

### 3.8 [P1] Event Channel 资源释放测试

**风险**：
- `EventEmitter` 持有 `mpsc::Sender`（`src/scheduler.rs:438`）
- 事件收集器 task（`src/scheduler.rs:465-470`）在 `rx.recv()` 上等待
- 如果 collector task panic，events 丢失

#### 3.8.1 `test_event_channel_cleanup_after_workflow`
- **目标**：工作流完成后 event channel 完全关闭
- **检测方法**：验证 `event_active` 标志为 false

#### 3.8.2 `test_events_disabled_no_leak`
- **目标**：`collect_events(false)` 模式下 channel 正确清理
- **检测方法**：验证 rx 被 drop、active 设为 false（`src/scheduler.rs:472-474`）

#### 3.8.3 `test_event_emitter_send_after_rx_drop`
- **目标**：验证 receiver drop 后 emitter.emit() 不阻塞、不 panic
- **检测方法**：drop rx 后调用 emit，验证返回正常

### 3.9 [P1] VariablePool 内存增长测试

**风险**：`VariablePool` 使用 `im::HashMap`（`src/core/variable_pool.rs:854`），clone 是 O(1) 但共享内部节点。大量 set 操作可能导致结构分叉累积。

#### 3.9.1 `test_pool_memory_growth_under_repeated_execution`
- **目标**：多次工作流执行后 pool 内存不无限增长
- **检测方法**：`estimate_total_bytes()` 多轮对比
- **代码骨架**：
  ```rust
  #[tokio::test]
  async fn test_pool_memory_growth_under_repeated_execution() {
      let setup = DispatcherSetup::from_yaml(&diamond_yaml());

      let mut max_bytes = 0usize;
      for _ in 0..100 {
          let pool = make_realistic_pool(10);
          let initial_bytes = pool.estimate_total_bytes();
          setup.run_hot(pool).await;
          max_bytes = max_bytes.max(initial_bytes);
      }

      // 验证：最大 pool 大小在合理范围内
      assert!(max_bytes < 1_000_000, "Pool 大小异常增长");
  }
  ```

#### 3.9.2 `test_pool_clone_structural_sharing`
- **目标**：验证 im::HashMap clone 是结构共享而非深拷贝
- **检测方法**：clone 后修改一个 key，验证 `Arc<SegmentObject>` 的 `strong_count()` 变化符合预期

### 3.10 [P2] Sandbox 资源释放测试

#### 3.10.1 `test_wasm_store_cleanup_after_execution`
- **目标**：验证 WASM Store/Instance 在 spawn_blocking 完成后释放
- **检测方法**：dhat 追踪单次执行前后堆增量
- **注意**：Store 在 blocking task 内创建和 drop，理论上自动清理

#### 3.10.2 `test_wasm_repeated_execution_memory`
- **目标**：重复执行 WASM 代码后 Engine 内存不无限增长
- **检测方法**：dhat 追踪 50 次执行后堆大小

#### 3.10.3 `test_js_context_cleanup_after_execution`
- **目标**：验证 boa_engine::Context 在 spawn_blocking 完成后释放
- **检测方法**：dhat 追踪

#### 3.10.4 `test_js_repeated_execution_memory`
- **目标**：重复执行 JS 代码后内存不增长
- **检测方法**：dhat 追踪

## 4. 实现方案

### 4.1 文件结构

```
tests/
  memory_tests.rs              # 主测试入口
  memory/
    mod.rs                     # 模块组织
    helpers.rs                 # 测试辅助工具
    stream_tests.rs            # 3.1 SegmentStream 资源释放
    task_tests.rs              # 3.2 Tokio task 泄漏
    js_runtime_tests.rs        # 3.3 JsStreamRuntime 泄漏
    plugin_tests.rs            # 3.4 Plugin shutdown
    dispatcher_tests.rs        # 3.6 Dispatcher 资源释放
    subgraph_tests.rs          # 3.7 SubGraph 资源释放
    channel_tests.rs           # 3.8 Event channel 释放
    pool_tests.rs              # 3.9 VariablePool 内存增长
    sandbox_tests.rs           # 3.10 WASM/JS sandbox
    stress_tests.rs            # 3.5 综合压力测试
```

### 4.2 新增 dev-dependency

```toml
[dev-dependencies]
dhat = "0.3"
```

**选择 dhat 的理由**：
- 纯 Rust 实现，无需外部工具（valgrind 等）
- 通过替换全局分配器实现，在 CI 中可直接运行
- 提供 `HeapStats::get()` 获取当前/峰值/总计堆分配统计
- RAII 式 `Profiler`，简洁易用

**限制**：dhat 使用 `#[global_allocator]`，同一进程只能有一个。因此 dhat 测试需要 `--test-threads=1` 或放在独立 binary 中。

### 4.3 测试辅助工具

```rust
// tests/memory/helpers.rs

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// Drop 计数器 — 追踪对象析构次数
pub struct DropCounter {
    counter: Arc<AtomicUsize>,
}

impl DropCounter {
    pub fn new() -> (Self, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        (Self { counter: counter.clone() }, counter)
    }
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

/// 超时保护包装
pub async fn with_timeout<F, T>(label: &str, duration: Duration, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(duration, f)
        .await
        .unwrap_or_else(|_| panic!("'{}' 超时 ({:?})，疑似资源未释放", label, duration))
}

/// 验证 Weak 引用已失效（Arc 已完全释放）
pub fn assert_arc_dropped<T>(weak: &Weak<T>, label: &str) {
    assert!(
        weak.upgrade().is_none(),
        "{} 未被释放 — Arc strong_count > 0",
        label
    );
}

/// 等待异步条件满足或超时
pub async fn wait_for_condition(
    label: &str,
    timeout: Duration,
    interval: Duration,
    condition: impl Fn() -> bool,
) {
    let start = tokio::time::Instant::now();
    while !condition() {
        if start.elapsed() > timeout {
            panic!("条件 '{}' 在 {:?} 内未满足", label, timeout);
        }
        tokio::time::sleep(interval).await;
    }
}
```

### 4.4 与现有基础设施集成

| 现有工具 | 用途 |
|---------|------|
| `benches/helpers/mod.rs` — `DispatcherSetup`, `bench_context()` | 构造 Dispatcher 实例 |
| `benches/helpers/pool_factories.rs` — `make_realistic_pool()` 等 | 创建测试用 VariablePool |
| `benches/helpers/workflow_builders.rs` — `build_linear_workflow()` 等 | 构造测试工作流图 |
| `tests/e2e/runner.rs` — `run_case()` 模式 | 参考 WorkflowRunner 构建模式 |

### 4.5 CI 集成

```yaml
# 内存测试需要单线程运行（dhat 全局分配器限制）
memory-tests:
  runs-on: ubuntu-latest
  steps:
    - run: cargo test --test memory_tests -- --test-threads=1
```

**注意**：非 dhat 的测试（Arc 验证、timeout 检测等）可以多线程运行。建议将 dhat 压力测试单独分组。

## 5. 已发现问题的修复建议

### 5.1 [P0] StreamWriter 增加 Drop 实现

**位置**：`src/core/variable_pool.rs`，`StreamWriter` struct（第 322-327 行）

**问题**：StreamWriter drop 时如果 stream 仍处于 `Running` 状态，reader 在 `notify.notified().await` 处永久等待。

**建议修复**：

```rust
impl Drop for StreamWriter {
    fn drop(&mut self) {
        // 如果 stream 仍在 Running 状态，标记为 Failed
        if let Ok(mut state) = self.state.try_write() {
            if state.status == StreamStatus::Running {
                state.status = StreamStatus::Failed;
                state.error = Some("stream writer dropped without calling end()".to_string());
            }
        }
        self.notify.notify_waiters();
    }
}
```

**注意**：
- 使用 `try_write()` 而非 `.write().await`（Drop 不能是 async）
- `notify_waiters()` 唤醒所有等待的 reader
- 仅在 `Running` 状态时标记失败，避免覆盖正常的 `Completed`/`Failed` 状态

### 5.2 [P0] PluginRegistry shutdown 在工作流结束时调用

**位置**：`src/scheduler.rs` 工作流执行 task 内部（第 481-570 行附近）

**问题**：`PluginRegistry::shutdown_all()` 方法（`src/plugin_system/registry.rs:184`）存在但从未被调用。

**建议修复**：在 scheduler 的 `tokio::spawn` 块末尾、`security_gate.record_workflow_end()` 之后添加：

```rust
tokio::spawn(async move {
    // ... 现有的 dispatcher.run() 逻辑 ...

    security_gate.record_workflow_end(...).await;

    // 新增：shutdown plugins
    #[cfg(feature = "plugin-system")]
    if let Some(registry) = plugin_registry {
        if let Ok(mut reg) = Arc::try_unwrap(registry) {
            let _ = reg.shutdown_all().await;
        }
        // 如果 Arc 还有其他引用，说明被共享使用中，不能 shutdown
    }
});
```

**注意**：
- `shutdown_all()` 需要 `&mut self`，所以需要 `Arc::try_unwrap` 获取独占所有权
- 如果 Arc 有其他引用（如被其他组件共享），则跳过 shutdown（避免影响并发执行的其他工作流）
- 另一种方案：在 `PluginGate` trait 中添加 `shutdown()` 方法，由 `RealPluginGate` 实现

### 5.3 [P0] JsStreamRuntime 增加 Drop 实现

**位置**：`src/nodes/data_transform.rs`，`JsStreamRuntime` struct（第 988-990 行）

**问题**：如果 JsStreamRuntime 被 drop 但未调用 shutdown()，blocking thread 需等 tx drop 后 `blocking_recv()` 返回 `None` 才退出。虽然当前 tx 的 drop 会导致 channel 关闭，但显式 shutdown 更清晰。

**建议修复**：

```rust
impl Drop for JsStreamRuntime {
    fn drop(&mut self) {
        // 尝试发送 Shutdown 命令，如果 channel 已关闭则忽略
        let _ = self.tx.try_send(RuntimeCommand::Shutdown);
    }
}
```

**注意**：当前 tx drop 时 channel 关闭也能导致线程退出（第 1167 行 `None => break`），但 Drop impl 使意图更明确，也保证在 tx 被 clone 的场景下仍能触发 shutdown。

### 5.4 [P1] Spawned task 保留 JoinHandle

**位置**：`src/scheduler.rs:481`（工作流 task）、`src/llm/executor.rs:201`（LLM 流式 task）

**问题**：`tokio::spawn` 返回的 `JoinHandle` 被丢弃，无法追踪或取消 task。

**建议方案 A（abort-on-drop wrapper）**：

```rust
/// 包装 JoinHandle，在 drop 时自动 abort task
pub struct AbortOnDropHandle<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDropHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
```

在 scheduler 中保留 handle：
```rust
let workflow_handle = AbortOnDropHandle(tokio::spawn(async move { ... }));
// workflow_handle 存储在 WorkflowHandle 中
```

**建议方案 B（仅保留 handle，不 abort）**：

在 `WorkflowHandle` 中添加 `JoinHandle` 字段，用于等待 task 完成。这是侵入更小的方案，适合作为第一步。

## 6. 分阶段实施优先级

### Phase 1 — P0 测试（覆盖最高风险）

| 序号 | 测试用例 | 文件 | 验证目标 |
|------|---------|------|---------|
| 1 | 3.1.1 `stream_writer_drop_without_end` | stream_tests.rs | StreamWriter 无 Drop（**暴露 bug**） |
| 2 | 3.1.2 `stream_reader_drop_decrements_count` | stream_tests.rs | Reader 引用计数 |
| 3 | 3.1.3 `stream_arc_cleanup_after_completion` | stream_tests.rs | Stream Arc 释放 |
| 4 | 3.1.4 `stream_multiple_readers_partial_drop` | stream_tests.rs | 多 reader 场景 |
| 5 | 3.2.1 `scheduler_tasks_complete` | task_tests.rs | spawned task 完成 |
| 6 | 3.2.2 `event_collector_exits` | task_tests.rs | collector 退出 |
| 7 | 3.2.3 `llm_stream_task_completes` | task_tests.rs | LLM task 完成 |
| 8 | 3.3.1 `js_runtime_drop_without_shutdown` | js_runtime_tests.rs | blocking thread 退出 |
| 9 | 3.4.1 `plugin_shutdown_called` | plugin_tests.rs | shutdown 调用（**暴露 bug**） |
| 10 | 3.5.1 `repeated_execution_memory_stable` | stress_tests.rs | dhat 综合检测 |
| 11 | 3.5.2 `concurrent_execution_memory_stable` | stress_tests.rs | 并发压力 |

### Phase 2 — P1 测试

| 序号 | 测试用例 | 文件 |
|------|---------|------|
| 12 | 3.6.1 `dispatcher_arc_release` | dispatcher_tests.rs |
| 13 | 3.6.2 `dispatcher_cleanup_on_error` | dispatcher_tests.rs |
| 14 | 3.6.3 `dispatcher_cleanup_on_timeout` | dispatcher_tests.rs |
| 15 | 3.7.1 `subgraph_resources_release` | subgraph_tests.rs |
| 16 | 3.7.2 `subgraph_pool_isolation` | subgraph_tests.rs |
| 17 | 3.7.3 `nested_subgraph_resource_release` | subgraph_tests.rs |
| 18 | 3.8.1 `event_channel_cleanup` | channel_tests.rs |
| 19 | 3.8.2 `events_disabled_no_leak` | channel_tests.rs |
| 20 | 3.8.3 `event_emitter_send_after_rx_drop` | channel_tests.rs |
| 21 | 3.9.1 `pool_memory_growth` | pool_tests.rs |
| 22 | 3.9.2 `pool_clone_structural_sharing` | pool_tests.rs |

### Phase 3 — P2 测试

| 序号 | 测试用例 | 文件 |
|------|---------|------|
| 23 | 3.10.1 `wasm_store_cleanup` | sandbox_tests.rs |
| 24 | 3.10.2 `wasm_repeated_execution_memory` | sandbox_tests.rs |
| 25 | 3.10.3 `js_context_cleanup` | sandbox_tests.rs |
| 26 | 3.10.4 `js_repeated_execution_memory` | sandbox_tests.rs |
