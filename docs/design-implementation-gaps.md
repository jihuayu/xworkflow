# 设计文档实现差距分析

> 生成日期：2026-02-10
> 对照文档：`builtin-plugin-extraction-design.md`、`runtime-performance-design.md`、`security-design.md`

---

## 一、总览

| 设计文档 | 完成度 | 剩余工作量 |
|----------|--------|-----------|
| `runtime-performance-design.md` | **~99%** | 极少（1 处微小优化） |
| `security-design.md` | **~90%** | 少量（审计集成细节 + 运行时选择器验证） |
| `builtin-plugin-extraction-design.md` | **~35%** | **大量**（workspace 拆分、crate 提取、Plugin trait、cdylib） |

---

## 二、runtime-performance-design.md 剩余项

### P2-01：流式模板 base_vars 初始克隆（微小优化）

**文件**：`src/nodes/data_transform.rs:138`

**现状**：循环外 `base_vars.clone()` 初始化 `vars`，循环内复用 `vars` 并覆盖更新。核心优化（循环内复用）已完成，仅首次存在一次额外 HashMap 克隆。

**影响**：极小，可忽略。

**建议**：维持现状，不需单独处理。

---

## 三、security-design.md 剩余项

### 3.1 审计事件集成补全

以下 `SecurityEventType` 变体已定义但未在所有预期位置触发：

| 事件类型 | 已集成位置 | 缺失位置 |
|----------|-----------|----------|
| `SsrfBlocked` | ✅ `data_transform.rs` HTTP 节点 | — |
| `QuotaExceeded` | ✅ `dispatcher.rs` 配额检查 | — |
| `OutputSizeExceeded` | ✅ `dispatcher.rs` 输出检查 | — |
| `CodeAnalysisBlocked` | ✅ `sandbox/builtin.rs` AST 分析 | — |
| `SandboxViolation` | ⚠️ 事件类型已定义 | JS/WASM 沙箱执行异常时未触发审计 |
| `CredentialAccess` | ⚠️ 事件类型已定义 | CredentialProvider 调用时未记录审计 |
| `TemplateRenderingAnomaly` | ⚠️ 事件类型已定义 | 模板渲染超时/输出截断时未触发审计 |
| `DslValidationFailed` | ⚠️ 事件类型已定义 | DSL 验证失败时未记录审计事件 |

**需要修改的文件**：

1. **`src/sandbox/builtin.rs`** — 在 JS 沙箱执行异常（超时、内存超限）时触发 `SandboxViolation` 审计事件
2. **`src/sandbox/wasm_sandbox.rs`** — 在 WASM 执行 fuel 耗尽或 trap 时触发 `SandboxViolation`
3. **`src/llm/executor.rs`**（或 LLM provider 调用处）— 在调用 `CredentialProvider.get_credentials()` 时触发 `CredentialAccess` 审计事件（无论成功或失败）
4. **`src/template/engine.rs`** — 在模板渲染输出被截断或触发 fuel 限制时触发 `TemplateRenderingAnomaly`
5. **`src/dsl/validation/mod.rs`** 或 **`src/scheduler.rs`** — 在 DSL 验证失败时触发 `DslValidationFailed` 审计事件

### 3.2 运行时变量选择器验证

**设计要求**：变量选择器（`SelectorValidation`）除了在 DSL 解析阶段验证，还应在运行时变量访问时做检查。

**现状**：`SelectorValidation` 仅在 `src/dsl/validation/mod.rs` 的 DSL 验证阶段执行，运行时 `VariablePool::get()` 不做选择器限制检查。

**影响**：如果恶意用户绕过 DSL 验证（例如通过插件动态构造变量引用），理论上可访问不受限的变量路径。

**需要修改的文件**：

- **`src/core/variable_pool.rs`** — 在 `get()` / `get_segment()` 中可选地校验 selector 深度和前缀（需注入 `SelectorValidation` 配置）
- 或者在 **`src/core/dispatcher.rs`** 中，对节点输出写入 Pool 之前验证 key 路径

**建议**：鉴于当前 DSL 阶段已做验证，运行时验证优先级较低，可作为后续加固项。

---

## 四、builtin-plugin-extraction-design.md 剩余项（主要工作）

这是三份文档中差距最大的部分。已完成的是**架构基础**（trait 抽象、feature flags、registry 重构），未完成的是**物理结构拆分**。

### 4.1 已完成项（无需修改）

