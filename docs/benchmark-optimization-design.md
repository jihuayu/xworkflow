# Benchmark 冷启动开销优化方案

## 1. 问题总览

对现有 12 个 benchmark 文件进行逐一审计，发现 **5 个文件存在"冷启动"问题** — 在 `b.iter()` 测量循环内部重复执行初始化工作，导致测量结果包含大量非目标开销。

### 1.1 审计结果汇总

| 文件 | 分类 | 严重程度 | 问题描述 |
|------|------|----------|----------|
| `bench_meso_dispatcher.rs` | **COLD** | 高 | 每次迭代：parse_dsl + build_graph + Registry::new + channel + spawn |
| `bench_macro_topology.rs` | **COLD** | 高 | 每次迭代：WorkflowRunner::run() 全流程 + 50ms poll-sleep |
| `bench_macro_iteration.rs` | **COLD** | 高 | 同上 + 子图内部 Registry::new() × N次 |
| `bench_macro_scalability.rs` | **COLD** | 高 | 同上 + 迭代内 HashMap 构建（最多 500 条） |
| `bench_meso_sandbox_wasm.rs` | **MIXED** | 中 | precompiled bench 每次创建 Linker/Store/Instance |
| `bench_meso_node_executors.rs` | **HOT** | - | 正确 |
| `bench_meso_sandbox_js.rs` | **HOT** | - | 正确 |
| `bench_micro_variable_pool.rs` | **HOT** | - | 正确（append_array 有方法论问题） |
| `bench_micro_template.rs` | **HOT** | - | 正确 |
| `bench_micro_condition.rs` | **HOT** | - | 正确 |
| `bench_micro_dsl.rs` | **HOT** | - | 正确 |
| `bench_micro_segment.rs` | **HOT** | - | 正确 |

### 1.2 分类定义

- **COLD**：每次迭代都在重复执行初始化，测量结果被冷启动开销严重污染
- **MIXED**：大部分 setup 在外部，但部分非目标初始化仍在迭代内
- **HOT**：所有 setup 在 `b.iter()` 外部，迭代内只有目标操作

## 2. 问题详细分析

### 2.1 问题一：`bench_meso_dispatcher.rs` — 最严重的冷启动

`DispatcherSetup` 辅助结构已经存在于 `helpers/mod.rs`，但 **该文件完全没有使用它**，仍然使用旧的 `run_dispatcher()` 函数：

```rust
// 当前代码 — 每次迭代的完整冷启动路径
async fn run_dispatcher(yaml: &str, pool: VariablePool) {
    let schema = parse_dsl(yaml, DslFormat::Yaml).unwrap();   // ~100μs-1ms 重复解析
    let graph = build_graph(&schema).unwrap();                  // ~100-500μs 重复构建
    let registry = NodeExecutorRegistry::new();                 // ~10μs 重复创建 15 个执行器
    let (tx, mut rx) = mpsc::channel(256);                      // 每次新 channel
    tokio::spawn(async move { while rx.recv().await.is_some() {} }); // 每次 spawn（泄漏！）
    let active = Arc::new(AtomicBool::new(true));               // active=true，事件全部发送
    let emitter = EventEmitter::new(tx, active);
    let context = Arc::new(bench_context());                    // 每次重建 context
    let mut dispatcher = WorkflowDispatcher::new(...);
    let _ = dispatcher.run().await.unwrap();                    // ← 真正要测的
}
```

**具体问题**：
1. `parse_dsl()` + `build_graph()`：YAML 不变，每次重新解析是纯浪费
2. `NodeExecutorRegistry::new()`：15 次 `HashMap::insert` + Box 分配，可预创建 `Arc` 共享
3. `tokio::spawn` drain task：**累积泄漏**，每次迭代 spawn 一个 task 永远不会结束（直到 channel 发送端 drop）
4. `AtomicBool::new(true)`：事件收集开启，每个节点执行都走 channel send 路径
5. `bench_context()` 每次重建：`FakeTimeProvider` 和 `FakeIdGenerator` 都做了 Arc 包装

**已有解决方案未使用**：`helpers/mod.rs` 中的 `DispatcherSetup` 正是为解决此问题而设计：

