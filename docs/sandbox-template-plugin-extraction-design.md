# 沙盒插件与模板引擎插件独立化设计

## 1. 背景与目标

### 1.1 现状

当前沙盒实现（JS via boa_engine、WASM via wasmtime）和模板引擎（Jinja2 via minijinja）硬编码在主 crate 中：

- `src/sandbox/builtin.rs` — JavaScript 沙盒（~1174 行，依赖 boa_engine ~50KB）
- `src/sandbox/wasm_sandbox.rs` — WASM 沙盒（~491 行，依赖 wasmtime ~15MB）
- `src/sandbox/js_builtins.rs` — JS 内置函数（uuid、datetime、crypto 等）
- `src/template/engine.rs` — Jinja2 模板引擎（~350 行，依赖 minijinja）

这导致：
1. 无法独立启用/禁用某个沙盒（如不需要 WASM 时仍编译 wasmtime）
2. 沙盒与模板引擎无法作为插件被外部替换或扩展
3. 编译时间长（wasmtime 单独 ~90s）

### 1.2 目标

1. **沙盒插件独立化**：JS 沙盒和 WASM 沙盒各自独立为 crate，每个拆为 Boot 插件（注册基础设施）+ Normal 插件（注册 lang-provide），底层共享同一实例
2. **lang-provide 机制**：沙盒插件在 Normal 阶段作为语言提供者（lang-provide），为 code 节点提供语言执行能力
3. **模板引擎插件独立化**：Jinja2 引擎独立为 crate，作为 Bootstrap 插件
4. **零成本抽象**：通过 feature flag 控制，禁用时零编译开销

### 1.3 相关设计文档

- `docs/plugin-system-design.md` — 插件系统整体设计（两阶段生命周期）
- `docs/builtin-plugin-extraction-design.md` — 内置节点模块化设计（workspace 拆分）

---

## 2. 双插件模式：Boot 插件 + Normal 插件

### 2.1 问题

沙盒插件需要同时参与两个阶段：

- **Bootstrap 阶段**：注册沙盒基础设施（`CodeSandbox` 实现）
- **Normal 阶段**：注册为语言提供者（`LanguageProvider`），向 code 节点提供 lang-provide

### 2.2 方案：拆为两个独立插件

**不修改 `PluginCategory` 和 `Plugin` trait**，而是将每个沙盒拆为两个插件：

| 插件 | 类别 | 阶段 | 职责 |
|------|------|------|------|
| `JsSandboxBootPlugin` | Bootstrap | Boot | 注册 `CodeSandbox` 基础设施 |
| `JsSandboxLangPlugin` | Normal | Normal | 注册 `LanguageProvider`（lang-provide） |
| `WasmSandboxBootPlugin` | Bootstrap | Boot | 注册 `CodeSandbox` 基础设施 |
| `WasmSandboxLangPlugin` | Normal | Normal | 注册 `LanguageProvider`（lang-provide） |

两个插件底层共享同一套实现（同一个 `Arc<BuiltinSandbox>` 实例）。

### 2.3 共享机制

Boot 插件和 Normal 插件通过 `Arc` 共享底层沙盒实例：

```rust
// crates/xworkflow-sandbox-js/src/lib.rs

/// 创建一对 JS 沙盒插件（Boot + Normal），共享同一个沙盒实例
pub fn create_js_sandbox_plugins(
    config: BuiltinSandboxConfig,
) -> (JsSandboxBootPlugin, JsSandboxLangPlugin) {
    let sandbox = Arc::new(BuiltinSandbox::new(config));
    (
        JsSandboxBootPlugin::new(sandbox.clone()),
        JsSandboxLangPlugin::new(sandbox),
    )
}
```

### 2.4 优势

1. **零改动**：不修改 `PluginCategory`、`Plugin` trait、`PluginRegistry`
2. **职责清晰**：Boot 插件只管基础设施，Normal 插件只管业务注册
3. **灵活组合**：可以只注册 Boot 插件（仅基础设施），或两个都注册（完整 lang-provide）
4. **现有机制**：完全复用现有的两阶段生命周期，无需特殊处理加载顺序

### 2.5 加载顺序

