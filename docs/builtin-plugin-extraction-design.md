# 内置节点插件化与模块拆分设计

## 1. 背景与动机

### 1.1 问题陈述

当前 xworkflow 的所有内置节点执行器硬编码在 `NodeExecutorRegistry::new()` 中（`src/nodes/executor.rs:33-61`），导致：

1. **依赖不可分割**：即使用户只需要基础控制流节点，也必须编译 boa_engine（JS 沙箱）、wasmtime（WASM 运行时）、reqwest（HTTP 客户端）等全部重量级依赖
2. **无法独立部署**：节点实现与引擎核心紧耦合，无法将特定节点编译为独立的动态链接库
3. **扩展模式不统一**：内置节点通过硬编码注册，外部插件通过 `Plugin` trait 注册，两套机制并存
4. **循环依赖**：子图节点（iteration/loop/list-operator）直接依赖 `WorkflowDispatcher`，形成 nodes → dispatcher → nodes 的循环

### 1.2 目标

- 将内置节点拆分为独立的 Cargo crate
- 通过 feature flag 控制编译方式：**静态链接**（默认）或 **动态链接**（cdylib）
- 内置节点与外部插件使用统一的 `Plugin` trait 注册机制
- 解决子图节点的循环依赖问题
- 用户可按需裁剪依赖，最小化二进制体积

### 1.3 设计约束

- 不破坏现有公开 API（`WorkflowRunnerBuilder` 等）
- 静态链接模式下零额外开销（与当前行为一致）
- 动态链接模式复用已有的 `DllPluginLoader`（`src/plugin_system/loaders/dll_loader.rs`）
- 项目尚未发布第一个版本，无需向后兼容

## 2. 当前架构分析

### 2.1 内置节点清单

当前 `NodeExecutorRegistry::new()` 注册了以下执行器：

| 节点类型 | 执行器 | 源文件 | 关键依赖 |
|---------|--------|--------|---------|
| `start` | `StartNodeExecutor` | `control_flow.rs` | core types |
| `end` | `EndNodeExecutor` | `control_flow.rs` | core types |
| `answer` | `AnswerNodeExecutor` | `control_flow.rs` | core types |
| `if-else` | `IfElseNodeExecutor` | `control_flow.rs` | evaluator |
| `template-transform` | `TemplateTransformExecutor` | `data_transform.rs` | minijinja |
| `variable-aggregator` | `VariableAggregatorExecutor` | `data_transform.rs` | core types |
| `variable-assigner` | `LegacyVariableAggregatorExecutor` | `data_transform.rs` | core types |
| `assigner` | `VariableAssignerExecutor` | `data_transform.rs` | core types |
| `http-request` | `HttpRequestExecutor` | `data_transform.rs` | reqwest |
| `code` | `CodeNodeExecutor` | `data_transform.rs` | boa_engine, wasmtime |
| `llm` | `LlmNodeExecutor` | `llm/executor.rs` | reqwest, LLM providers |
| `iteration` | `IterationNodeExecutor` | `subgraph_nodes.rs` | **WorkflowDispatcher** |
| `loop` | `LoopNodeExecutor` | `subgraph_nodes.rs` | **WorkflowDispatcher** |
| `list-operator` | `ListOperatorNodeExecutor` | `subgraph_nodes.rs` | **WorkflowDispatcher** |
| `knowledge-retrieval` | `StubExecutor` | `executor.rs` | — |
| `question-classifier` | `StubExecutor` | `executor.rs` | — |
| `parameter-extractor` | `StubExecutor` | `executor.rs` | — |
| `tool` | `StubExecutor` | `executor.rs` | — |
| `document-extractor` | `StubExecutor` | `executor.rs` | — |
| `agent` | `StubExecutor` | `executor.rs` | — |
| `human-input` | `StubExecutor` | `executor.rs` | — |

### 2.2 依赖权重分析

各重量级依赖对编译时间和二进制体积的影响：

| 依赖 | 编译影响 | 使用者 |
|------|---------|--------|
| `wasmtime` + `wasmtime-wasi` | 极重（~60s 增量编译） | Code 节点、WASM 沙箱、WASM 插件 |
| `boa_engine` | 重（~30s） | Code 节点 JS 沙箱 |
| `reqwest` | 中（~15s） | HTTP 节点、LLM Provider |
| `minijinja` | 轻（~5s） | Template 节点、模板渲染 |
| `sha1/sha2/md-5/hmac/aes-gcm` | 轻 | JS 沙箱内置函数 |

### 2.3 循环依赖问题