```rust
// helpers/mod.rs — 已存在但未被使用
impl DispatcherSetup {
    pub fn from_yaml(yaml: &str) -> Self {
        // 一次性：parse_dsl + build_graph + Registry::new + bench_context
    }
    pub async fn run_hot(&self, pool: VariablePool) {
        // 每次迭代：graph.clone() + Arc::clone(registry) + Arc::clone(context)
        // EventEmitter active=false，不发送事件
    }
}
```

### 2.2 问题二：`bench_macro_*.rs` 系列 — WorkflowRunner 黑盒问题

`bench_macro_topology.rs`、`bench_macro_iteration.rs`、`bench_macro_scalability.rs` 三个文件都使用 `WorkflowRunner::builder(schema).run()` 作为测量路径。

`WorkflowRunner::run()` 内部（`scheduler.rs:459-737`）每次执行：

```
1. validate_schema()              ~100μs-1ms    Schema 校验
2. build_graph()                  ~100-500μs    图构建
3. VariablePool::new() + 变量注入   ~10-100μs    变量池初始化
4. NodeExecutorRegistry::new()    ~10μs         执行器注册
5. LlmProviderRegistry::new()    ~1μs          LLM 提供者注册
6. mpsc::channel(256)            ~1μs          事件 channel
7. tokio::spawn(event_collector)  ~10μs         事件收集 task
8. tokio::spawn(dispatcher)       ~10μs         调度器 task
9. dispatcher.run()               ~55-60ms      实际调度（>95% 耗时）
```

**额外问题 — `handle.wait()` 的 poll-sleep**：

```rust
// scheduler.rs 中 WorkflowHandle::wait() 的实现
pub async fn wait(&self) -> ExecutionStatus {
    loop {
        let status = self.status.lock().await;
        if *status != ExecutionStatus::Running { return status.clone(); }
        drop(status);
        tokio::time::sleep(Duration::from_millis(50)).await;  // ← 50ms 轮询间隔！
    }
}
```

每次迭代额外引入 **0-50ms 的随机延迟**（取决于 dispatcher 完成时刻与 poll 时刻的相对位置）。这对 benchmark 的影响是灾难性的 — 一个 60ms 的操作可能测出 60-110ms，且噪声完全不可控。

### 2.3 问题三：`DefaultSubGraphRunner` — 子图执行的 N 倍冷启动

`src/core/sub_graph_runner.rs:42-74` 中，每次子图执行都创建完整的运行环境：

```rust
async fn execute_sub_graph_default(sub_graph, parent_pool, scope_vars, context) {
    let mut scoped_pool = parent_pool.clone();          // clone 整个父变量池
    inject_scope_vars(&mut scoped_pool, scope_vars)?;
    let graph = build_sub_graph(sub_graph)?;             // 每次重建图
    let registry = NodeExecutorRegistry::new();          // 每次创建新 Registry（15 个执行器）
    let (tx, _rx) = mpsc::channel(16);                   // 每次新 channel
    let active = Arc::new(AtomicBool::new(false));
    let emitter = EventEmitter::new(tx.clone(), active);
    let sub_context = context.clone().with_event_tx(tx.clone());
    let mut dispatcher = WorkflowDispatcher::new(...);
    dispatcher.run().await?;
}
```

**影响范围**：所有迭代节点（Iteration/Loop/ListOperator）都通过 `SubGraphRunner` 执行子图。

**乘数效应**：

| Benchmark | 子图执行次数 | Registry 重建次数 | Graph 重建次数 |
|-----------|-------------|-------------------|---------------|
| `iter_seq_10` | 10 | 10 | 10 |
| `iter_seq_100` | 100 | 100 | 100 |
| `iter_par_100_p10` | 100 | 100 | 100 |
| `iter_par_100_p50` | 100 | 100 | 100 |

对于 `iter_par_100_p10`（100 项，并行度 10），每次 benchmark 迭代：
- 100 次 `parent_pool.clone()`
- 100 次 `build_sub_graph()`（HashMap 构建 + 边关系计算）
- 100 次 `NodeExecutorRegistry::new()`（15 × Box 分配 + HashMap insert）
- 100 次 `mpsc::channel(16)` + `context.clone()`
- 100 个 `WorkflowDispatcher` 实例创建 + 运行

