# xworkflow 安全体系设计

## 1. 背景与威胁模型

### 1.1 项目背景

xworkflow 是基于 Tokio 异步运行时的 DAG 工作流引擎库，支持 JS（boa_engine）、WASM（wasmtime）多语言沙箱和插件系统。作为基础库，xworkflow 本身不感知"租户"或"用户"等上层概念，但需要为集成平台提供足够的安全原语，使其能在面对不可信输入时安全运行。

库提供 **资源组（ResourceGroup）** 作为隔离和治理的基本单位。平台可自由将其映射到自身的租户、用户、项目等业务概念。同一资源组内的工作流共享配额、限制和配置。

### 1.2 当前安全审计结论

| 方面 | 状态 | 风险等级 | 说明 |
|------|------|----------|------|
| 工作流隔离 | ✅ 已防护 | 低 | 每个 workflow 独立 Graph/Pool/Registry |
| WASM 沙箱 | ✅ 已防护 | 低 | fuel 计量 + 内存限制（256 页 / 16MB）+ 无 WASI |
| 插件隔离 | ✅ 已防护 | 低 | WASM 沙箱内运行，能力声明制 |
| 错误信息泄露 | ✅ 已防护 | 低 | HTTP body 截断 512 字符 |
| 文件 I/O | ✅ 已防护 | 低 | 工作流执行无文件系统访问 |
| HTTP 节点 SSRF | ❌ 无防护 | **严重** | `HttpRequestExecutor` 无任何 URL 过滤 |
| JS 沙箱 | ⚠️ 不足 | **高** | 正则检测可绕过，无内存限制 |
| 资源组隔离 | ❌ 无防护 | **高** | `RuntimeContext` 无分组信息，无资源配额 |
| 并发限制 | ❌ 无防护 | **高** | `tokio::spawn` 无并发工作流数量限制 |
| 变量池大小 | ❌ 无防护 | **高** | `VariablePool` 可无限增长 |
| HTTP 响应体 | ❌ 无防护 | **中** | 响应体无大小限制，可耗尽内存 |
| LLM Token | ❌ 无防护 | **高** | 无 token 预算限制 |
| 模板注入 | ⚠️ 不足 | **中** | minijinja 无沙箱限制 |
| DSL 验证 | ⚠️ 不足 | **中** | 无环检测，无 schema 深度验证 |

### 1.3 威胁角色

从库的视角，威胁来源是**不可信的输入**，而非具体的"租户"或"用户"角色。平台负责认证与授权，库负责执行层的安全隔离。

| 威胁来源 | 能力 | 风险 |
|----------|------|------|
| **恶意 DSL 输入** | 可构造任意 DSL、代码节点、HTTP 配置 | SSRF、沙箱逃逸、资源耗尽 |
| **被滥用的凭证** | 持有合法 API 凭证，可正常调用 | Token 滥用、配额耗尽 |
| **错误的平台配置** | 平台集成时安全策略配置不当 | 意外暴露内部网络、资源无限制 |

### 1.4 攻击面分析

```
                    ┌──────────────┐
                    │   DSL 输入   │ ← 恶意 YAML/JSON
                    └──────┬───────┘
                           ▼
                ┌──────────────────┐
                │   DSL 解析/验证   │ ← 环路注入、超大节点图
                └──────┬───────────┘
                       ▼
          ┌────────────────────────┐
          │     工作流执行引擎      │
          │  ┌──────┐ ┌─────────┐ │
          │  │ JS   │ │  WASM   │ │ ← 沙箱逃逸、资源耗尽
          │  │ 沙箱  │ │  沙箱   │ │
          │  └──────┘ └─────────┘ │
          │  ┌──────┐ ┌─────────┐ │
          │  │ HTTP │ │  LLM    │ │ ← SSRF、Token 滥用
          │  │ 请求  │ │  调用   │ │
          │  └──────┘ └─────────┘ │
          │  ┌──────────────────┐  │
          │  │   变量池/模板    │  │ ← 内存膨胀、模板注入
          │  └──────────────────┘  │
          └────────────────────────┘
```

**四大攻击类别：**

1. **代码执行** — JS 沙箱逃逸、WASM 恶意代码、模板注入
2. **网络请求** — SSRF 访问内网/云元数据、DNS 重绑定
3. **资源耗尽** — 无限循环、内存膨胀、并发爆炸、Token 滥用
4. **数据泄露** — 跨资源组变量读取、凭证泄露、错误信息泄露

---

## 2. 安全层次架构

```
┌───────────────────────────────────────────────────┐
│ L5: 审计与可观测性（Audit & Observability）       │
├───────────────────────────────────────────────────┤
│ L4: 资源组隔离与资源治理（Resource Group Gov.）   │
├───────────────────────────────────────────────────┤
│ L3: 网络安全（Network Security / SSRF）           │
├───────────────────────────────────────────────────┤
│ L2: 执行沙箱加固（Sandbox Hardening）             │
├───────────────────────────────────────────────────┤
│ L1: 输入验证与 DSL 安全（Input Validation）       │
└───────────────────────────────────────────────────┘
```

每层独立防护、纵深叠加。低层失效时高层仍可拦截。

---

## 3. 设计原则：零成本抽象

安全功能作为库的可选能力，遵循 **"不用不付费"（you don't pay for what you don't use）** 原则。关闭安全功能时不引入任何运行时开销；开启安全功能但使用低安全级别时，未启用的检查项同样零开销。

### 3.1 核心原则

1. **编译期可裁剪** — Cargo feature `security` 控制安全模块是否参与编译，不启用时零代码体积
2. **组件级 `Option`** — 安全策略中每个检查组件（网络、模板、配额等）独立为 `Option<T>`。`None` = 该项检查完全不存在，运行时不进入任何检查逻辑
3. **不用"放松的配置"代替"关闭"** — Permissive 模式 ≠ "配置了一套宽松参数的检查"。它应该是组件级 `None`，让检查点直接跳过，避免进入检查函数后再逐条短路
4. **内联快速路径** — 安全检查点的 `Option` 守卫标注 `#[inline]`，确保编译器可内联消除

### 3.2 问题示例：为什么"放松的配置" ≠ 零成本

错误设计：Permissive 模式创建 `NetworkPolicy { block_private_ips: false, ... }`：

```
validate_url() 被调用 →
  url::Url::parse()                   ← 不必要的堆分配
  allowed_schemes.contains()          ← 不必要的 Vec 遍历
  SafeDnsResolver::resolve_and_validate()
    tokio::net::lookup_host()         ← 触发不必要的 DNS 系统调用！
    is_blocked_ip()                   ← 三个 flag 都是 false，但 DNS 已经发生了
```

即使所有 flag 设为 false，运行时仍执行了 URL 解析、DNS 查询等 I/O 操作。

正确设计：Permissive 模式将 `network` 设为 `None`：

