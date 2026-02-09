# WASM 执行引擎 + 插件系统设计文档

## Context

xworkflow 当前的代码沙箱仅支持基于 boa_engine 的 JavaScript 执行。需要新增一个 WASM 执行引擎，该引擎服务于两种场景：

1. **代码节点（Code Node）**：用户编写的 WASM 代码在沙箱中执行，类似当前的 JS 沙箱
2. **插件模块（Plugin Module）**：第三方/自定义 WASM 插件扩展工作流引擎能力，具备更丰富的宿主交互

运行时选用 **wasmtime**（Bytecode Alliance，Cranelift JIT，WASI 支持完善）。

### 两种模式的 ABI 区别

| | 代码节点（Sandbox） | 插件模块（Plugin） |
|---|---|---|
| 接口协议 | 纯自定义 ABI | WASI + 自定义 Host Functions |
| 文件/网络 I/O | 禁止 | 通过 WASI 受控提供 |
| 宿主 API | 仅 inputs/outputs 数据传递 | 可读写变量池、发事件、调用日志等 |
| 安全级别 | 最高（完全隔离） | 中等（按能力授权） |
| 典型使用者 | 工作流用户写一次性逻辑 | 扩展开发者发布可复用插件 |

---

## 一、新增依赖（Cargo.toml）

```toml
wasmtime = "27"
wasmtime-wasi = "27"
wat = "1"       # 可选，用于开发/测试阶段支持 WAT 文本格式
```

---

## 二、模块结构概览

```
src/
  sandbox/
    mod.rs              # 添加 pub mod wasm_sandbox;
    wasm_sandbox.rs     # 【新增】WasmSandbox — CodeSandbox 实现（代码节点用）
    types.rs            # CodeLanguage::Wasm 已存在
  plugin/
    mod.rs              # 【新增】模块声明 + re-exports
    manifest.rs         # 【新增】PluginManifest — 插件元数据与能力声明
    host_functions.rs   # 【新增】Host Functions — 宿主 API 定义
    runtime.rs          # 【新增】PluginRuntime — 单个插件实例管理
    manager.rs          # 【新增】PluginManager — 插件发现、加载、生命周期
    error.rs            # 【新增】PluginError
  lib.rs                # 添加 pub mod plugin; 导出
```

---

## 三、代码节点 WASM 沙箱（`src/sandbox/wasm_sandbox.rs`）

### 3.1 设计目标

- 实现 `CodeSandbox` trait，与 BuiltinSandbox（JS）并列
- 纯自定义 ABI，不提供 WASI（无文件/网络访问）
- 用户提供编译好的 `.wasm` 二进制（Base64 编码在 code 字段中）
- 通过线性内存传递 JSON inputs/outputs

### 3.2 WASM 模块约定（自定义 ABI）

用户编译的 WASM 模块必须导出以下函数：

```wat
;; 必须导出
(export "alloc" (func $alloc))     ;; fn(size: i32) -> i32  分配内存，返回指针
(export "dealloc" (func $dealloc)) ;; fn(ptr: i32, size: i32)  释放内存
(export "main" (func $main))       ;; fn(input_ptr: i32, input_len: i32) -> i32  主入口

;; main 返回值是指向 [ptr: i32, len: i32] 结构的指针（指向 JSON 输出字符串）
```

宿主提供的导入函数：

```wat
(import "env" "abort" (func $abort (param i32)))  ;; 错误中止
```

### 3.3 执行流程

```
┌─────────────────────────────────────────────────────┐
│ WasmSandbox::execute(request)                       │
│                                                      │
│ 1. Base64 解码 request.code → wasm_bytes             │
│ 2. 创建 wasmtime::Engine（带资源限制）                 │
│ 3. 编译 Module::new(engine, wasm_bytes)              │
│ 4. 创建 Store + Linker（仅注册 env.abort）            │
│ 5. 实例化 Instance                                   │
│ 6. 序列化 inputs → JSON string → UTF-8 bytes         │
│ 7. 调用 alloc(json_len) → input_ptr                  │
│ 8. 写入 JSON bytes 到 memory[input_ptr..+json_len]   │
│ 9. 调用 main(input_ptr, json_len) → result_ptr       │
│ 10. 从 memory[result_ptr] 读取 [out_ptr, out_len]    │
│ 11. 从 memory[out_ptr..+out_len] 读取 JSON bytes     │
│ 12. 反序列化 → serde_json::Value                     │
│ 13. 返回 SandboxResult                               │
└─────────────────────────────────────────────────────┘
```