```
src/nodes/subgraph.rs
  └─ use crate::core::dispatcher::{EngineConfig, WorkflowDispatcher}  ← 直接依赖
  └─ use crate::nodes::executor::NodeExecutorRegistry                 ← 直接依赖

SubGraphExecutor::execute()
  └─ NodeExecutorRegistry::new()     ← 创建完整的注册表
  └─ WorkflowDispatcher::new(...)    ← 创建调度器
  └─ dispatcher.run()                ← 执行子图
```

如果将节点拆分为独立 crate，子图节点 crate 将依赖调度器 crate，而调度器 crate 又依赖节点注册表，形成循环。

### 2.4 现有插件系统能力

`plugin_system` 模块（feature-gated）已提供：

- **`Plugin` trait**（`traits.rs:57-67`）：统一插件接口，`metadata()` + `register()` + `shutdown()`
- **`PluginContext`**（`context.rs:16-186`）：注册 NodeExecutor、LlmProvider、Hook、Sandbox 等
- **`DllPluginLoader`**（`loaders/dll_loader.rs`）：支持 C ABI 和 Rust ABI 两种加载模式
- **`HostPluginLoader`**：用于静态注入的 Host 插件
- **`PluginRegistry`**（`registry.rs`）：两阶段生命周期（Bootstrap + Normal）

内置节点只需实现 `Plugin` trait，即可无缝接入现有插件系统。

## 3. 节点分组策略

### 3.1 分组原则

按以下维度决定分组：

1. **依赖亲和性**：共享相同重量级依赖的节点归为一组
2. **功能内聚性**：功能相关的节点归为一组
3. **独立部署价值**：用户是否有单独裁剪该组的需求

### 3.2 分组方案

```
┌─────────────────────────────────────────────────────────────────┐
│                    xworkflow-nodes-core                         │
│  start · end · answer · if-else                                │
│  依赖：xworkflow-types, evaluator                              │
│  体积：极小（~50KB）                                            │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                  xworkflow-nodes-transform                      │
│  template-transform · variable-aggregator                       │
│  variable-assigner(legacy) · assigner                           │
│  依赖：xworkflow-types, minijinja                              │
│  体积：小（~200KB）                                             │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                    xworkflow-nodes-http                          │
│  http-request                                                   │
│  依赖：xworkflow-types, reqwest, minijinja                     │
│  体积：中（~1MB，reqwest + TLS）                                │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                    xworkflow-nodes-code                          │
│  code                                                           │
│  依赖：xworkflow-types, boa_engine, wasmtime, crypto libs      │
│  体积：大（~15MB，含 WASM 运行时）                              │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                  xworkflow-nodes-subgraph                        │
│  iteration · loop · list-operator                               │
│  依赖：xworkflow-types（通过 SubGraphRunner trait）             │
│  体积：极小（~30KB）                                            │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                    xworkflow-nodes-llm                           │
│  llm                                                            │
│  依赖：xworkflow-types, reqwest, eventsource-stream            │
│  体积：中（~1MB，与 http 共享 reqwest）                         │
└─────────────────────────────────────────────────────────────────┘
```

### 3.3 Stub 节点处理

当前的 7 个 `StubExecutor`（knowledge-retrieval、question-classifier 等）保留在主 crate 中，不单独拆分。它们是占位符，未来由外部插件实现。

## 4. Workspace 架构设计

### 4.1 目录结构

```
xworkflow/
├── Cargo.toml                          ← workspace 定义
├── crates/
│   ├── xworkflow-types/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── node_executor.rs        ← NodeExecutor trait
│   │       ├── variable_pool.rs        ← VariablePool + Segment
│   │       ├── runtime_context.rs      ← RuntimeContext
│   │       ├── sub_graph_runner.rs     ← SubGraphRunner trait（新增）
│   │       ├── plugin.rs              ← Plugin trait + PluginContext
│   │       ├── schema.rs             ← NodeRunResult 等 DSL 类型
│   │       └── error.rs              ← NodeError, WorkflowError
│   │
│   ├── xworkflow-nodes-core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                 ← CoreNodesPlugin + cdylib 导出
│   │       ├── start.rs
│   │       ├── end.rs
│   │       ├── answer.rs
│   │       └── if_else.rs
│   │
│   ├── xworkflow-nodes-transform/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                 ← TransformNodesPlugin
│   │       ├── template_transform.rs
│   │       ├── variable_aggregator.rs
│   │       └── variable_assigner.rs
│   │
│   ├── xworkflow-nodes-http/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                 ← HttpNodePlugin
│   │       └── http_request.rs
│   │
│   ├── xworkflow-nodes-code/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                 ← CodeNodePlugin
│   │       ├── code_executor.rs
│   │       ├── js_sandbox.rs          ← boa_engine 沙箱
│   │       ├── js_builtins.rs         ← JS 内置函数
│   │       └── wasm_sandbox.rs        ← wasmtime 沙箱
│   │
│   ├── xworkflow-nodes-subgraph/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                 ← SubGraphNodesPlugin
│   │       ├── iteration.rs
│   │       ├── loop_node.rs
│   │       └── list_operator.rs
│   │
│   └── xworkflow-nodes-llm/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs                 ← LlmNodePlugin
│           ├── executor.rs
│           ├── types.rs
│           └── provider/
│               ├── mod.rs
│               ├── openai.rs
│               └── wasm_provider.rs
│
└── xworkflow/                          ← 主 crate（原 src/）
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        ├── core/                       ← dispatcher, graph, events
        ├── dsl/
        ├── evaluator/
        ├── scheduler.rs
        ├── plugin/                     ← 旧 WASM 插件系统
        ├── plugin_system/              ← 新插件系统
        └── template/
```