这里有两层问题：
1. **Benchmark 层面**：测量值被子图初始化开销污染，无法区分"迭代调度性能"和"子图启动性能"
2. **运行时层面**：`DefaultSubGraphRunner` 没有复用 Registry 和 Graph 模板，是一个**真实的性能优化点**

### 2.4 问题四：`bench_macro_scalability.rs` — 迭代内数据构建

```rust
// scale_pool_size 组：最多 500 次 HashMap::insert 在 iter 内
b.to_async(&rt).iter(|| async {
    let mut sys_vars = HashMap::new();
    for i in 0..*size {                                    // size 最大 500
        sys_vars.insert(format!("k{}", i), serde_json::json!(i));  // 500 次 String 分配 + JSON 序列化
    }
    run_workflow(schema.clone(), HashMap::new(), sys_vars).await;
});

// scale_data_size 组：最多 100KB 字符串在 iter 内
b.to_async(&rt).iter(|| async {
    let mut sys_vars = HashMap::new();
    sys_vars.insert("blob".to_string(), serde_json::json!("x".repeat(*size))); // 最大 100KB
    run_workflow(schema.clone(), HashMap::new(), sys_vars).await;
});
```

这些数据构建开销虽然不大（~10-100μs），但与 `run_workflow` 的初始化开销叠加后，使得 scalability 曲线反映的是"初始化 + 数据构建 + 执行"的混合开销，而非纯调度性能随参数的扩展特性。

### 2.5 问题五：`bench_meso_sandbox_wasm.rs` — WASM 实例化开销

`wasm_execute_precompiled` benchmark 名字暗示测量"预编译 WASM 执行"，但实际每次迭代创建 `Linker` + `Store` + `Instance`：

```rust
// execute_precompiled() 内部
fn execute_precompiled(engine, module, input) {
    let mut linker = Linker::new(engine);           // 每次创建 Linker
    linker.func_wrap(...)?;                          // 每次注册宿主函数
    let mut store = Store::new(engine, limits);      // 每次创建 Store
    let instance = linker.instantiate(&mut store, module)?; // 每次实例化模块
    // ... 执行
}
```

如果目标是测"纯 WASM 函数调用性能"，那 Linker/Store/Instance 创建应该在 setup 中。如果目标是测"WASM 实例化 + 执行"，则当前是正确的，但名称应改为 `wasm_instantiate_and_execute`。

### 2.6 次要问题：`pool_append_array` 状态累积

`bench_micro_variable_pool.rs` 中的 `pool_append_array`：

```rust
c.bench_function("pool_append_array", |b| {
    let mut pool = VariablePool::new();           // 只创建一次
    let selector = vec!["n".to_string(), "arr".to_string()];
    b.iter(|| {
        pool.append_array(&selector, Segment::String("v".into())); // 数组不断增长
    });
});
```

随着 criterion 执行数千次迭代，数组会无限增长，导致后期迭代比早期慢得多。这不是冷启动问题，但属于方法论错误 — 测量的是"向不断增长的数组追加"而非"向固定大小数组追加"。

## 3. 优化方案

### 3.1 方案一：`bench_meso_dispatcher.rs` 迁移到 `DispatcherSetup`

**改动**：删除 `run_dispatcher()` 函数，所有 bench 改用已有的 `DispatcherSetup`。

改造前（每次迭代 ~1-2ms 冷启动）：
```rust
c.bench_function("dispatch_start_end", |b| {
    let yaml = build_linear_workflow(1, "template-transform");
    b.to_async(&rt).iter(|| async {
        run_dispatcher(&yaml, VariablePool::new()).await;
    });
});
```

改造后（冷启动归零）：
```rust
c.bench_function("dispatch_start_end", |b| {
    let yaml = build_linear_workflow(1, "template-transform");
    let setup = DispatcherSetup::from_yaml(&yaml);
    b.to_async(&rt).iter(|| async {
        setup.run_hot(VariablePool::new()).await;
    });
});
```

**消除的开销**：
- parse_dsl：~100μs-1ms/iter → 0
- build_graph：~100-500μs/iter → 0
- NodeExecutorRegistry::new：~10μs/iter → 0（Arc::clone ~0ns）
- channel + spawn drain：~50μs/iter → 0（EventEmitter active=false，无 spawn）
- bench_context 重建：~10μs/iter → 0（Arc::clone ~0ns）
- **消除 task 泄漏**：旧代码每次迭代 spawn 一个永不结束的 drain task

