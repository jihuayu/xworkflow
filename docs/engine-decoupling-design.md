# 引擎解耦设计 — 消除 src/ 对 JS/WASM 引擎的强依赖

## 一、背景

xworkflow 已开始将 JS sandbox 和 WASM sandbox 抽取为独立 crate（`crates/xworkflow-sandbox-js`、`crates/xworkflow-sandbox-wasm`），并定义了引擎无关的 trait 抽象层（`xworkflow-types` 中的 `CodeSandbox`、`LanguageProvider`）。

但这个迁移**未完成**。`src/` 核心代码中仍然残留了大量直接依赖 `boa_engine` 和 `wasmtime` 的代码，包括：

- 与 crate 完全重复的实现文件
- 安全模块中硬编码的 boa_engine AST 分析
- 节点执行器中直接创建 boa_engine Context 的流式运行时
- WASM 插件系统中直接使用 wasmtime 类型
- 主 crate Cargo.toml 中不必要的直接引擎依赖

这违反了"核心不依赖具体引擎"的架构目标，导致：
- 不启用 sandbox feature 时仍可能引入引擎编译开销
- 无法替换为其他 JS/WASM 引擎（如 V8、wasmer）
- 代码重复带来维护负担

---

## 二、问题清单

### 问题 1：`src/sandbox/` 与 crate 实现完全重复

| src/ 文件 | crate 文件 | 关系 |
|-----------|-----------|------|
| `src/sandbox/builtin.rs` (407 行) | `crates/xworkflow-sandbox-js/src/sandbox.rs` | 几乎完全相同的 `BuiltinSandbox` 实现 |
| `src/sandbox/wasm_sandbox.rs` (491 行) | `crates/xworkflow-sandbox-wasm/src/sandbox.rs` | 几乎完全相同的 `WasmSandbox` 实现 |
| `src/sandbox/js_builtins.rs` (1 行 + 130 行测试) | `crates/xworkflow-sandbox-js/src/builtins.rs` | 仅 `pub use xworkflow_sandbox_js::builtins::*;` 薄包装 |

`src/sandbox/mod.rs` 已经通过 feature gate 从 crate 重新导出类型：

```rust
#[cfg(feature = "builtin-sandbox-js")]
pub use xworkflow_sandbox_js::{BuiltinSandbox, BuiltinSandboxConfig};

#[cfg(feature = "builtin-sandbox-wasm")]
pub use xworkflow_sandbox_wasm::{WasmSandbox, WasmSandboxConfig};
```

但 `builtin.rs` 和 `wasm_sandbox.rs` 中的完整实现副本仍然存在。

### 问题 2：`src/security/sandbox.rs` 硬编码 boa_engine AST 分析

`src/security/sandbox.rs` 第 3-11 行直接 import 了 9 个 `boa_engine` AST 模块：

```rust
use boa_engine::ast::expression::access::{PropertyAccess, PropertyAccessField};
use boa_engine::ast::expression::literal::Literal;
use boa_engine::ast::expression::{Call, Expression, Identifier, ImportCall, New};
use boa_engine::ast::statement::iteration::{DoWhileLoop, ForLoop, WhileLoop};
use boa_engine::ast::visitor::{VisitWith, Visitor};
use boa_engine::ast::Script;
use boa_engine::interner::Interner;
use boa_engine::parser::{Parser, Source};
use boa_engine::ast::scope::Scope;
```

实现了 `AstCodeAnalyzer` 和 `AstSecurityVisitor`，用于检测：
- 动态执行（`eval`, `Function` 构造器）
- 原型链篡改（`__proto__`, `constructor`, `prototype`）
- 禁止的全局变量（`process`, `require`, `globalThis`, `import`）
- 无限循环风险（`while(true)`, `for(;;)`）

**相同代码已存在于 `crates/xworkflow-sandbox-js/src/security.rs`**，属于纯粹的重复。

这导致 `security` feature 隐性依赖 `boa_engine`——即使用户只想启用安全策略管理（审计、凭证、网络策略），也被迫引入 JS 引擎。