```
check_security_before_node() →
  policy.network 是 None → 直接跳过，一次分支判断
```

### 3.3 Cargo Feature 设计

```toml
[features]
default = ["security"]
security = []  # 启用安全模块
```

不启用 `security` feature 时：
- `SecurityPolicy`、`ResourceGroup` 等类型不存在
- `RuntimeContext` 不包含安全相关字段
- Dispatcher 执行循环无安全检查点
- 编译产物不增加任何代码体积

### 3.4 SecurityPolicy 组件级 Option 设计

```rust
/// 统一安全策略 — 每个组件独立 Option，None = 不检查
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    /// 安全级别（仅用于日志/审计标注，不驱动运行时逻辑）
    pub level: SecurityLevel,
    /// 网络策略 — None = 不做任何 URL/IP/DNS 检查
    pub network: Option<NetworkPolicy>,
    /// 模板安全配置 — None = 不限制模板
    pub template: Option<TemplateSafetyConfig>,
    /// DSL 验证配置 — None = 仅基础验证
    pub dsl_validation: Option<DslValidationConfig>,
    /// 节点资源限制 — 空 HashMap = 不限制
    pub node_limits: HashMap<String, NodeResourceLimits>,
    /// 审计日志 — None = 不记录
    pub audit_logger: Option<Arc<dyn AuditLogger>>,
}
```

**关键**：`SecurityLevel` 仅作为元信息（日志标注、审计分类），**不参与任何运行时分支判断**。所有运行时行为完全由各组件的 `Option` 控制。

### 3.5 检查点实现

```rust
/// Dispatcher 安全检查 — 组件级 Option 守卫
#[inline]
async fn check_security_before_node(
    context: &RuntimeContext,
    node_id: &str,
    node_type: &str,
) -> Result<(), NodeError> {
    // 无 security_policy 时直接返回
    let Some(policy) = &context.security_policy else {
        return Ok(());
    };

    // 网络检查 — 仅在 network 为 Some 时执行
    if node_type == "http_request" || node_type == "llm" {
        if let Some(network) = &policy.network {
            // 此处才做 URL 解析、DNS 查询等操作
            // ...
        }
    }

    // 配额检查 — 仅在 governor 为 Some 时执行
    if let Some(governor) = &context.resource_governor {
        if let Some(group) = &context.resource_group {
            match node_type {
                "http_request" => governor.check_http_rate(&group.group_id).await?,
                "llm" => governor.check_llm_request(&group.group_id, 0).await?,
                _ => {}
            }
        }
    }

    Ok(())
}

/// 输出大小检查 — 仅在配置了 node_limits 时执行
#[inline]
fn check_output_size(
    policy: Option<&SecurityPolicy>,
    node_type: &str,
    output_size: usize,
) -> Result<(), NodeError> {
    let Some(policy) = policy else { return Ok(()) };
    let Some(limits) = policy.node_limits.get(node_type) else { return Ok(()) };
    if output_size > limits.max_output_bytes {
        return Err(NodeError::OutputTooLarge { /* ... */ });
    }
    Ok(())
}
```

### 3.6 审计日志零成本

```rust
/// 审计日志宏 — 无 logger 时编译为空操作
macro_rules! audit_log {
    ($context:expr, $event:expr) => {
        if let Some(logger) = &$context.audit_logger {
            logger.log_event($event).await;
        }
    };
}
```

### 3.7 性能对比

| 配置 | 每节点额外开销 | 说明 |
|------|----------------|------|
| 不编译 `security` feature | **零** | 安全代码不存在于二进制中 |
| 编译但不调用 `.security_policy()` | **1 次 `Option::is_none()` 判断** | `context.security_policy` 为 `None` |
| `SecurityPolicy::permissive()` | **1 次 `Option::is_none()` 判断** | 各组件均为 `None` |
| `SecurityPolicy::standard()` | **网络节点**: URL 解析 + DNS 查询 + IP 检查<br>**其他节点**: 输出大小比较 | 仅启用的组件产生开销 |
| `SecurityPolicy::strict()` + `ResourceGovernor` | 完整安全检查 | 全部组件激活，按需付费 |

### 3.8 对 RuntimeContext 的影响

```rust
#[derive(Clone)]
pub struct RuntimeContext {
    // --- 现有字段（不变） ---
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,

    // --- 安全字段（全部 Option，未配置时为 None） ---
    #[cfg(feature = "security")]
    pub resource_group: Option<ResourceGroup>,
    #[cfg(feature = "security")]
    pub security_policy: Option<SecurityPolicy>,
    #[cfg(feature = "security")]
    pub resource_governor: Option<Arc<dyn ResourceGovernor>>,
    #[cfg(feature = "security")]
    pub credential_provider: Option<Arc<dyn CredentialProvider>>,
    #[cfg(feature = "security")]
    pub audit_logger: Option<Arc<dyn AuditLogger>>,
}
```

---

## 4. L1: 输入验证与 DSL 安全

**目标**：在工作流执行之前拦截恶意输入。

### 4.1 DSL Schema 验证增强

**当前问题**：`validate_schema()` 仅做基本结构检查，缺少环检测和节点配置深度验证。

**改进方案**：

```rust
/// DSL 验证配置
pub struct DslValidationConfig {
    /// 最大节点数量
    pub max_nodes: usize,                // 默认 200
    /// 最大边数量
    pub max_edges: usize,                // 默认 500
    /// 最大嵌套深度（迭代/条件）
    pub max_nesting_depth: usize,        // 默认 10
    /// 是否启用环检测
    pub enable_cycle_detection: bool,    // 默认 true
    /// 节点 ID 最大长度
    pub max_node_id_length: usize,       // 默认 128
    /// 单个节点配置最大字节数
    pub max_node_config_bytes: usize,    // 默认 64KB
}
```

**环检测**：在 `build_graph()` 阶段使用拓扑排序（Kahn 算法），检测到环路时返回包含环路节点列表的 `ValidationError`。

**配置大小限制**：对每个节点的 `config` JSON 做大小检查，防止超大配置导致解析耗时。

### 4.2 代码节点 AST 级安全分析

**当前问题**：`src/sandbox/builtin.rs:67-82` 使用正则匹配危险模式，容易绕过。

```rust
// 当前实现 — 正则可被绕过
let dangerous_patterns = [
    (r"eval\s*\(", "eval()"),       // 可用 eval/**/(x) 绕过
    (r"Function\s*\(", "Function()"),
    (r"__proto__", "__proto__"),     // 可用 Unicode 转义绕过
    // ...
];
```

**改进方案**：使用 boa_engine 的 AST 解析能力做语法树级分析。