### 3.2 方案二：`bench_macro_*.rs` 系列改用 `DispatcherSetup`

**核心改动**：不再使用 `WorkflowRunner::builder().run()`，而是直接使用 `DispatcherSetup`。

**原因**：`WorkflowRunner::run()` 是为生产使用设计的完整启动路径，包含验证、插件初始化、event 收集、task spawn 等。Benchmark 需要的只是调度器热路径。

#### 3.2.1 `bench_macro_topology.rs`

改造前：
```rust
async fn run_workflow(schema: WorkflowSchema, inputs: HashMap<String, Value>) {
    let runner = WorkflowRunner::builder(schema).user_inputs(inputs);
    let handle = runner.run().await.unwrap();   // 全量初始化
    let status = handle.wait().await;           // 50ms poll-sleep
}
```

改造后：
```rust
// setup 阶段
let yaml = build_linear_workflow(10, "template-transform");
let setup = DispatcherSetup::from_yaml(&yaml);

// 热路径
b.to_async(&rt).iter(|| async {
    let mut pool = VariablePool::new();
    pool.set(&["start".into(), "query".into()], Segment::String("bench".into()));
    setup.run_hot(pool).await;
});
```

**消除的开销**：
- validate_schema：~100μs-1ms/iter → 0
- build_graph：~100-500μs/iter → 0
- Registry + LlmRegistry 创建：~10μs/iter → 0
- 2 × tokio::spawn：~20μs/iter → 0
- **50ms poll-sleep：~0-50ms/iter → 0**（这是最关键的改进）

#### 3.2.2 `bench_macro_scalability.rs`

除了使用 `DispatcherSetup` 外，还需将数据构建移到 setup 阶段：

```rust
// scale_pool_size 改造
for size in [10, 50, 100, 500] {
    let yaml = build_linear_workflow(5, "template-transform");
    let setup = DispatcherSetup::from_yaml(&yaml);

    // 预构建变量池（不计入测量）
    let base_pool = {
        let mut pool = VariablePool::new();
        for i in 0..size {
            pool.set(&["sys".into(), format!("k{}", i)], Segment::from_value(&json!(i)));
        }
        pool
    };

    group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
        b.to_async(&rt).iter(|| async {
            setup.run_hot(base_pool.clone()).await;  // 只测量 pool clone + dispatch
        });
    });
}
```

`scale_data_size` 同理，预构建包含大字符串的变量池。

#### 3.2.3 `bench_macro_iteration.rs`

```rust
let yaml = build_iteration_workflow(100, true, 10);
let setup = DispatcherSetup::from_yaml(&yaml);

// 预构建输入
let base_pool = {
    let mut pool = VariablePool::new();
    let items: Vec<String> = (0..100).map(|i| format!("item{}", i)).collect();
    pool.set(&["start".into(), "items".into()], Segment::from_value(&json!(items)));
    pool
};

b.to_async(&rt).iter(|| async {
    setup.run_hot(base_pool.clone()).await;
});
```

### 3.3 方案三：`DefaultSubGraphRunner` 资源复用（运行时优化）

这是一个**运行时代码优化**，不仅改善 benchmark 结果，还能提升生产环境性能。

#### 3.3.1 问题根源

`DefaultSubGraphRunner::execute_sub_graph_default()` 的三个可复用资源：

| 资源 | 当前 | 优化后 | 说明 |
|------|------|--------|------|
| `NodeExecutorRegistry` | 每次子图执行创建 | 从父 context 继承 | 子图使用的节点类型与父图相同 |
| `Graph`（从 SubGraphDefinition） | 每次 `build_sub_graph()` | 缓存或预构建 | 同一 iteration 节点的子图定义不变 |
| `mpsc::channel` | 每次创建 | 复用父级 EventEmitter 或关闭 | 子图事件可以合并到父级 |

#### 3.3.2 优化方案

**方案 A：通过 RuntimeContext 传递共享 Registry**

