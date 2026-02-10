# xworkflow 通用插件系统设计文档

## 一、概述

### 1.1 背景

xworkflow 当前已有一套基于 WASM 的插件系统（位于 `src/plugin/`），通过 `PluginManager` 管理 WASM 插件的发现、加载和执行。但该系统存在以下局限：

- **仅支持 WASM 单一加载方式**：无法加载原生动态库（DLL/SO/dylib）、宿主端 Rust 代码注入、远程网络插件
- **无分阶段生命周期**：所有插件统一加载，无法在引擎初始化早期注册基础设施级扩展（如自定义沙箱、自定义加载器）
- **注册点有限**：仅支持自定义节点类型和 5 种 Hook，无法扩展 LLM Provider、模板函数、DSL 校验器等
- **加载器不可扩展**：加载逻辑硬编码在 `PluginManager` 中，无法由第三方注册新的加载方式
- **非零成本**：无论是否使用插件，引擎始终携带 `Option<Arc<PluginManager>>` 分支检查开销

### 1.2 设计目标

1. **两阶段生命周期**：Bootstrap 阶段 + Normal 阶段，Bootstrap 插件优先加载，可注册基础设施扩展
2. **两种内置加载方式**：DLL 加载（双模式 C ABI + Rust trait）、宿主端注入。WASM、网络等加载方式均通过 Bootstrap 插件注册
3. **加载器可扩展**：Bootstrap 插件可注册新的 `PluginLoader`（如 WASM 加载器、Python 插件加载器等）
4. **丰富的注册点**：覆盖节点类型、LLM Provider、沙箱、Hook、模板函数、DSL 校验器
5. **统一接口**：所有插件无论加载方式，都实现统一的 `Plugin` trait
6. **零成本抽象**：通过 Cargo feature gate 实现编译期零成本——不启用插件 feature 时，无任何运行时开销

### 1.3 设计原则

- **显式优于隐式**：插件通过 `PluginContext` 的方法显式注册扩展，而非魔法约定
- **最小权限**：Bootstrap 插件和 Normal 插件拥有不同的注册权限，`PluginContext` 在不同阶段暴露不同方法
- **安全隔离**：DLL/Host 插件为完全信任（原生代码），WASM/网络等由各自加载器负责隔离
- **编译期零成本**：插件系统整体位于 `feature = "plugin-system"` 门控之后，不启用时零编译产物、零运行时开销；启用但无插件注册时，仅有空 Vec/HashMap 和一次 `is_empty()` 检查

---

## 二、零成本抽象策略

### 2.1 Feature Gate

整个插件系统通过 Cargo feature 控制：

```toml
[features]
default = []
plugin-system = ["libloading"]
```

**不启用 `plugin-system` 时**：

- `src/plugin_system/` 模块完全不编译
- `WorkflowRunnerBuilder` 不包含任何插件相关字段
- `WorkflowDispatcher` 不包含 `plugin_registry` 字段
- 所有 Hook 调用点不存在——零分支、零函数调用
- `NodeExecutorRegistry` / `LlmProviderRegistry` / `SandboxManager` 无 `apply_plugin_*` 方法

**启用 `plugin-system` 但无插件注册时**：

- `PluginRegistry` 内部为空 HashMap/Vec
- Hook 分发路径：`if handlers.is_empty() { return Ok(vec![]); }` — 一次 len 判断即返回
- `apply_plugin_*` 方法遍历空集合 — 编译器优化为 noop

### 2.2 条件编译模式

```rust
// src/scheduler.rs
pub struct WorkflowRunnerBuilder {
    schema: WorkflowSchema,
    user_inputs: HashMap<String, Value>,
    // ... 基础字段 ...

    #[cfg(feature = "plugin-system")]
    plugin_system_config: Option<PluginSystemConfig>,
    #[cfg(feature = "plugin-system")]
    host_bootstrap_plugins: Vec<Box<dyn Plugin>>,
    #[cfg(feature = "plugin-system")]
    host_normal_plugins: Vec<Box<dyn Plugin>>,
    #[cfg(feature = "plugin-system")]
    plugin_registry: Option<Arc<PluginRegistry>>,

    llm_provider_registry: Option<Arc<LlmProviderRegistry>>,
    debug_config: Option<DebugConfig>,
}

// src/core/dispatcher.rs
pub struct WorkflowDispatcher<G, H> {
    graph: Arc<RwLock<Graph>>,
    variable_pool: Arc<RwLock<VariablePool>>,
    registry: Arc<NodeExecutorRegistry>,
    event_tx: mpsc::Sender<GraphEngineEvent>,
    config: EngineConfig,
    // ...

    #[cfg(feature = "plugin-system")]
    plugin_registry: Option<Arc<PluginRegistry>>,

    debug_gate: G,
    debug_hook: H,
}
```

### 2.3 Hook 调用的零成本模式

```rust
impl<G: DebugGate, H: DebugHook> WorkflowDispatcher<G, H> {
    #[cfg(feature = "plugin-system")]
    async fn execute_hooks(
        &self,
        hook_point: HookPoint,
        data: Value,
    ) -> Result<Vec<Value>, WorkflowError> {
        // ... 实际 Hook 分发 ...
    }

    #[cfg(not(feature = "plugin-system"))]
    #[inline(always)]
    async fn execute_hooks(
        &self,
        _hook_point: HookPoint,
        _data: Value,
    ) -> Result<Vec<Value>, WorkflowError> {
        Ok(vec![])  // 编译器内联并消除
    }
}
```

**效果**：不启用 feature 时，所有 `self.execute_hooks(...)` 调用被编译器内联为空操作（zero-cost）。启用 feature 时，Hook 列表为空则快速返回（near-zero-cost）。

### 2.4 Registry apply 方法的零成本模式

```rust
impl NodeExecutorRegistry {
    #[cfg(feature = "plugin-system")]
    pub fn apply_plugin_executors(
        &mut self,
        executors: HashMap<String, Box<dyn NodeExecutor>>,
    ) {
        for (node_type, executor) in executors {
            self.register(&node_type, executor);
        }
    }
}
```

不启用 feature 时，此方法不存在，不占用任何编译产物。

---

## 三、核心抽象

### 3.1 Plugin trait（统一插件接口）

所有插件无论通过何种方式加载，最终都必须实现此 trait：

```rust
use std::any::Any;

/// 插件类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginCategory {
    /// Bootstrap 阶段插件：注册基础设施（沙箱、加载器等）
    Bootstrap,
    /// Normal 阶段插件：注册业务扩展（节点、Provider、Hook 等）
    Normal,
}

/// 插件元数据
#[derive(Debug, Clone)]
pub struct PluginMetadata {
    /// 插件唯一标识（推荐 reverse-domain 风格，如 "com.example.my-sandbox"）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 版本号（semver 格式）
    pub version: String,
    /// 插件类别
    pub category: PluginCategory,
    /// 描述
    pub description: String,
    /// 来源信息
    pub source: PluginSource,
}

/// 插件来源标识
#[derive(Debug, Clone)]
pub enum PluginSource {
    /// 动态库加载
    Dll { path: PathBuf },
    /// 宿主端注入
    Host,
    /// 自定义加载器
    Custom { loader_type: String, detail: String },
}

/// 统一插件接口
///
/// 所有插件实现此 trait。注册逻辑在 register() 中通过 PluginContext 完成。
#[async_trait]
pub trait Plugin: Send + Sync {
    /// 返回插件元数据
    fn metadata(&self) -> &PluginMetadata;

    /// 初始化插件并注册扩展
    ///
    /// Bootstrap 插件收到的 context 拥有 Bootstrap 阶段注册权限，
    /// Normal 插件收到的 context 拥有 Normal 阶段注册权限。
    async fn register(&self, context: &mut PluginContext) -> Result<(), PluginError>;

    /// 插件关闭前的清理（可选覆写）
    async fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }

    /// 向下转型支持（用于宿主端与特定插件交互）
    fn as_any(&self) -> &dyn Any;
}
```

