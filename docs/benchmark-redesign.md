# Benchmark 重新设计方案

## 1. 背景与动机

### 1.1 问题陈述

当前 bench 存在两个核心问题：

**问题一：每次迭代包含不必要的重复工作**

- `bench_meso_dispatcher.rs`：每次迭代都调用 `parse_dsl` + `build_graph` + `NodeExecutorRegistry::new()` + 创建 channel + spawn task
- `bench_macro_*.rs`：每次迭代都走完整的 `WorkflowRunner::builder().run()` 路径（验证、图构建、注册表创建、channel 创建、task spawn + wait）

虽然这些初始化开销不大（~1-2ms），但它们引入了不必要的测量噪声，且不反映生产环境的使用模式。

**问题二：无法精确定位性能瓶颈**

~60ms 的总耗时中，绝大部分来自 `dispatcher.run()` 内部的执行循环。但当前 bench 将初始化和执行混在一起测量，无法区分：
- 调度器本身的开销（队列管理、状态转换）
- VariablePool clone 的开销（已知瓶颈 #1）
- 节点执行的开销
- RwLock 竞争的开销

**生产环境中**：引擎初始化一次，然后执行多个工作流。Bench 应该反映这种使用模式。

### 1.2 开销分析

通过代码审查，各阶段的实际开销：

| 阶段 | 代码位置 | 实际开销 | 说明 |
|------|---------|---------|------|
| DSL 验证 | `scheduler.rs:462-479` | ~100μs-1ms | Schema 校验，轻量 |
| 图构建 | `scheduler.rs:534` | ~100μs-500μs | HashMap 插入 + JSON 序列化 |
| Registry 创建 | `executor.rs:47-111` | **~10μs** | 15 个 HashMap::insert，大部分执行器是 ZST |
| 插件初始化 | `scheduler.rs:481-517` | **0**（bench 中） | `should_init = false`，不执行 |
| 变量池初始化 | `scheduler.rs:537-574` | ~10-100μs | HashMap 插入 |
| Channel + Task spawn | `scheduler.rs:615-688` | ~10-50μs | mpsc::channel + tokio::spawn |
| Dispatcher 创建 | `scheduler.rs:698` | ~1μs | Arc/RwLock 包装 |
| **dispatcher.run()** | `dispatcher.rs:495-1222` | **~55-60ms** | **占总耗时 >95%** |

**关键发现**：

- `NodeExecutorRegistry::new()` 只做 ~15 次 `HashMap::insert`，大部分执行器是零大小结构体（ZST），Code 节点用 `register_lazy` 延迟创建，**不会触发 boa_engine/wasmtime 初始化**
- 插件初始化在 bench 中根本不执行（没有配置插件）
- **~60ms 中 >95% 来自 `dispatcher.run()` 的执行循环**

### 1.3 dispatcher.run() 内部开销分解

`dispatcher.run()` 对每个节点的执行路径（`dispatcher.rs` 主循环）：

```
每个节点执行：
  1. graph.read().await              — RwLock 读锁
  2. variable_pool.read().await.clone()  — 完整 clone 变量池 ← 主要开销
  3. executor.execute()              — 节点逻辑执行
  4. variable_pool.write().await     — 写回输出（set_node_outputs）
  5. graph.write().await             — 更新节点状态
```

对于 50 节点线性工作流：
- 50 次 VariablePool clone（随着变量累积，每次 clone 越来越大）
- 50 次 RwLock 读写切换
- 50 次节点执行（template-transform 涉及 Jinja2 渲染）

**已知瓶颈**（来自 `docs/benchmark-design.md`）：
1. VariablePool 每次节点执行都做完整 clone（`dispatcher.rs:705`）
2. Regex 每次 render_template 调用都重新编译（`template/engine.rs:7`）
3. Jinja2 每次都创建新 Environment（`template/engine.rs:22`）

### 1.4 改造目标