```rust
// RuntimeContext 中添加可选的 Registry 引用
pub struct RuntimeContext {
    // ... 已有字段
    pub node_executor_registry: Option<Arc<NodeExecutorRegistry>>,
}

// DefaultSubGraphRunner 优先使用 context 中的 Registry
async fn execute_sub_graph_default(sub_graph, parent_pool, scope_vars, context) {
    let mut scoped_pool = parent_pool.clone();
    inject_scope_vars(&mut scoped_pool, scope_vars)?;
    let graph = build_sub_graph(sub_graph)?;

    // 复用父级 Registry，避免每次重建
    let registry = context.node_executor_registry
        .clone()
        .unwrap_or_else(|| Arc::new(NodeExecutorRegistry::new()));

    let (tx, _rx) = mpsc::channel(16);
    let active = Arc::new(AtomicBool::new(false));
    let emitter = EventEmitter::new(tx.clone(), active);
    let sub_context = context.clone().with_event_tx(tx.clone());

    let mut dispatcher = WorkflowDispatcher::new_with_registry(
        graph, scoped_pool, registry, emitter,
        EngineConfig::default(), Arc::new(sub_context), None,
    );
    dispatcher.run().await?;
}
```

**方案 B：缓存子图 Graph 构建结果**

对于 Iteration 节点，同一个 `SubGraphDefinition` 在所有迭代项中是相同的。可以在 Iteration 执行器层面预构建 Graph，然后每次迭代只 clone：

```rust
// IterationNodeExecutor::execute_parallel() 中
let base_graph = build_sub_graph(&config.sub_graph)?;  // 只构建一次

for (index, item) in items.iter().enumerate() {
    let graph = base_graph.clone();  // 每次迭代只 clone
    // ... 传递给 SubGraphRunner
}
```

这需要修改 `SubGraphRunner` trait 接受 `Graph` 而非 `SubGraphDefinition`，或者添加一个接受预构建 Graph 的方法。

**方案 C：批量执行优化（高级）**

为 Iteration 场景提供专门的批量子图执行器，一次性创建 Registry + Graph，然后只切换变量池执行多次：

```rust
pub trait SubGraphRunner: Send + Sync {
    // 已有方法
    async fn run_sub_graph(...) -> Result<Value, SubGraphError>;

    // 新增：批量执行，共享 Registry 和 Graph
    async fn run_sub_graph_batch(
        &self,
        sub_graph: &SubGraphDefinition,
        parent_pool: &VariablePool,
        batch_scope_vars: Vec<HashMap<String, Value>>,
        context: &RuntimeContext,
    ) -> Result<Vec<Value>, SubGraphError> {
        // 默认实现：逐个调用 run_sub_graph（兼容）
    }
}
```

**推荐**：先实现方案 A（通过 context 传递 Registry），再考虑方案 B（Graph 缓存）。方案 C 属于进一步优化，可后续实施。

#### 3.3.3 预期效果

以 `iter_par_100_p10` 为例（100 项，并行度 10）：

| 阶段 | 优化前（每次子图） | 方案A后 | 方案A+B后 |
|------|-------------------|---------|-----------|
| Registry 创建 | ~10μs × 100 = 1ms | 0（Arc::clone） | 0 |
| Graph 构建 | ~50μs × 100 = 5ms | ~50μs × 100 = 5ms | ~50μs × 1 + clone × 99 |
| Pool clone | ~10μs × 100 = 1ms | ~10μs × 100 = 1ms | 同左 |
| **总减少** | - | **~1ms** | **~5-6ms** |

### 3.4 方案四：WASM precompiled benchmark 语义澄清

两个选项：

**选项 A**：保持当前行为，修改名称为 `wasm_instantiate_and_execute`，明确测量的是"实例化+执行"。

**选项 B**：新增一个真正的 `wasm_execute_only` benchmark，在 setup 中预创建 Instance：

```rust
// setup（不计入测量）
let (store, instance) = setup_wasm_instance(&engine, &module, &input);

// 热路径（只测函数调用）
b.iter(|| {
    let func = instance.get_typed_func::<...>(&store, "transform").unwrap();
    let result = func.call(&mut store, ...).unwrap();
    black_box(result);
});
```

**推荐**：选项 A（改名），因为 WASM 的典型使用模式就是"编译一次、每次请求实例化+执行"，当前测量方式反映了真实场景。