### 3.2 PluginLoader trait（插件加载器）

抽象「如何将外部资源转换为 Plugin 实例」：

```rust
/// 插件加载源描述
#[derive(Debug, Clone)]
pub struct PluginLoadSource {
    /// 加载器类型标识（如 "dll", "host", "wasm", "network", "python" 等）
    pub loader_type: String,
    /// 加载参数（路径、URL 等）
    pub params: HashMap<String, String>,
}

/// 插件加载器 trait
///
/// 每种加载方式实现此 trait。
/// 内置两种：DllPluginLoader、HostPluginLoader
/// Bootstrap 插件可注册自定义 PluginLoader（如 WasmPluginLoader、PythonPluginLoader 等）
#[async_trait]
pub trait PluginLoader: Send + Sync {
    /// 加载器类型标识
    fn loader_type(&self) -> &str;

    /// 从指定源加载插件
    async fn load(&self, source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError>;

    /// 检查是否能处理给定的源（用于自动发现）
    fn can_load(&self, source: &PluginLoadSource) -> bool {
        source.loader_type == self.loader_type()
    }
}
```

内置加载器一览：

| 加载器 | `loader_type` | 说明 |
|---|---|---|
| `DllPluginLoader` | `"dll"` | 通过 `libloading` 加载 .dll/.so/.dylib，支持 C ABI 和 Rust ABI 双模式 |
| `HostPluginLoader` | `"host"` | 宿主端直接提供 `Box<dyn Plugin>` |

其他加载方式（WASM、Network、Python 等）均通过 Bootstrap 插件注册对应的 `PluginLoader` 来获得。

### 3.3 PluginContext（阶段感知注册上下文）

传递给 `Plugin::register()` 的上下文对象，不同阶段暴露不同的注册方法：

```rust
/// 插件上下文
///
/// 不同阶段暴露不同的注册方法。
/// Bootstrap 阶段可调用 register_sandbox / register_plugin_loader 等。
/// Normal 阶段可调用 register_node_executor / register_llm_provider 等。
pub struct PluginContext<'a> {
    phase: PluginPhase,
    registry_inner: &'a mut PluginRegistryInner,
    plugin_id: String,
}

impl<'a> PluginContext<'a> {
    // ==== Bootstrap 阶段专属方法 ====

    /// 注册新的代码沙箱实现
    /// Bootstrap 阶段外调用返回 PluginError::WrongPhase
    pub fn register_sandbox(
        &mut self,
        language: CodeLanguage,
        sandbox: Arc<dyn CodeSandbox>,
    ) -> Result<(), PluginError>;

    /// 注册新的插件加载器
    /// Bootstrap 阶段外调用返回 PluginError::WrongPhase
    pub fn register_plugin_loader(
        &mut self,
        loader: Arc<dyn PluginLoader>,
    ) -> Result<(), PluginError>;

    /// 注册自定义 TimeProvider（替换默认实现）
    pub fn register_time_provider(
        &mut self,
        provider: Arc<dyn TimeProvider>,
    ) -> Result<(), PluginError>;

    /// 注册自定义 IdGenerator（替换默认实现）
    pub fn register_id_generator(
        &mut self,
        generator: Arc<dyn IdGenerator>,
    ) -> Result<(), PluginError>;

    // ==== Normal 阶段专属方法 ====

    /// 注册自定义节点执行器
    /// Normal 阶段外调用返回 PluginError::WrongPhase
    pub fn register_node_executor(
        &mut self,
        node_type: &str,
        executor: Box<dyn NodeExecutor>,
    ) -> Result<(), PluginError>;

    /// 注册 LLM Provider
    pub fn register_llm_provider(
        &mut self,
        provider: Arc<dyn LlmProvider>,
    ) -> Result<(), PluginError>;

    /// 注册 Hook 处理器
    pub fn register_hook(
        &mut self,
        hook_point: HookPoint,
        handler: Arc<dyn HookHandler>,
    ) -> Result<(), PluginError>;

    /// 注册模板函数（扩展 Jinja2 模板引擎）
    pub fn register_template_function(
        &mut self,
        name: &str,
        func: Arc<dyn TemplateFunction>,
    ) -> Result<(), PluginError>;

    /// 注册 DSL 校验器
    pub fn register_dsl_validator(
        &mut self,
        validator: Arc<dyn DslValidator>,
    ) -> Result<(), PluginError>;

    // ==== 通用方法（两个阶段都可调用） ====

    /// 获取当前阶段
    pub fn phase(&self) -> PluginPhase;

    /// 获取当前插件 ID
    pub fn plugin_id(&self) -> &str;

    /// 记录日志
    pub fn log(&self, level: tracing::Level, message: &str);
}
```

### 3.4 PluginRegistry（中央注册中心）

管理所有已加载插件和加载器，收集插件注册的扩展：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginPhase {
    Uninitialized,
    Bootstrap,
    Normal,
    Ready,
}

pub struct PluginRegistry {
    /// 所有已加载的插件
    plugins: HashMap<String, Arc<dyn Plugin>>,
    /// 加载顺序
    bootstrap_plugin_ids: Vec<String>,
    normal_plugin_ids: Vec<String>,
    /// 已注册的加载器
    loaders: HashMap<String, Arc<dyn PluginLoader>>,
    /// 当前阶段
    phase: PluginPhase,

    // 从插件收集的注册内容
    inner: PluginRegistryInner,
}

struct PluginRegistryInner {
    node_executors: HashMap<String, Box<dyn NodeExecutor>>,
    llm_providers: Vec<Arc<dyn LlmProvider>>,
    sandboxes: Vec<(CodeLanguage, Arc<dyn CodeSandbox>)>,
    hooks: HashMap<HookPoint, Vec<Arc<dyn HookHandler>>>,
    template_functions: HashMap<String, Arc<dyn TemplateFunction>>,
    dsl_validators: Vec<Arc<dyn DslValidator>>,
    custom_time_provider: Option<Arc<dyn TimeProvider>>,
    custom_id_generator: Option<Arc<dyn IdGenerator>>,
}

impl PluginRegistry {
    pub fn new() -> Self;

    /// 注册加载器
    pub fn register_loader(&mut self, loader: Arc<dyn PluginLoader>);

    /// 执行 Bootstrap 阶段
    pub async fn run_bootstrap_phase(
        &mut self,
        bootstrap_sources: Vec<PluginLoadSource>,
        host_bootstrap_plugins: Vec<Box<dyn Plugin>>,
    ) -> Result<(), PluginError>;

    /// 执行 Normal 阶段
    pub async fn run_normal_phase(
        &mut self,
        normal_sources: Vec<PluginLoadSource>,
        host_normal_plugins: Vec<Box<dyn Plugin>>,
    ) -> Result<(), PluginError>;

    // 消费式取出收集的扩展
    pub fn take_node_executors(&mut self) -> HashMap<String, Box<dyn NodeExecutor>>;
    pub fn llm_providers(&self) -> &[Arc<dyn LlmProvider>];
    pub fn sandboxes(&self) -> &[(CodeLanguage, Arc<dyn CodeSandbox>)];
    pub fn hooks(&self, point: &HookPoint) -> Vec<Arc<dyn HookHandler>>;
    pub fn template_functions(&self) -> &HashMap<String, Arc<dyn TemplateFunction>>;
    pub fn dsl_validators(&self) -> &[Arc<dyn DslValidator>];
    pub fn custom_time_provider(&self) -> Option<Arc<dyn TimeProvider>>;
    pub fn custom_id_generator(&self) -> Option<Arc<dyn IdGenerator>>;

