# xworkflow 性能基准测试方案

## Context

xworkflow 是一个高性能的 Rust 工作流引擎，基于 Tokio 异步运行时、DAG 图执行模型、多语言沙箱（JS/WASM）和插件系统。目前项目没有任何性能基准测试。本方案设计一套全面的 criterion 基准测试套件，覆盖从单组件微基准到端到端工作流宏基准的所有典型场景，用于性能回归追踪和瓶颈定位。

## 已识别的潜在瓶颈

通过代码分析，发现以下热点需要基准测试量化：

1. **VariablePool 每次节点执行都做完整 clone** (`dispatcher.rs:179-181`) — 500 步工作流 = 500 次 HashMap clone
2. **Regex 每次 render_template 调用都重新编译** (`template/engine.rs:7`) — 未缓存
3. **boa_engine 每次执行都创建新 Context** (`sandbox/builtin.rs`) — JS 沙箱的固定开销
4. **子图执行每次都创建新 NodeExecutorRegistry** — 迭代 100 次 = 100 次 registry 初始化
5. **Jinja2 每次都创建新 Environment** (`template/engine.rs:22`)

---

## 文件结构

```
benches/
├── helpers/
│   ├── mod.rs                    # 共享工具：RuntimeContext、Tokio runtime
│   ├── workflow_builders.rs      # 程序化工作流 DSL 生成器
│   └── pool_factories.rs         # 不同规模的 VariablePool 工厂
├── bench_micro_variable_pool.rs  # 变量池 get/set/clone
├── bench_micro_template.rs       # Dify 模板 + Jinja2 渲染
├── bench_micro_condition.rs      # 条件评估器
├── bench_micro_dsl.rs            # DSL 解析 + 图构建
├── bench_micro_segment.rs        # Segment <-> Value 转换
├── bench_meso_dispatcher.rs      # 调度器核心循环（无沙箱）
├── bench_meso_sandbox_js.rs      # JS 沙箱执行
├── bench_meso_sandbox_wasm.rs    # WASM 沙箱执行
├── bench_meso_node_executors.rs  # 各节点执行器
├── bench_macro_topology.rs       # 不同拓扑的端到端工作流
├── bench_macro_scalability.rs    # 参数化扩展性测试
└── bench_macro_iteration.rs      # 迭代节点 顺序 vs 并行
```

## Cargo.toml 修改

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports", "async_tokio"] }