### 3.4 资源限制

```rust
pub struct WasmSandboxConfig {
    /// 最大 WASM 二进制大小（字节），默认 5MB
    pub max_wasm_size: usize,
    /// 执行超时，默认 30s
    pub default_timeout: Duration,
    /// 最大线性内存页数（每页 64KB），默认 256 页 = 16MB
    pub max_memory_pages: u32,
    /// 最大燃料（指令计数），默认 1_000_000_000
    pub max_fuel: u64,
    /// 是否启用 fuel metering
    pub enable_fuel: bool,
}
```

通过 wasmtime 的 `StoreLimits` + fuel 机制实现：
- **内存限制**：`max_memory_pages` 控制最大线性内存
- **CPU 限制**：`fuel` 机制限制指令数（比 wall-clock timeout 更精确）
- **超时保底**：`tokio::time::timeout` 包裹整个执行

### 3.5 WasmSandbox 结构

```rust
pub struct WasmSandbox {
    engine: wasmtime::Engine,
    config: WasmSandboxConfig,
    stats: Arc<RwLock<SandboxStats>>,
}

#[async_trait]
impl CodeSandbox for WasmSandbox {
    fn sandbox_type(&self) -> SandboxType { SandboxType::Wasm }
    fn supported_languages(&self) -> Vec<CodeLanguage> { vec![CodeLanguage::Wasm] }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        // 在 spawn_blocking 中执行（wasmtime 是同步的）
    }

    async fn validate(&self, code: &str, _language: CodeLanguage) -> Result<(), SandboxError> {
        // 1. Base64 解码
        // 2. 验证 WASM magic number (\0asm)
        // 3. 验证大小限制
        // 4. 尝试编译验证格式正确性
    }
}
```

### 3.6 注册到 SandboxManager

```rust
// src/sandbox/manager.rs — SandboxManager::new() 中
let wasm_sandbox = Arc::new(WasmSandbox::new(config.wasm_config.clone()));
manager.register_sandbox(CodeLanguage::Wasm, wasm_sandbox);
```

```rust
// SandboxManagerConfig 新增
pub struct SandboxManagerConfig {
    pub builtin_config: BuiltinSandboxConfig,
    pub wasm_config: WasmSandboxConfig,  // ← 新增
}
```

### 3.7 CodeNodeExecutor 改动

`src/nodes/data_transform.rs` 中 `CodeNodeExecutor` 语言映射新增：

```rust
"wasm" => crate::sandbox::CodeLanguage::Wasm,
```

此项已在代码中存在但之前无后端实现，现在有了 WasmSandbox 自然生效。

---

## 四、插件模块（`src/plugin/`）

### 4.1 插件 Manifest（`plugin/manifest.rs`）