    /// 关闭所有插件（逆序 shutdown）
    pub async fn shutdown_all(&mut self) -> Result<(), PluginError>;
}
```

---

## 四、Bootstrap 阶段

### 4.1 Bootstrap 插件注册点

Bootstrap 插件在引擎初始化的最早期运行，用于注册**基础设施级**扩展：

| 注册方法 | 目标 | 典型用途 |
|---|---|---|
| `register_sandbox(language, sandbox)` | `SandboxManager` | Python 沙箱、Lua 沙箱、远程沙箱 |
| `register_plugin_loader(loader)` | `PluginRegistry.loaders` | WASM 加载器、Python 插件加载器、Docker 容器加载器 |
| `register_time_provider(provider)` | `RuntimeContext` | 自定义时间源（分布式时钟等） |
| `register_id_generator(generator)` | `RuntimeContext` | 自定义 ID 策略（Snowflake 等） |

**关键约束**：

- Bootstrap 插件**只能通过 DLL 和 Host 两种方式加载**（此时自定义加载器尚未注册）
- Bootstrap 插件调用 Normal 阶段方法（如 `register_node_executor`）返回 `PluginError::WrongPhase`
- Bootstrap 插件加载顺序由配置声明顺序决定

### 4.2 Bootstrap 阶段执行流程

```
PluginRegistry::run_bootstrap_phase()
│
├─ 1. phase = Bootstrap
│
├─ 2. 处理 host_bootstrap_plugins（宿主端注入的 Bootstrap 插件）
│     for plugin in host_bootstrap_plugins:
│       assert!(plugin.metadata().category == Bootstrap)
│       ctx = PluginContext::new(phase=Bootstrap, &mut inner, plugin.id)
│       plugin.register(&mut ctx)?
│       plugins.insert(id, plugin)
│       bootstrap_plugin_ids.push(id)
│
├─ 3. 处理 bootstrap_sources（DLL Bootstrap 插件）
│     for source in bootstrap_sources:
│       loader = loaders.get(source.loader_type)?  // 此时只有 Dll 和 Host
│       plugin = loader.load(source)?
│       assert!(plugin.metadata().category == Bootstrap)
│       ctx = PluginContext::new(phase=Bootstrap, &mut inner, plugin.id)
│       plugin.register(&mut ctx)?
│       plugins.insert(id, plugin)
│       bootstrap_plugin_ids.push(id)
│
├─ 4. 将 Bootstrap 插件注册的加载器加入 loaders
│     （后续 Normal 阶段可使用这些新加载器）
│
└─ 5. Bootstrap 阶段完成
```

### 4.3 典型 Bootstrap 插件示例

#### WASM Bootstrap 插件（注册 WASM 加载器）

```rust
/// Bootstrap 插件：注册 WASM 插件加载器
///
/// 引擎不内置 WASM 加载，而是通过此 Bootstrap 插件注入。
/// 用户按需启用：.bootstrap_plugin(Box::new(WasmBootstrapPlugin::new(config)))
pub struct WasmBootstrapPlugin {
    metadata: PluginMetadata,
    config: WasmPluginConfig,
}

impl WasmBootstrapPlugin {
    pub fn new(config: WasmPluginConfig) -> Self {
        Self {
            metadata: PluginMetadata {
                id: "xworkflow.wasm-loader".into(),
                name: "WASM Plugin Loader".into(),
                version: "0.1.0".into(),
                category: PluginCategory::Bootstrap,
                description: "Registers WasmPluginLoader for loading WASM plugins".into(),
                source: PluginSource::Host,
            },
            config,
        }
    }
}

#[async_trait]
impl Plugin for WasmBootstrapPlugin {
    fn metadata(&self) -> &PluginMetadata { &self.metadata }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let loader = Arc::new(WasmPluginLoader::new(self.config.clone()));
        ctx.register_plugin_loader(loader)?;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any { self }
}
```

#### Python Bootstrap 插件（注册沙箱 + 加载器）

```rust
/// Bootstrap 插件：注册 Python 沙箱和 Python 插件加载器
struct PythonBootstrapPlugin {
    metadata: PluginMetadata,
    python_path: PathBuf,
}

#[async_trait]
impl Plugin for PythonBootstrapPlugin {
    fn metadata(&self) -> &PluginMetadata { &self.metadata }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        // 注册 Python 代码沙箱（Code 节点可以运行 Python）
        let sandbox = Arc::new(PythonSandbox::new(&self.python_path));
        ctx.register_sandbox(CodeLanguage::Python, sandbox)?;

        // 注册 Python 插件加载器（Normal 阶段可加载 .py 插件）
        let loader = Arc::new(PythonPluginLoader::new(&self.python_path));
        ctx.register_plugin_loader(loader)?;

        Ok(())
    }

    fn as_any(&self) -> &dyn Any { self }
}
```

---

## 五、Normal 阶段

### 5.1 普通插件注册点

Normal 插件在 Bootstrap 阶段完成后加载，可使用 Bootstrap 插件注册的所有基础设施（包括自定义加载器和沙箱）：

| 注册方法 | 目标注册表 | 典型用途 |
|---|---|---|
| `register_node_executor(type, executor)` | `NodeExecutorRegistry` | 自定义节点（Redis、Kafka、数据库节点） |
| `register_llm_provider(provider)` | `LlmProviderRegistry` | 自定义 LLM 后端（本地模型、私有 API） |
| `register_hook(point, handler)` | Hook 系统 | 工作流/节点执行前后的拦截和增强 |
| `register_template_function(name, func)` | 模板引擎 | 自定义 Jinja2 函数 |
| `register_dsl_validator(validator)` | DSL 校验管线 | 自定义 DSL 校验规则 |

### 5.2 Hook 系统

将现有 `PluginHookType`（5 种，WASM 紧耦合）重构为通用 Hook 系统：

```rust
/// Hook 触发点
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HookPoint {
    /// 工作流开始前
    BeforeWorkflowRun,
    /// 工作流完成后
    AfterWorkflowRun,
    /// 节点执行前
    BeforeNodeExecute,
    /// 节点执行后
    AfterNodeExecute,
    /// 变量写入前（可拦截修改）
    BeforeVariableWrite,
    /// DSL 校验后（可追加诊断）
    AfterDslValidation,
    /// 插件加载后
    AfterPluginLoaded,
}

/// Hook 处理器 trait
#[async_trait]
pub trait HookHandler: Send + Sync {
    /// 处理 Hook 事件
    ///
    /// 返回 Ok(Some(value)) 表示修改数据（如 BeforeVariableWrite 修改变量值）
    /// 返回 Ok(None) 表示不修改
    async fn handle(&self, payload: &HookPayload) -> Result<Option<Value>, PluginError>;

    /// 处理器名称（用于日志和调试）
    fn name(&self) -> &str;

    /// 优先级（数字越小优先级越高，默认 100）
    fn priority(&self) -> i32 { 100 }
}

/// Hook 载荷
#[derive(Debug, Clone)]
pub struct HookPayload {
    pub hook_point: HookPoint,
    pub data: Value,
    /// 只读变量池快照
    pub variable_pool: Option<Arc<VariablePool>>,
    /// 事件发送器
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
}
```

**执行规则**：

- 同一 `HookPoint` 的多个处理器按 `priority()` 排序执行
- Hook 执行错误**不中断工作流**，仅发送 `GraphEngineEvent::PluginError` 事件
- `BeforeVariableWrite` 返回 `Some(value)` 时，替换即将写入的变量值

### 5.3 模板函数扩展接口

```rust
/// 自定义模板函数
pub trait TemplateFunction: Send + Sync {
    /// 函数名（注册到 Jinja2 环境中）
    fn name(&self) -> &str;