# 每个 bench 文件一个 [[bench]] 入口
[[bench]]
name = "bench_micro_variable_pool"
harness = false
# ... (共 12 个 bench 入口)
```

---

## 一、微基准 (Micro-Benchmarks)

### 1.1 VariablePool (`bench_micro_variable_pool.rs`)

| 基准 | 说明 | 目的 |
|------|------|------|
| `pool_set_string` | 单次 `pool.set()` | 基线写入开销 |
| `pool_get_hit` | `pool.get()` 命中 | 基线读取开销 |
| `pool_get_miss` | `pool.get()` 未命中 | HashMap miss 路径 |
| `pool_set_node_outputs_5` | `set_node_outputs()` 5 个键值对 | 节点输出写入 |
| `pool_clone_{10,50,100,500}` | 参数化 clone | **量化 dispatcher 每步 clone 开销** |
| `pool_clone_large_objects` | clone 含嵌套 Object 的 pool | 深拷贝开销 |
| `pool_get_node_variables` | `get_node_variables()` 全扫描 | 线性扫描开销 |
| `pool_append_array` | `pool.append()` 追加到数组 | 数组增长路径 |
| `pool_has_check` | `pool.has()` 存在性检查 | 热路径频繁调用 |

**关键文件**: `src/core/variable_pool.rs`

### 1.2 模板渲染 (`bench_micro_template.rs`)

| 基准 | 说明 | 目的 |
|------|------|------|
| `dify_no_vars` | 无变量模板 | **量化 Regex::new() 固定开销** |
| `dify_1_var` | 1 个变量替换 | 基线 |
| `dify_10_vars` | 10 个变量替换 | 多变量扩展性 |
| `dify_50_vars` | 50 个变量替换 | 大模板压力 |
| `dify_large_1kb` | 1KB 模板文本 | 正则扫描大文本 |
| `jinja2_simple` | `{{ name }}` | Jinja2 基线 |
| `jinja2_loop_100` | `{% for %}` 100 项 | 循环渲染 |
| `jinja2_conditional` | `{% if %}` 分支 | 条件渲染 |
| `jinja2_complex` | 嵌套循环+条件+过滤器 | 复杂模板 |

**关键文件**: `src/template/engine.rs`

### 1.3 条件评估 (`bench_micro_condition.rs`)

| 基准 | 说明 |
|------|------|
| `eval_string_is` | 单个 `Is` 比较 |
| `eval_string_contains` | `Contains` 在 1KB 字符串上 |
| `eval_numeric_gt` | 数值 `GreaterThan` |
| `eval_in_10_items` / `eval_in_100_items` | `In` 操作随集合大小变化 |
| `eval_case_and_5_conditions` | AND 逻辑 5 个条件 |
| `eval_case_or_5_conditions` | OR 逻辑 5 个条件 |
| `eval_cases_10_first_match` | 10 个 case，首个匹配 |
| `eval_cases_10_last_match` | 10 个 case，末个匹配（最坏情况）|

**关键文件**: `src/evaluator/condition.rs`

### 1.4 DSL 解析与图构建 (`bench_micro_dsl.rs`)

| 基准 | 说明 |
|------|------|
| `parse_yaml_{2,10,50,200}_nodes` | YAML 解析随规模变化 |
| `parse_json_{2,50,200}_nodes` | JSON 解析对比 |
| `validate_schema_50_nodes` | Schema 校验 |
| `build_graph_{2,50,200}_nodes` | 图构建随规模变化 |
| `is_node_ready_check` | `graph.is_node_ready()` 内循环热点 |
| `process_normal_edges` | 正常边处理 |
| `process_branch_edges_5` | 5 分支边处理 |

**关键文件**: `src/dsl/parser.rs`, `src/graph/builder.rs`, `src/graph/types.rs`

### 1.5 Segment 转换 (`bench_micro_segment.rs`)

| 基准 | 说明 |
|------|------|
| `segment_to_value_string` | String Segment -> Value |
| `segment_to_value_nested_object` | 3 层嵌套 Object |
| `segment_to_value_array_1000` | 1000 元素 ArrayAny |
| `segment_from_value_string` | Value -> String Segment |
| `segment_from_value_nested_object` | 嵌套 JSON -> Segment |
| `segment_from_value_array_1000` | 1000 元素数组 -> Segment |

**关键文件**: `src/core/variable_pool.rs` (Segment impl)

---

## 二、中基准 (Meso-Benchmarks)

### 2.1 调度器核心循环 (`bench_meso_dispatcher.rs`)

使用纯控制流节点（start/end/if-else/template-transform/variable-aggregator），隔离调度开销。

| 基准 | 拓扑 | 说明 |
|------|------|------|
| `dispatch_start_end` | 2 节点 | 最小工作流基线 |
| `dispatch_linear_5` | 5 节点线性 | 短链 |
| `dispatch_linear_10` | 10 节点线性 | 中链 |
| `dispatch_linear_50` | 50 节点线性 | 长链，pool clone 开销可见 |
| `dispatch_branch_2_way` | 2 路分支 | 基础分支 |
| `dispatch_branch_10_way` | 10 路分支 | 多分支 |
| `dispatch_diamond_10` | 10 宽菱形 | 扇出+汇聚 |
| `dispatch_deep_branch_10` | 10 层嵌套 IfElse | 深度分支链 |

**实现方式**: 直接使用 `WorkflowDispatcher::new()` + `FakeIdGenerator` + `FakeTimeProvider`，避免调度器轮询开销。使用 `criterion::async_executor` 的 Tokio 支持。

**关键文件**: `src/core/dispatcher.rs`

### 2.2 JS 沙箱 (`bench_meso_sandbox_js.rs`)

| 基准 | 说明 | 目的 |
|------|------|------|
| `js_noop` | `function main(inputs) { return {}; }` | **量化 Context 创建固定开销** |
| `js_arithmetic` | 简单加减 | 纯计算基线 |
| `js_string_manipulation` | 字符串拼接+分割 | 字符串操作 |
| `js_json_parse_stringify` | JSON 序列化 | 数据转换 |
| `js_array_100` / `js_array_1000` | 数组处理 | 数据量扩展 |
| `js_object_transform_20_fields` | 对象转换 | 典型业务逻辑 |
| `js_builtins_sha256` | `crypto.sha256()` | 加密 API 开销 |
| `js_builtins_uuid` | `uuidv4()` | UUID 生成 |
| `js_builtins_base64` | `btoa()`/`atob()` | 编码 API |
| `js_large_input_{10k,100k}` | 大输入 | I/O 序列化开销 |
| `js_validation_only` | `validate()` 无执行 | 安全检查开销 |

**关键文件**: `src/sandbox/builtin.rs`, `src/sandbox/js_builtins.rs`

### 2.3 WASM 沙箱 (`bench_meso_sandbox_wasm.rs`)

| 基准 | 说明 |
|------|------|
| `wasm_compile_and_execute` | 完整编译+执行周期 |
| `wasm_execute_precompiled` | 预编译模块执行 |
| `wasm_validation_only` | WAT 解码+验证 |
| `wasm_fuel_overhead` | 启用 vs 禁用 fuel 的对比 |

**关键文件**: `src/sandbox/wasm_sandbox.rs`

### 2.4 节点执行器 (`bench_meso_node_executors.rs`)

| 基准 | 说明 |
|------|------|
| `start_node_5_vars` | Start 节点初始化 5 个变量 |
| `end_node_5_outputs` | End 节点收集 5 个输出 |
| `answer_node_5_vars` | Answer 节点模板渲染 |
| `ifelse_5_cases_5_conditions` | IfElse 25 个条件 |
| `template_transform_5_vars` | 模板转换 5 个变量映射 |
| `variable_aggregator_5_selectors` | 聚合器 5 个选择器 |
| `code_node_js_simple` | Code 节点 + 简单 JS |

**关键文件**: `src/nodes/control_flow.rs`, `src/nodes/data_transform.rs`

---

## 三、宏基准 (Macro-Benchmarks)

### 3.1 拓扑基准 (`bench_macro_topology.rs`)

通过 `WorkflowRunner` 端到端执行，覆盖不同 DAG 拓扑：

| 基准 | 拓扑 | 节点数 | 说明 |
|------|------|--------|------|
| `topo_minimal` | 线性 | 2 | start -> end |
| `topo_linear_5` | 线性 | 5 | 5 节点 template-transform 链 |
| `topo_linear_10` | 线性 | 10 | 10 节点链 |
| `topo_branch_2_way` | 分支 | 5 | start -> if -> [A, B] -> end |
| `topo_branch_5_way` | 分支 | 8 | 5 路分支 |
| `topo_diamond_5` | 菱形 | 8 | 5 宽菱形 |
| `topo_diamond_10` | 菱形 | 13 | 10 宽菱形 |
| `topo_deep_branch_5` | 深分支 | 12 | 5 层嵌套 IfElse |
| `topo_mixed_realistic` | 混合 | 15 | 模拟真实业务流：start -> code -> template -> if -> [code, code] -> agg -> answer -> end |

### 3.2 扩展性基准 (`bench_macro_scalability.rs`)

参数化测试，使用 `BenchmarkGroup` + `BenchmarkId` 生成可视化扩展曲线：

| 基准组 | 变化参数 | 参数值 | 拓扑 |
|--------|---------|--------|------|
| `scale_node_count` | 节点数 | 5, 10, 25, 50, 100, 200 | 线性 template-transform 链 |
| `scale_branch_count` | IfElse 分支数 | 2, 5, 10, 20, 50 | 单 IfElse 扇出 |
| `scale_pool_size` | 变量池大小 | 10, 50, 100, 500 | 固定 5 节点 |
| `scale_data_size` | 单变量数据量 | 100B, 1KB, 10KB, 100KB | 固定 5 节点 |
| `scale_condition_count` | 条件数 | 1, 5, 10, 20, 50 | 单 IfElse |
| `scale_template_vars` | 模板变量数 | 1, 5, 10, 25, 50 | 单 template-transform |

### 3.3 迭代基准 (`bench_macro_iteration.rs`)

| 基准 | 项数 | 模式 | 并行度 | 说明 |
|------|------|------|--------|------|
| `iter_seq_10` | 10 | 顺序 | - | 顺序迭代基线 |
| `iter_seq_50` | 50 | 顺序 | - | 中规模顺序迭代 |
| `iter_seq_100` | 100 | 顺序 | - | 大规模顺序迭代 |
| `iter_par_10_p5` | 10 | 并行 | 5 | 低并行度 |
| `iter_par_10_p10` | 10 | 并行 | 10 | 全并行 |
| `iter_par_50_p10` | 50 | 并行 | 10 | 中规模并行 |
| `iter_par_100_p10` | 100 | 并行 | 10 | 大规模并行 |
| `iter_par_100_p50` | 100 | 并行 | 50 | 高并行度 |
| `iter_seq_vs_par_50` | 50 | 对比 | 10 | 顺序 vs 并行直接对比 |

---

## 四、共享辅助模块 (`benches/helpers/`)

### `mod.rs`
- `bench_context()` -> 使用 `FakeIdGenerator` + `FakeTimeProvider` 的 `RuntimeContext`
- `bench_runtime()` -> 多线程 Tokio runtime

### `workflow_builders.rs`
- `build_linear_workflow(node_count, node_type)` -> 线性链 YAML
- `build_diamond_workflow(branch_count)` -> 菱形拓扑 YAML
- `build_fanout_workflow(branch_count)` -> IfElse 扇出 YAML
- `build_deep_branch_chain(depth)` -> 嵌套 IfElse YAML
- `build_realistic_mixed_workflow()` -> 模拟真实业务流
- `build_iteration_workflow(items, parallel, parallelism)` -> 迭代工作流

### `pool_factories.rs`
- `make_pool_with_strings(count, string_size)` -> 字符串变量池
- `make_pool_with_objects(count, depth)` -> 嵌套对象变量池
- `make_realistic_pool(node_count)` -> 模拟真实工作流状态

---

## 五、实现步骤

### Phase 1: 基础设施
1. `Cargo.toml` 添加 criterion 依赖和 `[[bench]]` 入口
2. 创建 `benches/helpers/` 辅助模块
3. 实现 `workflow_builders.rs` 和 `pool_factories.rs`

### Phase 2: 微基准
4. `bench_micro_variable_pool.rs` — 最高优先级（dispatcher 瓶颈 #1）
5. `bench_micro_template.rs` — 高优先级（Regex 编译瓶颈）
6. `bench_micro_condition.rs`
7. `bench_micro_segment.rs`
8. `bench_micro_dsl.rs`

### Phase 3: 中基准
9. `bench_meso_dispatcher.rs`
10. `bench_meso_sandbox_js.rs`
11. `bench_meso_sandbox_wasm.rs`
12. `bench_meso_node_executors.rs`

### Phase 4: 宏基准
13. `bench_macro_topology.rs`
14. `bench_macro_scalability.rs`
15. `bench_macro_iteration.rs`

### Phase 5: CI 集成
16. GitHub Actions workflow，master 存基线，PR 对比回归

---

## 六、运行与回归追踪

```bash
# 运行全部基准
cargo bench

# 运行单个基准组
cargo bench --bench bench_micro_variable_pool

# 运行特定基准（名称过滤）
cargo bench --bench bench_macro_topology -- "topo_linear"

# 保存基线
cargo bench -- --save-baseline master

# PR 对比
cargo bench -- --baseline master
```

### CI 回归检测策略
- master push: `--save-baseline master` 保存基线
- PR: `--baseline master` 对比，>10% 回归标记为 warning
- 使用 criterion HTML 报告 (`target/criterion/report/index.html`)

---

## 七、验证方式

1. `cargo bench` 全部通过，无编译错误
2. 检查 `target/criterion/report/index.html` 生成 HTML 报告
3. 参数化基准 (`scale_*`) 生成可视化扩展曲线
4. 通过引入已知瓶颈的优化（如缓存 Regex）验证基准灵敏度