```rust
/// 代码安全分析结果
pub struct CodeAnalysisResult {
    /// 是否安全
    pub is_safe: bool,
    /// 发现的违规项
    pub violations: Vec<CodeViolation>,
}

pub struct CodeViolation {
    pub kind: ViolationKind,
    pub location: (usize, usize),  // (行, 列)
    pub description: String,
}

pub enum ViolationKind {
    /// 动态代码执行（eval, Function 构造器）
    DynamicExecution,
    /// 原型链操纵（__proto__, constructor）
    PrototypeTampering,
    /// 禁止的全局访问（process, require, import）
    ForbiddenGlobal,
    /// 无限循环风险（while(true) 等）
    InfiniteLoopRisk,
}

/// AST 安全分析器 trait
pub trait CodeAnalyzer: Send + Sync {
    fn analyze(&self, code: &str) -> Result<CodeAnalysisResult, SandboxError>;
}
```

AST 分析器遍历语法树，检测：
- `CallExpression` 调用目标为 `eval` / `Function`
- `MemberExpression` 访问 `__proto__` / `constructor`
- 标识符引用 `process` / `require` / `globalThis`
- `WhileStatement` / `ForStatement` 缺少明确终止条件

### 4.3 模板注入防护

**当前问题**：minijinja 模板渲染无安全限制，恶意模板可能执行复杂逻辑或触发拒绝服务。

**改进方案**：

```rust
/// 模板安全配置
pub struct TemplateSafetyConfig {
    /// 模板最大长度（字符）
    pub max_template_length: usize,      // 默认 10_000
    /// 最大渲染输出长度（字符）
    pub max_output_length: usize,        // 默认 100_000
    /// 最大递归深度
    pub max_recursion_depth: u32,        // 默认 5
    /// 禁用的过滤器
    pub disabled_filters: Vec<String>,   // 如 ["debug"]
    /// 禁用的函数
    pub disabled_functions: Vec<String>, // 如 ["range"]
    /// 最大循环迭代次数
    pub max_loop_iterations: usize,      // 默认 1000
}
```

配置 minijinja `Environment`：
- 设置 `fuel` 限制（minijinja 原生支持）控制模板计算量
- 移除 `debug` 等可能泄露信息的过滤器
- 限制 `range` 函数的最大值，防止内存膨胀

### 4.4 变量选择器白名单

限制变量引用路径的深度和来源范围：

```rust
/// 变量选择器验证
pub struct SelectorValidation {
    /// 允许的变量来源前缀
    pub allowed_prefixes: Vec<String>,  // ["sys", "env", 具体节点 ID]
    /// 最大选择器深度
    pub max_depth: usize,              // 默认 5
    /// 最大选择器长度（字符）
    pub max_length: usize,             // 默认 256
}
```

在 DSL 验证阶段，遍历所有节点配置中的变量引用，检查：
- 引用的源节点是否在 DAG 中存在
- 引用路径深度是否超限
- 引用前缀是否在白名单中

---

## 5. L2: 执行沙箱加固

**目标**：限制代码执行的资源消耗和能力边界。

### 5.1 JS 沙箱加固

**当前问题**：
- boa_engine `Context::default()` 创建，未冻结全局对象
- 无内存使用量估算
- 无输出大小限制

**改进方案**：

```rust
/// JS 沙箱安全配置（扩展现有 SandboxConfig）
pub struct JsSandboxSecurityConfig {
    // --- 现有配置 ---
    pub max_code_length: usize,       // 已有
    pub default_timeout: Duration,    // 已有

    // --- 新增安全配置 ---
    /// 估算内存上限（字节）
    pub max_memory_estimate: usize,   // 默认 32MB
    /// 输出 JSON 最大字节数
    pub max_output_bytes: usize,      // 默认 1MB
    /// 全局对象冻结模式
    pub freeze_globals: bool,         // 默认 true
    /// API 白名单（仅允许的全局函数/对象）
    pub allowed_globals: Vec<String>, // ["JSON", "Math", "parseInt", ...]
}
```

**全局对象冻结**：创建 boa `Context` 后，执行 `Object.freeze(globalThis)` 等初始化脚本，防止全局对象被修改。

**API 白名单**：

```rust
/// JS 沙箱允许的全局 API
const JS_ALLOWED_GLOBALS: &[&str] = &[
    // 数据处理
    "JSON", "Math", "parseInt", "parseFloat", "isNaN", "isFinite",
    "Number", "String", "Boolean", "Array", "Object",
    // 编码
    "encodeURIComponent", "decodeURIComponent", "encodeURI", "decodeURI",
    "btoa", "atob",
    // 正则
    "RegExp",
    // 日期（只读）
    "Date",
    // xworkflow 注入
    "inputs", "outputs",
];
```

非白名单的全局属性在 Context 初始化后删除或设为 undefined。

**内存估算**：boa_engine 不提供精确内存计量，采用启发式方法：
- 监控输出 JSON 序列化大小
- 对输入数据大小做预检查
- 设置执行超时作为兜底

### 5.2 WASM 沙箱校准

**当前状态**（`src/sandbox/wasm_sandbox.rs`）：已有较好的限制配置。

```rust
// 当前默认值
WasmSandboxConfig {
    max_wasm_size: 5 * 1024 * 1024,   // 5MB
    max_memory_pages: 256,             // 16MB
    max_fuel: 1_000_000_000,           // 10 亿指令
    enable_fuel: true,
}
```

**补充限制**：

```rust
/// WASM 沙箱安全增强
pub struct WasmSecurityConfig {
    /// 输入 JSON 最大字节数
    pub max_input_bytes: usize,    // 默认 1MB
    /// 输出 JSON 最大字节数
    pub max_output_bytes: usize,   // 默认 1MB
    /// 是否允许 WASI（始终 false）
    pub allow_wasi: bool,          // 固定 false
    /// fuel 消耗告警阈值
    pub fuel_warning_threshold: u64,
}
```

### 5.3 节点级资源限制

为每个节点类型定义资源上限：

```rust
/// 节点资源限制
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResourceLimits {
    /// 单节点最大执行时长
    pub max_execution_time: Duration,
    /// 单节点最大输出大小（字节）
    pub max_output_bytes: usize,
    /// 单节点最大内存使用（字节，仅 WASM 可精确控制）
    pub max_memory_bytes: Option<usize>,
}

/// 各节点类型的默认资源限制
impl NodeResourceLimits {
    pub fn for_code_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(30),
            max_output_bytes: 1 * 1024 * 1024,   // 1MB
            max_memory_bytes: Some(32 * 1024 * 1024), // 32MB
        }
    }

    pub fn for_http_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(30),
            max_output_bytes: 10 * 1024 * 1024,  // 10MB
            max_memory_bytes: None,
        }
    }

    pub fn for_llm_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(120),
            max_output_bytes: 1 * 1024 * 1024,   // 1MB
            max_memory_bytes: None,
        }
    }

    pub fn for_template_node() -> Self {
        Self {
            max_execution_time: Duration::from_secs(5),
            max_output_bytes: 512 * 1024,         // 512KB
            max_memory_bytes: None,
        }
    }
}
```