```rust
/// 插件元数据描述
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// 插件唯一标识（reverse domain 风格）
    pub id: String,                    // "com.example.my-plugin"
    /// 插件版本（semver）
    pub version: String,               // "1.0.0"
    /// 显示名称
    pub name: String,
    /// 描述
    pub description: String,
    /// 作者
    pub author: Option<String>,
    /// WASM 文件路径（相对于插件目录）
    pub wasm_file: String,             // "plugin.wasm"
    /// 声明的能力（权限模型）
    pub capabilities: PluginCapabilities,
    /// 插件提供的节点类型（可注册为自定义节点）
    pub node_types: Vec<PluginNodeType>,
    /// 插件提供的 Hook 点
    pub hooks: Vec<PluginHook>,
}

/// 能力/权限声明
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCapabilities {
    /// 是否可读取变量池
    pub read_variables: bool,
    /// 是否可写入变量池
    pub write_variables: bool,
    /// 是否可发送事件
    pub emit_events: bool,
    /// 是否可访问 HTTP（通过宿主代理）
    pub http_access: bool,
    /// 是否可访问文件系统（WASI 受限目录）
    pub fs_access: Option<Vec<String>>,  // 允许的目录列表
    /// 最大内存页数
    pub max_memory_pages: Option<u32>,
    /// 最大 fuel
    pub max_fuel: Option<u64>,
}

/// 插件提供的自定义节点类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginNodeType {
    /// 节点类型名（注册到 NodeExecutorRegistry）
    pub node_type: String,           // "my-plugin.custom-transform"
    /// 节点显示名
    pub label: String,
    /// 输入 schema（JSON Schema）
    pub input_schema: Option<Value>,
    /// 输出 schema（JSON Schema）
    pub output_schema: Option<Value>,
    /// WASM 导出函数名
    pub handler: String,             // "handle_custom_transform"
}

/// 插件 Hook
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHook {
    /// Hook 类型
    pub hook_type: PluginHookType,
    /// WASM 导出函数名
    pub handler: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PluginHookType {
    /// 工作流开始前
    BeforeWorkflowRun,
    /// 工作流完成后
    AfterWorkflowRun,
    /// 节点执行前
    BeforeNodeExecute,
    /// 节点执行后
    AfterNodeExecute,
    /// 变量写入前（拦截/修改）
    BeforeVariableWrite,
}
```

Manifest 文件为 JSON 格式，放在插件目录下：

```
plugins/
  com.example.my-plugin/
    manifest.json
    plugin.wasm
  com.example.another/
    manifest.json
    plugin.wasm
```

### 4.2 Host Functions（`plugin/host_functions.rs`）

插件 WASM 模块可调用的宿主 API，通过 wasmtime Linker 注入：

```rust
/// 宿主函数模块名
const HOST_MODULE: &str = "xworkflow";

/// 注册所有 Host Functions 到 Linker
pub fn register_host_functions(
    linker: &mut Linker<PluginState>,
) -> Result<(), PluginError> {
    // --- 变量池操作 ---
    register_var_get(linker)?;       // xworkflow.var_get(key_ptr, key_len) -> (ptr, len)
    register_var_set(linker)?;       // xworkflow.var_set(key_ptr, key_len, val_ptr, val_len) -> i32

    // --- 日志 ---
    register_log(linker)?;           // xworkflow.log(level, msg_ptr, msg_len)

    // --- 事件 ---
    register_emit_event(linker)?;    // xworkflow.emit_event(event_ptr, event_len) -> i32

    // --- HTTP（宿主代理） ---
    register_http_request(linker)?;  // xworkflow.http_request(req_ptr, req_len) -> (ptr, len)

    Ok(())
}
```

#### Host Function 详细定义

```
┌──────────────────────────────────────────────────────────────┐
│ Host Functions (import "xworkflow" ...)                       │
├──────────────────────────────────────────────────────────────┤
│ var_get(selector_ptr, selector_len) -> (result_ptr, len)     │
│   从变量池读取值，selector 为 JSON 数组 ["node_id", "key"]    │
│   返回 JSON 序列化的值                                        │
│                                                               │
│ var_set(selector_ptr, selector_len, value_ptr, value_len)    │
│   -> status: i32 (0=成功, -1=无权限, -2=序列化错误)           │
│   写入变量池                                                  │
│                                                               │
│ log(level: i32, msg_ptr, msg_len)                            │
│   level: 0=trace, 1=debug, 2=info, 3=warn, 4=error          │
│   写入 tracing 日志                                           │
│                                                               │
│ emit_event(event_ptr, event_len) -> status: i32              │
│   发送 GraphEngineEvent::PluginEvent(data)                   │
│   event 为 JSON 格式: {"type": "...", "data": ...}           │
│                                                               │
│ http_request(req_ptr, req_len) -> (result_ptr, result_len)   │
│   req: {"method":"GET","url":"...","headers":{},"body":""}   │
│   result: {"status":200,"headers":{},"body":"..."}           │
│   由宿主通过 reqwest 代理执行，受能力声明控制                   │
└──────────────────────────────────────────────────────────────┘
```

#### PluginState（Store 中的状态）