    /// 执行函数
    fn call(&self, args: &[Value]) -> Result<Value, String>;
}
```

注册后可在 Jinja2 模板中使用：`{{ my_custom_func(arg1, arg2) }}`

### 5.4 DSL 校验器扩展接口

```rust
/// 自定义 DSL 校验器
pub trait DslValidator: Send + Sync {
    /// 校验器名称
    fn name(&self) -> &str;

    /// 执行校验，返回额外的诊断信息
    fn validate(&self, schema: &WorkflowSchema) -> Vec<Diagnostic>;
}
```

### 5.5 Normal 阶段执行流程

```
PluginRegistry::run_normal_phase()
│
├─ 1. phase = Normal
│
├─ 2. 处理 host_normal_plugins（宿主端注入的 Normal 插件）
│     for plugin in host_normal_plugins:
│       assert!(plugin.metadata().category == Normal)
│       ctx = PluginContext::new(phase=Normal, &mut inner, plugin.id)
│       plugin.register(&mut ctx)?
│       plugins.insert(id, plugin)
│       normal_plugin_ids.push(id)
│
├─ 3. 处理 normal_sources
│     使用所有可用 Loader（内置 DLL/Host + Bootstrap 阶段注册的 WASM/Python/...）
│     for source in normal_sources:
│       loader = loaders.get(source.loader_type)?
│       plugin = loader.load(source)?
│       assert!(plugin.metadata().category == Normal)
│       ctx = PluginContext::new(phase=Normal, &mut inner, plugin.id)
│       plugin.register(&mut ctx)?
│       plugins.insert(id, plugin)
│       normal_plugin_ids.push(id)
│
└─ 4. phase = Ready
```

---

## 六、两种内置加载方式

### 6.1 DLL 加载（双模式）

使用 `libloading` crate 实现跨平台动态库加载。支持两种 ABI 模式，通过导出符号自动区分。

**新增依赖**（仅在 `plugin-system` feature 下）：

```toml
[dependencies]
libloading = { version = "0.8", optional = true }

[features]
plugin-system = ["libloading"]
```

#### 6.1.1 C ABI 模式

DLL 导出 C 函数符号，支持任何能编译为 C ABI 的语言（C/C++/Go/Rust）：

```c
// 必须导出 — 创建插件实例
extern "C" fn xworkflow_plugin_create() -> *mut c_void;

// 必须导出 — 销毁插件实例
extern "C" fn xworkflow_plugin_destroy(plugin: *mut c_void);

// 可选 — 返回 ABI 版本号（用于兼容性检查）
extern "C" fn xworkflow_plugin_abi_version() -> u32;
```

#### 6.1.2 Rust ABI 模式

DLL 导出特定符号，加载器识别后直接转型为 Rust trait object：

```rust
// Rust ABI 标志符号（加载器检测此符号存在则走 Rust 路径）
#[no_mangle]
pub static XWORKFLOW_RUST_PLUGIN: bool = true;

// Rust ABI 创建函数 — 返回 Box<dyn Plugin>
#[no_mangle]
pub fn xworkflow_rust_plugin_create() -> Box<dyn Plugin>;
```

**Rust 模式限制**：要求插件与宿主使用相同 Rust 编译器版本（ABI 不稳定）。适合同一项目内部的快速开发。

#### 6.1.3 DllPluginLoader 实现

```rust
pub struct DllPluginLoader {
    abi_version: u32,
}

/// 持有动态库句柄的 Plugin 包装器（C ABI 模式）
struct DllPluginWrapper {
    inner: Box<dyn Plugin>,
    _library: Library,  // 保持 DLL 加载状态
    destroy_fn: unsafe extern "C" fn(*mut c_void),
    raw_ptr: *mut c_void,
}

impl Drop for DllPluginWrapper {
    fn drop(&mut self) {
        unsafe { (self.destroy_fn)(self.raw_ptr); }
    }
}

#[async_trait]
impl Plugin for DllPluginWrapper {
    fn metadata(&self) -> &PluginMetadata { self.inner.metadata() }
    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        self.inner.register(ctx).await
    }
    async fn shutdown(&self) -> Result<(), PluginError> { self.inner.shutdown().await }
    fn as_any(&self) -> &dyn Any { self.inner.as_any() }
}

/// Rust ABI 模式的包装器
struct RustDllPluginWrapper {
    inner: Box<dyn Plugin>,
    _library: Library,
}

// Plugin 委托实现同上

const CURRENT_ABI_VERSION: u32 = 1;

#[async_trait]
impl PluginLoader for DllPluginLoader {
    fn loader_type(&self) -> &str { "dll" }

    async fn load(&self, source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError> {
        let path = source.params.get("path")
            .ok_or(PluginError::InvalidConfig("DLL path not specified".into()))?;

        unsafe {
            let library = Library::new(path)
                .map_err(|e| PluginError::LoadError(format!("Failed to load DLL: {}", e)))?;

            // 检测模式：是否存在 Rust ABI 标志
            let is_rust_abi = library.get::<*const bool>(b"XWORKFLOW_RUST_PLUGIN\0").is_ok();

            if is_rust_abi {
                self.load_rust_abi(library)
            } else {
                self.load_c_abi(library)
            }
        }
    }
}

impl DllPluginLoader {
    unsafe fn load_c_abi(&self, library: Library) -> Result<Box<dyn Plugin>, PluginError> {
        // 1. 检查 ABI 版本
        if let Ok(version_fn) = library.get::<extern "C" fn() -> u32>(
            b"xworkflow_plugin_abi_version\0"
        ) {
            let version = version_fn();
            if version != CURRENT_ABI_VERSION {
                return Err(PluginError::AbiVersionMismatch {
                    expected: CURRENT_ABI_VERSION,
                    actual: version,
                });
            }
        }

        // 2. 获取创建和销毁函数
        let create_fn = library.get::<extern "C" fn() -> *mut c_void>(
            b"xworkflow_plugin_create\0"
        ).map_err(|e| PluginError::MissingExport(e.to_string()))?;

        let destroy_fn = *library.get::<extern "C" fn(*mut c_void)>(
            b"xworkflow_plugin_destroy\0"
        ).map_err(|e| PluginError::MissingExport(e.to_string()))?;

        // 3. 创建插件
        let raw_ptr = create_fn();
        if raw_ptr.is_null() {
            return Err(PluginError::LoadError("Plugin create returned null".into()));
        }
        let inner = Box::from_raw(raw_ptr as *mut dyn Plugin);

        Ok(Box::new(DllPluginWrapper {
            inner,
            _library: library,
            destroy_fn,
            raw_ptr,
        }))
    }