- 将初始化（DSL 解析、图构建、Registry 创建）移到 bench setup 阶段，消除 ~1-2ms 噪声
- 关闭事件收集，消除 channel/spawn 噪声
- 保持 `dispatcher.run()` 的完整执行路径，精确测量真实调度性能
- 通过更好的 bench 组织，支持定位具体瓶颈

## 2. 设计原则

1. **预初始化共享组件**：Graph、Registry、RuntimeContext 在 bench setup 中创建，不计入测量
2. **只测量热路径**：每次迭代只包含 graph.clone() + VariablePool 创建 + Dispatcher 执行
3. **精简文件结构**：从 12 个文件合并为 4 个，按测量目标组织
4. **更好的报告**：自定义 criterion 配置 + 有意义的 benchmark 命名/分组

## 3. 新文件结构

```
benches/
├── helpers/
│   ├── mod.rs                    # 共享工具 + DispatcherSetup（新增）
│   ├── workflow_builders.rs      # DSL 生成器（保留）
│   └── pool_factories.rs         # VariablePool 工厂（保留）
├── bench_dispatch.rs             # 核心：调度器热路径性能
├── bench_scalability.rs          # 核心：参数化扩展性
├── bench_components.rs           # 组件：节点执行器 + 沙箱
└── bench_data_ops.rs             # 数据：变量池 + 模板 + Segment + 条件 + DSL 解析
```

### 3.1 文件合并映射

| 旧文件 | 新文件 | 原因 |
|--------|--------|------|
| `bench_meso_dispatcher.rs` | `bench_dispatch.rs` | 调度器热路径 |
| `bench_macro_topology.rs` | `bench_dispatch.rs` | 拓扑也是调度 |
| `bench_macro_iteration.rs` | `bench_dispatch.rs` | 迭代也是调度 |
| `bench_macro_scalability.rs` | `bench_scalability.rs` | 保留，修复热路径 |
| `bench_meso_node_executors.rs` | `bench_components.rs` | 组件性能 |
| `bench_meso_sandbox_js.rs` | `bench_components.rs` | 组件性能 |
| `bench_meso_sandbox_wasm.rs` | `bench_components.rs` | 组件性能 |
| `bench_micro_variable_pool.rs` | `bench_data_ops.rs` | 数据操作 |
| `bench_micro_template.rs` | `bench_data_ops.rs` | 数据操作 |
| `bench_micro_condition.rs` | `bench_data_ops.rs` | 数据操作 |
| `bench_micro_dsl.rs` | `bench_data_ops.rs` | 数据操作 |
| `bench_micro_segment.rs` | `bench_data_ops.rs` | 数据操作 |

## 4. 关键改造：热路径 Dispatcher 测量

### 4.1 当前问题

以 `bench_meso_dispatcher.rs` 为例，每次迭代的 `run_dispatcher()` 包含：

```
parse_dsl()                      ~500μs  ← 不必要的重复
build_graph()                    ~200μs  ← 不必要的重复
NodeExecutorRegistry::new()      ~10μs   ← 不必要的重复
mpsc::channel + spawn            ~50μs   ← 噪声
dispatcher.run()                 ~58ms   ← 真正要测的
```

虽然初始化开销只占 ~1-2%，但它们是不必要的噪声，且 `tokio::spawn` 的 task 调度会引入不确定性。

### 4.2 改造方案

引入 `DispatcherSetup` 辅助结构，将初始化移到 bench setup 阶段：

**Setup 阶段**（不计入测量）：
- 解析 DSL → Graph
- 创建 NodeExecutorRegistry（Arc 共享）
- 创建 RuntimeContext（Arc 共享）

**热路径**（每次迭代测量）：
- `graph.clone()` — 重置节点状态
- 创建 VariablePool — 注入输入变量
- 创建 EventEmitter（事件收集关闭，减少噪声）
- `WorkflowDispatcher::new()` + `dispatcher.run()`

### 4.3 改造效果

