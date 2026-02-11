# xworkflow 四包变体设计方案

## 背景

为发布前做准备，将 xworkflow 拆分为 4 种包变体以适应不同部署场景：

| 变体 | 描述 | 构建方式 |
|------|------|---------|
| **All** | 全功能静态链接（当前默认行为） | `cargo build` |
| **Slim** | 纯引擎核心，无任何内置插件 | `cargo build --no-default-features` |
| **Base** | 除 code 节点外全部插件静态链接（去掉 boa_engine 重依赖） | `cargo build --no-default-features --features base` |
| **Dynamic** | Slim 核心 + 插件自动发现，所有插件编译为独立动态库 | `cargo build --no-default-features --features dynamic` |

通过 Cargo features 实现，不需要独立 binary crate。

---

## 一、Feature Flag 重组

### 1.1 现有 Feature 结构

当前所有插件通过 `default` feature 一次性全部启用，无法按需裁剪：

```
default → security + plugin-system + 所有 builtin-* features
```

### 1.2 新 Feature 结构

**文件**: `Cargo.toml`

```toml
[features]
default = ["all"]

# ── 顶层变体 features ──
all = [
    "security", "plugin-system",
    "builtin-sandbox-js", "builtin-sandbox-wasm", "builtin-template-jinja",
    "builtin-core-nodes", "builtin-transform-nodes", "builtin-http-node",
    "builtin-code-node", "builtin-subgraph-nodes", "builtin-llm-node",
]

base = [
    "security", "plugin-system",
    "builtin-sandbox-wasm", "builtin-template-jinja",
    "builtin-core-nodes", "builtin-transform-nodes", "builtin-http-node",
    "builtin-subgraph-nodes", "builtin-llm-node",
]
# base 不含 builtin-sandbox-js 和 builtin-code-node（省去 boa_engine 重依赖）

dynamic = ["security", "plugin-system", "auto-discover"]
# dynamic 不含任何 builtin-* feature，所有插件通过目录扫描动态加载

# ── 基础能力 features ──
security = ["xworkflow-sandbox-js?/security"]
plugin-system = ["libloading"]
auto-discover = ["plugin-system"]                    # 新增：插件自动发现

# ── 内置插件 features（不变） ──
builtin-sandbox-js = ["dep:xworkflow-sandbox-js", "dep:boa_engine"]
builtin-sandbox-wasm = ["dep:xworkflow-sandbox-wasm"]
builtin-template-jinja = ["dep:xworkflow-template-jinja"]
builtin-core-nodes = []
builtin-transform-nodes = ["builtin-template-jinja"]
builtin-http-node = []
builtin-code-node = ["builtin-sandbox-js"]
builtin-subgraph-nodes = []
builtin-llm-node = []
```

### 1.3 各变体的构建命令

```bash
cargo build                                          # All（默认）
cargo build --no-default-features                    # Slim
cargo build --no-default-features --features base    # Base
cargo build --no-default-features --features dynamic # Dynamic（主程序）

# Dynamic 变体还需编译所有 dylib 插件：
cargo build -p xworkflow-sandbox-js-dylib \
            -p xworkflow-sandbox-wasm-dylib \
            -p xworkflow-template-jinja-dylib \
            -p xworkflow-nodes-core-dylib \
            -p xworkflow-nodes-transform-dylib \
            -p xworkflow-nodes-http-dylib \
            -p xworkflow-nodes-code-dylib \
            -p xworkflow-nodes-subgraph-dylib \
            -p xworkflow-nodes-llm-dylib
```

### 1.4 各变体包含能力对比