```rust
/// 每个插件实例的运行时状态
pub struct PluginState {
    /// WASI context（提供文件系统、环境变量等）
    pub wasi: WasiCtx,
    /// 变量池引用（只读或读写，取决于能力）
    pub variable_pool: Option<Arc<RwLock<VariablePool>>>,
    /// 事件发送器
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    /// 能力声明（运行时权限检查）
    pub capabilities: PluginCapabilities,
    /// 内存限制
    pub limiter: StoreLimitsBuilder,
    /// 日志前缀
    pub plugin_id: String,
    /// 已分配的宿主侧缓冲区（供返回数据用）
    pub host_buffers: Vec<Vec<u8>>,
}
```

### 4.3 Plugin Runtime（`plugin/runtime.rs`）

单个插件实例的管理：

```rust
/// 插件运行时实例
pub struct PluginRuntime {
    manifest: PluginManifest,
    engine: wasmtime::Engine,
    module: wasmtime::Module,
    linker: wasmtime::Linker<PluginState>,
    status: PluginStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PluginStatus {
    Loaded,       // WASM 已加载编译
    Ready,        // 初始化完成，可接受调用
    Running,      // 正在执行
    Error(String),
    Unloaded,
}

impl PluginRuntime {
    /// 从 manifest 和 wasm 字节创建
    pub fn new(manifest: PluginManifest, wasm_bytes: &[u8]) -> Result<Self, PluginError>;

    /// 调用插件导出函数（通用入口）
    pub fn call_function(
        &self,
        function_name: &str,
        input: &Value,
        state: PluginState,
    ) -> Result<Value, PluginError>;

    /// 执行插件定义的节点处理
    pub async fn execute_node(
        &self,
        node_type: &PluginNodeType,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError>;

    /// 执行 Hook
    pub async fn execute_hook(
        &self,
        hook_type: PluginHookType,
        payload: &Value,
        variable_pool: Arc<RwLock<VariablePool>>,
        event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    ) -> Result<Option<Value>, PluginError>;
}
```

### 4.4 Plugin Manager（`plugin/manager.rs`）

```rust
/// 插件管理器 — 发现、加载、生命周期管理
pub struct PluginManager {
    /// 已加载插件（plugin_id → runtime）
    plugins: HashMap<String, Arc<PluginRuntime>>,
    /// 节点类型 → 插件 ID 映射（用于 NodeExecutor 路由）
    node_type_registry: HashMap<String, String>,
    /// Hook 注册表
    hook_registry: HashMap<PluginHookType, Vec<(String, String)>>,  // (plugin_id, handler)
    /// wasmtime Engine（所有插件共享）
    engine: wasmtime::Engine,
    /// 配置
    config: PluginManagerConfig,
}

pub struct PluginManagerConfig {
    /// 插件目录路径
    pub plugin_dir: PathBuf,
    /// 是否自动发现和加载
    pub auto_discover: bool,
    /// 全局默认资源限制
    pub default_max_memory_pages: u32,
    pub default_max_fuel: u64,
    /// 允许的能力白名单
    pub allowed_capabilities: AllowedCapabilities,
}

impl PluginManager {
    pub fn new(config: PluginManagerConfig) -> Result<Self, PluginError>;

    /// 扫描插件目录，发现并加载所有插件
    pub fn discover_and_load(&mut self) -> Result<Vec<String>, PluginError>;

    /// 手动加载单个插件（从目录路径）
    pub fn load_plugin(&mut self, plugin_dir: &Path) -> Result<String, PluginError>;

    /// 卸载插件
    pub fn unload_plugin(&mut self, plugin_id: &str) -> Result<(), PluginError>;

    /// 获取插件提供的所有自定义节点类型
    pub fn get_plugin_node_types(&self) -> Vec<(String, PluginNodeType)>;

    /// 获取指定节点类型对应的插件 runtime
    pub fn get_executor_for_node_type(&self, node_type: &str) -> Option<Arc<PluginRuntime>>;

    /// 执行指定 Hook 类型的所有已注册 handler
    pub async fn execute_hooks(
        &self,
        hook_type: PluginHookType,
        payload: &Value,
        variable_pool: Arc<RwLock<VariablePool>>,
        event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    ) -> Result<Vec<Value>, PluginError>;

    /// 列出所有已加载插件
    pub fn list_plugins(&self) -> Vec<&PluginManifest>;

    /// 获取插件状态
    pub fn get_plugin_status(&self, plugin_id: &str) -> Option<PluginStatus>;
}
```