### 4.2 Workspace Cargo.toml

```toml
# 根 Cargo.toml
[workspace]
members = [
    "crates/xworkflow-types",
    "crates/xworkflow-nodes-core",
    "crates/xworkflow-nodes-transform",
    "crates/xworkflow-nodes-http",
    "crates/xworkflow-nodes-code",
    "crates/xworkflow-nodes-subgraph",
    "crates/xworkflow-nodes-llm",
    "xworkflow",
]

[workspace.dependencies]
# 共享版本管理
tokio = { version = "1.35", features = ["full"] }
async-trait = "0.1"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
thiserror = "1.0"
tracing = "0.1"
xworkflow-types = { path = "crates/xworkflow-types" }
```

### 4.3 依赖关系图

```
                    ┌──────────────────┐
                    │ xworkflow-types  │  ← 零外部重量级依赖
                    └────────┬─────────┘
                             │
          ┌──────────────────┼──────────────────────────┐
          │                  │                           │
    ┌─────┴──────┐   ┌──────┴───────┐   ┌──────────────┴──────┐
    │ nodes-core │   │nodes-transform│   │  nodes-subgraph     │
    │            │   │  +minijinja   │   │ (SubGraphRunner)    │
    └────────────┘   └──────────────┘   └─────────────────────┘
                             │
                    ┌────────┴─────────┐
              ┌─────┴──────┐   ┌───────┴──────┐
              │ nodes-http │   │  nodes-llm   │
              │  +reqwest  │   │  +reqwest    │
              └────────────┘   └──────────────┘
                    │
              ┌─────┴──────┐
              │ nodes-code │
              │ +boa_engine│
              │ +wasmtime  │
              └────────────┘

                    ┌──────────────────┐
                    │    xworkflow     │  ← 主 crate
                    │  (dispatcher,    │
                    │   scheduler,     │
                    │   plugin_system) │
                    │                  │
                    │  feature flags   │
                    │  控制静态依赖     │
                    └──────────────────┘
```

## 5. xworkflow-types 共享类型 crate

### 5.1 设计目标

`xworkflow-types` 是所有节点 crate 的唯一依赖入口，包含节点开发所需的全部 trait 和类型定义。它**不包含**任何具体实现（如调度器、沙箱引擎），确保依赖链极轻。

### 5.2 Cargo.toml

```toml
[package]
name = "xworkflow-types"
version = "0.1.0"
edition = "2021"

[dependencies]
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true }

# 插件系统类型（可选）
libloading = { version = "0.8", optional = true }

[features]
default = []
plugin-system = ["libloading"]
```

### 5.3 导出类型清单

```rust
// crates/xworkflow-types/src/lib.rs

// ── 节点执行 ──
pub mod node_executor;    // NodeExecutor trait
pub mod schema;           // NodeRunResult, WorkflowNodeExecutionStatus
pub mod error;            // NodeError, WorkflowError

// ── 变量系统 ──
pub mod variable_pool;    // VariablePool, Segment, SegmentType, Value
pub mod segment_stream;   // SegmentStream（异步流式变量）

// ── 运行时上下文 ──
pub mod runtime_context;  // RuntimeContext, TimeProvider, IdGenerator

// ── 子图抽象 ──
pub mod sub_graph_runner; // SubGraphRunner trait（新增）

// ── 插件系统 ──
#[cfg(feature = "plugin-system")]
pub mod plugin;           // Plugin, PluginMetadata, PluginContext, PluginError
```

### 5.4 从主 crate 迁移的类型