| 能力 | All | Base | Dynamic | Slim |
|------|-----|------|---------|------|
| 核心引擎（DSL/Graph/Dispatcher） | ✓ | ✓ | ✓ | ✓ |
| security 模块 | ✓ | ✓ | ✓ | ✗ |
| plugin-system（libloading） | ✓ | ✓ | ✓ | ✗ |
| auto-discover（目录扫描） | ✗ | ✗ | ✓ | ✗ |
| JS Sandbox（boa_engine） | ✓静态 | ✗ | ✓动态 | ✗ |
| WASM Sandbox（wasmtime） | ✓静态 | ✓静态 | ✓动态 | ✗ |
| Jinja 模板引擎 | ✓静态 | ✓静态 | ✓动态 | ✗ |
| Core Nodes | ✓静态 | ✓静态 | ✓动态 | ✗ |
| Transform Nodes | ✓静态 | ✓静态 | ✓动态 | ✗ |
| HTTP Node | ✓静态 | ✓静态 | ✓动态 | ✗ |
| Code Node | ✓静态 | ✗ | ✓动态 | ✗ |
| Subgraph Nodes | ✓静态 | ✓静态 | ✓动态 | ✗ |
| LLM Node | ✓静态 | ✓静态 | ✓动态 | ✗ |

---

## 二、PluginLoader Trait 签名变更（支持多插件 Dylib）

### 2.1 问题

JS Sandbox 和 WASM Sandbox 各需要导出 **两个** 插件：

- **BootPlugin**（Bootstrap 阶段）：注册 sandbox 基础设施
- **LangPlugin**（Normal 阶段）：注册 LanguageProvider 服务

两个插件共享同一个 `Arc<BuiltinSandbox>` 实例。当前 dylib ABI 只支持单插件导出（`xworkflow_rust_plugin_create() -> Box<dyn Plugin>`），无法满足需求。

### 2.2 方案：多插件导出协议

在现有 Rust ABI 基础上新增 `xworkflow_rust_plugin_create_all` 导出符号：

```
单插件 dylib（现有，不变）：
  XWORKFLOW_RUST_PLUGIN: bool = true
  xworkflow_rust_plugin_create() -> Box<dyn Plugin>

多插件 dylib（新增）：
  XWORKFLOW_RUST_PLUGIN: bool = true
  xworkflow_rust_plugin_create_all() -> Vec<Box<dyn Plugin>>
```

DllPluginLoader 加载逻辑：**优先检测 `_create_all`，回退到 `_create`**，兼容现有全部 dylib。

### 2.3 PluginLoader::load 返回类型变更

**文件**: `src/plugin_system/loader.rs`

```rust
// 变更前
async fn load(&self, source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError>;

// 变更后
async fn load(&self, source: &PluginLoadSource) -> Result<Vec<Box<dyn Plugin>>, PluginError>;
```

无需向后兼容（未发布第一版）。

### 2.4 DllPluginLoader 变更

**文件**: `src/plugin_system/loaders/dll_loader.rs`

#### a) `RustDllPluginWrapper._library` 改为 `Arc<Library>`

多个 Plugin 对象来自同一个 dylib 时，需共享 Library 生命周期：

```rust
struct RustDllPluginWrapper {
    inner: Box<dyn Plugin>,
    _library: Arc<Library>,   // 原为 Library，改为 Arc<Library>
}
```

#### b) `load_rust_abi` 实现多插件检测

```rust
unsafe fn load_rust_abi(&self, library: Library) -> Result<Vec<Box<dyn Plugin>>, PluginError> {
    let lib = Arc::new(library);

    // 优先多插件导出
    if let Ok(create_all_fn) = lib.get::<fn() -> Vec<Box<dyn Plugin>>>(
        b"xworkflow_rust_plugin_create_all\0"
    ) {
        let plugins = create_all_fn();
        return Ok(plugins.into_iter().map(|inner| -> Box<dyn Plugin> {
            Box::new(RustDllPluginWrapper { inner, _library: lib.clone() })
        }).collect());
    }

    // 回退单插件
    let create_fn = lib.get::<fn() -> Box<dyn Plugin>>(
        b"xworkflow_rust_plugin_create\0"
    ).map_err(|e| PluginError::MissingExport(e.to_string()))?;
    let inner = create_fn();
    Ok(vec![Box::new(RustDllPluginWrapper { inner, _library: lib })])
}
```

#### c) `load_c_abi` 包装为 Vec

```rust
unsafe fn load_c_abi(&self, library: Library) -> Result<Vec<Box<dyn Plugin>>, PluginError> {
    // ... 原有逻辑不变 ...
    Ok(vec![Box::new(CPluginWrapper { ... })])
}
```