```
1. Bootstrap 阶段
   ├─ JsSandboxBootPlugin.register()   → register_sandbox(JavaScript, BuiltinSandbox)
   ├─ WasmSandboxBootPlugin.register() → register_sandbox(Wasm, WasmSandbox)
   └─ JinjaTemplatePlugin.register()   → register_template_engine(JinjaEngine)

2. Normal 阶段
   ├─ JsSandboxLangPlugin.register()   → register_language_provider(JsProvider)
   ├─ WasmSandboxLangPlugin.register() → register_language_provider(WasmProvider)
   ├─ CodeNodePlugin.register()        → query_language_providers() → 构建 SandboxManager
   └─ TransformPlugin.register()       → query_template_engine() → 获取 TemplateEngine
```

**注意**：Normal 阶段的 lang-provide 插件需要在 code 节点插件之前注册。这通过 `WorkflowRunnerBuilder` 中的注册顺序保证（内置插件按固定顺序注册）。

---

## 3. lang-provide 机制

### 3.1 概念

`lang-provide` 是沙盒插件在 Normal 阶段向 code 节点提供语言执行能力的机制。

**核心原则**：`LanguageProvider` trait 由 code 插件（`xworkflow-nodes-code`）定义和导出，不属于核心代码。核心插件系统只提供通用的跨插件服务发现机制。

**两阶段的区别：**

| 阶段 | 注册内容 | 用途 |
|------|---------|------|
| Bootstrap | `CodeSandbox` | 基础设施，供任意组件直接使用沙盒 |
| Normal | `LanguageProvider` | 业务层，为 code 节点提供语言支持 + 元数据 |

### 3.2 通用服务发现机制（核心插件系统）

**位置：`src/plugin_system/context.rs`**

核心插件系统提供基于字符串 key 的通用服务注册/查询，不感知具体业务类型：

```rust
use std::any::Any;

/// 在 PluginRegistryInner 中新增
pub(crate) services: HashMap<String, Vec<Arc<dyn Any + Send + Sync>>>,

/// 在 PluginContext 中新增
/// 注册一个服务实例到指定 key（Normal 阶段）
pub fn provide_service(
    &mut self,
    key: &str,
    service: Arc<dyn Any + Send + Sync>,
) -> Result<(), PluginError> {
    self.ensure_phase(PluginPhase::Normal)?;
    self.registry_inner
        .services
        .entry(key.to_string())
        .or_default()
        .push(service);
    Ok(())
}

/// 查询指定 key 下的所有服务实例
pub fn query_services(
    &self,
    key: &str,
) -> &[Arc<dyn Any + Send + Sync>] {
    self.registry_inner
        .services
        .get(key)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}
```

这是一个通用的 SPI（Service Provider Interface）机制，任何插件都可以用它来定义扩展点。

### 3.3 LanguageProvider trait（code 插件定义）

**位置：`crates/xworkflow-nodes-code/src/lang_provide.rs`**

```rust
use std::sync::Arc;
use xworkflow_types::{CodeLanguage, CodeSandbox, ExecutionConfig};

/// code 插件定义的服务 key
pub const LANG_PROVIDE_KEY: &str = "code.lang-provide";

/// 语言提供者 —— 由 code 插件定义，沙盒插件实现
pub trait LanguageProvider: Send + Sync + 'static {
    /// 支持的语言标识
    fn language(&self) -> CodeLanguage;

    /// 语言显示名称（如 "JavaScript (boa)"、"WebAssembly"）
    fn display_name(&self) -> &str;

    /// 关联的沙盒实现
    fn sandbox(&self) -> Arc<dyn CodeSandbox>;

    /// 默认执行配置（超时、内存限制等）
    fn default_config(&self) -> ExecutionConfig;

    /// 支持的文件扩展名（用于 DSL 校验，如 [".js", ".mjs"]）
    fn file_extensions(&self) -> Vec<&str>;

    /// 是否支持流式执行（如 JS 的 onData/onComplete 回调模式）
    fn supports_streaming(&self) -> bool { false }
}

/// 便捷函数：沙盒插件用来注册 lang-provide
pub fn register_language_provider(
    ctx: &mut PluginContext,
    provider: Arc<dyn LanguageProvider>,
) -> Result<(), PluginError> {
    ctx.provide_service(LANG_PROVIDE_KEY, provider)
}

/// 便捷函数：code 插件用来查询所有 lang-provide
pub fn query_language_providers(
    ctx: &PluginContext,
) -> Vec<Arc<dyn LanguageProvider>> {
    ctx.query_services(LANG_PROVIDE_KEY)
        .iter()
        .filter_map(|s| s.clone().downcast::<dyn LanguageProvider>().ok())
        .collect()
}
```

