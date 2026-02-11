# 轻量级插件整合进核心项目 — 设计文档

## 1. 背景与问题

### 1.1 现状

当前项目采用 workspace 多 crate 架构，将节点执行器和模板引擎拆分为 16 个独立 crate：

```
crates/
├── xworkflow-types/                  # 共享 trait 定义
├── xworkflow-sandbox-js/             # JS 沙箱 (boa_engine)
├── xworkflow-sandbox-wasm/           # WASM 沙箱 (wasmtime)
├── xworkflow-template-jinja/         # Jinja2 模板引擎 (minijinja)
├── xworkflow-nodes-core/             # 控制流节点 Plugin wrapper
├── xworkflow-nodes-transform/        # 数据转换节点 Plugin wrapper
├── xworkflow-nodes-http/             # HTTP 请求节点 Plugin wrapper
├── xworkflow-nodes-code/             # 代码执行节点 Plugin wrapper
├── xworkflow-nodes-subgraph/         # 子图节点 Plugin wrapper
├── xworkflow-nodes-llm/              # LLM 节点 Plugin wrapper
├── xworkflow-nodes-core-dylib/       # core 动态链接变体
├── xworkflow-nodes-transform-dylib/  # transform 动态链接变体
├── xworkflow-nodes-http-dylib/       # http 动态链接变体
├── xworkflow-nodes-code-dylib/       # code 动态链接变体
├── xworkflow-nodes-subgraph-dylib/   # subgraph 动态链接变体
└── xworkflow-nodes-llm-dylib/        # llm 动态链接变体
```

### 1.2 问题分析

经过深入分析，发现 **节点 Plugin wrapper crate 是纯粹的架构冗余**：

| Crate | 代码行数 | 实际作用 | 实现位置 |
|-------|---------|---------|---------|
| `xworkflow-nodes-core` | 62 行 | import `src/nodes/control_flow.rs` 的执行器，包装为 Plugin | `src/nodes/control_flow.rs` (532 行) |
| `xworkflow-nodes-transform` | 74 行 | import `src/nodes/data_transform.rs` 的执行器，包装为 Plugin | `src/nodes/data_transform.rs` (2070 行) |
| `xworkflow-nodes-http` | 58 行 | import `HttpRequestExecutor`，包装为 Plugin | `src/nodes/data_transform.rs` |
| `xworkflow-nodes-subgraph` | 67 行 | import `src/nodes/subgraph_nodes.rs` 的执行器，包装为 Plugin | `src/nodes/subgraph_nodes.rs` (1248 行) |
| `xworkflow-nodes-llm` | 60 行 | import `src/llm/executor.rs` 的执行器，包装为 Plugin | `src/llm/executor.rs` |

**核心发现**：这些 crate 仅是 50-75 行的薄 wrapper，全部节点执行逻辑已在 `src/` 中实现，并通过 `src/nodes/executor.rs::with_builtins()` 以 feature flag 方式直接注册：

```rust
// src/nodes/executor.rs - 现有的内置注册逻辑
pub fn with_builtins() -> Self {
    let mut registry = NodeExecutorRegistry::empty();

    #[cfg(feature = "builtin-core-nodes")]
    { registry.register("start", Box::new(StartNodeExecutor)); /* ... */ }

    #[cfg(feature = "builtin-transform-nodes")]
    { registry.register("template-transform", Box::new(TemplateTransformExecutor)); /* ... */ }

    // ... 各节点类型同理
}
```

因此删除这些 wrapper crate **不需要修改任何节点注册逻辑**。

### 1.3 特殊情况：xworkflow-template-jinja

`xworkflow-template-jinja` 与节点 crate 不同，它包含约 **150 行实际实现代码**（非 wrapper）：

- `engine.rs` (84 行)：`JinjaTemplateEngine` struct + `TemplateEngine` trait 实现 + `register_template_functions` 辅助函数
- `compiled.rs` (62 行)：`JinjaCompiledTemplate` struct + `CompiledTemplateHandle` trait 实现 + `Drop` 实现

该 crate 仅依赖 `minijinja` 和 `xworkflow-types`，不依赖主 crate，属于轻量级依赖。

## 2. 整合范围

### 2.1 分类标准

按二进制体积增量分为三档：