### 问题 3：`src/nodes/data_transform.rs` 直接使用 boa_engine

第 6 行（feature-gated）：
```rust
#[cfg(feature = "builtin-sandbox-js")]
use boa_engine::{Context, Source};
```

第 1076 行，`spawn_js_stream_runtime_inner` 函数直接创建 boa_engine 上下文：
```rust
let mut context = Context::default();
js_builtins::register_all(&mut context)?;
context.eval(Source::from_bytes(&code))?;
```

此函数约 200 行，实现了完整的 JS 流式运行时：
- 创建 boa_engine `Context`
- 注入 stream callback 机制（`on_chunk` / `on_end` / `on_error`）
- 维持一个 blocking 线程内的命令循环（`RuntimeCommand::Invoke`）

这些代码虽然被 `#[cfg(feature = "builtin-sandbox-js")]` 保护，但仍属于核心节点执行器模块，且当前 `CodeSandbox` trait 没有流式执行接口，无法通过 trait object 调用。

### 问题 4：`src/plugin_system/wasm/` 直接使用 wasmtime

三个文件直接依赖 wasmtime 类型：

**`src/plugin_system/wasm/runtime.rs`**（第 5-6 行）：
```rust
use wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};
use wasmtime_wasi::preview1;
```
- `PluginRuntime` 结构体持有 `Engine`, `Module`, `Linker<PluginState>` 字段
- `call_function` 方法直接操作 wasmtime Store/Memory

**`src/plugin_system/wasm/host_functions.rs`**（第 6, 19, 23 行）：
```rust
use wasmtime::{Caller, Linker, Memory, StoreLimits};
pub wasi: wasmtime_wasi::preview1::WasiP1Ctx,
pub limits: StoreLimits,
```
- `PluginState` 结构体包含 `WasiP1Ctx` 和 `StoreLimits` 字段
- 所有 host function 签名绑定 `Caller<'_, PluginState>`

**`src/plugin_system/builtins/wasm_bootstrap.rs`**（第 85-93 行）：
```rust
pub struct WasmPluginLoader {
    engine: wasmtime::Engine,
}

impl WasmPluginLoader {
    pub fn new(config: WasmPluginConfig) -> Result<Self, PluginError> {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(true);
        let engine = wasmtime::Engine::new(&cfg)?;
        Ok(Self { engine, config })
    }
}
```

虽然这些模块在 `wasm-runtime` feature 后面，但它们位于 `src/plugin_system/` 核心目录中，使得插件系统架构上绑定了 wasmtime。

### 问题 5：主 crate Cargo.toml 直接声明引擎依赖

`Cargo.toml` 第 52-59 行：
```toml
boa_engine = { version = "0.20", optional = true }
wasmtime = { version = "27", optional = true }
wasmtime-wasi = { version = "27", optional = true }
wat = { version = "1", optional = true }
```

Feature 定义（第 91-94 行）：
```toml
wasm-runtime = ["dep:wasmtime", "dep:wasmtime-wasi", "dep:wat"]
builtin-sandbox-js = ["dep:xworkflow-sandbox-js", "dep:boa_engine"]
builtin-sandbox-wasm = ["dep:xworkflow-sandbox-wasm", "wasm-runtime"]
```

`builtin-sandbox-js` 同时依赖 `xworkflow-sandbox-js` crate **和**直接依赖 `boa_engine`——后者是多余的，因为 crate 内部已经封装了引擎。这种双重依赖正是问题 2、3 中 `src/` 代码直接使用 `boa_engine` 的根源。

---

## 三、目标架构