    unsafe fn load_rust_abi(&self, library: Library) -> Result<Box<dyn Plugin>, PluginError> {
        let create_fn = library.get::<fn() -> Box<dyn Plugin>>(
            b"xworkflow_rust_plugin_create\0"
        ).map_err(|e| PluginError::MissingExport(e.to_string()))?;

        let inner = create_fn();

        Ok(Box::new(RustDllPluginWrapper {
            inner,
            _library: library,
        }))
    }
}
```

#### 6.1.4 Rust 插件开发辅助宏

```rust
/// C ABI 模式辅助宏
#[macro_export]
macro_rules! xworkflow_declare_plugin {
    ($plugin_type:ty) => {
        #[no_mangle]
        pub extern "C" fn xworkflow_plugin_create() -> *mut std::ffi::c_void {
            let plugin: Box<dyn $crate::plugin_system::Plugin> =
                Box::new(<$plugin_type>::default());
            Box::into_raw(plugin) as *mut std::ffi::c_void
        }

        #[no_mangle]
        pub extern "C" fn xworkflow_plugin_destroy(ptr: *mut std::ffi::c_void) {
            if !ptr.is_null() {
                unsafe { let _ = Box::from_raw(ptr as *mut dyn $crate::plugin_system::Plugin); }
            }
        }

        #[no_mangle]
        pub extern "C" fn xworkflow_plugin_abi_version() -> u32 { 1 }
    };
}

/// Rust ABI 模式辅助宏
#[macro_export]
macro_rules! xworkflow_declare_rust_plugin {
    ($plugin_type:ty) => {
        #[no_mangle]
        pub static XWORKFLOW_RUST_PLUGIN: bool = true;

        #[no_mangle]
        pub fn xworkflow_rust_plugin_create() -> Box<dyn $crate::plugin_system::Plugin> {
            Box::new(<$plugin_type>::default())
        }
    };
}
```

### 6.2 宿主端注入

最简单的加载方式——宿主程序直接构造 `Box<dyn Plugin>` 并注册：

```rust
pub struct HostPluginLoader;

#[async_trait]
impl PluginLoader for HostPluginLoader {
    fn loader_type(&self) -> &str { "host" }

    async fn load(&self, _source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError> {
        Err(PluginError::InvalidConfig(
            "HostPluginLoader does not use load(). Use builder API directly.".into()
        ))
    }
}
```

**宿主端使用方式**：

```rust
let runner = WorkflowRunner::builder(schema)
    // Bootstrap 阶段：注册 WASM 加载器
    .bootstrap_plugin(Box::new(WasmBootstrapPlugin::new(wasm_config)))
    // Bootstrap 阶段：注册 Python 沙箱
    .bootstrap_plugin(Box::new(PythonBootstrapPlugin::new("/usr/bin/python3")))
    // Normal 阶段：注册 Redis 节点
    .plugin(Box::new(RedisNodePlugin::new(redis_config)))
    // Normal 阶段：注册自定义 LLM Provider
    .plugin(Box::new(LocalLlmPlugin::new(model_path)))
    .user_inputs(inputs)
    .run()
    .await?;
```

### 6.3 网络加载（暂缓实现）

仅设计接口。网络加载器通过 Bootstrap 插件注册，与 WASM 类似：

```rust
/// 网络插件 Bootstrap 插件（暂缓实现）
pub struct NetworkBootstrapPlugin { /* ... */ }

impl Plugin for NetworkBootstrapPlugin {
    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_plugin_loader(Arc::new(NetworkPluginLoader::new(self.config.clone())))?;
        Ok(())
    }
    // ...
}
```

远程插件服务 HTTP 协议：

| 端点 | 方法 | 说明 |
|---|---|---|
| `/metadata` | GET | 返回 `PluginMetadata`（JSON） |
| `/register` | POST | 返回该插件要注册的扩展声明列表 |
| `/execute-node` | POST | 执行节点（`NodeExecutor` 代理） |
| `/execute-hook` | POST | 执行 Hook（`HookHandler` 代理） |
| `/shutdown` | POST | 通知远程服务关闭 |

---

## 七、WASM 加载器（Bootstrap 插件）

WASM 不作为内置加载方式，而是通过 `WasmBootstrapPlugin` 在 Bootstrap 阶段注册 `WasmPluginLoader`。

### 7.1 WasmPluginLoader

将现有 `PluginManager` 的加载逻辑封装为 `WasmPluginLoader`：

```rust
/// WASM 插件加载器（由 WasmBootstrapPlugin 注册）
pub struct WasmPluginLoader {
    engine: wasmtime::Engine,
    config: WasmPluginConfig,
}

#[derive(Debug, Clone)]
pub struct WasmPluginConfig {
    pub auto_discover: bool,
    pub default_max_memory_pages: u32,
    pub default_max_fuel: u64,
    pub allowed_capabilities: AllowedCapabilities,
}

#[async_trait]
impl PluginLoader for WasmPluginLoader {
    fn loader_type(&self) -> &str { "wasm" }

    async fn load(&self, source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError> {
        let dir = source.params.get("dir")
            .ok_or(PluginError::InvalidConfig("WASM plugin dir not specified".into()))?;

        let manifest_path = Path::new(dir).join("manifest.json");
        let manifest: PluginManifest = /* 读取并解析 */;
        let wasm_bytes = /* 读取 .wasm 文件 */;

        let runtime = Arc::new(PluginRuntime::new(manifest.clone(), &wasm_bytes)?);

        Ok(Box::new(WasmPlugin {
            metadata: self.manifest_to_metadata(&manifest, &manifest_path),
            runtime,
            manifest,
        }))
    }
}
```

### 7.2 WasmPlugin 适配器

```rust
/// WASM 插件的 Plugin trait 适配器
struct WasmPlugin {
    metadata: PluginMetadata,
    runtime: Arc<PluginRuntime>,
    manifest: PluginManifest,
}

#[async_trait]
impl Plugin for WasmPlugin {
    fn metadata(&self) -> &PluginMetadata { &self.metadata }

    async fn register(&self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        // 注册节点类型
        for node_type_def in &self.manifest.node_types {
            let executor = Box::new(WasmNodeExecutorAdapter {
                runtime: self.runtime.clone(),
                node_type: node_type_def.clone(),
            });
            ctx.register_node_executor(&node_type_def.node_type, executor)?;
        }

        // 注册 Hook
        for hook in &self.manifest.hooks {
            let handler = Arc::new(WasmHookHandlerAdapter {
                runtime: self.runtime.clone(),
                hook: hook.clone(),
            });
            let hook_point = match hook.hook_type {
                PluginHookType::BeforeWorkflowRun => HookPoint::BeforeWorkflowRun,
                PluginHookType::AfterWorkflowRun  => HookPoint::AfterWorkflowRun,
                PluginHookType::BeforeNodeExecute  => HookPoint::BeforeNodeExecute,
                PluginHookType::AfterNodeExecute   => HookPoint::AfterNodeExecute,
                PluginHookType::BeforeVariableWrite => HookPoint::BeforeVariableWrite,
            };
            ctx.register_hook(hook_point, handler)?;
        }

        Ok(())
    }