| 项目 | 位置 |
|------|------|
| `SubGraphRunner` trait 定义 | `src/core/sub_graph_runner.rs` |
| `DefaultSubGraphRunner` 实现 | `src/core/sub_graph_runner.rs` |
| `RuntimeContext.sub_graph_runner` 字段 | `src/core/runtime_context.rs` |
| 子图节点通过 `resolve_sub_graph_runner(context)` 使用 trait | `src/nodes/subgraph_nodes.rs` |
| Feature flags: 6 个 `builtin-*` feature | `Cargo.toml` |
| `NodeExecutorRegistry::with_builtins()` + `empty()` | `src/nodes/executor.rs` |
| `WorkflowRunnerBuilder.sub_graph_runner()` builder 方法 | `src/scheduler.rs` |

### 4.2 未完成项

#### (1) Workspace 结构搭建

**目标**：将单一 crate 重构为 Cargo workspace。

**需要创建**：

```
xworkflow/
├── Cargo.toml                          ← 改为 [workspace] 根
├── crates/
│   ├── xworkflow-types/                ← 共享类型 crate（新建）
│   ├── xworkflow-nodes-core/           ← 控制流节点 crate（新建）
│   ├── xworkflow-nodes-transform/      ← 数据转换节点 crate（新建）
│   ├── xworkflow-nodes-http/           ← HTTP 节点 crate（新建）
│   ├── xworkflow-nodes-code/           ← 代码执行节点 crate（新建）
│   ├── xworkflow-nodes-subgraph/       ← 子图节点 crate（新建）
│   └── xworkflow-nodes-llm/            ← LLM 节点 crate（新建）
└── xworkflow/                          ← 主 crate（现有 src/ 迁移过来）
```

**需要修改**：

- 根 `Cargo.toml` — 添加 `[workspace]` 定义，`members` 列表
- 新增 `crates/xworkflow-types/Cargo.toml`
- 新增每个节点 crate 的 `Cargo.toml`
- 将现有 `src/` 迁移到 `xworkflow/src/`

#### (2) xworkflow-types 共享类型 crate

**目标**：将节点开发所需的共享类型提取到独立 crate。

**需要迁移的类型**：

| 原位置 | 迁移目标 |
|--------|---------|
| `src/nodes/executor.rs` → `NodeExecutor` trait | `crates/xworkflow-types/src/node_executor.rs` |
| `src/core/variable_pool.rs` → `VariablePool`, `Segment`, `SegmentType` | `crates/xworkflow-types/src/variable_pool.rs` |
| `src/core/runtime_context.rs` → `RuntimeContext`, `TimeProvider`, `IdGenerator` | `crates/xworkflow-types/src/runtime_context.rs` |
| `src/dsl/schema.rs` → `NodeRunResult`, `WorkflowNodeExecutionStatus` | `crates/xworkflow-types/src/schema.rs` |
| `src/error/node_error.rs` → `NodeError` | `crates/xworkflow-types/src/error.rs` |
| `src/core/sub_graph_runner.rs` → `SubGraphRunner` trait, 相关类型 | `crates/xworkflow-types/src/sub_graph_runner.rs` |
| `src/plugin_system/traits.rs` → `Plugin`, `PluginMetadata`, `PluginContext` | `crates/xworkflow-types/src/plugin.rs` (可选，feature-gated) |

**注意**：`NodeExecutorRegistry` 不迁移，它属于主 crate 的组装层。

#### (3) 6 个独立节点 crate

每个 crate 需要：
- 独立的 `Cargo.toml`（依赖 `xworkflow-types`）
- 从主 crate 的对应源文件迁移代码
- 实现 `Plugin` trait（见下）
- 导出 `create_plugin() -> Box<dyn Plugin>` 便捷函数

**迁移清单**：

| 节点 Crate | 源文件迁移 | 额外依赖 |
|------------|-----------|----------|
| `xworkflow-nodes-core` | `src/nodes/control_flow.rs` → `start.rs`, `end.rs`, `answer.rs`, `if_else.rs` | 仅 types |
| `xworkflow-nodes-transform` | `src/nodes/data_transform.rs` 中模板/聚合/赋值部分 | `minijinja` |
| `xworkflow-nodes-http` | `src/nodes/data_transform.rs` 中 HTTP 部分 | `reqwest`, `minijinja` |
| `xworkflow-nodes-code` | `src/nodes/data_transform.rs` 中 Code 部分 + `src/sandbox/` | `boa_engine`, `wasmtime`, crypto libs |
| `xworkflow-nodes-subgraph` | `src/nodes/subgraph_nodes.rs` | 仅 types (通过 SubGraphRunner trait) |
| `xworkflow-nodes-llm` | `src/llm/` 整个目录 | `reqwest`, `eventsource-stream`, `tokio-stream` |

#### (4) Plugin trait 统一注册