> **注意**：`Arc<dyn Any>` 到 `Arc<dyn LanguageProvider>` 的 downcast 需要具体类型。实际实现中，可以用 wrapper struct 包装：

```rust
/// 包装器，用于 Any downcast
struct LanguageProviderWrapper(Arc<dyn LanguageProvider>);

// 注册时
ctx.provide_service(LANG_PROVIDE_KEY, Arc::new(
    LanguageProviderWrapper(provider)
))?;

// 查询时
ctx.query_services(LANG_PROVIDE_KEY)
    .iter()
    .filter_map(|s| s.downcast_ref::<LanguageProviderWrapper>())
    .map(|w| w.0.clone())
    .collect()
```

### 3.4 依赖关系

```
xworkflow-types          ← CodeSandbox, CodeLanguage, ExecutionConfig
    ↑
xworkflow-nodes-code     ← 定义 LanguageProvider trait + LANG_PROVIDE_KEY
    ↑
xworkflow-sandbox-js     ← 实现 LanguageProvider，注册到 provide_service()
xworkflow-sandbox-wasm   ← 实现 LanguageProvider，注册到 provide_service()
```

沙盒插件 crate 依赖 `xworkflow-nodes-code`（仅用于 `LanguageProvider` trait 和注册函数），不依赖 code 节点的执行逻辑。

### 3.5 Code 节点消费 lang-provide

```rust
// crates/xworkflow-nodes-code/src/plugin.rs

impl Plugin for CodeNodePlugin {
    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let providers = query_language_providers(ctx);

        let mut manager = SandboxManager::new_empty();
        for provider in &providers {
            manager.register_sandbox(provider.language(), provider.sandbox());
        }

        ctx.register_node_executor("code", Box::new(
            CodeNodeExecutor::new_with_manager(Arc::new(manager))
        ))?;

        Ok(())
    }
}
```

**无 lang-provide 时的行为**：如果没有沙盒插件注册 LanguageProvider，code 节点仍然注册但执行时会返回 `UnsupportedLanguage` 错误。

---

## 4. 沙盒插件独立化

### 4.1 JS 沙盒插件

**Crate：`crates/xworkflow-sandbox-js/`**

```
crates/xworkflow-sandbox-js/
├── Cargo.toml
└── src/
    ├── lib.rs          # 导出两个插件 + create 工厂函数
    ├── sandbox.rs      # BuiltinSandbox 实现（从 src/sandbox/builtin.rs 迁移）
    ├── builtins.rs     # JS 内置函数（从 src/sandbox/js_builtins.rs 迁移）
    ├── boot.rs         # JsSandboxBootPlugin（Bootstrap 插件）
    └── lang.rs         # JsSandboxLangPlugin（Normal 插件）+ JsLanguageProvider
```

**依赖：**

```toml
[package]
name = "xworkflow-sandbox-js"
version = "0.1.0"

[dependencies]
xworkflow-types = { path = "../xworkflow-types" }
xworkflow-nodes-code = { path = "../xworkflow-nodes-code" }  # LanguageProvider trait
boa_engine = "0.20"
boa_parser = "0.20"
async-trait = "0.1"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1", features = ["rt"] }
tracing = "0.1"
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
md-5 = "0.10"
sha2 = "0.10"
hmac = "0.12"
aes = "0.8"
rand = "0.8"
base64 = "0.22"
```

**插件实现：**

```rust
// crates/xworkflow-sandbox-js/src/lib.rs

/// 创建一对 JS 沙盒插件（Boot + Normal），共享同一个沙盒实例
pub fn create_js_sandbox_plugins(
    config: BuiltinSandboxConfig,
) -> (JsSandboxBootPlugin, JsSandboxLangPlugin) {
    let sandbox = Arc::new(BuiltinSandbox::new(config));
    (
        JsSandboxBootPlugin::new(sandbox.clone()),
        JsSandboxLangPlugin::new(sandbox),
    )
}
```

```rust
// crates/xworkflow-sandbox-js/src/boot.rs

pub struct JsSandboxBootPlugin {
    metadata: PluginMetadata,
    sandbox: Arc<BuiltinSandbox>,
}

impl JsSandboxBootPlugin {
    pub fn new(sandbox: Arc<BuiltinSandbox>) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.sandbox-js.boot".into(),
                name: "JavaScript Sandbox (Boot)".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                category: PluginCategory::Bootstrap,
                description: "Registers JS sandbox infrastructure".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
            sandbox,
        }
    }
}

#[async_trait]
impl Plugin for JsSandboxBootPlugin {
    fn metadata(&self) -> &PluginMetadata { &self.metadata }

    async fn register(
        &self, ctx: &mut PluginContext
    ) -> Result<(), PluginError> {
        ctx.register_sandbox(CodeLanguage::JavaScript, self.sandbox.clone())?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any { self }
}
```