### 2.5 Registry 适配

**文件**: `src/plugin_system/registry.rs`

`run_bootstrap_phase` 和 `run_normal_phase` 中遍历 Vec 注册：

```rust
// 变更前
let plugin = loader.load(&source).await?;
self.register_plugin(plugin, PluginPhase::Bootstrap).await?;

// 变更后
let plugins = loader.load(&source).await?;
for plugin in plugins {
    self.register_plugin(plugin, expected_phase).await?;
}
```

### 2.6 其他 Loader 适配

| Loader | 文件 | 变更 |
|--------|------|------|
| HostPluginLoader | `src/plugin_system/loaders/host_loader.rs` | 返回类型改为 `Vec<...>` |
| WasmPluginLoader | `src/plugin_system/builtins/wasm_bootstrap.rs` | 返回 `Ok(vec![plugin])` |

### 2.7 新增宏 `xworkflow_declare_rust_plugins!`

**文件**: `src/plugin_system/macros.rs`

```rust
/// 声明一个导出多个 Rust ABI 插件的动态库。
/// $create_fn 应为一个返回 Vec<Box<dyn Plugin>> 的表达式。
#[macro_export]
macro_rules! xworkflow_declare_rust_plugins {
    ($create_fn:expr) => {
        #[no_mangle]
        pub static XWORKFLOW_RUST_PLUGIN: bool = true;

        #[no_mangle]
        pub fn xworkflow_rust_plugin_create_all() -> Vec<Box<dyn $crate::plugin_system::Plugin>> {
            $create_fn
        }
    };
}
```

保留现有 `xworkflow_declare_rust_plugin!`（单数）和 `xworkflow_declare_plugin!`（C ABI）不变。

---

## 三、插件自动发现机制

### 3.1 设计目标

Dynamic 变体下，主程序启动时自动扫描指定目录中的动态库文件，加载所有 xworkflow 插件，按 `metadata().category` 分类后分别送入 Bootstrap 和 Normal 阶段。

用户也可通过配置文件指定插件目录。

### 3.2 PluginSystemConfig 扩展

**文件**: `src/plugin_system/config.rs`

```rust
#[derive(Debug, Clone, Default)]
pub struct PluginSystemConfig {
    // 现有字段保持不变
    pub bootstrap_dll_paths: Vec<PathBuf>,
    pub normal_dll_paths: Vec<PathBuf>,
    pub normal_load_sources: Vec<PluginLoadSource>,

    // ── 新增 ──
    /// 插件搜索目录列表。启用 auto-discover 时，扫描这些目录中的动态库文件。
    pub plugin_dirs: Vec<PathBuf>,

    /// 是否启用自动发现。为 true 时扫描 plugin_dirs 中的目录。
    pub auto_discover: bool,
}
```

### 3.3 新增 discovery 模块

**新建文件**: `src/plugin_system/discovery.rs`

仅在 `auto-discover` feature 下编译：`#[cfg(feature = "auto-discover")]`

#### 核心函数

```rust
/// 扫描指定目录列表，返回所有匹配的 DLL PluginLoadSource。
/// 文件名过滤规则：扩展名为 .dll/.so/.dylib，且以 xworkflow/libxworkflow 开头。
pub fn discover_plugin_sources(
    dirs: &[PathBuf],
) -> Result<Vec<PluginLoadSource>, PluginError>

/// 将已加载的插件按 metadata().category 分类为 (bootstrap, normal) 两组。
pub fn classify_plugins(
    plugins: Vec<Box<dyn Plugin>>,
) -> (Vec<Box<dyn Plugin>>, Vec<Box<dyn Plugin>>)
```

#### 文件名过滤规则

```rust
fn is_shared_library(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("dll") | Some("so") | Some("dylib")
    )
}

fn is_xworkflow_plugin(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|name| {
            name.starts_with("xworkflow_")
                || name.starts_with("xworkflow-")
                || name.starts_with("libxworkflow_")
                || name.starts_with("libxworkflow-")
        })
        .unwrap_or(false)
}
```