```
                    ┌─────────────────────────────────────┐
                    │         xworkflow (主 crate)         │
                    │                                     │
                    │  src/sandbox/mod.rs  (re-export)    │
                    │  src/sandbox/manager.rs             │
                    │  src/sandbox/types.rs  (re-export)  │
                    │  src/sandbox/error.rs  (re-export)  │
                    │                                     │
                    │  src/security/  (无引擎依赖)         │
                    │  src/nodes/    (无引擎依赖)          │
                    │  src/plugin_system/ (无 wasm 目录)   │
                    └──────────┬──────────────────────────┘
                               │ 仅通过 trait object 交互
           ┌───────────────────┼───────────────────────┐
           │                   │                       │
           ▼                   ▼                       ▼
┌──────────────────┐ ┌──────────────────┐ ┌──────────────────────┐
│ xworkflow-       │ │ xworkflow-       │ │ xworkflow-           │
│ sandbox-js       │ │ sandbox-wasm     │ │ plugin-wasm          │
│                  │ │                  │ │                      │
│ boa_engine       │ │ wasmtime         │ │ wasmtime + wasi      │
│ AST 安全分析     │ │ fuel/内存限制    │ │ PluginRuntime        │
│ builtins         │ │                  │ │ host functions       │
│ 流式运行时       │ │                  │ │ WasmPluginLoader     │
└──────────────────┘ └──────────────────┘ └──────────────────────┘
           │                   │                       │
           ▼                   ▼                       ▼
    ┌─────────────────────────────────────────────────────┐
    │              xworkflow-types (trait 定义)            │
    │  CodeSandbox, LanguageProvider, CodeAnalyzer,       │
    │  StreamingSandbox, WasmPluginEngine                  │
    └─────────────────────────────────────────────────────┘
```

**核心原则**：`src/` 中的代码只通过 `xworkflow-types` 定义的 trait 与引擎交互，绝不直接 import `boa_engine` 或 `wasmtime`。

---

## 四、改动方案

### 4.1 删除 `src/sandbox/` 中的重复实现

**删除文件**：
- `src/sandbox/builtin.rs` — 完整的 BuiltinSandbox 副本，crate 中已存在
- `src/sandbox/wasm_sandbox.rs` — 完整的 WasmSandbox 副本，crate 中已存在
- `src/sandbox/js_builtins.rs` — 仅 `pub use` + 测试，测试应移至 crate

**保留文件**：
- `src/sandbox/mod.rs` — 简化为仅 re-export（已有）
- `src/sandbox/manager.rs` — SandboxManager（使用 trait object，无引擎依赖）
- `src/sandbox/types.rs` — re-export `xworkflow_types::sandbox::*`
- `src/sandbox/error.rs` — re-export `xworkflow_types::sandbox::SandboxError`

`src/sandbox/mod.rs` 最终形态：

```rust
pub mod types;
pub mod error;
pub mod manager;

pub use types::*;
pub use error::SandboxError;
pub use manager::{SandboxManager, SandboxManagerConfig};

#[cfg(feature = "builtin-sandbox-js")]
pub use xworkflow_sandbox_js::{BuiltinSandbox, BuiltinSandboxConfig};

#[cfg(feature = "builtin-sandbox-wasm")]
pub use xworkflow_sandbox_wasm::{WasmSandbox, WasmSandboxConfig};
```

### 4.2 `CodeAnalyzer` trait 提升到 `xworkflow-types`

在 `crates/xworkflow-types/src/sandbox.rs`（或新建 `security.rs`）中定义引擎无关的分析接口：

```rust
/// 代码安全分析结果
#[derive(Debug, Clone)]
pub struct CodeAnalysisResult {
    pub is_safe: bool,
    pub violations: Vec<CodeViolation>,
}

#[derive(Debug, Clone)]
pub struct CodeViolation {
    pub kind: ViolationKind,
    pub location: (usize, usize),
    pub description: String,
}

#[derive(Debug, Clone)]
pub enum ViolationKind {
    DynamicExecution,
    PrototypeTampering,
    ForbiddenGlobal,
    InfiniteLoopRisk,
}

/// 引擎无关的代码安全分析 trait
pub trait CodeAnalyzer: Send + Sync {
    fn analyze(&self, code: &str) -> Result<CodeAnalysisResult, SandboxError>;
}
```

**`AstCodeAnalyzer`**（boa_engine 实现）保留在 `crates/xworkflow-sandbox-js/src/security.rs`，这是它唯一应该存在的位置。