### 5.4 输出大小限制

在 Dispatcher 执行循环中，对每个节点的 `NodeRunResult.outputs` 做序列化大小检查：

```rust
// dispatcher.rs 执行循环中
let result = executor.execute(node_id, config, &pool, &context).await?;
let output_size: usize = result.outputs.values()
    .map(|v| serde_json::to_string(v).map(|s| s.len()).unwrap_or(0))
    .sum();

if output_size > node_limits.max_output_bytes {
    return Err(NodeError::OutputTooLarge {
        node_id: node_id.to_string(),
        max: node_limits.max_output_bytes,
        actual: output_size,
    });
}
```

同时设置工作流级别的总输出累积上限，防止大量小输出累积导致内存膨胀。

---

## 6. L3: 网络安全

**目标**：防止 SSRF 攻击和未授权的外部访问。

### 6.1 SSRF 防护 — NetworkPolicy

**当前问题**：`src/nodes/data_transform.rs:348-351` 直接使用 `reqwest::Client::builder()` 构建 HTTP 客户端，对用户提供的 URL 无任何过滤。

```rust
// 当前代码 — 无任何 URL 验证
let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(timeout))
    .build()?;
// 直接请求用户控制的 url
let resp = req_builder.headers(headers).send().await?;
```

**改进方案**：

```rust
/// 网络策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// 策略模式
    pub mode: NetworkPolicyMode,
    /// 域名白名单（mode=AllowList 时生效）
    pub allowed_domains: Vec<String>,
    /// 域名黑名单（mode=DenyList 时生效）
    pub denied_domains: Vec<String>,
    /// 是否阻止私有 IP
    pub block_private_ips: bool,           // 默认 true
    /// 是否阻止云元数据端点
    pub block_metadata_endpoints: bool,    // 默认 true
    /// 是否阻止环回地址
    pub block_loopback: bool,              // 默认 true
    /// 允许的协议
    pub allowed_schemes: Vec<String>,      // 默认 ["https", "http"]
    /// 允许的端口（空 = 全部允许）
    pub allowed_ports: Vec<u16>,           // 默认 [80, 443, 8080, 8443]
    /// 最大重定向跟踪次数
    pub max_redirects: usize,             // 默认 3
    /// DNS 重绑定防护
    pub dns_rebinding_protection: bool,    // 默认 true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkPolicyMode {
    /// 允许所有（仅 IP 过滤生效）
    AllowAll,
    /// 白名单模式：仅允许指定域名
    AllowList,
    /// 黑名单模式：阻止指定域名
    DenyList,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            mode: NetworkPolicyMode::AllowAll,
            allowed_domains: vec![],
            denied_domains: vec![],
            block_private_ips: true,
            block_metadata_endpoints: true,
            block_loopback: true,
            allowed_schemes: vec!["https".into(), "http".into()],
            allowed_ports: vec![80, 443, 8080, 8443],
            max_redirects: 3,
            dns_rebinding_protection: true,
        }
    }
}
```

### 6.2 IP 地址过滤

阻止访问的 IP 范围：

```rust
/// 检查 IP 是否为私有/受保护地址
pub fn is_blocked_ip(ip: &IpAddr, policy: &NetworkPolicy) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if policy.block_loopback && v4.is_loopback() { return true; }
            if policy.block_private_ips && (
                v4.is_private()             // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local()       // 169.254.0.0/16
                || v4.octets()[0] == 0      // 0.0.0.0/8
            ) { return true; }
            if policy.block_metadata_endpoints {
                // AWS/GCP/Azure 元数据端点
                let octets = v4.octets();
                if octets == [169, 254, 169, 254] { return true; }  // AWS/GCP
                if octets == [169, 254, 170, 2]   { return true; }  // GCP alt
                if octets == [100, 100, 100, 200]  { return true; } // Tailscale
            }
            false
        }
        IpAddr::V6(v6) => {
            if policy.block_loopback && v6.is_loopback() { return true; }
            // IPv6 映射的 IPv4 地址
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(&IpAddr::V4(v4), policy);
            }
            // 链路本地 fe80::/10
            if policy.block_private_ips && (v6.segments()[0] & 0xffc0) == 0xfe80 {
                return true;
            }
            false
        }
    }
}
```

### 6.3 DNS 重绑定防护

攻击原理：首次 DNS 解析返回合法外部 IP（通过安全检查），短 TTL 使后续请求解析到内网 IP。

**防护策略**：

```rust
/// 安全 DNS 解析器
pub struct SafeDnsResolver {
    policy: NetworkPolicy,
}

impl SafeDnsResolver {
    /// 解析域名并验证所有 IP 地址
    pub async fn resolve_and_validate(&self, host: &str) -> Result<Vec<IpAddr>, NetworkError> {
        let addrs = tokio::net::lookup_host(format!("{}:0", host)).await?;
        let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();

        if ips.is_empty() {
            return Err(NetworkError::DnsResolutionFailed(host.to_string()));
        }

        // 检查所有解析到的 IP
        for ip in &ips {
            if is_blocked_ip(ip, &self.policy) {
                return Err(NetworkError::BlockedIp {
                    host: host.to_string(),
                    ip: *ip,
                });
            }
        }

        Ok(ips)
    }
}
```

关键：使用自定义 `reqwest::dns::Resolve` 实现，确保 DNS 解析结果在连接前被验证，防止 reqwest 内部的 DNS 解析绕过检查。

### 6.4 安全 HTTP 客户端工厂

```rust
/// 创建经过安全配置的 HTTP 客户端
pub struct SecureHttpClientFactory;

impl SecureHttpClientFactory {
    pub fn build(policy: &NetworkPolicy) -> Result<reqwest::Client, NetworkError> {
        let resolver = SafeDnsResolver::new(policy.clone());

        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(policy.max_redirects))
            .dns_resolver(Arc::new(resolver))
            .connect_timeout(Duration::from_secs(10));

        // 如果需要 DNS 重绑定防护，禁用连接复用
        if policy.dns_rebinding_protection {
            builder = builder.pool_max_idle_per_host(0);
        }

        builder.build().map_err(|e| NetworkError::ClientBuildFailed(e.to_string()))
    }
}
```

### 6.5 URL 验证流程

在 HTTP 请求和 LLM 图片 URL 发送前执行：