```rust
// crates/xworkflow-sandbox-js/src/lang.rs
use xworkflow_nodes_code::{LanguageProvider, register_language_provider, LANG_PROVIDE_KEY};

pub struct JsSandboxLangPlugin {
    metadata: PluginMetadata,
    sandbox: Arc<BuiltinSandbox>,
}

impl JsSandboxLangPlugin {
    pub fn new(sandbox: Arc<BuiltinSandbox>) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.sandbox-js.lang".into(),
                name: "JavaScript Language Provider".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                category: PluginCategory::Normal,
                description: "Provides JavaScript lang-provide for code node".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
            sandbox,
        }
    }
}

#[async_trait]
impl Plugin for JsSandboxLangPlugin {
    fn metadata(&self) -> &PluginMetadata { &self.metadata }

    async fn register(
        &self, ctx: &mut PluginContext
    ) -> Result<(), PluginError> {
        let provider = Arc::new(JsLanguageProvider {
            sandbox: self.sandbox.clone(),
        });
        register_language_provider(ctx, provider)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any { self }
}
```

**JsLanguageProvider 实现：**

```rust
pub struct JsLanguageProvider {
    sandbox: Arc<BuiltinSandbox>,
}

impl LanguageProvider for JsLanguageProvider {
    fn language(&self) -> CodeLanguage { CodeLanguage::JavaScript }
    fn display_name(&self) -> &str { "JavaScript (boa)" }
    fn sandbox(&self) -> Arc<dyn CodeSandbox> { self.sandbox.clone() }
    fn default_config(&self) -> ExecutionConfig {
        ExecutionConfig {
            timeout: Duration::from_secs(30),
            max_memory: 32 * 1024 * 1024,
            ..Default::default()
        }
    }
    fn file_extensions(&self) -> Vec<&str> { vec![".js", ".mjs"] }
    fn supports_streaming(&self) -> bool { true }
}
```

### 4.2 WASM 沙盒插件

**Crate：`crates/xworkflow-sandbox-wasm/`**

```
crates/xworkflow-sandbox-wasm/
├── Cargo.toml
└── src/
    ├── lib.rs          # 导出两个插件 + create 工厂函数
    ├── sandbox.rs      # WasmSandbox 实现（从 src/sandbox/wasm_sandbox.rs 迁移）
    ├── boot.rs         # WasmSandboxBootPlugin（Bootstrap 插件）
    └── lang.rs         # WasmSandboxLangPlugin（Normal 插件）+ WasmLanguageProvider
```

**依赖：**

```toml
[package]
name = "xworkflow-sandbox-wasm"
version = "0.1.0"

[dependencies]
xworkflow-types = { path = "../xworkflow-types" }
xworkflow-nodes-code = { path = "../xworkflow-nodes-code" }  # LanguageProvider trait
wasmtime = "29"
wat = "1"
async-trait = "0.1"
serde_json = "1.0"
tokio = { version = "1", features = ["rt", "time"] }
tracing = "0.1"
base64 = "0.22"
```

**插件实现：** 结构同 JS 沙盒，拆为 `WasmSandboxBootPlugin` + `WasmSandboxLangPlugin`，通过 `create_wasm_sandbox_plugins(config)` 工厂函数创建，共享同一个 `Arc<WasmSandbox>`。
- Boot 插件注册 `CodeLanguage::Wasm` 的 `WasmSandbox`
- Lang 插件注册 `WasmLanguageProvider`
- `WasmLanguageProvider.supports_streaming()` 返回 `false`
- `WasmLanguageProvider.file_extensions()` 返回 `[".wasm", ".wat"]`

### 4.3 沙盒核心保留

以下类型移入 `xworkflow-types` crate（供沙盒插件 crate 依赖）：