| 原位置 | 迁移到 xworkflow-types | 说明 |
|--------|----------------------|------|
| `src/nodes/executor.rs:NodeExecutor` | `node_executor.rs` | trait 定义 |
| `src/dsl/schema.rs:NodeRunResult` | `schema.rs` | 节点输出结构 |
| `src/error.rs:NodeError` | `error.rs` | 节点错误类型 |
| `src/core/variable_pool.rs` | `variable_pool.rs` | 完整的 VariablePool 实现 |
| `src/core/runtime_context.rs` | `runtime_context.rs` | RuntimeContext + traits |
| `src/plugin_system/traits.rs` | `plugin.rs` | Plugin trait 等 |
| `src/plugin_system/context.rs` | `plugin.rs` | PluginContext |
| `src/evaluator/condition.rs` | `evaluator.rs` | 条件求值（core-nodes 需要） |

> **注意**：`NodeExecutorRegistry` **不迁移**到 types crate，它属于主 crate 的组装层。

## 6. 循环依赖解决方案：SubGraphRunner trait

### 6.1 问题回顾

当前 `SubGraphExecutor`（`src/nodes/subgraph.rs:75-107`）直接创建 `WorkflowDispatcher` 和 `NodeExecutorRegistry`：

```rust
// 当前代码 — 直接依赖调度器
pub async fn execute(&self, sub_graph: &SubGraphDefinition, ...) -> Result<Value, SubGraphError> {
    let registry = NodeExecutorRegistry::new();          // ← 依赖 nodes 模块
    let mut dispatcher = WorkflowDispatcher::new(        // ← 依赖 core/dispatcher
        graph, scoped_pool, registry, tx, config, context, None,
    );
    dispatcher.run().await
}
```

### 6.2 解决方案：依赖倒置

在 `xworkflow-types` 中定义抽象 trait，子图节点依赖 trait 而非具体实现：

```rust
// crates/xworkflow-types/src/sub_graph_runner.rs

use std::collections::HashMap;
use serde_json::Value;
use crate::variable_pool::VariablePool;
use crate::runtime_context::RuntimeContext;

/// 子图定义（从 DSL 解析）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraphDefinition {
    pub nodes: Vec<SubGraphNode>,
    pub edges: Vec<SubGraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraphNode {
    pub id: String,
    #[serde(rename = "type", default)]
    pub node_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGraphEdge {
    #[serde(default)]
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, alias = "sourceHandle", alias = "source_handle")]
    pub source_handle: Option<String>,
}

/// 子图执行错误
#[derive(Debug, thiserror::Error)]
pub enum SubGraphError {
    #[error("Sub-graph execution failed: {0}")]
    ExecutionFailed(String),
    #[error("Max iterations exceeded: {0}")]
    MaxIterationsExceeded(usize),
    #[error("Invalid sub-graph definition: {0}")]
    InvalidDefinition(String),
}

/// 子图执行器抽象 — 打破循环依赖的关键
#[async_trait::async_trait]
pub trait SubGraphRunner: Send + Sync {
    /// 执行子图，返回子图 end 节点的输出
    async fn run(
        &self,
        sub_graph: &SubGraphDefinition,
        parent_pool: &VariablePool,
        scope_vars: HashMap<String, Value>,
        context: &RuntimeContext,
    ) -> Result<Value, SubGraphError>;
}
```

### 6.3 RuntimeContext 注入

`SubGraphRunner` 通过 `RuntimeContext` 注入到节点执行环境：

```rust
// crates/xworkflow-types/src/runtime_context.rs

pub struct RuntimeContext {
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,

    /// 子图执行器（由主 crate 注入）
    pub sub_graph_runner: Option<Arc<dyn SubGraphRunner>>,
}
```

### 6.4 主 crate 提供具体实现

```rust
// xworkflow/src/core/sub_graph_runner_impl.rs

use xworkflow_types::{SubGraphRunner, SubGraphDefinition, SubGraphError, ...};
use crate::core::dispatcher::{WorkflowDispatcher, EngineConfig};
use crate::nodes::executor::NodeExecutorRegistry;

pub struct DefaultSubGraphRunner {
    registry_factory: Arc<dyn Fn() -> NodeExecutorRegistry + Send + Sync>,
}

#[async_trait]
impl SubGraphRunner for DefaultSubGraphRunner {
    async fn run(
        &self,
        sub_graph: &SubGraphDefinition,
        parent_pool: &VariablePool,
        scope_vars: HashMap<String, Value>,
        context: &RuntimeContext,
    ) -> Result<Value, SubGraphError> {
        let mut scoped_pool = parent_pool.clone();
        inject_scope_vars(&mut scoped_pool, scope_vars)?;

        let graph = build_sub_graph(sub_graph)?;
        let registry = (self.registry_factory)();
        let (tx, _rx) = mpsc::channel(16);
        let sub_context = context.clone().with_event_tx(tx.clone());

        let mut dispatcher = WorkflowDispatcher::new(
            graph, scoped_pool, registry, tx,
            EngineConfig::default(), Arc::new(sub_context), None,
        );

        let outputs = dispatcher.run().await
            .map_err(|e| SubGraphError::ExecutionFailed(e.to_string()))?;
        Ok(Value::Object(outputs.into_iter().collect()))
    }
}
```