| 分类 | 依赖 | 体积增量 | 决策 |
|------|------|---------|------|
| 纯逻辑 | 无外部依赖 | ~0 KB | 整合 |
| 轻量依赖 | minijinja | ~200 KB | 整合 |
| 中量依赖 | reqwest (已为核心依赖) | ~0 KB (共享) | 整合 |
| 重依赖 | boa_engine, wasmtime | ~15 MB | **保留独立** |

### 2.2 整合清单

**删除的 crate（11 个）：**

| 类别 | 静态 crate | Dylib crate | 理由 |
|------|-----------|-------------|------|
| 控制流节点 | `xworkflow-nodes-core` | `xworkflow-nodes-core-dylib` | 纯 wrapper，~50 KB |
| 数据转换节点 | `xworkflow-nodes-transform` | `xworkflow-nodes-transform-dylib` | 纯 wrapper，依赖已在核心 |
| HTTP 节点 | `xworkflow-nodes-http` | `xworkflow-nodes-http-dylib` | 纯 wrapper，reqwest 已为核心依赖 |
| 子图节点 | `xworkflow-nodes-subgraph` | `xworkflow-nodes-subgraph-dylib` | 纯 wrapper，~30 KB |
| LLM 节点 | `xworkflow-nodes-llm` | `xworkflow-nodes-llm-dylib` | 纯 wrapper，reqwest 已为核心依赖 |
| 模板引擎 | `xworkflow-template-jinja` | — (无 dylib 变体) | 轻量依赖，~200 KB |

**保留的 crate（5 个）：**

| Crate | 理由 |
|-------|------|
| `xworkflow-types` | 共享 trait 定义，被保留 crate 依赖 |
| `xworkflow-sandbox-js` | 重依赖 boa_engine（~50 KB 代码但拉入整个 JS 引擎） |
| `xworkflow-sandbox-wasm` | 重依赖 wasmtime（~15 MB） |
| `xworkflow-nodes-code` | 依赖沙箱系统 |
| `xworkflow-nodes-code-dylib` | code 节点动态链接变体 |

### 2.3 依赖安全验证

确认无保留 crate 依赖被删除 crate：

- `xworkflow-nodes-code` → 依赖 `xworkflow`（主 crate）+ `xworkflow-types` ✅
- `xworkflow-sandbox-js` → 依赖 `xworkflow-types` ✅
- `xworkflow-sandbox-wasm` → 依赖 `xworkflow-types` ✅

## 3. 迁移方案

### 3.1 节点 crate（5 组 10 个 crate）— 直接删除

由于节点实现代码全部在 `src/nodes/` 中，且 `with_builtins()` 不引用任何外部 crate，这 10 个 crate **可以直接删除**，无需迁移任何代码。

**不受影响的文件：**
- `src/nodes/executor.rs` — 注册逻辑完全自包含
- `src/nodes/control_flow.rs` — 实现代码不变
- `src/nodes/data_transform.rs` — 实现代码不变
- `src/nodes/subgraph_nodes.rs` — 实现代码不变
- `src/llm/executor.rs` — 实现代码不变

### 3.2 模板引擎 crate — 代码迁移

`xworkflow-template-jinja` 包含实际实现，需要迁移至 `src/template/` 目录。

#### 3.2.1 创建 `src/template/jinja.rs`

合并 `crates/xworkflow-template-jinja/src/engine.rs` 和 `compiled.rs` 为单文件：