    fn as_any(&self) -> &dyn Any { self }
}

/// WASM 节点执行器适配器
struct WasmNodeExecutorAdapter {
    runtime: Arc<PluginRuntime>,
    node_type: PluginNodeType,
}

#[async_trait]
impl NodeExecutor for WasmNodeExecutorAdapter {
    async fn execute(
        &self, node_id: &str, config: &Value,
        variable_pool: &VariablePool, context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        self.runtime.execute_node(&self.node_type, config, variable_pool, context).await
    }
}

/// WASM Hook 处理器适配器
struct WasmHookHandlerAdapter {
    runtime: Arc<PluginRuntime>,
    hook: PluginHook,
}

#[async_trait]
impl HookHandler for WasmHookHandlerAdapter {
    async fn handle(&self, payload: &HookPayload) -> Result<Option<Value>, PluginError> {
        let pool = payload.variable_pool.as_ref()
            .map(|p| Arc::new(std::sync::RwLock::new((*p).clone())));
        match pool {
            Some(pool) => self.runtime.execute_hook(
                self.hook.hook_type.clone(),
                &payload.data,
                pool,
                payload.event_tx.clone(),
            ).await,
            None => Ok(None),
        }
    }
    fn name(&self) -> &str { &self.hook.handler }
}
```

### 7.3 迁移对比

| 旧组件 | 新组件 | 处理方式 |
|---|---|---|
| `PluginManager` | `PluginRegistry` + `WasmBootstrapPlugin` + `WasmPluginLoader` | 加载逻辑迁移到 WasmPluginLoader，由 Bootstrap 插件注册 |
| `PluginNodeExecutor` | `WasmNodeExecutorAdapter` | 在 WasmPlugin::register() 中创建 |
| `PluginRuntime` | `PluginRuntime`（保持不变） | 被 WasmPlugin 持有 |
| `PluginManifest` | `PluginManifest`（保持不变） | 转换为 PluginMetadata |
| `PluginHookType` | `HookPoint` | 枚举映射 |
| `PluginManagerConfig` | `WasmPluginConfig` | 配置迁移 |
| `host_functions.rs` | `host_functions.rs`（保持不变） | 仍由 PluginRuntime 使用 |

直接移除旧 `PluginManager` API（尚未发布第一个版本）。

---

## 八、现有代码改造

### 8.1 PluginSystemConfig

新增统一的插件配置结构（仅在 `plugin-system` feature 下编译）：

```rust
/// 插件系统配置
#[derive(Debug, Clone, Default)]
pub struct PluginSystemConfig {
    /// Bootstrap 阶段 DLL 插件路径列表
    pub bootstrap_dll_paths: Vec<PathBuf>,
    /// Normal 阶段 DLL 插件路径列表
    pub normal_dll_paths: Vec<PathBuf>,
    /// Normal 阶段自定义加载源（由 Bootstrap 插件注册的加载器处理）
    pub normal_load_sources: Vec<PluginLoadSource>,
}
```

注意：不再有 `wasm_plugin_dir` / `wasm_config` 字段。WASM 配置由 `WasmBootstrapPlugin` 自行管理。

### 8.2 NodeExecutorRegistry 改造

**文件**：`src/nodes/executor.rs`

```rust
impl NodeExecutorRegistry {
    /// 保持不变：创建仅包含内置节点的注册表
    pub fn new() -> Self { /* 20+ 内置节点注册 */ }

    /// 新增（仅 plugin-system feature）：注入插件注册的节点执行器
    #[cfg(feature = "plugin-system")]
    pub fn apply_plugin_executors(
        &mut self,
        executors: HashMap<String, Box<dyn NodeExecutor>>,
    ) {
        for (node_type, executor) in executors {
            self.register(&node_type, executor);
        }
    }

    /// 保持不变
    pub fn register(&mut self, node_type: &str, executor: Box<dyn NodeExecutor>);
    pub fn set_llm_provider_registry(&mut self, registry: Arc<LlmProviderRegistry>);
    pub fn get(&self, node_type: &str) -> Option<&dyn NodeExecutor>;
}
```

移除 `new_with_plugins(plugin_manager: Arc<PluginManager>)` 方法。

### 8.3 LlmProviderRegistry 改造

**文件**：`src/llm/mod.rs`

```rust
impl LlmProviderRegistry {
    /// 保持不变
    pub fn new() -> Self;
    pub fn register(&mut self, provider: Arc<dyn LlmProvider>);
    pub fn get(&self, provider_id: &str) -> Option<Arc<dyn LlmProvider>>;
    pub fn with_builtins() -> Self;

    /// 新增（仅 plugin-system feature）
    #[cfg(feature = "plugin-system")]
    pub fn apply_plugin_providers(&mut self, providers: &[Arc<dyn LlmProvider>]) {
        for provider in providers {
            self.register(provider.clone());
        }
    }
}
```

### 8.4 SandboxManager 改造

**文件**：`src/sandbox/manager.rs`

```rust
impl SandboxManager {
    /// 保持不变
    pub fn new(config: SandboxManagerConfig) -> Self;
    pub fn register_sandbox(&mut self, language: CodeLanguage, sandbox: Arc<dyn CodeSandbox>);

    /// 新增（仅 plugin-system feature）
    #[cfg(feature = "plugin-system")]
    pub fn apply_plugin_sandboxes(
        &mut self,
        sandboxes: &[(CodeLanguage, Arc<dyn CodeSandbox>)],
    ) {
        for (language, sandbox) in sandboxes {
            self.register_sandbox(*language, sandbox.clone());
        }
    }
}
```

### 8.5 WorkflowRunnerBuilder 改造

**文件**：`src/scheduler.rs`

```rust
pub struct WorkflowRunnerBuilder {
    schema: WorkflowSchema,
    user_inputs: HashMap<String, Value>,
    system_vars: HashMap<String, Value>,
    environment_vars: HashMap<String, Value>,
    conversation_vars: HashMap<String, Value>,
    config: EngineConfig,
    context: RuntimeContext,

    // 仅在 plugin-system feature 下存在
    #[cfg(feature = "plugin-system")]
    plugin_system_config: Option<PluginSystemConfig>,
    #[cfg(feature = "plugin-system")]
    host_bootstrap_plugins: Vec<Box<dyn Plugin>>,
    #[cfg(feature = "plugin-system")]
    host_normal_plugins: Vec<Box<dyn Plugin>>,
    #[cfg(feature = "plugin-system")]
    plugin_registry: Option<Arc<PluginRegistry>>,

    llm_provider_registry: Option<Arc<LlmProviderRegistry>>,
    debug_config: Option<DebugConfig>,
}

impl WorkflowRunnerBuilder {
    /// 配置插件系统
    #[cfg(feature = "plugin-system")]
    pub fn plugin_config(mut self, config: PluginSystemConfig) -> Self {
        self.plugin_system_config = Some(config);
        self
    }

    /// 注入宿主端 Bootstrap 插件
    #[cfg(feature = "plugin-system")]
    pub fn bootstrap_plugin(mut self, plugin: Box<dyn Plugin>) -> Self {
        assert_eq!(plugin.metadata().category, PluginCategory::Bootstrap);
        self.host_bootstrap_plugins.push(plugin);
        self
    }

    /// 注入宿主端 Normal 插件
    #[cfg(feature = "plugin-system")]
    pub fn plugin(mut self, plugin: Box<dyn Plugin>) -> Self {
        assert_eq!(plugin.metadata().category, PluginCategory::Normal);
        self.host_normal_plugins.push(plugin);
        self
    }

    /// 执行插件系统初始化
    #[cfg(feature = "plugin-system")]
    async fn init_plugins(&mut self) -> Result<(), PluginError> {
        let mut registry = PluginRegistry::new();

        // 仅注册两种内置加载器
        registry.register_loader(Arc::new(DllPluginLoader::new()));
        registry.register_loader(Arc::new(HostPluginLoader));

        // Bootstrap 阶段
        let bootstrap_sources = self.collect_bootstrap_sources();
        let bootstrap_plugins = std::mem::take(&mut self.host_bootstrap_plugins);
        registry.run_bootstrap_phase(bootstrap_sources, bootstrap_plugins).await?;

        // Normal 阶段（此时已有 Bootstrap 插件注册的加载器，如 WASM）
        let normal_sources = self.collect_normal_sources();
        let normal_plugins = std::mem::take(&mut self.host_normal_plugins);
        registry.run_normal_phase(normal_sources, normal_plugins).await?;

        self.plugin_registry = Some(Arc::new(registry));
        Ok(())
    }

    #[cfg(feature = "plugin-system")]
    fn collect_bootstrap_sources(&self) -> Vec<PluginLoadSource> {
        let mut sources = Vec::new();
        if let Some(config) = &self.plugin_system_config {
            for path in &config.bootstrap_dll_paths {
                sources.push(PluginLoadSource {
                    loader_type: "dll".into(),
                    params: [("path".into(), path.to_string_lossy().into())].into(),
                });
            }
        }
        sources
    }