### 6.5 子图节点使用方式

```rust
// crates/xworkflow-nodes-subgraph/src/iteration.rs

#[async_trait]
impl NodeExecutor for IterationNodeExecutor {
    async fn execute(
        &self, node_id: &str, config: &Value,
        variable_pool: &VariablePool, context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let runner = context.sub_graph_runner.as_ref()
            .ok_or_else(|| NodeError::ConfigError(
                "SubGraphRunner not available in context".into()
            ))?;

        // 使用 trait 对象，无需依赖 WorkflowDispatcher
        let result = runner.run(&config.sub_graph, variable_pool, scope, context).await?;
        // ...
    }
}
```

### 6.6 依赖关系对比

**改造前**（循环依赖）：
```
nodes ──→ dispatcher ──→ NodeExecutorRegistry ──→ nodes
  ↑                                                 │
  └─────────────────────────────────────────────────┘
```

**改造后**（依赖倒置）：
```
xworkflow-types（定义 SubGraphRunner trait）
       ↑                    ↑
       │                    │
nodes-subgraph         xworkflow（主 crate）
（依赖 trait）         （提供 DefaultSubGraphRunner 实现）
```

## 7. 节点 crate 实现模式

### 7.1 Plugin trait 实现

每个节点 crate 导出一个实现 `Plugin` trait 的结构体：

```rust
// crates/xworkflow-nodes-core/src/lib.rs

mod start;
mod end;
mod answer;
mod if_else;

use async_trait::async_trait;
use xworkflow_types::plugin::*;

pub struct CoreNodesPlugin;

static METADATA: once_cell::sync::Lazy<PluginMetadata> = once_cell::sync::Lazy::new(|| {
    PluginMetadata {
        id: "xworkflow.builtin.core-nodes".to_string(),
        name: "Core Nodes".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        category: PluginCategory::Normal,
        description: "Built-in control flow nodes: start, end, answer, if-else".to_string(),
        source: PluginSource::Host,
        capabilities: Some(PluginCapabilities {
            register_nodes: true,
            ..Default::default()
        }),
    }
});

#[async_trait]
impl Plugin for CoreNodesPlugin {
    fn metadata(&self) -> &PluginMetadata { &METADATA }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_node_executor("start", Box::new(start::StartNodeExecutor))?;
        ctx.register_node_executor("end", Box::new(end::EndNodeExecutor))?;
        ctx.register_node_executor("answer", Box::new(answer::AnswerNodeExecutor))?;
        ctx.register_node_executor("if-else", Box::new(if_else::IfElseNodeExecutor))?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any { self }
}

/// 便捷函数：创建插件实例
pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(CoreNodesPlugin)
}
```

### 7.2 cdylib 导出

每个节点 crate 通过 `cdylib` feature 控制是否导出 DLL 符号：

```rust
// crates/xworkflow-nodes-core/src/lib.rs（续）

/// Rust ABI 标记：DllPluginLoader 通过此符号判断加载模式
#[cfg(feature = "cdylib")]
#[no_mangle]
pub static XWORKFLOW_RUST_PLUGIN: bool = true;

/// ABI 版本号
#[cfg(feature = "cdylib")]
#[no_mangle]
pub extern "C" fn xworkflow_plugin_abi_version() -> u32 { 1 }

/// 创建插件实例（Rust ABI）
#[cfg(feature = "cdylib")]
#[no_mangle]
pub fn xworkflow_rust_plugin_create() -> Box<dyn Plugin> {
    create_plugin()
}
```

### 7.3 cdylib wrapper crate 模式

Cargo 不支持通过 feature 切换 `crate-type`。推荐为每个节点 crate 创建独立的 cdylib 包装 crate：