**`src/security/sandbox.rs`**：
- 删除所有 `boa_engine` import 和 `AstCodeAnalyzer` / `AstSecurityVisitor` 实现
- 仅保留 `JsSandboxSecurityConfig`（如果安全模块仍需要它）
- 或将整个文件删除，`CodeAnalyzer` trait 已在 `xworkflow-types` 中

**`src/security/mod.rs`** 更新导出，移除 `sandbox` 子模块（若已清空）。

### 4.3 JS 流式运行时移入 `crates/xworkflow-sandbox-js`

#### 4.3.1 扩展 `xworkflow-types` 中的 trait

当前 `CodeSandbox` trait 仅支持同步执行（`execute(&self, request) -> Result<SandboxResult>`）。需要扩展流式执行接口：

```rust
/// 流式沙箱执行句柄
#[async_trait]
pub trait StreamingSandboxHandle: Send + Sync {
    /// 向流式运行时发送 chunk
    async fn send_chunk(&self, var_name: &str, chunk: &str) -> Result<(), SandboxError>;
    /// 通知流结束
    async fn end_stream(&self, var_name: &str) -> Result<(), SandboxError>;
    /// 通知流错误
    async fn error_stream(&self, var_name: &str, error: &str) -> Result<(), SandboxError>;
    /// 获取最终输出
    async fn finalize(self: Box<Self>) -> Result<SandboxResult, SandboxError>;
}

/// 支持流式执行的沙箱扩展 trait
#[async_trait]
pub trait StreamingSandbox: CodeSandbox {
    /// 创建流式执行环境，返回初始输出和控制句柄
    async fn execute_streaming(
        &self,
        request: SandboxRequest,
        stream_vars: Vec<String>,
    ) -> Result<(Value, bool, Box<dyn StreamingSandboxHandle>), SandboxError>;
}
```

#### 4.3.2 在 JS crate 中实现

将 `src/nodes/data_transform.rs` 中的 `spawn_js_stream_runtime_inner`、`JsStreamRuntime`、`RuntimeCommand` 等类型整体移入 `crates/xworkflow-sandbox-js/src/streaming.rs`（新文件），实现 `StreamingSandbox` trait。

#### 4.3.3 `data_transform.rs` 改用 trait object

```rust
// 之前（硬编码 boa_engine）：
let mut context = Context::default();
js_builtins::register_all(&mut context)?;

// 之后（通过 trait）：
let streaming_sandbox: &dyn StreamingSandbox = sandbox_manager
    .get_streaming_sandbox(CodeLanguage::JavaScript)?;
let (initial_output, has_callbacks, handle) = streaming_sandbox
    .execute_streaming(request, stream_vars).await?;
```

### 4.4 WASM 插件系统移入独立 crate

#### 4.4.1 新建 `crates/xworkflow-plugin-wasm/`

将以下文件从 `src/plugin_system/wasm/` 移入新 crate：

| 原位置 | 新位置 |
|--------|--------|
| `src/plugin_system/wasm/runtime.rs` | `crates/xworkflow-plugin-wasm/src/runtime.rs` |
| `src/plugin_system/wasm/host_functions.rs` | `crates/xworkflow-plugin-wasm/src/host_functions.rs` |
| `src/plugin_system/wasm/mod.rs` | `crates/xworkflow-plugin-wasm/src/lib.rs` |
| `src/plugin_system/builtins/wasm_bootstrap.rs` | `crates/xworkflow-plugin-wasm/src/bootstrap.rs` |

新 crate 的 `Cargo.toml`：

```toml
[package]
name = "xworkflow-plugin-wasm"
version = "0.1.0"
edition = "2021"

[dependencies]
wasmtime = "27"
wasmtime-wasi = "27"
wat = "1"
xworkflow-types = { path = "../xworkflow-types" }
# 需要访问 PluginContext, PluginError 等类型
# 可能需要将这些类型也提升到 xworkflow-types
```

#### 4.4.2 在 `xworkflow-types` 中定义插件运行时 trait