### 3.5 方案五：`pool_append_array` 方法论修复

```rust
// 改造前：数组无限增长
c.bench_function("pool_append_array", |b| {
    let mut pool = VariablePool::new();
    b.iter(|| {
        pool.append_array(&selector, Segment::String("v".into())); // 越来越慢
    });
});

// 改造后：每次迭代重置到固定大小
c.bench_function("pool_append_array", |b| {
    b.iter_batched(
        || {
            let mut pool = VariablePool::new();
            pool.set(&selector, Segment::Array(vec![Segment::String("x".into()); 100]));
            pool
        },
        |mut pool| {
            pool.append_array(&selector, Segment::String("v".into()));
        },
        criterion::BatchSize::SmallInput,
    );
});
```

## 4. 实施优先级

| 优先级 | 方案 | 改动范围 | 影响 |
|--------|------|----------|------|
| **P0** | 方案一：dispatcher bench 迁移到 DispatcherSetup | `bench_meso_dispatcher.rs` | 消除冷启动 + 修复 task 泄漏 |
| **P0** | 方案二：macro bench 迁移到 DispatcherSetup | `bench_macro_topology.rs`、`bench_macro_scalability.rs`、`bench_macro_iteration.rs` | 消除冷启动 + **消除 50ms poll-sleep** |
| **P1** | 方案三-A：SubGraphRunner 复用 Registry | `sub_graph_runner.rs`、`runtime_context.rs`、`scheduler.rs` | 运行时性能优化，迭代场景 ~1ms 改善 |
| **P1** | 方案三-B：SubGraphRunner 缓存 Graph | `sub_graph_runner.rs`、`subgraph_nodes.rs` | 运行时性能优化，迭代场景额外 ~5ms 改善 |
| **P2** | 方案四：WASM bench 语义澄清 | `bench_meso_sandbox_wasm.rs` | 改名或新增 bench |
| **P2** | 方案五：append_array 方法论修复 | `bench_micro_variable_pool.rs` | 测量精度改善 |

## 5. helpers/mod.rs 扩展

当前 `DispatcherSetup` 需要补充以支持 macro bench 的使用场景：

```rust
impl DispatcherSetup {
    /// 从 YAML 创建（已有）
    pub fn from_yaml(yaml: &str) -> Self { ... }

    /// 从已解析的 schema 创建（新增，避免 topology/iteration bench 重复解析）
    pub fn from_schema(schema: &WorkflowSchema) -> Self {
        let graph = build_graph(schema).unwrap();
        let registry = Arc::new(NodeExecutorRegistry::new());
        let context = Arc::new(bench_context());
        Self { graph, registry, context, config: EngineConfig::default() }
    }

    /// 热路径执行（已有）
    pub async fn run_hot(&self, pool: VariablePool) { ... }

    /// 热路径执行，返回输出（新增，供需要检查结果的 bench 使用）
    pub async fn run_hot_with_outputs(&self, pool: VariablePool)
        -> HashMap<String, Value>
    {
        let graph = self.graph.clone();
        let emitter = Self::make_emitter();
        let mut dispatcher = WorkflowDispatcher::new_with_registry(
            graph, pool, Arc::clone(&self.registry), emitter,
            self.config.clone(), Arc::clone(&self.context), None,
        );
        dispatcher.run().await.unwrap()
    }
}
```

## 6. 验证标准

1. **编译通过**：`cargo bench --no-run` 无错误
2. **结果对比**：
   - `dispatch_start_end`：应从 ~5ms 降到 ~1ms（去除冷启动后，2 节点的纯调度开销）
   - `topo_minimal`：应从 ~60-110ms 降到 ~1ms（去除冷启动 + 50ms poll-sleep）
   - `iter_par_100_p10`：应从 ~200ms+ 降到 ~150ms（去除外层冷启动，子图冷启动需要方案三）
3. **稳定性**：消除 50ms poll-sleep 后，`bench_macro_*` 系列的标准差应大幅降低
4. **task 泄漏**：改造后，benchmark 进程内存使用应保持稳定（不再随迭代次数增长）
5. **报告检查**：`target/criterion/report/index.html` 中各 bench 数据合理