**目标**：每个节点 crate 实现 `Plugin` trait，通过统一的插件机制注册。

**示例**（以 core-nodes 为例）：

```rust
// crates/xworkflow-nodes-core/src/lib.rs
pub struct CoreNodesPlugin;

#[async_trait]
impl Plugin for CoreNodesPlugin {
    fn metadata(&self) -> &PluginMetadata { &METADATA }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_node_executor("start", Box::new(StartNodeExecutor))?;
        ctx.register_node_executor("end", Box::new(EndNodeExecutor))?;
        ctx.register_node_executor("answer", Box::new(AnswerNodeExecutor))?;
        ctx.register_node_executor("if-else", Box::new(IfElseNodeExecutor))?;
        Ok(())
    }
}

pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(CoreNodesPlugin)
}
```

**需要修改**：
- 主 crate 的 `NodeExecutorRegistry::with_builtins()` 改为收集各节点 crate 的 `create_plugin()` 并通过 `PluginContext` 注册
- 或新增 `collect_builtin_plugins()` 函数统一收集

#### (5) cdylib 动态链接支持

**目标**：每个节点 crate 可编译为动态链接库 (.dll/.so)。

**需要创建**：每个节点 crate 对应一个 `-dylib` 包装 crate，仅导出 DLL 符号。

```
crates/
├── xworkflow-nodes-core/          ← rlib（包含逻辑）
└── xworkflow-nodes-core-dylib/    ← cdylib（仅导出符号）
    ├── Cargo.toml: crate-type = ["cdylib"]
    └── src/lib.rs                 ← 3 个 #[no_mangle] 导出函数
```

**需要导出的符号**：
- `XWORKFLOW_RUST_PLUGIN: bool = true` — Rust ABI 标记
- `xworkflow_plugin_abi_version() -> u32` — ABI 版本
- `xworkflow_rust_plugin_create() -> Box<dyn Plugin>` — 创建插件实例

#### (6) SubGraphExecutor 迁移

**文件**：`src/nodes/subgraph.rs`

**现状**：`SubGraphExecutor::execute()` 仍直接创建 `NodeExecutorRegistry::new()` 和 `WorkflowDispatcher::new()`，未使用 `SubGraphRunner` trait。

**需要修改**：
- `SubGraphExecutor::execute()` 应通过 `context.sub_graph_runner` 获取 `SubGraphRunner` trait 对象
- 与 `src/nodes/subgraph_nodes.rs` 中的 `resolve_sub_graph_runner()` 模式保持一致
- 消除 `subgraph.rs` 对 `dispatcher` 和 `executor` 模块的直接依赖

### 4.3 实施路线图（按设计文档 Phase 划分）

#### Phase 1：基础设施（前置条件）
1. 创建 workspace 根 `Cargo.toml`
2. 创建 `crates/xworkflow-types/`，迁移共享类型
3. 修改主 crate 依赖 `xworkflow-types`，用 re-export 保持公开 API 不变
4. 修改 `SubGraphExecutor` 使用 `context.sub_graph_runner`
5. 验证：`cargo test --workspace` 全部通过

#### Phase 2：节点 crate 拆分（按依赖复杂度递增）
1. `xworkflow-nodes-core`（最简单，无外部依赖）
2. `xworkflow-nodes-subgraph`（仅依赖 types）
3. `xworkflow-nodes-transform`（+ minijinja）
4. `xworkflow-nodes-http`（+ reqwest）
5. `xworkflow-nodes-llm`（+ reqwest + eventsource）
6. `xworkflow-nodes-code`（最复杂，+ boa_engine + wasmtime + crypto）

每步：迁移代码 → 实现 Plugin trait → 添加 feature flag → `cargo test --workspace`

#### Phase 3：cdylib 支持
1. 为每个节点 crate 创建 `-dylib` 包装 crate
2. 实现 DLL 导出符号
3. 集成测试：编译 DLL → `DllPluginLoader` 加载 → 执行工作流

---

## 五、优先级建议

| 优先级 | 项目 | 原因 |
|--------|------|------|
| **可选** | runtime-performance P2-01 微调 | 影响极小 |
| **低** | security 审计事件补全 | 功能正确，仅缺少部分审计记录 |
| **低** | security 运行时选择器验证 | DSL 阶段已覆盖，运行时为纵深防护 |
| **中** | SubGraphExecutor 迁移到使用 trait | 消除循环依赖的最后一环 |
| **高** | builtin-plugin-extraction Phase 1-2 | 设计目标的核心内容，影响可维护性和编译效率 |
| **低** | builtin-plugin-extraction Phase 3 (cdylib) | 仅在需要动态加载时有价值 |