```rust
/// URL 安全验证
pub async fn validate_url(url: &str, policy: &NetworkPolicy) -> Result<(), NetworkError> {
    let parsed = url::Url::parse(url)
        .map_err(|_| NetworkError::InvalidUrl(url.to_string()))?;

    // 1. 协议检查
    if !policy.allowed_schemes.contains(&parsed.scheme().to_string()) {
        return Err(NetworkError::BlockedScheme(parsed.scheme().to_string()));
    }

    // 2. 端口检查
    let port = parsed.port_or_known_default().unwrap_or(0);
    if !policy.allowed_ports.is_empty() && !policy.allowed_ports.contains(&port) {
        return Err(NetworkError::BlockedPort(port));
    }

    // 3. 域名检查
    let host = parsed.host_str()
        .ok_or_else(|| NetworkError::InvalidUrl(url.to_string()))?;

    match &policy.mode {
        NetworkPolicyMode::AllowList => {
            if !policy.allowed_domains.iter().any(|d| domain_matches(host, d)) {
                return Err(NetworkError::DomainNotAllowed(host.to_string()));
            }
        }
        NetworkPolicyMode::DenyList => {
            if policy.denied_domains.iter().any(|d| domain_matches(host, d)) {
                return Err(NetworkError::DomainDenied(host.to_string()));
            }
        }
        NetworkPolicyMode::AllowAll => {}
    }

    // 4. DNS 解析 + IP 检查
    let resolver = SafeDnsResolver::new(policy.clone());
    resolver.resolve_and_validate(host).await?;

    Ok(())
}

/// 域名匹配（支持通配符前缀 *.example.com）
fn domain_matches(host: &str, pattern: &str) -> bool {
    if pattern.starts_with("*.") {
        let suffix = &pattern[1..]; // ".example.com"
        host.ends_with(suffix) || host == &pattern[2..]
    } else {
        host == pattern
    }
}
```

### 6.6 LLM 请求同等过滤

LLM 节点中包含图片 URL 等外部资源引用，需应用同等的 NetworkPolicy 过滤。在 `ChatCompletionRequest` 构建阶段，对所有 `image_url` 类型的 content 做 `validate_url()` 检查。

---

## 7. L4: 资源组隔离与资源治理

**目标**：为集成平台提供资源隔离和配额管理的原语。

### 7.1 设计理念

xworkflow 作为基础库，不感知"租户"、"用户"等上层业务概念。库提供 **资源组（ResourceGroup）** 作为隔离和治理的最小单位：

- 平台可将一个租户映射为一个资源组
- 也可为同一租户创建多个资源组（如按项目、环境分组）
- 同一资源组内的工作流共享配额、安全策略和凭证配置

```
┌─────────────────────────────────────────┐
│              平台层（不属于库）            │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐ │
│  │ 租户 A  │  │ 租户 B  │  │ 租户 C  │ │
│  └────┬────┘  └────┬────┘  └──┬───┬──┘ │
│       │            │          │   │     │
├───────┼────────────┼──────────┼───┼─────┤
│       ▼            ▼          ▼   ▼     │
│  ┌─────────┐  ┌─────────┐  ┌───┐┌───┐  │
│  │ Group-1 │  │ Group-2 │  │G-3││G-4│  │
│  │ prod    │  │ prod    │  │dev││prd│  │
│  └─────────┘  └─────────┘  └───┘└───┘  │
│         xworkflow 库层（资源组）          │
└─────────────────────────────────────────┘
```

### 7.2 ResourceGroup

```rust
/// 资源组 — 库级别的隔离和治理单位
///
/// 平台可自由将其映射到租户、用户、项目等业务概念。
/// 同一资源组内的工作流共享配额、安全策略和凭证。
#[derive(Debug, Clone)]
pub struct ResourceGroup {
    /// 资源组唯一标识（由平台分配）
    pub group_id: String,
    /// 资源组显示名称（可选，用于日志和审计）
    pub group_name: Option<String>,
    /// 安全级别
    pub security_level: SecurityLevel,
    /// 资源配额
    pub quota: ResourceQuota,
    /// 凭证引用（不存储明文，仅存储逻辑 key）
    pub credential_refs: HashMap<String, String>,
}

/// 安全级别
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum SecurityLevel {
    /// 宽松模式 — 开发/测试环境
    Permissive,
    /// 标准模式 — 默认生产环境
    #[default]
    Standard,
    /// 严格模式 — 高安全需求场景
    Strict,
}
```

### 7.3 资源配额系统

```rust
/// 资源组配额
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceQuota {
    /// 最大并发工作流数
    pub max_concurrent_workflows: usize,      // 默认 10
    /// 单工作流变量池最大条目数
    pub max_variable_pool_entries: usize,      // 默认 10_000
    /// 单工作流变量池最大内存估算（字节）
    pub max_variable_pool_bytes: usize,        // 默认 50MB
    /// 单工作流最大步数
    pub max_steps: i32,                        // 默认 500
    /// 单工作流最大执行时间（秒）
    pub max_execution_time_secs: u64,          // 默认 600
    /// 每分钟最大 HTTP 请求数（全资源组）
    pub http_rate_limit_per_minute: usize,     // 默认 100
    /// 每分钟最大 LLM 请求数
    pub llm_rate_limit_per_minute: usize,      // 默认 60
    /// 每日 LLM Token 预算（输入 + 输出）
    pub llm_daily_token_budget: u64,           // 默认 1_000_000
    /// 单次 LLM 请求最大 Token
    pub llm_max_tokens_per_request: u32,       // 默认 4096
    /// HTTP 响应体最大大小（字节）
    pub http_max_response_bytes: usize,        // 默认 10MB
}

impl Default for ResourceQuota {
    fn default() -> Self {
        Self {
            max_concurrent_workflows: 10,
            max_variable_pool_entries: 10_000,
            max_variable_pool_bytes: 50 * 1024 * 1024,
            max_steps: 500,
            max_execution_time_secs: 600,
            http_rate_limit_per_minute: 100,
            llm_rate_limit_per_minute: 60,
            llm_daily_token_budget: 1_000_000,
            llm_max_tokens_per_request: 4096,
            http_max_response_bytes: 10 * 1024 * 1024,
        }
    }
}
```

### 7.4 ResourceGovernor — 配额检查与用量追踪