```rust
/// WASM 插件运行时抽象（允许替换 wasmtime 为其他 WASM 引擎）
#[async_trait]
pub trait WasmPluginRuntime: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
    fn status(&self) -> PluginStatus;
    fn call_function(
        &self,
        function_name: &str,
        input: &Value,
    ) -> Result<Value, PluginError>;
    async fn execute_node(
        &self,
        node_type: &PluginNodeType,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError>;
}
```

#### 4.4.3 主 crate 通过 feature 引入

```toml
# Cargo.toml
xworkflow-plugin-wasm = { path = "crates/xworkflow-plugin-wasm", optional = true }

[features]
wasm-runtime = ["dep:xworkflow-plugin-wasm"]
```

`src/plugin_system/mod.rs` 改为条件导入：

```rust
#[cfg(feature = "wasm-runtime")]
pub use xworkflow_plugin_wasm as wasm;
```

### 4.5 清理 Cargo.toml

**移除主 crate 的直接引擎依赖**：

```toml
# 删除这些行：
# boa_engine = { version = "0.20", optional = true }
# wasmtime = { version = "27", optional = true }
# wasmtime-wasi = { version = "27", optional = true }
# wat = { version = "1", optional = true }
```

**更新 feature 定义**：

```toml
[features]
# 之前：
# wasm-runtime = ["dep:wasmtime", "dep:wasmtime-wasi", "dep:wat"]
# builtin-sandbox-js = ["dep:xworkflow-sandbox-js", "dep:boa_engine"]

# 之后：
wasm-runtime = ["dep:xworkflow-plugin-wasm"]
builtin-sandbox-js = ["dep:xworkflow-sandbox-js"]
builtin-sandbox-wasm = ["dep:xworkflow-sandbox-wasm"]
```

### 4.6 单元测试迁移

在重复代码删除过程中，以下测试需要确认归属：

| 当前位置 | 正确位置 | 说明 |
|----------|----------|------|
| `src/sandbox/builtin.rs` 内 `#[cfg(test)] mod tests` | `crates/xworkflow-sandbox-js/src/sandbox.rs` | crate 中已存在相同测试，src/ 副本应删除 |
| `src/sandbox/wasm_sandbox.rs` 内 `#[cfg(test)] mod tests` | `crates/xworkflow-sandbox-wasm/src/sandbox.rs` | crate 中已存在相同测试，src/ 副本应删除 |
| `src/sandbox/js_builtins.rs` 内 `#[cfg(test)] mod tests` | `crates/xworkflow-sandbox-js/src/builtins.rs` 的测试模块 | 测试直接 `use boa_engine`，应在 crate 测试中 |
| `src/security/sandbox.rs` 内 `#[cfg(test)] mod tests` | `crates/xworkflow-sandbox-js/src/security.rs` | 测试 AstCodeAnalyzer，属于 JS crate 的测试 |

---

## 五、文件变更清单

### 删除

| 文件 | 原因 |
|------|------|
| `src/sandbox/builtin.rs` | 与 `crates/xworkflow-sandbox-js/src/sandbox.rs` 重复 |
| `src/sandbox/wasm_sandbox.rs` | 与 `crates/xworkflow-sandbox-wasm/src/sandbox.rs` 重复 |
| `src/sandbox/js_builtins.rs` | 薄包装 + 测试重复 |
| `src/security/sandbox.rs` | boa_engine 依赖移入 JS crate |
| `src/plugin_system/wasm/runtime.rs` | 移入 `crates/xworkflow-plugin-wasm` |
| `src/plugin_system/wasm/host_functions.rs` | 移入 `crates/xworkflow-plugin-wasm` |
| `src/plugin_system/wasm/mod.rs` | 移入 `crates/xworkflow-plugin-wasm` |
| `src/plugin_system/builtins/wasm_bootstrap.rs` | 移入 `crates/xworkflow-plugin-wasm` |

### 修改