| 类型 | 原位置 | 说明 |
|------|--------|------|
| `CodeSandbox` trait | `src/sandbox/types.rs` | 沙盒执行接口 |
| `SandboxType` | `src/sandbox/types.rs` | 沙盒类型枚举 |
| `CodeLanguage` | `src/sandbox/types.rs` | 语言枚举 |
| `SandboxRequest` | `src/sandbox/types.rs` | 执行请求 |
| `SandboxResult` | `src/sandbox/types.rs` | 执行结果 |
| `ExecutionConfig` | `src/sandbox/types.rs` | 执行配置 |
| `HealthStatus` | `src/sandbox/types.rs` | 健康状态 |
| `SandboxStats` | `src/sandbox/types.rs` | 统计信息 |
| `SandboxError` | `src/sandbox/error.rs` | 错误类型 |

以下保留在主 crate `src/sandbox/`：

| 类型 | 说明 |
|------|------|
| `SandboxManager` | 沙盒路由管理器（改为从 LanguageProvider 构建） |

**SandboxManager 变更：**

```rust
impl SandboxManager {
    /// 创建空的管理器（由 LanguageProvider 填充）
    pub fn new_empty() -> Self {
        Self {
            default_sandbox: None,  // 改为 Option
            sandboxes: HashMap::new(),
        }
    }

    /// 从 LanguageProvider 列表构建
    pub fn from_providers(providers: &[Arc<dyn LanguageProvider>]) -> Self {
        let mut manager = Self::new_empty();
        for provider in providers {
            manager.register_sandbox(provider.language(), provider.sandbox());
        }
        // 第一个注册的作为默认
        if let Some(first) = providers.first() {
            manager.default_sandbox = Some(first.sandbox());
        }
        manager
    }
}
```

`src/sandbox/mod.rs` 不再直接导出 `BuiltinSandbox` 和 `WasmSandbox`。

---

## 5. 模板引擎插件独立化

### 5.1 拆分策略

模板引擎分为两部分：

| 部分 | 保留/迁移 | 原因 |
|------|----------|------|
| Dify 语法（`{{#node.var#}}`） | 保留在核心 `src/template/` | 依赖 `VariablePool`，是核心变量引用机制 |
| Jinja2 引擎（minijinja） | 迁移到插件 crate | 可替换的模板引擎实现 |

### 5.2 TemplateEngine trait

**位置：`src/plugin_system/extensions.rs`**

```rust
use std::collections::HashMap;
use serde_json::Value;

/// 模板引擎接口 —— 可替换的模板渲染实现
pub trait TemplateEngine: Send + Sync {
    /// 渲染模板字符串
    fn render(
        &self,
        template: &str,
        variables: &HashMap<String, Value>,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<String, String>;

    /// 预编译模板（用于重复渲染场景，如流式处理）
    fn compile(
        &self,
        template: &str,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<Box<dyn CompiledTemplateHandle>, String>;

    /// 引擎名称
    fn engine_name(&self) -> &str;
}

/// 预编译模板句柄
pub trait CompiledTemplateHandle: Send + Sync {
    fn render(
        &self,
        variables: &HashMap<String, Value>,
    ) -> Result<String, String>;
}
```

### 5.3 Jinja2 模板引擎插件

**Crate：`crates/xworkflow-template-jinja/`**

```
crates/xworkflow-template-jinja/
├── Cargo.toml
└── src/
    ├── lib.rs          # JinjaTemplatePlugin（Bootstrap 插件入口）
    ├── engine.rs       # JinjaTemplateEngine 实现
    └── compiled.rs     # JinjaCompiledTemplate 实现
```

**依赖：**

```toml
[package]
name = "xworkflow-template-jinja"
version = "0.1.0"

[dependencies]
xworkflow-types = { path = "../xworkflow-types" }
minijinja = { version = "2.0", features = ["builtins", "fuel"] }
serde_json = "1.0"
```

**插件实现：**

```rust
pub struct JinjaTemplatePlugin {
    metadata: PluginMetadata,
}

impl JinjaTemplatePlugin {
    pub fn new() -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.template-jinja".into(),
                name: "Jinja2 Template Engine".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                category: PluginCategory::Bootstrap,
                description: "Jinja2 template engine via minijinja".into(),
                source: PluginSource::Host,
                capabilities: None,
            },
        }
    }
}

#[async_trait]
impl Plugin for JinjaTemplatePlugin {
    fn metadata(&self) -> &PluginMetadata { &self.metadata }

    async fn register(
        &self, ctx: &mut PluginContext
    ) -> Result<(), PluginError> {
        let engine = Arc::new(JinjaTemplateEngine::new());
        ctx.register_template_engine(engine)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any { self }
}
```

**迁移内容：**