```rust
/// 资源治理器 trait
///
/// 负责在工作流执行过程中检查配额和追踪用量。
/// 库提供内存内的默认实现，平台可替换为基于 Redis 等的分布式实现。
#[async_trait]
pub trait ResourceGovernor: Send + Sync {
    /// 检查是否可以启动新工作流
    async fn check_workflow_start(&self, group_id: &str) -> Result<(), QuotaError>;

    /// 记录工作流启动
    async fn record_workflow_start(&self, group_id: &str, workflow_id: &str);

    /// 记录工作流结束
    async fn record_workflow_end(&self, group_id: &str, workflow_id: &str);

    /// 检查 HTTP 请求速率
    async fn check_http_rate(&self, group_id: &str) -> Result<(), QuotaError>;

    /// 检查 LLM 请求速率和 Token 预算
    async fn check_llm_request(
        &self,
        group_id: &str,
        estimated_tokens: u32,
    ) -> Result<(), QuotaError>;

    /// 记录 LLM Token 使用
    async fn record_llm_usage(&self, group_id: &str, usage: &LlmUsage);

    /// 检查变量池大小
    async fn check_variable_pool_size(
        &self,
        group_id: &str,
        current_entries: usize,
        current_bytes_estimate: usize,
    ) -> Result<(), QuotaError>;

    /// 获取资源组当前用量快照
    async fn get_usage(&self, group_id: &str) -> GroupUsage;
}

/// 配额超限错误
#[derive(Debug, Clone)]
pub enum QuotaError {
    ConcurrentWorkflowLimit { max: usize, current: usize },
    HttpRateLimit { max_per_minute: usize },
    LlmRateLimit { max_per_minute: usize },
    LlmTokenBudgetExhausted { budget: u64, used: u64 },
    VariablePoolTooLarge { max_entries: usize, current: usize },
    VariablePoolMemoryExceeded { max_bytes: usize, current: usize },
}

/// 资源组用量快照
#[derive(Debug, Clone, Default)]
pub struct GroupUsage {
    pub active_workflows: usize,
    pub http_requests_this_minute: usize,
    pub llm_requests_this_minute: usize,
    pub llm_tokens_today: u64,
}
```

**内置实现 — InMemoryResourceGovernor**：

使用 `DashMap` + `AtomicU64` 实现高性能的内存内配额管理，适用于单实例部署。平台可替换为基于 Redis 的分布式实现。

```rust
pub struct InMemoryResourceGovernor {
    quotas: HashMap<String, ResourceQuota>,
    usage: DashMap<String, GroupUsageState>,
}
```

### 7.5 凭证隔离

**当前问题**：`OpenAiConfig.api_key` 为全局明文字符串（`src/llm/provider/openai.rs:18`），所有工作流共享同一凭证。

**改进方案**：

```rust
/// 凭证提供器 trait
///
/// 平台实现此 trait，库在运行时按资源组获取凭证。
/// 库不存储也不缓存凭证明文。
#[async_trait]
pub trait CredentialProvider: Send + Sync {
    /// 按资源组获取凭证
    async fn get_credentials(
        &self,
        group_id: &str,
        provider_name: &str,
    ) -> Result<HashMap<String, String>, CredentialError>;
}

/// 凭证错误
pub enum CredentialError {
    NotFound { group_id: String, provider: String },
    AccessDenied { group_id: String, provider: String },
    ProviderError(String),
}
```

集成方式：
- `LlmProviderRegistry` 在初始化时注入 `Arc<dyn CredentialProvider>`
- 每次 LLM 调用时，根据 `ResourceGroup.group_id` 获取对应凭证
- 凭证不再硬编码在 `OpenAiConfig` 中，而是运行时动态获取
- 平台可使用密钥管理服务（如 HashiCorp Vault、AWS Secrets Manager）实现此 trait

### 7.6 变量池大小限制

扩展 `VariablePool`（`src/core/variable_pool.rs`），增加大小追踪：

```rust
/// 变量池安全约束
pub struct VariablePoolLimits {
    /// 最大条目数
    pub max_entries: usize,
    /// 最大估算内存（字节）
    pub max_memory_bytes: usize,
}

// 在 VariablePool::set() 中增加检查
impl VariablePool {
    pub fn set_checked(
        &mut self,
        key: &[String],
        value: Segment,
        limits: &VariablePoolLimits,
    ) -> Result<(), QuotaError> {
        if self.pool.len() >= limits.max_entries {
            return Err(QuotaError::VariablePoolTooLarge {
                max_entries: limits.max_entries,
                current: self.pool.len(),
            });
        }
        let estimated_size = self.estimate_total_bytes() + estimate_segment_bytes(&value);
        if estimated_size > limits.max_memory_bytes {
            return Err(QuotaError::VariablePoolMemoryExceeded {
                max_bytes: limits.max_memory_bytes,
                current: estimated_size,
            });
        }
        self.set(key, value);
        Ok(())
    }
}
```

---

## 8. L5: 审计与可观测性

**目标**：记录安全相关事件，支持事后分析和实时告警。

### 8.1 安全事件类型

```rust
/// 安全事件
#[derive(Debug, Clone, Serialize)]
pub struct SecurityEvent {
    /// 事件时间戳
    pub timestamp: i64,
    /// 资源组 ID
    pub group_id: String,
    /// 工作流 ID
    pub workflow_id: Option<String>,
    /// 节点 ID
    pub node_id: Option<String>,
    /// 事件类型
    pub event_type: SecurityEventType,
    /// 事件详情
    pub details: Value,
    /// 事件严重级别
    pub severity: EventSeverity,
}

#[derive(Debug, Clone, Serialize)]
pub enum SecurityEventType {
    /// SSRF 拦截
    SsrfBlocked { url: String, reason: String },
    /// 沙箱违规
    SandboxViolation { sandbox_type: String, violation: String },
    /// 配额超限
    QuotaExceeded { quota_type: String, limit: u64, current: u64 },
    /// 凭证访问
    CredentialAccess { provider: String, success: bool },
    /// 代码安全分析拦截
    CodeAnalysisBlocked { violations: Vec<String> },
    /// 模板渲染异常
    TemplateRenderingAnomaly { template_length: usize },
    /// DSL 验证失败
    DslValidationFailed { errors: Vec<String> },
    /// 输出大小超限
    OutputSizeExceeded { node_id: String, max: usize, actual: usize },
}

#[derive(Debug, Clone, Serialize)]
pub enum EventSeverity {
    /// 信息级
    Info,
    /// 警告级
    Warning,
    /// 危险级（需要关注）
    Critical,
}
```

### 8.2 审计日志接口

```rust
/// 审计日志记录器 trait
#[async_trait]
pub trait AuditLogger: Send + Sync {
    /// 记录安全事件
    async fn log_event(&self, event: SecurityEvent);

    /// 批量记录
    async fn log_events(&self, events: Vec<SecurityEvent>) {
        for event in events {
            self.log_event(event).await;
        }
    }
}

/// 内置实现：写入 tracing 日志
pub struct TracingAuditLogger;

#[async_trait]
impl AuditLogger for TracingAuditLogger {
    async fn log_event(&self, event: SecurityEvent) {
        match event.severity {
            EventSeverity::Critical => {
                tracing::error!(
                    group_id = %event.group_id,
                    event_type = ?event.event_type,
                    "SECURITY: {:?}", event
                );
            }
            EventSeverity::Warning => {
                tracing::warn!(
                    group_id = %event.group_id,
                    event_type = ?event.event_type,
                    "SECURITY: {:?}", event
                );
            }
            EventSeverity::Info => {
                tracing::info!(
                    group_id = %event.group_id,
                    event_type = ?event.event_type,
                    "SECURITY: {:?}", event
                );
            }
        }
    }
}
```