| 项目 | 改造前 | 改造后 |
|------|--------|--------|
| Registry | 每次迭代创建 ~10μs | 预创建，Arc::clone ~0ns |
| DSL 解析 + 图构建 | 每次迭代 ~700μs | 预完成，只 clone 图状态 |
| 事件收集 | 开启，spawn task ~50μs | 关闭，无 task 调度噪声 |
| 测量精度 | 包含 ~1-2ms 噪声 | 纯 dispatcher.run() |

> 注意：改造后总耗时不会显著下降（~60ms → ~58ms），因为 >95% 的时间本来就在 dispatcher.run() 中。改造的价值在于**消除噪声、提高测量精度、反映真实使用模式**。

## 5. 各 Bench 文件详细设计

### 5.1 `bench_dispatch.rs` — 调度器热路径（核心）

使用 `benchmark_group` 组织，所有 bench 共享预初始化的 Registry + Context。

| Group | Benchmark | 拓扑 | 说明 |
|-------|-----------|------|------|
| `dispatch/linear` | `2_nodes` | start→end | 最小基线 |
| `dispatch/linear` | `5_nodes` | 5 节点链 | 短链 |
| `dispatch/linear` | `10_nodes` | 10 节点链 | 中链 |
| `dispatch/linear` | `50_nodes` | 50 节点链 | 长链，pool clone 开销可见 |
| `dispatch/branch` | `2_way` | 2 路 if-else | 基础分支 |
| `dispatch/branch` | `10_way` | 10 路扇出 | 多分支 |
| `dispatch/diamond` | `10_wide` | 10 宽菱形 | 扇出+汇聚 |
| `dispatch/diamond` | `deep_10` | 10 层嵌套 | 深度分支链 |
| `dispatch/iteration` | `seq_10` | 顺序迭代 10 项 | 迭代基线 |
| `dispatch/iteration` | `seq_100` | 顺序迭代 100 项 | 大规模迭代 |
| `dispatch/iteration` | `par_10_p10` | 并行迭代 10 项 | 全并行 |
| `dispatch/iteration` | `par_100_p10` | 并行迭代 100 项 | 大规模并行 |
| `dispatch/realistic` | `mixed` | 混合拓扑 | 模拟真实业务流 |

### 5.2 `bench_scalability.rs` — 参数化扩展性（核心）

同样使用热路径模式。每个 group 用 `BenchmarkId::from_parameter` 生成扩展曲线。

| Group | 参数 | 值 | 说明 |
|-------|------|-----|------|
| `scale/nodes` | 节点数 | 5, 10, 25, 50, 100, 200 | 线性链扩展 |
| `scale/branches` | 分支数 | 2, 5, 10, 20, 50 | if-else 扇出 |
| `scale/pool_size` | 变量数 | 10, 50, 100, 500 | 变量池大小 |
| `scale/data_size` | 数据量 | 100B, 1KB, 10KB, 100KB | 单变量大小 |
| `scale/conditions` | 条件数 | 1, 5, 10, 20, 50 | 条件评估 |
| `scale/template_vars` | 模板变量数 | 1, 5, 10, 25, 50 | 模板渲染 |

### 5.3 `bench_components.rs` — 组件性能

这些已经是正确的热路径测量（执行器/沙箱预创建），保持现有逻辑，合并到一个文件。

| Group | Benchmark | 说明 |
|-------|-----------|------|
| `node/start` | `5_vars` | Start 节点 |
| `node/end` | `5_outputs` | End 节点 |
| `node/answer` | `5_vars` | Answer 模板渲染 |
| `node/ifelse` | `5x5_conditions` | 25 个条件 |
| `node/template` | `5_vars` | 模板转换 |
| `node/aggregator` | `5_selectors` | 变量聚合 |
| `node/code` | `js_simple` | JS 代码节点 |
| `sandbox/js` | `noop`, `arithmetic`, `string`, `json`, `array_100`, `array_1000`, `sha256`, `uuid`, `base64`, `input_10k`, `input_100k` | JS 沙箱各场景 |
| `sandbox/wasm` | `compile_execute`, `precompiled`, `validation`, `fuel_overhead` | WASM 沙箱各场景 |