| 原位置 | 目标位置 | 内容 |
|--------|---------|------|
| `src/template/engine.rs` 中 `render_jinja2*` | `engine.rs` | Jinja2 渲染函数 |
| `src/template/engine.rs` 中 `CompiledTemplate` | `compiled.rs` | 预编译模板 |
| `src/template/engine.rs` 中 `register_template_functions` | `engine.rs` | 插件函数注册 |
| `src/template/engine.rs` 中 `apply_template_safety` | `engine.rs` | 安全配置 |

**保留在核心 `src/template/engine.rs`：**
- `render_template()` — Dify 语法同步渲染
- `render_template_async()` — Dify 语法异步渲染
- `TEMPLATE_RE` — 正则表达式

### 5.4 PluginContext 新增注册方法

```rust
// src/plugin_system/context.rs

/// 注册模板引擎（Bootstrap 阶段）
pub fn register_template_engine(
    &mut self,
    engine: Arc<dyn TemplateEngine>,
) -> Result<(), PluginError> {
    self.ensure_phase(PluginPhase::Bootstrap)?;
    if self.registry_inner.template_engine.is_some() {
        return Err(PluginError::ConflictError(
            "Template engine already registered".into()
        ));
    }
    self.registry_inner.template_engine = Some(engine);
    Ok(())
}

/// 查询已注册的模板引擎（Normal 阶段，供节点消费）
pub fn query_template_engine(&self) -> Option<&Arc<dyn TemplateEngine>> {
    self.registry_inner.template_engine.as_ref()
}
```

**PluginRegistryInner 新增字段：**

```rust
pub(crate) template_engine: Option<Arc<dyn TemplateEngine>>,
```

### 5.5 模板函数注册流程

模板函数由 Normal 阶段插件通过现有 `register_template_function()` 注册。在 Ready 阶段，WorkflowRunner 将收集到的 template functions 传递给使用模板引擎的节点。

```
1. Bootstrap: JinjaTemplatePlugin → register_template_engine(JinjaEngine)
2. Normal:    其他插件 → register_template_function("func_name", func)
3. Ready:     WorkflowRunner 将 template_functions 注入 RuntimeContext
4. 执行时:    节点通过 TemplateEngine.render(..., functions) 传入函数
```

模板函数不直接注入 `TemplateEngine` 实例，而是在每次渲染时作为参数传入，保持引擎的无状态性。

---

## 6. Crate 结构总览

```
crates/
├── xworkflow-types/              # 共享类型（已有）
│   └── 新增：
│       ├── CodeSandbox, SandboxRequest, SandboxResult
│       ├── CodeLanguage, SandboxType, SandboxError
│       ├── ExecutionConfig, HealthStatus, SandboxStats
│       └── TemplateEngine, CompiledTemplateHandle
│
├── xworkflow-sandbox-js/         # JS 沙盒插件（新增）
│   ├── Cargo.toml                # deps: xworkflow-types, boa_engine, boa_parser
│   └── src/
│       ├── lib.rs                # create_js_sandbox_plugins() 工厂函数
│       ├── sandbox.rs            # BuiltinSandbox 实现
│       ├── builtins.rs           # JS 内置函数 (uuid, crypto, datetime...)
│       ├── boot.rs               # JsSandboxBootPlugin (Bootstrap)
│       └── lang.rs               # JsSandboxLangPlugin (Normal) + JsLanguageProvider
│
├── xworkflow-sandbox-wasm/       # WASM 沙盒插件（新增）
│   ├── Cargo.toml                # deps: xworkflow-types, wasmtime, wat
│   └── src/
│       ├── lib.rs                # create_wasm_sandbox_plugins() 工厂函数
│       ├── sandbox.rs            # WasmSandbox 实现
│       ├── boot.rs               # WasmSandboxBootPlugin (Bootstrap)
│       └── lang.rs               # WasmSandboxLangPlugin (Normal) + WasmLanguageProvider
│
├── xworkflow-template-jinja/     # Jinja2 模板引擎插件（新增）
│   ├── Cargo.toml                # deps: xworkflow-types, minijinja
│   └── src/
│       ├── lib.rs                # JinjaTemplatePlugin (Bootstrap)
│       ├── engine.rs             # JinjaTemplateEngine 实现
│       └── compiled.rs           # JinjaCompiledTemplate
│
├── xworkflow-nodes-code/         # Code 节点插件（已规划）
│   └── 定义 LanguageProvider trait + LANG_PROVIDE_KEY
│   └── 消费 LanguageProvider（通过 query_services）
└── xworkflow-nodes-transform/    # Transform 节点插件（已规划）
    └── 消费 TemplateEngine
```