```
crates/
├── xworkflow-nodes-core/          ← rlib，包含实际逻辑
│   └── Cargo.toml: crate-type = ["rlib"]
└── xworkflow-nodes-core-dylib/    ← cdylib，仅导出符号
    ├── Cargo.toml: crate-type = ["cdylib"]
    └── src/lib.rs
```

```rust
// crates/xworkflow-nodes-core-dylib/src/lib.rs
use xworkflow_nodes_core::create_plugin;
use xworkflow_types::plugin::Plugin;

#[no_mangle]
pub static XWORKFLOW_RUST_PLUGIN: bool = true;

#[no_mangle]
pub extern "C" fn xworkflow_plugin_abi_version() -> u32 { 1 }

#[no_mangle]
pub fn xworkflow_rust_plugin_create() -> Box<dyn Plugin> {
    create_plugin()
}
```

逻辑与导出分离，rlib crate 保持纯净，cdylib crate 仅做胶水层。

### 7.4 各节点 crate 特殊依赖

| Crate | 额外依赖 | 说明 |
|-------|---------|------|
| `xworkflow-nodes-core` | — | 仅依赖 types |
| `xworkflow-nodes-transform` | `minijinja` | 模板渲染 |
| `xworkflow-nodes-http` | `reqwest`, `minijinja` | HTTP 请求 + URL 模板 |
| `xworkflow-nodes-code` | `boa_engine`, `wasmtime`, `sha1/sha2/md-5/hmac/aes-gcm` | JS + WASM 沙箱 |
| `xworkflow-nodes-subgraph` | — | 仅依赖 types（SubGraphRunner trait） |
| `xworkflow-nodes-llm` | `reqwest`, `eventsource-stream`, `tokio-stream` | LLM API 调用 + SSE 流 |

## 8. 主 crate Feature Flag 设计

### 8.1 Feature 定义

```toml
# xworkflow/Cargo.toml

[features]
default = [
    "builtin-core-nodes",
    "builtin-transform-nodes",
    "builtin-http-node",
    "builtin-code-node",
    "builtin-subgraph-nodes",
    "builtin-llm-node",
]

# ── 内置节点静态链接 ──
builtin-core-nodes      = ["dep:xworkflow-nodes-core"]
builtin-transform-nodes = ["dep:xworkflow-nodes-transform"]
builtin-http-node       = ["dep:xworkflow-nodes-http"]
builtin-code-node       = ["dep:xworkflow-nodes-code"]
builtin-subgraph-nodes  = ["dep:xworkflow-nodes-subgraph"]
builtin-llm-node        = ["dep:xworkflow-nodes-llm"]

# ── 插件系统 ──
plugin-system = ["dep:libloading", "xworkflow-types/plugin-system"]

[dependencies]
xworkflow-types = { workspace = true }

# 可选的内置节点 crate
xworkflow-nodes-core      = { workspace = true, optional = true }
xworkflow-nodes-transform = { workspace = true, optional = true }
xworkflow-nodes-http      = { workspace = true, optional = true }
xworkflow-nodes-code      = { workspace = true, optional = true }
xworkflow-nodes-subgraph  = { workspace = true, optional = true }
xworkflow-nodes-llm       = { workspace = true, optional = true }
```

### 8.2 Feature 组合场景

| 场景 | Feature 配置 | 效果 |
|------|-------------|------|
| **全功能**（默认） | `default` | 所有节点静态链接，与当前行为一致 |
| **最小核心** | `builtin-core-nodes` | 仅控制流节点，无 JS/WASM/HTTP |
| **无代码沙箱** | `default` 去掉 `builtin-code-node` | 不编译 boa_engine/wasmtime |
| **纯动态加载** | 无 `builtin-*` + `plugin-system` | 所有节点通过 DLL 加载 |
| **混合模式** | 部分 `builtin-*` + `plugin-system` | 核心静态 + 重量级动态 |

### 8.3 典型用法

```toml
# 用户的 Cargo.toml — 最小化编译
[dependencies]
xworkflow = { version = "0.1", default-features = false, features = [
    "builtin-core-nodes",
    "builtin-transform-nodes",
    "plugin-system",  # 其余节点运行时加载
] }
```

## 9. 静态链接 vs 动态链接的编译流程

### 9.1 静态链接流程（默认）

```
cargo build
  ├─ 编译 xworkflow-types（共享类型）
  ├─ 编译 xworkflow-nodes-core（rlib）
  ├─ 编译 xworkflow-nodes-transform（rlib）
  ├─ 编译 xworkflow-nodes-http（rlib）
  ├─ 编译 xworkflow-nodes-code（rlib）
  ├─ 编译 xworkflow-nodes-subgraph（rlib）
  ├─ 编译 xworkflow-nodes-llm（rlib）
  └─ 链接 xworkflow（主 crate）→ 单一二进制
```