### 5.4 `bench_data_ops.rs` — 数据操作

纯同步操作，无需 async runtime。合并 5 个 micro bench。

| Group | Benchmark | 说明 |
|-------|-----------|------|
| `pool/write` | `set_string`, `set_node_outputs_5` | 写入 |
| `pool/read` | `get_hit`, `get_miss`, `has_check`, `get_node_variables` | 读取 |
| `pool/clone` | `10`, `50`, `100`, `500`, `large_objects` | clone 开销 |
| `pool/mutate` | `append_array` | 追加 |
| `template/dify` | `no_vars`, `1_var`, `10_vars`, `50_vars`, `large_1kb` | Dify 模板 |
| `template/jinja2` | `simple`, `loop_100`, `conditional`, `complex` | Jinja2 |
| `condition/eval` | `string_is`, `string_contains`, `numeric_gt`, `in_10`, `in_100` | 条件 |
| `condition/case` | `and_5`, `or_5`, `10_first_match`, `10_last_match` | Case 评估 |
| `segment/to_value` | `string`, `nested_object`, `array_1000` | Segment→Value |
| `segment/from_value` | `string`, `nested_object`, `array_1000` | Value→Segment |
| `dsl/parse` | `yaml_2`, `yaml_10`, `yaml_50`, `yaml_200`, `json_50` | DSL 解析 |
| `dsl/graph` | `build_2`, `build_50`, `build_200` | 图构建 |

## 6. HTML 报告优化

### 6.1 问题

Criterion 默认 HTML 报告可读性差：
- 每个 benchmark 独立页面，缺乏全局概览
- 扁平命名（`dispatch_linear_50`）导致导航困难
- 没有统一的测量参数配置

### 6.2 方案

**分层命名**：使用 `/` 分隔符，Criterion 会按层级组织 HTML 报告目录：
- `dispatch/linear/50_nodes` 而非 `dispatch_linear_50`
- `scale/nodes/100` 而非 `scale_node_count_100`

**参数化 bench**：使用 `BenchmarkId::from_parameter` 自动生成对比图表和扩展曲线。

**统一 Criterion 配置**：

| 参数 | 值 | 说明 |
|------|-----|------|
| `sample_size` | 100 | 足够的样本量 |
| `measurement_time` | 5s | 每个 bench 5 秒 |
| `warm_up_time` | 2s | 预热 2 秒 |
| `significance_level` | 0.05 | 5% 显著性 |
| `noise_threshold` | 0.02 | 2% 噪声阈值 |

## 7. Cargo.toml 修改

删除旧的 12 个 `[[bench]]` 入口，替换为 4 个：

```toml
[[bench]]
name = "bench_dispatch"
harness = false

[[bench]]
name = "bench_scalability"
harness = false

[[bench]]
name = "bench_components"
harness = false

[[bench]]
name = "bench_data_ops"
harness = false
```

## 8. helpers/mod.rs 新增

新增 `DispatcherSetup` 结构体，封装预初始化逻辑：

- `DispatcherSetup::from_yaml(yaml)` — 解析 DSL、构建图、创建 Registry 和 Context
- `DispatcherSetup::run_hot(pool)` — 热路径执行：clone 图 + 创建 dispatcher + run
- 事件收集默认关闭（`AtomicBool::new(false)`），减少测量噪声

## 9. 验证方式

1. `cargo bench` 全部通过，无编译错误
2. 对比改造前后：总耗时基本不变（~60ms → ~58ms），但测量噪声降低
3. `target/criterion/report/index.html` 报告按 group 分层展示
4. 参数化 bench（`scale/*`）生成可视化扩展曲线
5. 通过引入已知优化（如缓存 Regex、VariablePool COW）验证 bench 灵敏度