---

## 7. Feature Flag 设计

### 7.1 主 crate 新增 feature

```toml
# Cargo.toml
[features]
default = [
    "security",
    "builtin-sandbox-js",       # 内置 JS 沙盒
    "builtin-sandbox-wasm",     # 内置 WASM 沙盒
    "builtin-template-jinja",   # 内置 Jinja2 引擎
    "builtin-core-nodes",
    "builtin-transform-nodes",
    "builtin-http-node",
    "builtin-code-node",
    "builtin-subgraph-nodes",
    "builtin-llm-node",
]

# 沙盒插件
builtin-sandbox-js = ["dep:xworkflow-sandbox-js"]
builtin-sandbox-wasm = ["dep:xworkflow-sandbox-wasm"]

# 模板引擎插件
builtin-template-jinja = ["dep:xworkflow-template-jinja"]

# code 节点默认依赖 JS 沙盒
builtin-code-node = ["builtin-sandbox-js"]

# transform 节点默认依赖 Jinja2 引擎
builtin-transform-nodes = ["builtin-template-jinja"]

[dependencies]
xworkflow-sandbox-js = { path = "crates/xworkflow-sandbox-js", optional = true }
xworkflow-sandbox-wasm = { path = "crates/xworkflow-sandbox-wasm", optional = true }
xworkflow-template-jinja = { path = "crates/xworkflow-template-jinja", optional = true }
```

### 7.2 零成本抽象

| 配置 | 效果 |
|------|------|
| 禁用 `builtin-sandbox-wasm` | wasmtime (~15MB) 不参与编译 |
| 禁用 `builtin-sandbox-js` | boa_engine (~50KB) 不参与编译 |
| 禁用 `builtin-template-jinja` | minijinja 不参与编译 |
| 全部禁用 | 仅保留核心引擎 + 插件系统框架 |

### 7.3 内置插件注册（scheduler.rs）

```rust
// src/scheduler.rs — WorkflowRunnerBuilder::build()

fn collect_builtin_bootstrap_plugins(&self) -> Vec<Box<dyn Plugin>> {
    let mut plugins: Vec<Box<dyn Plugin>> = Vec::new();

    #[cfg(feature = "builtin-sandbox-js")]
    {
        let (boot, _lang) = xworkflow_sandbox_js::create_js_sandbox_plugins(Default::default());
        plugins.push(Box::new(boot));
        // _lang 存入 normal 列表（见下方）
    }

    #[cfg(feature = "builtin-sandbox-wasm")]
    {
        let (boot, _lang) = xworkflow_sandbox_wasm::create_wasm_sandbox_plugins(Default::default());
        plugins.push(Box::new(boot));
    }

    #[cfg(feature = "builtin-template-jinja")]
    plugins.push(Box::new(
        xworkflow_template_jinja::JinjaTemplatePlugin::new()
    ));

    plugins
}

fn collect_builtin_normal_plugins(&self) -> Vec<Box<dyn Plugin>> {
    let mut plugins: Vec<Box<dyn Plugin>> = Vec::new();

    // lang-provide 插件（需在 code 节点之前注册）
    #[cfg(feature = "builtin-sandbox-js")]
    {
        let (_boot, lang) = xworkflow_sandbox_js::create_js_sandbox_plugins(Default::default());
        plugins.push(Box::new(lang));
    }

    #[cfg(feature = "builtin-sandbox-wasm")]
    {
        let (_boot, lang) = xworkflow_sandbox_wasm::create_wasm_sandbox_plugins(Default::default());
        plugins.push(Box::new(lang));
    }

    // 节点插件（消费 lang-provide）
    // ... code node, transform node 等

    plugins
}
```

> **注意**：实际实现中，`create_*_plugins()` 应只调用一次，Boot 和 Lang 插件分别放入对应列表。上述代码为示意，实际需要在 builder 中统一管理。

---

## 8. 注册流程（完整时序）