运行时：`WorkflowRunnerBuilder::build()` 调用 `collect_builtin_plugins()` 注册所有内置插件，执行路径与当前完全一致。

### 9.2 动态链接流程

```bash
# 步骤 1：编译节点为动态库
cargo build -p xworkflow-nodes-code-dylib --release
# → target/release/xworkflow_nodes_code.dll（Windows）
# → target/release/libxworkflow_nodes_code.so（Linux）

# 步骤 2：编译主程序（不含 code 节点）
cargo build -p xworkflow --no-default-features \
    --features "builtin-core-nodes,plugin-system"
```

运行时：
1. 注册静态内置插件（仅 core-nodes）
2. `DllPluginLoader` 加载 `.dll/.so`
3. 检测 `XWORKFLOW_RUST_PLUGIN` 标记 → Rust ABI 加载
4. `xworkflow_rust_plugin_create()` → `Box<dyn Plugin>`
5. `Plugin::register()` → NodeExecutor 注册到 registry
6. 后续执行与静态链接完全一致

### 9.3 ABI 兼容性

| 模式 | 适用场景 | 要求 |
|------|---------|------|
| **Rust ABI** | 同 workspace 编译的内置节点 DLL | 相同 Rust 编译器版本 |
| **C ABI** | 第三方独立开发的插件 | 跨编译器兼容，需 FFI 层 |

内置节点始终使用 Rust ABI，因为它们与主 crate 在同一 workspace 中编译。

## 10. NodeExecutorRegistry 改造

### 10.1 改造后的实现

```rust
impl NodeExecutorRegistry {
    /// 创建空注册表
    pub fn empty() -> Self {
        Self { executors: HashMap::new() }
    }

    /// 创建包含所有静态链接内置节点的注册表
    pub fn with_builtins() -> Self {
        let mut registry = Self::empty();

        #[cfg(feature = "builtin-core-nodes")]
        {
            use xworkflow_nodes_core::*;
            registry.register("start", Box::new(StartNodeExecutor));
            registry.register("end", Box::new(EndNodeExecutor));
            registry.register("answer", Box::new(AnswerNodeExecutor));
            registry.register("if-else", Box::new(IfElseNodeExecutor));
        }

        #[cfg(feature = "builtin-transform-nodes")]
        {
            use xworkflow_nodes_transform::*;
            registry.register("template-transform", Box::new(TemplateTransformExecutor));
            registry.register("variable-aggregator", Box::new(VariableAggregatorExecutor));
            registry.register("variable-assigner", Box::new(LegacyVariableAggregatorExecutor));
            registry.register("assigner", Box::new(VariableAssignerExecutor));
        }

        #[cfg(feature = "builtin-http-node")]
        registry.register("http-request", Box::new(xworkflow_nodes_http::HttpRequestExecutor));

        #[cfg(feature = "builtin-code-node")]
        registry.register("code", Box::new(xworkflow_nodes_code::CodeNodeExecutor::new()));

        #[cfg(feature = "builtin-subgraph-nodes")]
        {
            use xworkflow_nodes_subgraph::*;
            registry.register("iteration", Box::new(IterationNodeExecutor::new()));
            registry.register("loop", Box::new(LoopNodeExecutor::new()));
            registry.register("list-operator", Box::new(ListOperatorNodeExecutor::new()));
        }

        // Stub 执行器始终注册
        for stub_type in &[
            "knowledge-retrieval", "question-classifier", "parameter-extractor",
            "tool", "document-extractor", "agent", "human-input",
        ] {
            registry.register(stub_type, Box::new(StubExecutor(stub_type)));
        }

        registry
    }
}
```

### 10.2 LLM 节点的特殊处理

LLM 节点需要 `LlmProviderRegistry` 注入，保持现有模式：

```rust
#[cfg(feature = "builtin-llm-node")]
{
    let llm_registry = self.build_llm_registry();
    node_registry.set_llm_provider_registry(Arc::new(llm_registry));
}
```

### 10.3 动态插件执行器合并

```rust
#[cfg(feature = "plugin-system")]
if let Some(plugin_registry) = &self.plugin_registry {
    let plugin_executors = plugin_registry.take_node_executors();
    node_registry.apply_plugin_executors(plugin_executors);
}
```

## 11. 文件变更清单

### 11.1 新增