```rust
//! Jinja2 template engine implementation via minijinja.
//!
//! Provides `JinjaTemplateEngine` (implements `TemplateEngine` trait) and
//! `JinjaCompiledTemplate` (implements `CompiledTemplateHandle` trait).

use std::collections::HashMap;
use std::sync::Arc;

use minijinja::Environment;
use serde_json::Value;

use xworkflow_types::template::{CompiledTemplateHandle, TemplateEngine, TemplateFunction};

// ─── JinjaTemplateEngine ───────────────────────────────────────────

pub struct JinjaTemplateEngine;

impl JinjaTemplateEngine {
    pub fn new() -> Self { Self }
}

impl Default for JinjaTemplateEngine {
    fn default() -> Self { Self::new() }
}

impl TemplateEngine for JinjaTemplateEngine {
    fn render(
        &self,
        template: &str,
        variables: &HashMap<String, Value>,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<String, String> {
        let mut env = Environment::new();
        register_template_functions(&mut env, functions);
        env.add_template("tpl", template)
            .map_err(|e| format!("Template parse error: {}", e))?;
        let tmpl = env.get_template("tpl")
            .map_err(|e| format!("Template not found: {}", e))?;
        let ctx = minijinja::Value::from_serialize(variables);
        tmpl.render(ctx)
            .map_err(|e| format!("Template render error: {}", e))
    }

    fn compile(
        &self,
        template: &str,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<Box<dyn CompiledTemplateHandle>, String> {
        let compiled = JinjaCompiledTemplate::new(template, functions)?;
        Ok(Box::new(compiled))
    }

    fn engine_name(&self) -> &str { "jinja2" }
}

fn register_template_functions<'a>(
    env: &mut Environment<'a>,
    functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
) {
    if let Some(funcs) = functions {
        let owned = funcs.iter()
            .map(|(name, func)| (name.clone(), func.clone()))
            .collect::<Vec<_>>();
        for (name, func) in owned {
            env.add_function(name, move |args: Vec<minijinja::Value>| {
                let json_args = args.iter()
                    .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                let result = func.call(&json_args).map_err(|e| {
                    minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e)
                })?;
                Ok(minijinja::Value::from_serialize(result))
            });
        }
    }
}

// ─── JinjaCompiledTemplate ─────────────────────────────────────────

pub struct JinjaCompiledTemplate {
    env: Environment<'static>,
    template_source: *mut str,
}

// SAFETY: JinjaCompiledTemplate owns its template source pointer and the
// environment only references that owned memory.
unsafe impl Send for JinjaCompiledTemplate {}
unsafe impl Sync for JinjaCompiledTemplate {}

impl JinjaCompiledTemplate {
    pub fn new(
        template: &str,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<Self, String> {
        let boxed: Box<str> = template.to_owned().into_boxed_str();
        let raw = Box::into_raw(boxed);
        let static_str: &'static str = unsafe { &*raw };

        let mut env = Environment::new();
        register_template_functions(&mut env, functions);  // 直接调用同模块函数
        env.add_template("tpl", static_str)
            .map_err(|e| format!("Template parse error: {}", e))?;

        Ok(Self { env, template_source: raw })
    }
}

impl CompiledTemplateHandle for JinjaCompiledTemplate {
    fn render(&self, variables: &HashMap<String, Value>) -> Result<String, String> {
        let tmpl = self.env.get_template("tpl")
            .map_err(|e| format!("Template not found: {}", e))?;
        let ctx = minijinja::Value::from_serialize(variables);
        tmpl.render(ctx)
            .map_err(|e| format!("Template render error: {}", e))
    }
}

impl Drop for JinjaCompiledTemplate {
    fn drop(&mut self) {
        unsafe { let _ = Box::from_raw(self.template_source); }
    }
}
```

**关键变化点：**
- `crate::compiled::JinjaCompiledTemplate` → 同模块内直接引用（无需跨模块导入）
- `super::engine::register_template_functions` → 同模块内直接调用 `register_template_functions`

#### 3.2.2 修改 `src/template/mod.rs`

```rust
pub mod engine;

#[cfg(feature = "builtin-template-jinja")]
pub mod jinja;

pub use engine::*;
```

#### 3.2.3 修改 `src/template/engine.rs` 第 148 行

引用路径更新：

```rust
// 修改前：
Arc::new(xworkflow_template_jinja::JinjaTemplateEngine::new())

// 修改后：
Arc::new(crate::template::jinja::JinjaTemplateEngine::new())
```

#### 3.2.4 修改 `src/plugin_system/builtins/template_jinja.rs` 第 7 行

引用路径更新：

```rust
// 修改前：
use xworkflow_template_jinja::JinjaTemplateEngine;

// 修改后：
use crate::template::jinja::JinjaTemplateEngine;
```

### 3.3 Cargo.toml 变更

#### 3.3.1 依赖变更

```toml
# 新增直接依赖（替代 xworkflow-template-jinja crate）
minijinja = { version = "2.0", features = ["builtins", "fuel"], optional = true }

# 删除
xworkflow-template-jinja = { path = "crates/xworkflow-template-jinja", optional = true }
```

注：`serde_json` 已为主 crate 依赖，无需额外添加。

#### 3.3.2 Feature 变更