平台可实现此 trait 对接自己的日志系统（如 Elasticsearch、Datadog、Loki 等）。

---

## 9. SecurityPolicy 统一配置

### 9.1 统一安全策略

将所有安全配置聚合为一个统一入口，每个检查组件独立 `Option`：

```rust
/// 统一安全策略 — 每个组件独立 Option，None = 该项不检查
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    /// 安全级别（仅用于日志标注，不驱动运行时分支）
    pub level: SecurityLevel,
    /// 网络策略 — None = 不做任何 URL/IP/DNS 检查
    pub network: Option<NetworkPolicy>,
    /// 模板安全配置 — None = 不限制模板
    pub template: Option<TemplateSafetyConfig>,
    /// DSL 验证配置 — None = 仅基础验证
    pub dsl_validation: Option<DslValidationConfig>,
    /// 节点资源限制 — 空 HashMap = 不限制
    pub node_limits: HashMap<String, NodeResourceLimits>,
    /// 审计日志 — None = 不记录
    pub audit_logger: Option<Arc<dyn AuditLogger>>,
}
```

### 9.2 分级默认配置

```rust
impl SecurityPolicy {
    /// 宽松模式 — 所有检查组件关闭，运行时零开销
    pub fn permissive() -> Self {
        Self {
            level: SecurityLevel::Permissive,
            network: None,         // 不做网络检查
            template: None,        // 不限制模板
            dsl_validation: None,  // 仅基础验证
            node_limits: HashMap::new(), // 不限制节点资源
            audit_logger: None,    // 不记录审计日志
        }
    }

    /// 标准模式 — 默认生产环境
    pub fn standard() -> Self {
        Self {
            level: SecurityLevel::Standard,
            network: Some(NetworkPolicy::default()),  // 阻止私有 IP / 元数据端点
            template: Some(TemplateSafetyConfig::default()),
            dsl_validation: Some(DslValidationConfig::default()),
            node_limits: default_node_limits(),
            audit_logger: Some(Arc::new(TracingAuditLogger)),
        }
    }

    /// 严格模式 — 高安全需求场景
    pub fn strict() -> Self {
        Self {
            level: SecurityLevel::Strict,
            network: Some(NetworkPolicy {
                mode: NetworkPolicyMode::AllowList,
                allowed_domains: vec![],  // 必须显式配置
                block_private_ips: true,
                block_metadata_endpoints: true,
                block_loopback: true,
                allowed_ports: vec![443],  // 仅 HTTPS
                max_redirects: 0,          // 禁止重定向
                dns_rebinding_protection: true,
                ..Default::default()
            }),
            template: Some(TemplateSafetyConfig {
                max_template_length: 5_000,
                max_output_length: 50_000,
                max_recursion_depth: 3,
                max_loop_iterations: 100,
                disabled_filters: vec!["debug".into()],
                disabled_functions: vec!["range".into()],
            }),
            dsl_validation: Some(DslValidationConfig {
                max_nodes: 100,
                max_edges: 200,
                max_nesting_depth: 5,
                max_node_config_bytes: 16 * 1024,
                ..Default::default()
            }),
            node_limits: strict_node_limits(),
            audit_logger: Some(Arc::new(TracingAuditLogger)),
        }
    }
}

fn default_node_limits() -> HashMap<String, NodeResourceLimits> {
    let mut m = HashMap::new();
    m.insert("code".into(), NodeResourceLimits::for_code_node());
    m.insert("http_request".into(), NodeResourceLimits::for_http_node());
    m.insert("llm".into(), NodeResourceLimits::for_llm_node());
    m.insert("template_transform".into(), NodeResourceLimits::for_template_node());
    m
}

fn strict_node_limits() -> HashMap<String, NodeResourceLimits> {
    let mut m = HashMap::new();
    m.insert("code".into(), NodeResourceLimits {
        max_execution_time: Duration::from_secs(10),
        max_output_bytes: 256 * 1024,
        max_memory_bytes: Some(16 * 1024 * 1024),
    });
    m.insert("http_request".into(), NodeResourceLimits {
        max_execution_time: Duration::from_secs(10),
        max_output_bytes: 1 * 1024 * 1024,
        max_memory_bytes: None,
    });
    m.insert("llm".into(), NodeResourceLimits {
        max_execution_time: Duration::from_secs(60),
        max_output_bytes: 256 * 1024,
        max_memory_bytes: None,
    });
    m.insert("template_transform".into(), NodeResourceLimits {
        max_execution_time: Duration::from_secs(2),
        max_output_bytes: 128 * 1024,
        max_memory_bytes: None,
    });
    m
}
```

### 9.3 使用示例

```rust
use xworkflow::security::{SecurityPolicy, NetworkPolicy, NetworkPolicyMode};

// 宽松模式 — 零运行时开销
let policy = SecurityPolicy::permissive();

// 标准模式 — 大多数场景
let policy = SecurityPolicy::standard();

// 自定义：仅启用网络检查（域名白名单），其他关闭
let policy = SecurityPolicy {
    network: Some(NetworkPolicy {
        mode: NetworkPolicyMode::AllowList,
        allowed_domains: vec![
            "api.openai.com".into(),
            "*.googleapis.com".into(),
        ],
        ..NetworkPolicy::default()
    }),
    ..SecurityPolicy::permissive()  // 其余组件关闭
};

// 严格模式
let policy = SecurityPolicy::strict();
```

---

## 10. 集成方式

### 10.1 RuntimeContext 扩展

```rust
/// 扩展后的 RuntimeContext
#[derive(Clone)]
pub struct RuntimeContext {
    pub time_provider: Arc<dyn TimeProvider>,
    pub id_generator: Arc<dyn IdGenerator>,
    pub event_tx: Option<mpsc::Sender<GraphEngineEvent>>,
    // --- 新增（全部 Option，未配置时为 None，零成本） ---
    #[cfg(feature = "security")]
    pub resource_group: Option<ResourceGroup>,
    #[cfg(feature = "security")]
    pub security_policy: Option<SecurityPolicy>,
    #[cfg(feature = "security")]
    pub resource_governor: Option<Arc<dyn ResourceGovernor>>,
    #[cfg(feature = "security")]
    pub credential_provider: Option<Arc<dyn CredentialProvider>>,
    #[cfg(feature = "security")]
    pub audit_logger: Option<Arc<dyn AuditLogger>>,
}
```

### 10.2 WorkflowRunner Builder 扩展