### 4.5 插件节点执行器（`plugin/mod.rs`）

```rust
/// 插件节点执行器 — 桥接 NodeExecutor trait 与 PluginRuntime
pub struct PluginNodeExecutor {
    plugin_manager: Arc<PluginManager>,
}

#[async_trait]
impl NodeExecutor for PluginNodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let node_type = config.get("plugin_node_type")
            .and_then(|v| v.as_str())
            .ok_or(NodeError::ConfigError("Missing plugin_node_type".into()))?;

        let runtime = self.plugin_manager
            .get_executor_for_node_type(node_type)
            .ok_or(NodeError::ConfigError(format!("No plugin for node type: {}", node_type)))?;

        let plugin_node = runtime.manifest.node_types
            .iter()
            .find(|n| n.node_type == node_type)
            .ok_or(NodeError::ConfigError("Node type not found in plugin".into()))?;

        runtime.execute_node(plugin_node, config, variable_pool, context).await
    }
}
```

---

## 五、与工作流引擎的集成

### 5.1 WorkflowRunner 集成

```rust
// src/scheduler.rs — WorkflowRunnerBuilder 新增
impl WorkflowRunnerBuilder {
    /// 设置插件管理器
    pub fn plugin_manager(mut self, pm: Arc<PluginManager>) -> Self;
}
```

WorkflowRunner 在 `run()` 时：
1. 如果有 plugin_manager，遍历 `get_plugin_node_types()`，将每个插件节点注册到 `NodeExecutorRegistry`
2. 在 dispatcher 启动前后触发 `BeforeWorkflowRun` / `AfterWorkflowRun` hooks
3. dispatcher 执行每个节点前后触发 `BeforeNodeExecute` / `AfterNodeExecute` hooks

### 5.2 NodeExecutorRegistry 集成

```rust
// src/nodes/executor.rs — NodeExecutorRegistry::new_with_plugins()
impl NodeExecutorRegistry {
    pub fn new_with_plugins(plugin_manager: Arc<PluginManager>) -> Self {
        let mut registry = Self::new(); // 先注册内置节点

        // 注册插件节点
        for (node_type, _) in plugin_manager.get_plugin_node_types() {
            registry.register(
                &node_type,
                Box::new(PluginNodeExecutor {
                    plugin_manager: plugin_manager.clone(),
                }),
            );
        }

        registry
    }
}
```

### 5.3 Dispatcher Hook 集成

```rust
// src/core/dispatcher.rs — 新增 hook 调用点
impl WorkflowDispatcher {
    async fn run(&mut self) -> Result<...> {
        // ← BeforeWorkflowRun hook
        if let Some(pm) = &self.plugin_manager {
            pm.execute_hooks(PluginHookType::BeforeWorkflowRun, &payload, ...).await?;
        }

        // 主循环
        while let Some(node) = queue.pop_front() {
            // ← BeforeNodeExecute hook
            if let Some(pm) = &self.plugin_manager {
                pm.execute_hooks(PluginHookType::BeforeNodeExecute, &node_info, ...).await?;
            }

            let result = executor.execute(...).await;

            // ← AfterNodeExecute hook
            if let Some(pm) = &self.plugin_manager {
                pm.execute_hooks(PluginHookType::AfterNodeExecute, &node_result, ...).await?;
            }
        }

        // ← AfterWorkflowRun hook
        if let Some(pm) = &self.plugin_manager {
            pm.execute_hooks(PluginHookType::AfterWorkflowRun, &final_result, ...).await?;
        }
    }
}
```

### 5.4 事件系统扩展

```rust
// src/core/mod.rs — GraphEngineEvent 新增
pub enum GraphEngineEvent {
    // ... 现有事件
    PluginLoaded { plugin_id: String, name: String },
    PluginUnloaded { plugin_id: String },
    PluginEvent { plugin_id: String, event_type: String, data: Value },
    PluginError { plugin_id: String, error: String },
}
```

---

## 六、安全模型

### 6.1 代码节点沙箱（最高隔离）

- 无 WASI（无文件/网络）
- 无 Host Functions（除 `abort`）
- 仅通过线性内存传递 JSON
- fuel 限制 + 内存限制 + 超时