```toml
# 修改前：
builtin-template-jinja = ["dep:xworkflow-template-jinja"]

# 修改后：
builtin-template-jinja = ["dep:minijinja"]
```

其他 feature flag 保持不变（`builtin-core-nodes = []`、`builtin-transform-nodes = ["builtin-template-jinja"]` 等）。

#### 3.3.3 Workspace 变更

```toml
# 修改前（16 个 member）：
[workspace]
resolver = "2"
members = [
    ".",
    "crates/xworkflow-types",
    "crates/xworkflow-sandbox-js",
    "crates/xworkflow-sandbox-wasm",
    "crates/xworkflow-template-jinja",         # 删除
    "crates/xworkflow-nodes-core",             # 删除
    "crates/xworkflow-nodes-transform",        # 删除
    "crates/xworkflow-nodes-http",             # 删除
    "crates/xworkflow-nodes-code",
    "crates/xworkflow-nodes-subgraph",         # 删除
    "crates/xworkflow-nodes-llm",              # 删除
    "crates/xworkflow-nodes-core-dylib",       # 删除
    "crates/xworkflow-nodes-transform-dylib",  # 删除
    "crates/xworkflow-nodes-http-dylib",       # 删除
    "crates/xworkflow-nodes-code-dylib",
    "crates/xworkflow-nodes-subgraph-dylib",   # 删除
    "crates/xworkflow-nodes-llm-dylib",        # 删除
]

# 修改后（5 个 member）：
[workspace]
resolver = "2"
members = [
    ".",
    "crates/xworkflow-types",
    "crates/xworkflow-sandbox-js",
    "crates/xworkflow-sandbox-wasm",
    "crates/xworkflow-nodes-code",
    "crates/xworkflow-nodes-code-dylib",
]
```

## 4. Feature Flag 保留策略

整合后仍保留所有 feature flag，用户可按需关闭：

```toml
[features]
default = [
    "security",
    "plugin-system",
    "builtin-sandbox-js",
    "builtin-sandbox-wasm",
    "builtin-template-jinja",      # 现在拉入 minijinja（~200 KB）
    "builtin-core-nodes",          # 零额外依赖
    "builtin-transform-nodes",     # 依赖 builtin-template-jinja
    "builtin-http-node",           # 零额外依赖（reqwest 已为核心依赖）
    "builtin-code-node",           # 依赖 builtin-sandbox-js
    "builtin-subgraph-nodes",      # 零额外依赖
    "builtin-llm-node",            # 零额外依赖
]
```

**精简构建示例：**

```bash
# 最小核心（无任何内置节点）
cargo build --no-default-features

# 仅控制流 + 子图节点
cargo build --no-default-features --features "builtin-core-nodes,builtin-subgraph-nodes"

# 无模板引擎的构建
cargo build --no-default-features --features "builtin-core-nodes,builtin-http-node,builtin-llm-node"
```

## 5. 变更文件汇总

| 文件 | 操作 | 说明 |
|------|------|------|
| `src/template/jinja.rs` | **新建** | 从 `xworkflow-template-jinja` 迁移 engine.rs + compiled.rs |
| `src/template/mod.rs` | **修改** | 添加 `#[cfg] pub mod jinja` |
| `src/template/engine.rs` | **修改** | 第 148 行引用路径更新 |
| `src/plugin_system/builtins/template_jinja.rs` | **修改** | 第 7 行引用路径更新 |
| `Cargo.toml` | **修改** | 依赖、feature、workspace members |
| `crates/xworkflow-template-jinja/` | **删除** | 代码已迁移 |
| `crates/xworkflow-nodes-core/` | **删除** | 纯 wrapper，不需要迁移 |
| `crates/xworkflow-nodes-core-dylib/` | **删除** | dylib 变体 |
| `crates/xworkflow-nodes-transform/` | **删除** | 纯 wrapper |
| `crates/xworkflow-nodes-transform-dylib/` | **删除** | dylib 变体 |
| `crates/xworkflow-nodes-http/` | **删除** | 纯 wrapper |
| `crates/xworkflow-nodes-http-dylib/` | **删除** | dylib 变体 |
| `crates/xworkflow-nodes-subgraph/` | **删除** | 纯 wrapper |
| `crates/xworkflow-nodes-subgraph-dylib/` | **删除** | dylib 变体 |
| `crates/xworkflow-nodes-llm/` | **删除** | 纯 wrapper |
| `crates/xworkflow-nodes-llm-dylib/` | **删除** | dylib 变体 |