#### 分类逻辑

```rust
pub fn classify_plugins(
    plugins: Vec<Box<dyn Plugin>>,
) -> (Vec<Box<dyn Plugin>>, Vec<Box<dyn Plugin>>) {
    let mut bootstrap = Vec::new();
    let mut normal = Vec::new();
    for plugin in plugins {
        match plugin.metadata().category {
            PluginCategory::Bootstrap => bootstrap.push(plugin),
            PluginCategory::Normal => normal.push(plugin),
        }
    }
    (bootstrap, normal)
}
```

**模块注册**: `src/plugin_system/mod.rs` 新增：

```rust
#[cfg(feature = "auto-discover")]
pub mod discovery;
```

### 3.4 scheduler.rs 集成

**文件**: `src/scheduler.rs`

在 `init_plugins()` 方法中，**"收集内置插件"之后、"run_bootstrap_phase"之前**插入自动发现逻辑：

```rust
// ── 自动发现（auto-discover feature 下生效） ──
#[cfg(feature = "auto-discover")]
if let Some(config) = &self.plugin_system_config {
    if config.auto_discover && !config.plugin_dirs.is_empty() {
        let sources = crate::plugin_system::discovery::discover_plugin_sources(
            &config.plugin_dirs,
        )?;
        let loader = DllPluginLoader::new();
        for source in &sources {
            let plugins = loader.load(source).await?;
            let (boot, normal) = crate::plugin_system::discovery::classify_plugins(plugins);
            bootstrap_plugins.extend(boot);
            self.host_normal_plugins.extend(normal);
        }
    }
}
```

设计要点：
- 先加载全部 DLL → 按 category 分类 → 再分别送入两个 phase
- 无需修改 `register_plugin` 的 phase 校验逻辑（分类已在外层完成）
- 手动指定的 `bootstrap_dll_paths` / `normal_dll_paths` 仍然保留，与自动发现共存

### 3.5 init_plugins 触发条件修改

**文件**: `src/scheduler.rs`

当前触发条件不会在 dynamic 模式（有 config 但无 builtin）下触发内置插件注册。需扩展：

```rust
let should_init = builder.plugin_system_config.is_some()
    || !builder.host_bootstrap_plugins.is_empty()
    || !builder.host_normal_plugins.is_empty()
    || cfg!(any(
        feature = "builtin-sandbox-js",
        feature = "builtin-sandbox-wasm",
        feature = "builtin-template-jinja",
    ));
```

---

## 四、新增 3 个 Dylib Crate

### 4.1 代码复制问题与方案

**问题**：当前 Plugin trait 定义在 xworkflow 主 crate 的 `plugin_system` 模块中。Sandbox/template 的 Plugin 实现（`JsSandboxBootPlugin` 等）也在主 crate 的 `builtins/` 模块下，gated by `builtin-*` feature。

Dylib crate 依赖 `xworkflow` 但**不启用** `builtin-*` feature（否则就静态链接了），无法引用 builtins 模块中的 Plugin 实现。

将 Plugin trait 移到 `xworkflow-types` 会导致循环依赖：
```
xworkflow-sandbox-js → xworkflow-types（Plugin trait）→ 但 PluginContext 在 xworkflow 中
```

**最终方案**：在 dylib crate 中**直接复制** Plugin 实现代码。

理由：
- 每个文件仅 50~140 行，代码量小
- Plugin 实现逻辑稳定，不会频繁变动
- 避免了循环依赖和 trait 迁移的架构代价
- 与现有 6 个 node dylib 的模式完全一致

### 4.2 xworkflow-sandbox-js-dylib

**新建目录**: `crates/xworkflow-sandbox-js-dylib/`

**Cargo.toml**:
```toml
[package]
name = "xworkflow-sandbox-js-dylib"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]
test = false
doctest = false
bench = false

[dependencies]
async-trait = "0.1"
xworkflow = { path = "../..", features = ["plugin-system"] }
xworkflow-sandbox-js = { path = "../xworkflow-sandbox-js" }
xworkflow-types = { path = "../xworkflow-types" }
```