### 6.2 插件安全（能力授权模型）

```
┌─────────────────────────────────────────────────────┐
│ 插件加载流程                                         │
│                                                      │
│ 1. 读取 manifest.json                                │
│ 2. 检查 capabilities 是否在 allowed_capabilities 内   │
│ 3. 拒绝超出授权的能力                                 │
│ 4. 仅注册 manifest 中声明的 Host Functions            │
│ 5. WASI 配置仅开放声明的目录（preopened dirs）         │
│ 6. 运行时每次 Host Function 调用检查能力               │
└─────────────────────────────────────────────────────┘
```

能力检查示例：

```rust
// host_functions.rs 中
fn var_set_impl(mut caller: Caller<'_, PluginState>, ...) -> i32 {
    let state = caller.data();
    if !state.capabilities.write_variables {
        return -1; // 无权限
    }
    // ... 执行写入
}
```

### 6.3 资源隔离

| 资源 | 代码节点 | 插件 |
|------|---------|------|
| 内存 | 16MB (256 pages) | manifest 声明，上限 64MB |
| CPU (fuel) | 1B instructions | manifest 声明，上限 10B |
| 超时 | 30s | 60s |
| 实例数 | 每次执行创建新实例 | 可复用实例 |

---

## 七、插件 WASM 接口约定

### 7.1 插件必须导出的函数

```wat
;; 内存管理（必须）
(export "alloc" (func $alloc))           ;; fn(size: i32) -> i32
(export "dealloc" (func $dealloc))       ;; fn(ptr: i32, size: i32)

;; 插件生命周期（可选）
(export "plugin_init" (func $init))       ;; fn() -> i32 (0=成功)
(export "plugin_destroy" (func $destroy)) ;; fn()

;; 节点处理函数（按 manifest 中 node_types[].handler 声明）
(export "handle_xxx" (func $handle))     ;; fn(input_ptr: i32, input_len: i32) -> i32

;; Hook 处理函数（按 manifest 中 hooks[].handler 声明）
(export "on_before_run" (func $hook))    ;; fn(payload_ptr: i32, payload_len: i32) -> i32
```

### 7.2 数据传递协议

所有数据通过线性内存以 JSON UTF-8 字节传递：

**输入**：宿主 → 插件
1. 宿主调用 `alloc(json_len)` 获取 ptr
2. 宿主写入 JSON bytes 到 `memory[ptr..ptr+json_len]`
3. 宿主调用 `handler(ptr, json_len)`

**输出**：插件 → 宿主
1. handler 返回 result_ptr
2. result_ptr 指向 `[out_ptr: i32, out_len: i32]`（8 bytes）
3. 宿主从 `memory[out_ptr..out_ptr+out_len]` 读取 JSON bytes

---

## 八、错误类型（`plugin/error.rs`）

```rust
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Plugin not found: {0}")]
    NotFound(String),

    #[error("Invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("WASM compilation error: {0}")]
    CompilationError(String),

    #[error("WASM instantiation error: {0}")]
    InstantiationError(String),

    #[error("WASM execution error: {0}")]
    ExecutionError(String),

    #[error("Capability denied: {0}")]
    CapabilityDenied(String),

    #[error("Plugin timeout")]
    Timeout,

    #[error("Memory limit exceeded")]
    MemoryLimitExceeded,

    #[error("Fuel exhausted (instruction limit)")]
    FuelExhausted,

    #[error("Missing export function: {0}")]
    MissingExport(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Plugin already loaded: {0}")]
    AlreadyLoaded(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}
```

---

## 九、文件改动清单

### 新增文件

| 文件 | 说明 |
|------|------|
| `src/sandbox/wasm_sandbox.rs` | WasmSandbox — CodeSandbox 实现 |
| `src/plugin/mod.rs` | 插件模块声明 + PluginNodeExecutor |
| `src/plugin/manifest.rs` | PluginManifest + 能力声明 |
| `src/plugin/host_functions.rs` | Host Functions 定义与注册 |
| `src/plugin/runtime.rs` | PluginRuntime — 单个插件实例 |
| `src/plugin/manager.rs` | PluginManager — 发现/加载/生命周期 |
| `src/plugin/error.rs` | PluginError |