**无需修改的文件：**
- `src/nodes/executor.rs` — 注册逻辑自包含，不引用外部 crate
- `src/nodes/control_flow.rs`、`data_transform.rs`、`subgraph_nodes.rs` — 实现代码不变
- `src/llm/executor.rs` — 实现代码不变
- `.github/workflows/ci.yml` — 使用 `--workspace` 自动适应
- 所有测试和 benchmark 文件 — 无外部 crate 引用

## 6. 整合前后对比

### 6.1 项目结构对比

```
# 整合前: 16 个 workspace member
crates/
├── xworkflow-types/                ← 保留
├── xworkflow-sandbox-js/           ← 保留
├── xworkflow-sandbox-wasm/         ← 保留
├── xworkflow-template-jinja/       ← 删除（代码迁至 src/template/jinja.rs）
├── xworkflow-nodes-core/           ← 删除
├── xworkflow-nodes-transform/      ← 删除
├── xworkflow-nodes-http/           ← 删除
├── xworkflow-nodes-code/           ← 保留
├── xworkflow-nodes-subgraph/       ← 删除
├── xworkflow-nodes-llm/            ← 删除
├── xworkflow-nodes-core-dylib/     ← 删除
├── xworkflow-nodes-transform-dylib/← 删除
├── xworkflow-nodes-http-dylib/     ← 删除
├── xworkflow-nodes-code-dylib/     ← 保留
├── xworkflow-nodes-subgraph-dylib/ ← 删除
└── xworkflow-nodes-llm-dylib/      ← 删除

# 整合后: 5 个 workspace member
crates/
├── xworkflow-types/
├── xworkflow-sandbox-js/
├── xworkflow-sandbox-wasm/
├── xworkflow-nodes-code/
└── xworkflow-nodes-code-dylib/
```

### 6.2 量化影响

| 指标 | 整合前 | 整合后 | 变化 |
|------|--------|--------|------|
| Workspace member 数 | 16 | 5 | -11 |
| Cargo.toml 文件数 | 16 | 5 | -11 |
| 总 wrapper 代码行数 | ~600 | 0 | -600 |
| 新增核心代码 | 0 | ~150 (jinja.rs) | +150 |
| 二进制体积变化 | — | — | 无变化 |
| 编译时间 | — | — | 略有减少（减少 crate 解析开销） |

## 7. 验证方案

### 7.1 编译验证

```bash
# 默认 feature 全开
cargo check

# 无 feature（最小构建）
cargo check --no-default-features

# 仅部分 feature
cargo check --no-default-features --features "builtin-core-nodes,builtin-subgraph-nodes"

# 含模板但无沙箱
cargo check --no-default-features --features "builtin-template-jinja,builtin-transform-nodes"
```

### 7.2 测试验证

```bash
# 全量测试
cargo test --workspace

# 核心模板测试（覆盖 Jinja 迁移）
cargo test --lib -- template::

# E2E 测试（覆盖节点注册和执行）
cargo test --test integration_tests

# 插件系统测试
cargo test --test plugin_system_registry
cargo test --test plugin_system_tests

# 内存测试
cargo test --test memory_tests -- --test-threads=1
```

### 7.3 Benchmark 验证

```bash
cargo bench
```

### 7.4 关键验证点

1. **Jinja 模板渲染正确性** — `src/template/engine.rs` 中的 `test_render_jinja2*` 和 `test_compiled_template*` 测试
2. **Feature flag 隔离性** — `--no-default-features` 编译不报错
3. **插件系统兼容性** — `JinjaTemplatePlugin` 仍能通过插件系统注册模板引擎
4. **E2E 工作流执行** — 完整工作流运行不受影响

## 8. 实施顺序

建议按以下顺序原子化提交：

1. **创建 `src/template/jinja.rs`** + 更新 `mod.rs`
2. **更新引用路径**（`engine.rs`、`template_jinja.rs`）
3. **更新 `Cargo.toml`**（依赖、feature、workspace）
4. **删除 11 个 crate 目录**
5. **验证** `cargo check && cargo test --workspace`

步骤 1-4 可作为单次提交，因为 Cargo 整体解析不存在中间状态问题。