| 文件 | 说明 |
|------|------|
| `Cargo.toml`（workspace root） | workspace 定义 |
| `crates/xworkflow-types/` | 共享类型 crate |
| `crates/xworkflow-nodes-core/` | 核心节点 crate |
| `crates/xworkflow-nodes-transform/` | 数据转换节点 crate |
| `crates/xworkflow-nodes-http/` | HTTP 节点 crate |
| `crates/xworkflow-nodes-code/` | 代码执行节点 crate |
| `crates/xworkflow-nodes-subgraph/` | 子图节点 crate |
| `crates/xworkflow-nodes-llm/` | LLM 节点 crate |
| `crates/xworkflow-nodes-*-dylib/` | 各节点的 cdylib 包装 crate |
| `xworkflow/src/core/sub_graph_runner_impl.rs` | SubGraphRunner 具体实现 |

### 11.2 迁移到 xworkflow-types

| 原路径 | 新路径 |
|--------|--------|
| `src/nodes/executor.rs`（NodeExecutor trait） | `crates/xworkflow-types/src/node_executor.rs` |
| `src/core/variable_pool.rs` | `crates/xworkflow-types/src/variable_pool.rs` |
| `src/core/runtime_context.rs` | `crates/xworkflow-types/src/runtime_context.rs` |
| `src/dsl/schema.rs`（NodeRunResult 等） | `crates/xworkflow-types/src/schema.rs` |
| `src/error.rs`（NodeError） | `crates/xworkflow-types/src/error.rs` |
| `src/plugin_system/traits.rs` | `crates/xworkflow-types/src/plugin.rs` |
| `src/evaluator/` | `crates/xworkflow-types/src/evaluator.rs` |

### 11.3 迁移到节点 crate

| 原路径 | 新路径 |
|--------|--------|
| `src/nodes/control_flow.rs` | `crates/xworkflow-nodes-core/src/` |
| `src/nodes/data_transform.rs`（template 部分） | `crates/xworkflow-nodes-transform/src/` |
| `src/nodes/data_transform.rs`（http 部分） | `crates/xworkflow-nodes-http/src/` |
| `src/nodes/data_transform.rs`（code 部分） | `crates/xworkflow-nodes-code/src/` |
| `src/sandbox/` | `crates/xworkflow-nodes-code/src/` |
| `src/nodes/subgraph_nodes.rs` | `crates/xworkflow-nodes-subgraph/src/` |
| `src/llm/` | `crates/xworkflow-nodes-llm/src/` |

### 11.4 修改

| 文件 | 变更 |
|------|------|
| `xworkflow/Cargo.toml` | 添加 feature flags，可选依赖节点 crate |
| `xworkflow/src/lib.rs` | 条件导入，re-export |
| `xworkflow/src/nodes/executor.rs` | `with_builtins()` 替代 `new()` |
| `xworkflow/src/core/dispatcher.rs` | 使用 xworkflow-types 的类型 |
| `xworkflow/src/scheduler.rs` | 注入 SubGraphRunner 到 RuntimeContext |

## 12. 实施路线图

### Phase 1：基础设施

**目标**：建立 workspace，提取共享类型

1. 创建 workspace 根 `Cargo.toml`
2. 将现有代码移入 `xworkflow/` 子目录
3. 创建 `crates/xworkflow-types/`，迁移共享类型
4. 定义 `SubGraphRunner` trait，实现 `DefaultSubGraphRunner`
5. 修改 `RuntimeContext` 添加 `sub_graph_runner` 字段
6. 修改子图节点使用 `context.sub_graph_runner`

**验证**：`cargo test --workspace` 全部通过

### Phase 2：节点 crate 拆分

**目标**：将内置节点迁移到独立 crate

按依赖复杂度从低到高：

1. `xworkflow-nodes-core`（start/end/answer/if-else）
2. `xworkflow-nodes-subgraph`（iteration/loop/list-operator）
3. `xworkflow-nodes-transform`（template/aggregator/assigner）
4. `xworkflow-nodes-http`（http-request）
5. `xworkflow-nodes-llm`（llm + providers）
6. `xworkflow-nodes-code`（code + JS/WASM 沙箱）

每个 crate：实现 Plugin trait → 添加 feature flag → 验证测试

**验证**：每个 crate 拆分后 `cargo test --workspace` 通过

### Phase 3：cdylib 支持

**目标**：支持编译为动态链接库

1. 为每个节点 crate 创建 `-dylib` 包装 crate
2. 实现 DLL 导出符号
3. 集成测试：编译 DLL → `DllPluginLoader` 加载 → 执行工作流

**验证**：
```bash
cargo build -p xworkflow-nodes-code-dylib --release
cargo test --features plugin-system -p xworkflow -- test_dynamic_code_node
```