```
WorkflowRunnerBuilder::build()
  │
  ├─ 1. Bootstrap 阶段
  │   ├─ [builtin-template-jinja] JinjaTemplatePlugin.register()
  │   │   └─ register_template_engine(JinjaEngine)
  │   ├─ [builtin-sandbox-js]     JsSandboxBootPlugin.register()
  │   │   └─ register_sandbox(JavaScript, BuiltinSandbox)  ← Arc 共享
  │   ├─ [builtin-sandbox-wasm]   WasmSandboxBootPlugin.register()
  │   │   └─ register_sandbox(Wasm, WasmSandbox)           ← Arc 共享
  │   └─ 外部 Bootstrap 插件...
  │
  ├─ 2. Normal 阶段（lang-provide 先注册）
  │   ├─ [builtin-sandbox-js]     JsSandboxLangPlugin.register()
  │   │   └─ register_language_provider(JsProvider)         ← 同一个 Arc
  │   ├─ [builtin-sandbox-wasm]   WasmSandboxLangPlugin.register()
  │   │   └─ register_language_provider(WasmProvider)       ← 同一个 Arc
  │   └─ 外部 Normal 插件（lang-provide 类）...
  │
  ├─ 3. Normal 阶段（节点插件消费）
  │   ├─ [builtin-code-node] CodeNodePlugin.register()
  │   │   ├─ query_language_providers() → [JsProvider, WasmProvider]
  │   │   ├─ SandboxManager::from_providers(providers)
  │   │   └─ register_node_executor("code", CodeNodeExecutor)
  │   ├─ [builtin-transform-nodes] TransformPlugin.register()
  │   │   ├─ query_template_engine() → Some(JinjaEngine)
  │   │   └─ register_node_executor("template-transform", ...)
  │   ├─ 其他内置节点插件...
  │   └─ 外部 Normal 插件...
  │
  └─ 4. Ready：构建 WorkflowDispatcher
      ├─ 合并 node_executors
      ├─ 合并 hooks
      └─ 注入 template_functions 到 RuntimeContext
```

---

## 9. 关键修改文件清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src/plugin_system/extensions.rs` | 修改 | 新增 `TemplateEngine`、`CompiledTemplateHandle` trait |
| `src/plugin_system/context.rs` | 修改 | 新增通用 `provide_service()`、`query_services()`、`register_template_engine()`、`query_template_engine()` |
| `src/plugin_system/registry.rs` | 修改 | `PluginRegistryInner` 新增 `services`、`template_engine` 字段 |
| `src/sandbox/types.rs` | 迁移 | 类型定义移入 `xworkflow-types` |
| `src/sandbox/error.rs` | 迁移 | 错误类型移入 `xworkflow-types` |
| `src/sandbox/builtin.rs` | 迁移 | 移入 `crates/xworkflow-sandbox-js/` |
| `src/sandbox/js_builtins.rs` | 迁移 | 移入 `crates/xworkflow-sandbox-js/` |
| `src/sandbox/wasm_sandbox.rs` | 迁移 | 移入 `crates/xworkflow-sandbox-wasm/` |
| `src/sandbox/manager.rs` | 修改 | 新增 `new_empty()`、`from_providers()`，移除硬编码沙盒 |
| `src/sandbox/mod.rs` | 修改 | 移除具体沙盒导出，保留 manager |
| `src/template/engine.rs` | 修改 | Jinja2 部分迁移到插件 crate，保留 Dify 语法 |
| `src/scheduler.rs` | 修改 | 内置插件注册逻辑，feature-gated |
| `Cargo.toml` | 修改 | 新增 workspace members、feature flags、optional deps |
| `crates/xworkflow-types/` | 修改 | 新增沙盒和模板引擎相关类型 |

---

## 10. 验证方案（实现时参考）

### 10.1 编译测试

```bash
# 全部默认
cargo build

# 仅核心（无沙盒、无模板）
cargo build --no-default-features --features builtin-core-nodes

# 仅 JS 沙盒
cargo build --no-default-features --features "builtin-core-nodes,builtin-sandbox-js,builtin-code-node"

# 仅 WASM 沙盒
cargo build --no-default-features --features "builtin-core-nodes,builtin-sandbox-wasm"
```

### 10.2 单元测试

```bash
cargo test -p xworkflow-sandbox-js
cargo test -p xworkflow-sandbox-wasm
cargo test -p xworkflow-template-jinja
```

### 10.3 集成测试

现有 e2e 测试在默认 feature 下全部通过：

```bash
cargo test --test e2e
```

### 10.4 依赖体积验证

```bash
# 禁用 WASM 沙盒后，wasmtime 不应出现在依赖树中
cargo tree --no-default-features --features builtin-sandbox-js | grep wasmtime
# 预期：无输出
```