    #[cfg(feature = "plugin-system")]
    fn collect_normal_sources(&self) -> Vec<PluginLoadSource> {
        let mut sources = Vec::new();
        if let Some(config) = &self.plugin_system_config {
            for path in &config.normal_dll_paths {
                sources.push(PluginLoadSource {
                    loader_type: "dll".into(),
                    params: [("path".into(), path.to_string_lossy().into())].into(),
                });
            }
            sources.extend(config.normal_load_sources.clone());
        }
        sources
    }
}
```

**改造后的 `run()` 流程**：

```
WorkflowRunnerBuilder::run()
│
├─ 1. DSL 校验
│
├─ 2. [仅 plugin-system feature] 插件系统初始化（两阶段）
│     if 有 plugin_config 或 host plugins:
│       self.init_plugins().await?
│
├─ 3. [仅 plugin-system feature] 构建 SandboxManager 并注入 Bootstrap 插件的沙箱
│     sandbox_manager.apply_plugin_sandboxes(registry.sandboxes())
│
├─ 4. 构建 NodeExecutorRegistry
│     [仅 plugin-system feature] registry.apply_plugin_executors(plugin_reg.take_node_executors())
│
├─ 5. 构建 LlmProviderRegistry
│     [仅 plugin-system feature] llm_reg.apply_plugin_providers(plugin_reg.llm_providers())
│     registry.set_llm_provider_registry(llm_reg)
│
├─ 6. 构建 RuntimeContext
│     [仅 plugin-system feature] if plugin_reg 有自定义 TimeProvider/IdGenerator → 替换
│
├─ 7. 构建 VariablePool（不变）
│
└─ 8. 创建并启动 WorkflowDispatcher
      [仅 plugin-system feature] 传入 plugin_registry
```

### 8.6 WorkflowDispatcher 改造

**文件**：`src/core/dispatcher.rs`

```rust
pub struct WorkflowDispatcher<G, H> {
    // ... 基础字段 ...
    #[cfg(feature = "plugin-system")]
    plugin_registry: Option<Arc<PluginRegistry>>,
    debug_gate: G,
    debug_hook: H,
}

impl<G: DebugGate, H: DebugHook> WorkflowDispatcher<G, H> {
    /// Hook 执行 — feature 启用版本
    #[cfg(feature = "plugin-system")]
    async fn execute_hooks(
        &self,
        hook_point: HookPoint,
        data: Value,
    ) -> Result<Vec<Value>, WorkflowError> {
        let mut results = Vec::new();
        if let Some(reg) = &self.plugin_registry {
            let mut handlers = reg.hooks(&hook_point);
            if handlers.is_empty() {
                return Ok(results);  // 快速路径：无 handler 则直接返回
            }
            handlers.sort_by_key(|h| h.priority());

            let pool_snapshot = self.variable_pool.read().await.clone();
            let payload = HookPayload {
                hook_point,
                data,
                variable_pool: Some(Arc::new(pool_snapshot)),
                event_tx: Some(self.event_tx.clone()),
            };

            for handler in handlers {
                match handler.handle(&payload).await {
                    Ok(Some(value)) => results.push(value),
                    Ok(None) => {}
                    Err(e) => {
                        let _ = self.event_tx.send(GraphEngineEvent::PluginError {
                            plugin_id: handler.name().to_string(),
                            error: e.to_string(),
                        }).await;
                    }
                }
            }
        }
        Ok(results)
    }