**src/lib.rs** — 复制 `src/plugin_system/builtins/sandbox_js.rs` 的完整 Plugin 实现（约 140 行），使用多插件导出：

```rust
use xworkflow::plugin_system::{Plugin, PluginCategory, PluginContext, PluginError, PluginMetadata, PluginSource};
use xworkflow_sandbox_js::{BuiltinSandbox, BuiltinSandboxConfig};
// ... JsSandboxBootPlugin, JsSandboxLangPlugin, JsLanguageProvider 定义 ...
// （与 builtins/sandbox_js.rs 相同，PluginSource 改为 Dll）

#[no_mangle]
pub static XWORKFLOW_RUST_PLUGIN: bool = true;

#[no_mangle]
pub fn xworkflow_rust_plugin_create_all() -> Vec<Box<dyn Plugin>> {
    let (boot, lang) = create_js_sandbox_plugins(BuiltinSandboxConfig::default());
    vec![Box::new(boot), Box::new(lang)]
}
```

### 4.3 xworkflow-sandbox-wasm-dylib

**新建目录**: `crates/xworkflow-sandbox-wasm-dylib/`

同上模式，复制 `builtins/sandbox_wasm.rs`（约 140 行），使用多插件导出返回 [BootPlugin, LangPlugin]。

**Cargo.toml 依赖**:
```toml
xworkflow = { path = "../..", features = ["plugin-system"] }
xworkflow-sandbox-wasm = { path = "../xworkflow-sandbox-wasm" }
xworkflow-types = { path = "../xworkflow-types" }
```

### 4.4 xworkflow-template-jinja-dylib

**新建目录**: `crates/xworkflow-template-jinja-dylib/`

复制 `builtins/template_jinja.rs`（约 44 行），使用标准**单插件**导出：

```rust
#[no_mangle]
pub static XWORKFLOW_RUST_PLUGIN: bool = true;

#[no_mangle]
pub fn xworkflow_rust_plugin_create() -> Box<dyn Plugin> {
    Box::new(JinjaTemplatePlugin::new())
}
```

**Cargo.toml 依赖**:
```toml
xworkflow = { path = "../..", features = ["plugin-system"] }
xworkflow-template-jinja = { path = "../xworkflow-template-jinja" }
```

### 4.5 Workspace 成员更新

**文件**: `Cargo.toml` [workspace] members 新增：

```toml
"crates/xworkflow-sandbox-js-dylib",
"crates/xworkflow-sandbox-wasm-dylib",
"crates/xworkflow-template-jinja-dylib",
```

---

## 五、关键文件清单

| 文件 | 操作 | 说明 |
|------|------|------|
| `Cargo.toml` | 修改 | features 重组（`all`/`base`/`dynamic`/`auto-discover`）+ workspace members |
| `src/plugin_system/loader.rs` | 修改 | `load()` 返回 `Vec<Box<dyn Plugin>>` |
| `src/plugin_system/loaders/dll_loader.rs` | 修改 | 多插件检测 + `Arc<Library>` + `_create_all` 回退逻辑 |
| `src/plugin_system/loaders/host_loader.rs` | 修改 | 返回类型适配 |
| `src/plugin_system/builtins/wasm_bootstrap.rs` | 修改 | `WasmPluginLoader::load` 返回类型适配 |
| `src/plugin_system/registry.rs` | 修改 | 遍历 Vec 注册 |
| `src/plugin_system/macros.rs` | 修改 | 新增 `xworkflow_declare_rust_plugins!` 宏 |
| `src/plugin_system/config.rs` | 修改 | 新增 `plugin_dirs` + `auto_discover` 字段 |
| `src/plugin_system/mod.rs` | 修改 | 新增 `discovery` 模块声明 |
| `src/scheduler.rs` | 修改 | 自动发现逻辑 + `init_plugins` 触发条件 |
| `src/plugin_system/discovery.rs` | **新建** | 目录扫描 + 插件分类 |
| `crates/xworkflow-sandbox-js-dylib/` | **新建** | JS sandbox dylib（2 文件） |
| `crates/xworkflow-sandbox-wasm-dylib/` | **新建** | WASM sandbox dylib（2 文件） |
| `crates/xworkflow-template-jinja-dylib/` | **新建** | Jinja template dylib（2 文件） |