| 文件 | 改动 |
|------|------|
| `src/sandbox/mod.rs` | 移除 `builtin` / `wasm_sandbox` / `js_builtins` 子模块声明 |
| `src/security/mod.rs` | 移除 `sandbox` 子模块声明和导出 |
| `src/nodes/data_transform.rs` | 移除 `use boa_engine`，改用 `StreamingSandbox` trait |
| `src/plugin_system/mod.rs` | `wasm` 模块改为 re-export `xworkflow_plugin_wasm` |
| `src/plugin_system/builtins/mod.rs` | 移除 `wasm_bootstrap` 子模块 |
| `src/lib.rs` | 无需改动（已通过 feature gate 条件编译） |
| `Cargo.toml` | 移除 `boa_engine`/`wasmtime`/`wasmtime-wasi`/`wat` 直接依赖 |

### 新建

| 文件 | 说明 |
|------|------|
| `crates/xworkflow-plugin-wasm/Cargo.toml` | WASM 插件 crate 配置 |
| `crates/xworkflow-plugin-wasm/src/lib.rs` | 公共导出 |
| `crates/xworkflow-plugin-wasm/src/runtime.rs` | PluginRuntime（从 src/ 移入） |
| `crates/xworkflow-plugin-wasm/src/host_functions.rs` | host functions（从 src/ 移入） |
| `crates/xworkflow-plugin-wasm/src/bootstrap.rs` | WasmPluginLoader（从 src/ 移入） |
| `crates/xworkflow-types/src/security.rs`（或扩展 sandbox.rs） | `CodeAnalyzer` trait + 相关类型 |
| `crates/xworkflow-sandbox-js/src/streaming.rs` | JS 流式运行时（从 data_transform.rs 移入） |

---

## 六、依赖关系变更

### 变更前

```
xworkflow (主 crate)
├── boa_engine (optional, 直接)
├── wasmtime (optional, 直接)
├── wasmtime-wasi (optional, 直接)
├── wat (optional, 直接)
├── xworkflow-sandbox-js (optional)
│   └── boa_engine (必需)
├── xworkflow-sandbox-wasm (optional)
│   └── wasmtime, wat (必需)
└── xworkflow-types
```

### 变更后

```
xworkflow (主 crate)
├── xworkflow-sandbox-js (optional) ── builtin-sandbox-js feature
│   └── boa_engine (必需, 仅在此 crate 内)
├── xworkflow-sandbox-wasm (optional) ── builtin-sandbox-wasm feature
│   └── wasmtime, wat (必需, 仅在此 crate 内)
├── xworkflow-plugin-wasm (optional) ── wasm-runtime feature
│   └── wasmtime, wasmtime-wasi (必需, 仅在此 crate 内)
└── xworkflow-types
    └── (无引擎依赖)
```

---

## 七、验证方式

1. **编译隔离验证**：
   ```bash
   # 不启用 sandbox feature 时不应引入引擎
   cargo build --no-default-features
   cargo tree | grep -E "boa_engine|wasmtime"  # 应无输出
   ```

2. **Feature 正确性验证**：
   ```bash
   cargo build --features builtin-sandbox-js   # JS sandbox 正常
   cargo build --features builtin-sandbox-wasm  # WASM sandbox 正常
   cargo build --features wasm-runtime          # WASM 插件系统正常
   ```

3. **依赖链验证**：
   ```bash
   # boa_engine 仅通过 xworkflow-sandbox-js 引入
   cargo tree -e features -i boa_engine
   # wasmtime 仅通过 xworkflow-sandbox-wasm 或 xworkflow-plugin-wasm 引入
   cargo tree -e features -i wasmtime
   ```

4. **测试通过**：
   ```bash
   cargo test                                    # 默认 feature
   cargo test --features builtin-code-node       # 包含 JS sandbox
   cargo test --features builtin-sandbox-wasm    # 包含 WASM sandbox
   ```

5. **代码搜索验证**：
   ```bash
   # src/ 中不应有 boa_engine 或 wasmtime 的 import
   grep -r "use boa_engine" src/      # 应无输出
   grep -r "use wasmtime" src/        # 应无输出
   ```