```rust
impl WorkflowRunnerBuilder {
    // --- 新增 builder 方法 ---

    pub fn security_policy(mut self, policy: SecurityPolicy) -> Self {
        self.security_policy = Some(policy);
        self
    }

    pub fn resource_group(mut self, group: ResourceGroup) -> Self {
        self.resource_group = Some(group);
        self
    }

    pub fn resource_governor(mut self, governor: Arc<dyn ResourceGovernor>) -> Self {
        self.resource_governor = Some(governor);
        self
    }

    pub fn credential_provider(mut self, provider: Arc<dyn CredentialProvider>) -> Self {
        self.credential_provider = Some(provider);
        self
    }
}
```

**使用示例**：

```rust
let handle = WorkflowRunner::builder(schema)
    .user_inputs(inputs)
    .config(EngineConfig::default())
    .security_policy(SecurityPolicy::standard())
    .resource_group(ResourceGroup {
        group_id: "group-prod-acme".into(),
        group_name: Some("Acme Corp Production".into()),
        security_level: SecurityLevel::Standard,
        quota: ResourceQuota::default(),
        credential_refs: HashMap::from([
            ("openai".into(), "acme-openai-key-ref".into()),
        ]),
    })
    .resource_governor(Arc::new(in_memory_governor))
    .credential_provider(Arc::new(vault_provider))
    .llm_providers(Arc::new(llm_registry))
    .run()
    .await?;
```

### 10.3 Dispatcher 执行循环中的安全检查点

在 Dispatcher 的节点执行循环中插入安全检查：

```
对每个待执行节点：
1. [L4] 检查变量池大小 → check_variable_pool_size()
2. [L4] 若为 HTTP 节点 → check_http_rate()
3. [L4] 若为 LLM 节点 → check_llm_request()
4. [L3] 若为 HTTP/LLM 节点 → validate_url()
5. [L2] 若为代码节点 → CodeAnalyzer::analyze()
6. [L2] 执行节点，检查输出大小
7. [L4] 记录资源使用 → record_llm_usage() 等
8. [L5] 若发生安全事件 → audit_logger.log_event()
```

### 10.4 HTTP 客户端工厂注入

修改 `HttpRequestExecutor`（`src/nodes/data_transform.rs`），注入安全 HTTP 客户端：

```rust
// 修改前
let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(timeout))
    .build()?;

// 修改后 — 仅在配置了 network policy 时做安全检查
let client = if let Some(policy) = context.security_policy
    .as_ref()
    .and_then(|p| p.network.as_ref())
{
    validate_url(&url, policy).await?;
    SecureHttpClientFactory::build(policy)?
} else {
    // 无网络策略 — 直接构建客户端，零额外开销
    reqwest::Client::builder().build()?
};

let client = client.builder()
    .timeout(std::time::Duration::from_secs(timeout))
    .build()?;
```

同时需要对重定向后的 URL 进行二次验证（通过自定义 redirect policy 实现）。

---

## 11. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|----------|------|
| `src/security/mod.rs` | **新增** | 安全模块入口 |
| `src/security/policy.rs` | **新增** | `SecurityPolicy`, `SecurityLevel` |
| `src/security/network.rs` | **新增** | `NetworkPolicy`, SSRF 防护, DNS 解析器 |
| `src/security/sandbox.rs` | **新增** | `JsSandboxSecurityConfig`, `CodeAnalyzer` |
| `src/security/resource_group.rs` | **新增** | `ResourceGroup`, `ResourceQuota` |
| `src/security/governor.rs` | **新增** | `ResourceGovernor` trait + `InMemoryResourceGovernor` |
| `src/security/credential.rs` | **新增** | `CredentialProvider` trait |
| `src/security/audit.rs` | **新增** | `SecurityEvent`, `AuditLogger` trait |
| `src/security/validation.rs` | **新增** | `DslValidationConfig`, `TemplateSafetyConfig` |
| `src/core/runtime_context.rs` | **修改** | 添加 `resource_group`, `security_policy` 等字段 |
| `src/core/dispatcher.rs` | **修改** | 执行循环中插入安全检查点 |
| `src/core/variable_pool.rs` | **修改** | 添加 `set_checked()`, 大小追踪 |
| `src/scheduler.rs` | **修改** | `WorkflowRunnerBuilder` 添加安全相关 builder 方法 |
| `src/nodes/data_transform.rs` | **修改** | `HttpRequestExecutor` 使用安全 HTTP 客户端 |
| `src/sandbox/builtin.rs` | **修改** | 替换正则检测为 AST 分析 |
| `src/llm/provider/openai.rs` | **修改** | 使用 `CredentialProvider` 获取凭证 |
| `src/lib.rs` | **修改** | 导出 `security` 模块 |

---

## 12. 实施路线图

### 第一阶段：关键漏洞修复（P0）

**优先级最高，应立即处理。**

| 任务 | 涉及文件 | 风险等级 |
|------|----------|----------|
| SSRF 防护：`NetworkPolicy` + IP 过滤 + DNS 验证 | `security/network.rs`, `data_transform.rs` | 严重 |
| HTTP 响应体大小限制 | `data_transform.rs` | 中 |
| 变量池大小限制 | `variable_pool.rs` | 高 |

### 第二阶段：沙箱加固（P1）

| 任务 | 涉及文件 | 风险等级 |
|------|----------|----------|
| JS AST 安全分析（替代正则） | `sandbox/builtin.rs`, `security/sandbox.rs` | 高 |
| JS 全局对象冻结 + API 白名单 | `sandbox/builtin.rs` | 高 |
| 节点输出大小限制 | `dispatcher.rs` | 中 |
| 模板安全配置 | 模板渲染相关代码 | 中 |

### 第三阶段：资源组与配额（P1）

| 任务 | 涉及文件 | 风险等级 |
|------|----------|----------|
| `ResourceGroup` + `RuntimeContext` 扩展 | `runtime_context.rs`, `security/resource_group.rs` | 高 |
| `ResourceQuota` + `ResourceGovernor` | `security/governor.rs` | 高 |
| `CredentialProvider` + LLM 凭证隔离 | `security/credential.rs`, `openai.rs` | 高 |
| 并发工作流数量限制 | `scheduler.rs` | 高 |
| LLM Token 预算 | `security/governor.rs`, LLM 节点 | 高 |

### 第四阶段：审计与配置（P2）

| 任务 | 涉及文件 | 风险等级 |
|------|----------|----------|
| `SecurityEvent` + `AuditLogger` | `security/audit.rs` | - |
| `SecurityPolicy` 统一配置 | `security/policy.rs` | - |
| `WorkflowRunnerBuilder` 安全 API | `scheduler.rs` | - |
| DSL 验证增强（环检测等） | 验证相关代码 | 中 |

### 第五阶段：高级防护（P2）

| 任务 | 涉及文件 | 风险等级 |
|------|----------|----------|
| DNS 重绑定防护 | `security/network.rs` | 中 |
| 重定向二次验证 | `data_transform.rs` | 中 |
| HTTP 请求频率限制 | `security/governor.rs` | 中 |
| 分级安全策略预设 | `security/policy.rs` | - |