---

## 六、实施顺序

1. **PluginLoader trait 签名变更** — `loader.rs` → `dll_loader.rs` → `host_loader.rs` → `wasm_bootstrap.rs` → `registry.rs`
2. **新增多插件宏** — `macros.rs`
3. **Feature flag 重组** — `Cargo.toml`（`all`、`base`、`dynamic`、`auto-discover`）
4. **PluginSystemConfig 扩展** — `config.rs`
5. **新建 discovery 模块** — `discovery.rs` + `mod.rs`
6. **scheduler.rs 集成** — auto-discover 逻辑 + init 触发条件
7. **新建 3 个 dylib crate** — sandbox-js-dylib、sandbox-wasm-dylib、template-jinja-dylib + workspace members
8. **编译验证** — 4 种变体 + 所有 dylib

---

## 七、边界情况与注意事项

### 7.1 Rust ABI 兼容性

动态加载 Rust dylib 要求所有 crate 使用**完全相同的编译器版本**编译（`#[repr(Rust)]` 布局不稳定）。这是已知的 Rust 生态限制，当前设计已接受此约束（同一 workspace、同一编译器）。

### 7.2 DLL 重复加载防护

自动发现和手动指定（`bootstrap_dll_paths` / `normal_dll_paths`）可能导致同一 DLL 被加载两次。应在 `init_plugins` 中对发现的路径做 canonicalize 去重，并排除已在手动路径中出现的条目。

### 7.3 Security Feature 在 Dynamic 模式下的行为

`security` feature 定义为 `["xworkflow-sandbox-js?/security"]`，其中 `?` 表示条件传递。在 dynamic 模式下 sandbox-js 不是静态依赖，该传递不生效。`security` feature 仅控制主 crate 的 `pub mod security` 模块编译。

对于动态加载的 sandbox-js dylib，其自身的 security 支持需在 dylib 的 Cargo.toml 中单独启用：

```toml
# crates/xworkflow-sandbox-js-dylib/Cargo.toml
[dependencies]
xworkflow-sandbox-js = { path = "../xworkflow-sandbox-js", features = ["security"] }
```

### 7.4 wasmtime 依赖

当前 `wasmtime` / `wasmtime-wasi` / `wat` 在根 Cargo.toml 中是**非 optional** 依赖，即使 Slim 模式也会编译。如需进一步瘦身，后续可将其改为 optional 并 gate 到 `builtin-sandbox-wasm` feature 下。本次不在范围内。

### 7.5 Slim 模式下的 NodeExecutorRegistry

Slim 模式下所有 `builtin-*` feature 关闭，`NodeExecutorRegistry::with_builtins()` 中的所有 `#[cfg]` 块跳过，仅注册 stub executor。用户需通过自定义 `NodeExecutorRegistry` 或插件系统手动注册执行器。这是预期行为。

---

## 八、验证方式

```bash
# 1. All 变体编译
cargo build

# 2. Slim 变体编译
cargo build --no-default-features

# 3. Base 变体编译
cargo build --no-default-features --features base

# 4. Dynamic 变体编译
cargo build --no-default-features --features dynamic

# 5. 所有 dylib 编译
cargo build \
    -p xworkflow-sandbox-js-dylib \
    -p xworkflow-sandbox-wasm-dylib \
    -p xworkflow-template-jinja-dylib \
    -p xworkflow-nodes-core-dylib \
    -p xworkflow-nodes-transform-dylib \
    -p xworkflow-nodes-http-dylib \
    -p xworkflow-nodes-code-dylib \
    -p xworkflow-nodes-subgraph-dylib \
    -p xworkflow-nodes-llm-dylib

# 6. 全 workspace 测试
cargo test --workspace

# 7. Dynamic 集成测试（手动验证）
# 编译 dynamic 主程序 + 全部 dylib → 将 dylib 放入同目录 → 验证自动加载
```