    /// Hook 执行 — feature 未启用版本（编译器内联为 noop）
    #[cfg(not(feature = "plugin-system"))]
    #[inline(always)]
    async fn execute_hooks(
        &self,
        _hook_point: (),  // 类型可为 unit，因为不会被调用
        _data: Value,
    ) -> Result<Vec<Value>, WorkflowError> {
        Ok(vec![])
    }
}
```

---

## 九、安全模型

### 9.1 按加载方式的信任级别

| 加载方式 | 信任级别 | 隔离方式 | 说明 |
|---|---|---|---|
| DLL | 完全信任 | 无隔离 | 原生代码，与宿主同进程 |
| Host | 完全信任 | 无隔离 | 宿主端代码，等同于宿主 |
| WASM（Bootstrap 插件注册） | 受限信任 | wasmtime 沙箱 | 能力受 manifest + AllowedCapabilities 双重约束 |
| Network（Bootstrap 插件注册） | 最低信任 | 协议隔离 | 仅 HTTP JSON 交换，无直接内存访问 |
| 自定义 | 取决于实现 | 取决于加载器 | 由注册该加载器的 Bootstrap 插件负责安全性 |

### 9.2 能力模型（扩展现有设计）

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginCapabilities {
    // 现有能力（保持不变）
    #[serde(default)]
    pub read_variables: bool,
    #[serde(default)]
    pub write_variables: bool,
    #[serde(default)]
    pub emit_events: bool,
    #[serde(default)]
    pub http_access: bool,
    #[serde(default)]
    pub fs_access: Option<Vec<String>>,
    #[serde(default)]
    pub max_memory_pages: Option<u32>,
    #[serde(default)]
    pub max_fuel: Option<u64>,

    // 新增能力
    #[serde(default)]
    pub register_nodes: bool,
    #[serde(default)]
    pub register_llm_providers: bool,
    #[serde(default)]
    pub register_hooks: bool,
}
```

### 9.3 Bootstrap 插件权限模型

Bootstrap 插件拥有更高的权限：

- 可注册沙箱（影响所有代码节点的执行环境）
- 可注册加载器（影响后续插件的加载方式）
- 可替换 TimeProvider / IdGenerator（影响引擎核心行为）
- **只能通过 DLL 或 Host 方式加载**（因为需要完全信任）

### 9.4 运行时权限检查

PluginContext 在注册方法中进行权限检查（仅对非信任来源插件生效）：

```rust
impl<'a> PluginContext<'a> {
    pub fn register_node_executor(
        &mut self,
        node_type: &str,
        executor: Box<dyn NodeExecutor>,
    ) -> Result<(), PluginError> {
        // 1. 阶段检查
        if self.phase != PluginPhase::Normal {
            return Err(PluginError::WrongPhase {
                expected: PluginPhase::Normal,
                actual: self.phase,
            });
        }

        // 2. 能力检查（对有能力约束的插件，如 WASM）
        if let Some(caps) = self.plugin_capabilities() {
            if !caps.register_nodes {
                return Err(PluginError::CapabilityDenied("register_nodes".into()));
            }
        }

        // 3. 冲突检查
        if self.registry_inner.node_executors.contains_key(node_type) {
            return Err(PluginError::ConflictError(format!(
                "Node type '{}' already registered", node_type
            )));
        }

        self.registry_inner.node_executors.insert(node_type.to_string(), executor);
        Ok(())
    }
}
```

---

## 十、改动文件清单

### 10.1 新增文件

| 文件路径 | 说明 |
|---|---|
| `src/plugin_system/mod.rs` | 新插件系统模块入口（`#[cfg(feature = "plugin-system")]`） |
| `src/plugin_system/traits.rs` | Plugin trait、PluginMetadata、PluginCategory、PluginSource |
| `src/plugin_system/loader.rs` | PluginLoader trait、PluginLoadSource |
| `src/plugin_system/registry.rs` | PluginRegistry、PluginRegistryInner、PluginPhase |
| `src/plugin_system/context.rs` | PluginContext 实现（含阶段检查和能力检查） |
| `src/plugin_system/config.rs` | PluginSystemConfig |
| `src/plugin_system/hooks.rs` | HookPoint、HookHandler trait、HookPayload |
| `src/plugin_system/extensions.rs` | TemplateFunction trait、DslValidator trait |
| `src/plugin_system/error.rs` | 扩展 PluginError 枚举 |
| `src/plugin_system/macros.rs` | xworkflow_declare_plugin!、xworkflow_declare_rust_plugin! 宏 |
| `src/plugin_system/loaders/mod.rs` | 内置加载器模块入口 |
| `src/plugin_system/loaders/dll_loader.rs` | DllPluginLoader（C ABI + Rust ABI 双模式） |
| `src/plugin_system/loaders/host_loader.rs` | HostPluginLoader |
| `src/plugin_system/builtins/mod.rs` | 引擎提供的预置 Bootstrap 插件 |
| `src/plugin_system/builtins/wasm_bootstrap.rs` | WasmBootstrapPlugin + WasmPluginLoader + 适配器 |

### 10.2 修改文件

| 文件路径 | 改动内容 |
|---|---|
| `Cargo.toml` | 新增 `libloading = { version = "0.8", optional = true }` 和 `plugin-system` feature |
| `src/lib.rs` | `#[cfg(feature = "plugin-system")] pub mod plugin_system;` 及 re-exports |
| `src/nodes/executor.rs` | `#[cfg]` 门控下新增 `apply_plugin_executors()`，移除 `new_with_plugins()` |
| `src/llm/mod.rs` | `#[cfg]` 门控下新增 `apply_plugin_providers()` |
| `src/sandbox/manager.rs` | `#[cfg]` 门控下新增 `apply_plugin_sandboxes()` |
| `src/scheduler.rs` | `#[cfg]` 门控下新增 `plugin_config()` / `bootstrap_plugin()` / `plugin()`，`run()` 加入两阶段初始化 |
| `src/core/dispatcher.rs` | `#[cfg]` 门控下替换 `plugin_manager` 为 `plugin_registry`，Hook 调用重构 |
| `src/core/event_bus.rs` | `#[cfg]` 门控下新增 `BootstrapPhaseCompleted`、`NormalPhaseCompleted` 事件 |
| `src/template/engine.rs` | `#[cfg]` 门控下 `render_jinja2()` 支持注入自定义函数 |

### 10.3 移除/重构文件

| 文件路径 | 处理方式 |
|---|---|
| `src/plugin/mod.rs` | 删除 `PluginNodeExecutor` |
| `src/plugin/manager.rs` | 加载逻辑迁移至 `plugin_system/builtins/wasm_bootstrap.rs`，原文件删除 |
| `src/plugin/manifest.rs` | 保留（被 WasmPluginLoader 继续使用） |
| `src/plugin/runtime.rs` | 保留（被 WasmPlugin 适配器继续使用） |
| `src/plugin/host_functions.rs` | 保留（被 PluginRuntime 继续使用） |
| `src/plugin/error.rs` | 迁移至 `plugin_system/error.rs`，扩展新错误类型 |

### 10.4 新增测试文件

| 文件路径 | 说明 |
|---|---|
| `tests/plugin_system_bootstrap.rs` | Bootstrap 阶段测试（沙箱注册、加载器注册） |
| `tests/plugin_system_host.rs` | 宿主端注入测试 |
| `tests/plugin_system_dll.rs` | DLL 加载测试（C ABI + Rust ABI） |
| `tests/plugin_system_wasm_bootstrap.rs` | WASM Bootstrap 插件 + WASM 插件加载测试 |
| `tests/plugin_system_hooks.rs` | Hook 系统测试（优先级、错误处理） |
| `tests/plugin_system_integration.rs` | 端到端集成测试 |
| `tests/plugin_system_zero_cost.rs` | 验证不启用 feature 时无插件代码编译 |

---

## 十一、验证方案

### 11.1 单元测试

| 测试 | 验证点 |
|---|---|
| `test_plugin_context_bootstrap_restrictions` | Bootstrap 阶段调用 Normal 方法返回 WrongPhase |
| `test_plugin_context_normal_restrictions` | Normal 阶段调用 Bootstrap 方法返回 WrongPhase |
| `test_registry_bootstrap_phase` | Bootstrap 阶段正确注册沙箱和加载器 |
| `test_registry_normal_phase` | Normal 阶段正确注册节点、Provider、Hook |
| `test_registry_shutdown_order` | 插件按逆序 shutdown |
| `test_hook_priority_ordering` | Hook 按优先级排序执行 |
| `test_hook_error_no_abort` | Hook 错误不中断工作流 |
| `test_node_type_conflict` | 重复注册节点类型返回 ConflictError |
| `test_capability_denied` | 能力不足返回 CapabilityDenied |

### 11.2 加载器测试

| 测试 | 验证点 |
|---|---|
| `test_dll_loader_c_abi` | C ABI DLL 正确加载并执行 |
| `test_dll_loader_rust_abi` | Rust ABI DLL 正确加载并执行 |
| `test_dll_loader_auto_detect` | 加载器自动检测 ABI 模式 |
| `test_dll_abi_version_mismatch` | ABI 版本不匹配拒绝加载 |
| `test_dll_missing_symbol` | 缺少导出符号报错 |
| `test_host_loader_direct_register` | 宿主端直接注册插件 |
| `test_wasm_bootstrap_registers_loader` | WasmBootstrapPlugin 正确注册 WasmPluginLoader |
| `test_wasm_node_via_bootstrap` | WASM 插件节点通过 Bootstrap 注册的加载器执行 |
| `test_wasm_hook_via_bootstrap` | WASM 插件 Hook 通过 Bootstrap 注册的加载器触发 |
| `test_custom_loader_from_bootstrap` | Bootstrap 插件注册自定义加载器，Normal 阶段可使用 |

### 11.3 零成本测试

| 测试 | 验证点 |
|---|---|
| `test_compile_without_feature` | `cargo build` 无 `plugin-system` feature 成功编译 |
| `test_no_plugin_code_in_binary` | 检查二进制中不含 `PluginRegistry` 等符号 |
| `test_no_overhead_without_plugins` | 启用 feature 但不注册插件，Hook 调用为空操作 |

### 11.4 集成测试

| 测试 | 验证点 |
|---|---|
| `test_e2e_host_plugin_custom_node` | 宿主端插件注册的节点在工作流中执行 |
| `test_e2e_bootstrap_sandbox_used_by_code_node` | Bootstrap 插件注册的沙箱被 Code 节点使用 |
| `test_e2e_plugin_llm_provider` | 插件注册的 LLM Provider 被 LLM 节点使用 |
| `test_e2e_hook_modifies_variable` | BeforeVariableWrite Hook 修改变量值 |
| `test_e2e_wasm_via_bootstrap` | WasmBootstrapPlugin → WASM 插件 → 节点执行全链路 |
| `test_e2e_mixed_loading_types` | DLL + Host + WASM(Bootstrap) 插件同时工作 |
| `test_e2e_no_plugins_zero_overhead` | 不使用插件时引擎行为完全不变 |
| `test_e2e_bootstrap_then_normal_ordering` | Bootstrap 注册的加载器在 Normal 阶段可用 |

### 11.5 验证命令

```bash
# 全量测试（含插件系统）
cargo test --features plugin-system

# 不含插件系统编译（验证零成本）
cargo build
cargo test

# 插件系统测试
cargo test --features plugin-system plugin_system

# 特定加载器测试
cargo test --features plugin-system plugin_system_dll
cargo test --features plugin-system plugin_system_host
cargo test --features plugin-system plugin_system_wasm

# 集成测试
cargo test --features plugin-system --test plugin_system_integration
```