### 修改文件

| 文件 | 改动 |
|------|------|
| `Cargo.toml` | 添加 wasmtime、wasmtime-wasi 依赖 |
| `src/sandbox/mod.rs` | 添加 `pub mod wasm_sandbox;` + re-export |
| `src/sandbox/manager.rs` | SandboxManagerConfig 新增 wasm_config，注册 WasmSandbox |
| `src/lib.rs` | 添加 `pub mod plugin;` + 导出 |
| `src/nodes/executor.rs` | 新增 `new_with_plugins()` 方法 |
| `src/nodes/data_transform.rs` | CodeNodeExecutor 语言映射新增 `"wasm"` |
| `src/scheduler.rs` | WorkflowRunnerBuilder 新增 `plugin_manager()` |
| `src/core/dispatcher.rs` | 持有 `Option<Arc<PluginManager>>`，Hook 调用点 |
| `src/core/event_bus.rs` | GraphEngineEvent 新增 Plugin 相关事件 |

---

## 十、实施步骤

### Phase 1：WASM 代码节点沙箱
1. `Cargo.toml` — 添加 wasmtime 依赖
2. `src/sandbox/wasm_sandbox.rs` — 实现 WasmSandbox
3. `src/sandbox/mod.rs` — 导出
4. `src/sandbox/manager.rs` — 注册 WasmSandbox
5. `src/nodes/data_transform.rs` — CodeNodeExecutor 语言映射
6. 单元测试 — 基本 WASM 执行、资源限制、超时

### Phase 2：插件基础设施
7. `src/plugin/error.rs` — 错误类型
8. `src/plugin/manifest.rs` — Manifest 定义与解析
9. `src/plugin/host_functions.rs` — Host Functions 实现
10. `src/plugin/runtime.rs` — PluginRuntime 实现
11. `src/plugin/manager.rs` — PluginManager 实现
12. `src/plugin/mod.rs` — 模块组织 + PluginNodeExecutor

### Phase 3：引擎集成
13. `src/lib.rs` — 导出 plugin 模块
14. `src/nodes/executor.rs` — `new_with_plugins()`
15. `src/scheduler.rs` — builder 接受 plugin_manager
16. `src/core/dispatcher.rs` — Hook 调用点
17. `src/core/event_bus.rs` — 事件系统扩展

### Phase 4：测试
18. WASM 沙箱集成测试（编译简单 .wasm 模块测试）
19. 插件加载/卸载测试
20. Host Functions 测试
21. 端到端：自定义插件节点在工作流中执行

---

## 十一、测试用例

### WASM 沙箱测试

| 测试名 | 验证点 |
|--------|--------|
| `test_wasm_sandbox_basic` | 加载+执行简单 WASM 模块，inputs → outputs |
| `test_wasm_sandbox_fuel_limit` | 死循环被 fuel 中断 |
| `test_wasm_sandbox_memory_limit` | 超出内存限制返回错误 |
| `test_wasm_sandbox_timeout` | 超时返回 ExecutionTimeout |
| `test_wasm_sandbox_invalid_wasm` | 非法 WASM 字节返回 CompilationError |
| `test_wasm_sandbox_missing_export` | 缺少 main 导出返回错误 |

### 插件系统测试

| 测试名 | 验证点 |
|--------|--------|
| `test_plugin_load_manifest` | 正确解析 manifest.json |
| `test_plugin_discover` | 自动发现插件目录 |
| `test_plugin_capability_deny` | 未授权能力被拒绝 |
| `test_plugin_host_var_get` | 插件能通过 host function 读变量池 |
| `test_plugin_host_var_set` | 插件能通过 host function 写变量池 |
| `test_plugin_custom_node` | 插件节点在工作流中被正确调用 |
| `test_plugin_hook_before_run` | BeforeWorkflowRun hook 被触发 |
| `test_plugin_unload` | 卸载后节点不可用 |

---

## 十二、验证方式

```bash
# 全量测试
cargo test

# WASM 沙箱测试
cargo test wasm_sandbox

# 插件系统测试
cargo test plugin

# 单个测试
cargo test test_wasm_sandbox_basic
cargo test test_plugin_custom_node
```
