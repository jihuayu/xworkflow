# 统一错误处理架构设计

## 一、背景与问题

xworkflow 是基于 DAG 的工作流引擎，支持多种节点类型（HTTP、LLM、Code sandbox、WASM plugin、控制流等）。随着引擎功能扩展，错误处理系统出现了以下结构性问题：

### 1.1 错误信息丢失

各层错误类型（`LlmError`、`SandboxError`、`PluginError`）在传递到 `NodeError` 时，全部通过 `.to_string()` 转换为纯字符串，丢失了结构化信息。

例如 `LlmError::RateLimitExceeded { retry_after: Some(30) }`（`src/llm/error.rs:16-17`）变成 `NodeError::ExecutionError("Rate limit exceeded: retry after 30s")`（`src/llm/error.rs:41-44`），消费者无法程序化地提取 `retry_after` 值。

### 1.2 重试不区分错误类型

当前 dispatcher（`src/core/dispatcher.rs:200-224`）对所有错误统一重试，无法区分可重试错误（网络超时、429 限速）与不可重试错误（401 认证失败、400 参数错误）。一个 401 认证错误会被无意义地重试 N 次。

### 1.3 IterationErrorMode 未实现

DSL schema 已定义 `IterationErrorMode`（Terminated / RemoveAbnormal / ContinueOnError）三种模式（`src/dsl/schema.rs:462-468`），但 `IterationNodeExecutor`（`src/nodes/subgraph_nodes.rs`）完全忽略此配置，任何子图错误都直接上抛。

### 1.4 error_strategy 使用原始 JSON

dispatcher 通过 `node_config.get("error_strategy")` 和字符串匹配 `"fail-branch"` / `"default-value"` 读取错误策略（`dispatcher.rs:230-233`），而非使用 schema 中已定义好的 `ErrorStrategyConfig` / `ErrorStrategyType` 类型（`schema.rs:89-103`）。

### 1.5 error_type 字段始终为 None

`NodeRunResult.error_type`（`schema.rs:570`）从未被赋值，事件消费者无法通过此字段区分错误种类。

### 1.6 缺少节点级超时

仅有全局 `max_execution_time_secs`（`EngineConfig`，默认 600s），无法为单个节点设置独立的执行超时。一个 HTTP 请求节点和一个简单的变量聚合节点共享相同的超时约束。

### 1.7 WorkflowError 变体冗余

`NodeExecutionFailed(String, String)` 和 `NodeExecutionError { node_id, error }` 语义重叠（`src/error/workflow_error.rs:24-25, 44-45`）。

---

## 二、现状分析

### 2.1 错误类型层级

```
WorkflowError (src/error/workflow_error.rs)     -- 20 variants
  │
  └─ NodeError (src/error/node_error.rs)        -- 11 variants，全部 String 载荷
       │
       ├─ LlmError (src/llm/error.rs)           -- 11 variants，有结构化字段
       ├─ SandboxError (src/sandbox/error.rs)    -- 11 variants，部分结构化
       ├─ PluginError (src/plugin/error.rs)      -- 13 variants，部分结构化
       └─ SubGraphError (src/nodes/subgraph.rs)  -- 5 variants
```

关键问题在 `NodeError` 层：所有 From 实现都通过 `e.to_string()` 转换，导致下层结构化信息全部丢失。

### 2.2 Dispatcher 当前错误处理流程

`src/core/dispatcher.rs:184-267`：

```
executor.execute()
  → Err(NodeError)
    → 重试循环 (0..=max_retries，固定间隔)
      → 全部失败后读取 error_strategy (原始 JSON 字符串匹配)
        → "fail-branch": 返回 Exception 状态 + fail-branch handle
        → "default-value": 返回 Exception 状态 + 默认输出
        → 其他/none: 返回 Err，终止工作流
```

### 2.3 事件系统中的错误表达

`GraphEngineEvent`（`src/core/event_bus.rs`）中错误相关事件：

| 事件 | 行号 | 载荷 |
|------|------|------|
| `NodeRunFailed` | 49-55 | `error: String` + `node_run_result` |
| `NodeRunException` | 56-62 | `error: String` + `node_run_result` |
| `NodeRunRetry` | 71-78 | `error: String` + `retry_index` |
| `GraphRunFailed` | 22-24 | `error: String` |

`to_json()` 序列化时只输出 `"error": error_string`，无结构化错误信息。

### 2.4 已有基础设施

| 组件 | 文件 | 状态 |
|------|------|------|
| ErrorStrategyConfig / ErrorStrategyType | `src/dsl/schema.rs:89-103` | 已定义，未被 dispatcher 使用 |
| RetryConfig | `src/dsl/schema.rs:105-111` | 已定义，dispatcher 从 JSON 读取 |
| IterationErrorMode | `src/dsl/schema.rs:462-468` | 已定义，执行未实现 |
| NodeRunResult.error / error_type | `src/dsl/schema.rs:569-570` | error_type 始终 None |
| WorkflowNodeExecutionStatus | `src/dsl/schema.rs:547-558` | Failed vs Exception 语义明确 |
| NodeExecutor trait | `src/nodes/executor.rs:12-22` | `Result<NodeRunResult, NodeError>` |
| thiserror | `Cargo.toml` | 已依赖 |

---

## 三、错误分类系统

### 3.1 新增 ErrorContext 结构体

```rust
// src/error/error_context.rs (新增文件)

use serde::{Deserialize, Serialize};

/// 错误是否可重试
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorRetryability {
    /// 可重试（网络超时、限速、临时服务不可用等）
    Retryable,
    /// 不可重试（认证失败、参数错误、代码逻辑错误等）
    NonRetryable,
    /// 未知（由 dispatcher 决定是否重试）
    Unknown,
}

/// 错误严重级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSeverity {
    /// 警告级：可被 error_strategy 处理，不一定终止工作流
    Warning,
    /// 错误级：节点失败，根据 error_strategy 决定后续行为
    Error,
    /// 致命级：必须终止工作流（如图结构错误、内部 bug）
    Fatal,
}

/// 错误分类码（用于 NodeRunResult.error_type 字段）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // -- 通用 --
    ConfigError,
    TypeError,
    Timeout,
    SerializationError,
    InternalError,

    // -- 网络/HTTP --
    NetworkError,
    HttpClientError,      // 4xx
    HttpServerError,      // 5xx
    HttpTimeout,

    // -- LLM --
    LlmAuthError,
    LlmRateLimit,
    LlmApiError,
    LlmModelNotFound,

    // -- 沙箱/代码 --
    SandboxCompilationError,
    SandboxExecutionError,
    SandboxTimeout,
    SandboxMemoryLimit,
    SandboxDangerousCode,

    // -- 插件 --
    PluginNotFound,
    PluginExecutionError,
    PluginCapabilityDenied,
    PluginResourceLimit,

    // -- 变量/模板 --
    VariableNotFound,
    TemplateError,
    InputValidationError,
}

/// 结构化错误上下文 — 附加在 NodeError 上，在整个错误链路中传递
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorContext {
    /// 错误分类码
    pub code: ErrorCode,
    /// 是否可重试
    pub retryability: ErrorRetryability,
    /// 严重级别
    pub severity: ErrorSeverity,
    /// 人类可读的错误消息
    pub message: String,
    /// 可选：建议重试等待时间（秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
    /// 可选：HTTP 状态码
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// 可选：额外上下文数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ErrorContext {
    /// 快速创建不可重试错误
    pub fn non_retryable(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            retryability: ErrorRetryability::NonRetryable,
            severity: ErrorSeverity::Error,
            message: message.into(),
            retry_after_secs: None,
            http_status: None,
            metadata: None,
        }
    }

    /// 快速创建可重试错误
    pub fn retryable(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            retryability: ErrorRetryability::Retryable,
            severity: ErrorSeverity::Error,
            message: message.into(),
            retry_after_secs: None,
            http_status: None,
            metadata: None,
        }
    }

    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = Some(status);
        self
    }
}
```

### 3.2 改造 NodeError

在保持所有现有 variant 不变的前提下，新增 `WithContext` variant：

```rust
// src/error/node_error.rs (修改)

use super::error_context::ErrorContext;

#[derive(Debug, Error)]
pub enum NodeError {
    // ... 保留全部 11 个现有 variants 不变 ...

    /// 新增：携带结构化上下文的错误
    #[error("{}", .context.message)]
    WithContext {
        #[source]
        source: Box<NodeError>,
        context: ErrorContext,
    },
}

impl NodeError {
    /// 附加结构化上下文
    pub fn with_context(self, context: ErrorContext) -> Self {
        NodeError::WithContext {
            source: Box::new(self),
            context,
        }
    }

    /// 提取错误上下文（如果存在）
    pub fn error_context(&self) -> Option<&ErrorContext> {
        match self {
            NodeError::WithContext { context, .. } => Some(context),
            _ => None,
        }
    }

    /// 判断是否可重试
    pub fn is_retryable(&self) -> bool {
        match self.error_context() {
            Some(ctx) => ctx.retryability == ErrorRetryability::Retryable,
            None => self.default_retryability(),
        }
    }

    /// 对没有 ErrorContext 的旧式错误，推断默认可重试性
    fn default_retryability(&self) -> bool {
        matches!(self, NodeError::Timeout | NodeError::HttpError(_))
    }

    /// 获取错误码字符串（用于 error_type 字段）
    pub fn error_code(&self) -> String {
        match self {
            NodeError::WithContext { context, .. } => {
                // 序列化 ErrorCode 枚举为 snake_case 字符串
                serde_json::to_value(&context.code)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown".to_string())
            }
            NodeError::ConfigError(_) => "config_error".to_string(),
            NodeError::VariableNotFound(_) => "variable_not_found".to_string(),
            NodeError::ExecutionError(_) => "execution_error".to_string(),
            NodeError::TypeError(_) => "type_error".to_string(),
            NodeError::TemplateError(_) => "template_error".to_string(),
            NodeError::InputValidationError(_) => "input_validation_error".to_string(),
            NodeError::Timeout => "timeout".to_string(),
            NodeError::SerializationError(_) => "serialization_error".to_string(),
            NodeError::HttpError(_) => "http_error".to_string(),
            NodeError::SandboxError(_) => "sandbox_error".to_string(),
            NodeError::EventSendError(_) => "event_send_error".to_string(),
        }
    }

    /// 将错误转为结构化 JSON（用于事件系统）
    pub fn to_structured_json(&self) -> serde_json::Value {
        match self.error_context() {
            Some(ctx) => serde_json::to_value(ctx).unwrap_or(serde_json::json!({
                "code": self.error_code(),
                "message": self.to_string(),
            })),
            None => serde_json::json!({
                "code": self.error_code(),
                "message": self.to_string(),
                "retryability": "unknown",
                "severity": "error",
            }),
        }
    }
}
```

### 3.3 改造 From 实现

#### LlmError → NodeError

```rust
// src/llm/error.rs (修改 From 实现)

impl From<LlmError> for NodeError {
    fn from(e: LlmError) -> Self {
        let context = match &e {
            LlmError::AuthenticationError(_) => ErrorContext::non_retryable(
                ErrorCode::LlmAuthError, e.to_string(),
            ),
            LlmError::RateLimitExceeded { retry_after } => {
                let mut ctx = ErrorContext::retryable(
                    ErrorCode::LlmRateLimit, e.to_string(),
                );
                if let Some(secs) = retry_after {
                    ctx = ctx.with_retry_after(*secs);
                }
                ctx
            }
            LlmError::ApiError { status, .. } => {
                let retryable = *status >= 500 || *status == 429;
                let ctx = if retryable {
                    ErrorContext::retryable(ErrorCode::LlmApiError, e.to_string())
                } else {
                    ErrorContext::non_retryable(ErrorCode::LlmApiError, e.to_string())
                };
                ctx.with_http_status(*status)
            }
            LlmError::NetworkError(_) => ErrorContext::retryable(
                ErrorCode::NetworkError, e.to_string(),
            ),
            LlmError::Timeout => ErrorContext::retryable(
                ErrorCode::Timeout, e.to_string(),
            ),
            LlmError::InvalidRequest(_) => ErrorContext::non_retryable(
                ErrorCode::ConfigError, e.to_string(),
            ),
            LlmError::ProviderNotFound(_) | LlmError::ModelNotSupported(_) =>
                ErrorContext::non_retryable(ErrorCode::LlmModelNotFound, e.to_string()),
            _ => ErrorContext::non_retryable(ErrorCode::InternalError, e.to_string()),
        };

        NodeError::ExecutionError(e.to_string()).with_context(context)
    }
}
```

#### SandboxError → NodeError（在 CodeNodeExecutor 中处理）

```rust
// src/nodes/data_transform.rs CodeNodeExecutor 中

// 替换:
//   .map_err(|e| NodeError::SandboxError(e.to_string()))?;
// 为:
.map_err(|e| {
    let context = match &e {
        SandboxError::ExecutionTimeout => ErrorContext::retryable(
            ErrorCode::SandboxTimeout, e.to_string()
        ),
        SandboxError::MemoryLimitExceeded => ErrorContext::non_retryable(
            ErrorCode::SandboxMemoryLimit, e.to_string()
        ),
        SandboxError::CompilationError(_) => ErrorContext::non_retryable(
            ErrorCode::SandboxCompilationError, e.to_string()
        ),
        SandboxError::DangerousCode(_) => ErrorContext::non_retryable(
            ErrorCode::SandboxDangerousCode, e.to_string()
        ),
        _ => ErrorContext::non_retryable(
            ErrorCode::SandboxExecutionError, e.to_string()
        ),
    };
    NodeError::SandboxError(e.to_string()).with_context(context)
})?;
```

#### PluginError → NodeError

```rust
// 类似模式
impl From<PluginError> for NodeError {
    fn from(e: PluginError) -> Self {
        let context = match &e {
            PluginError::NotFound(_) => ErrorContext::non_retryable(
                ErrorCode::PluginNotFound, e.to_string(),
            ),
            PluginError::Timeout | PluginError::FuelExhausted => ErrorContext::retryable(
                ErrorCode::PluginResourceLimit, e.to_string(),
            ),
            PluginError::MemoryLimitExceeded => ErrorContext::non_retryable(
                ErrorCode::PluginResourceLimit, e.to_string(),
            ),
            PluginError::CapabilityDenied(_) => ErrorContext::non_retryable(
                ErrorCode::PluginCapabilityDenied, e.to_string(),
            ),
            _ => ErrorContext::non_retryable(
                ErrorCode::PluginExecutionError, e.to_string(),
            ),
        };
        NodeError::ExecutionError(e.to_string()).with_context(context)
    }
}
```

---

## 四、DSL Schema 变更

### 4.1 RetryConfig 增强

```rust
// src/dsl/schema.rs

/// 重试退避策略
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum BackoffStrategy {
    /// 固定间隔
    Fixed,
    /// 指数退避
    Exponential,
    /// 指数退避 + 随机抖动
    ExponentialWithJitter,
}

fn default_backoff_strategy() -> BackoffStrategy { BackoffStrategy::Fixed }
fn default_backoff_multiplier() -> f64 { 2.0 }
fn default_max_retry_interval() -> i32 { 60000 } // 60s
fn default_retry_on_retryable_only() -> bool { true }

/// 增强版重试配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct RetryConfig {
    #[serde(default)]
    pub max_retries: i32,
    /// 基础重试间隔（毫秒）
    #[serde(default)]
    pub retry_interval: i32,
    /// 退避策略（默认 fixed，向后兼容）
    #[serde(default = "default_backoff_strategy")]
    pub backoff_strategy: BackoffStrategy,
    /// 指数退避时的倍率（默认 2.0）
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,
    /// 最大重试间隔（毫秒），避免退避时间无限增长
    #[serde(default = "default_max_retry_interval")]
    pub max_retry_interval: i32,
    /// 仅对可重试错误重试（默认 true）
    #[serde(default = "default_retry_on_retryable_only")]
    pub retry_on_retryable_only: bool,
}
```

> 向后兼容：旧 DSL 只包含 `max_retries` + `retry_interval` 时，新增字段全部使用默认值。`backoff_strategy` 默认 `Fixed` 与当前行为一致。

### 4.2 NodeData 增加 timeout_secs

```rust
// src/dsl/schema.rs

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct NodeData {
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub error_strategy: Option<ErrorStrategyConfig>,
    #[serde(default)]
    pub retry_config: Option<RetryConfig>,
    /// 新增：节点级执行超时（秒），None 表示使用全局超时
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}
```

### 4.3 NodeRunResult 增加 error_detail

```rust
// src/dsl/schema.rs

#[derive(Debug, Clone)]
pub struct NodeRunResult {
    pub status: WorkflowNodeExecutionStatus,
    pub inputs: HashMap<String, Value>,
    pub process_data: HashMap<String, Value>,
    pub outputs: HashMap<String, Value>,
    pub metadata: HashMap<String, Value>,
    pub llm_usage: Option<LlmUsage>,
    pub edge_source_handle: String,
    pub error: Option<String>,
    /// 修改：现在由 dispatcher 从 NodeError.error_code() 自动填充
    pub error_type: Option<String>,
    pub retry_index: i32,
    /// 新增：结构化错误详情（从 NodeError.to_structured_json() 生成）
    pub error_detail: Option<serde_json::Value>,
}
```

### 4.4 DSL YAML 示例

```yaml
nodes:
  - id: http_1
    data:
      type: http-request
      title: Call API
      url: "https://api.example.com/data"
      method: GET
      timeout_secs: 30                       # 节点级超时
      retry_config:
        max_retries: 3
        retry_interval: 1000
        backoff_strategy: exponential         # 指数退避
        backoff_multiplier: 2.0
        max_retry_interval: 30000            # 最大 30s
        retry_on_retryable_only: true        # 仅对可重试错误重试
      error_strategy:
        type: fail-branch

  - id: error_handler
    data:
      type: template-transform
      title: Handle Error
      template: "Error: {{#http_1.error#}}"
edges:
  - source: http_1
    target: next_node
    sourceHandle: source
  - source: http_1
    target: error_handler
    sourceHandle: fail-branch               # 错误分支
```

---

## 五、Dispatcher 改造

### 5.1 类型化 error_strategy 读取

替换原始 JSON 字符串匹配，改为反序列化为已有的类型：

```rust
// src/core/dispatcher.rs

// 旧代码 (第186-195行):
//   let error_strategy = node_config.get("error_strategy");
//   let retry_config = node_config.get("retry_config");
//   let max_retries = retry_config.and_then(...).unwrap_or(0);

// 新代码:
let error_strategy: Option<ErrorStrategyConfig> = node_config
    .get("error_strategy")
    .and_then(|v| serde_json::from_value(v.clone()).ok());

let retry_config: Option<RetryConfig> = node_config
    .get("retry_config")
    .and_then(|v| serde_json::from_value(v.clone()).ok());

let node_timeout: Option<u64> = node_config
    .get("timeout_secs")
    .and_then(|v| v.as_u64());
```

### 5.2 可重试性感知的重试循环

```rust
// src/core/dispatcher.rs — 新重试逻辑

let max_retries = retry_config.as_ref().map(|rc| rc.max_retries).unwrap_or(0);
let retry_on_retryable_only = retry_config.as_ref()
    .map(|rc| rc.retry_on_retryable_only)
    .unwrap_or(true);

let mut last_error: Option<NodeError> = None;
let mut result = None;

for attempt in 0..=max_retries {
    // 节点级超时包装
    let exec_future = exec.execute(&node_id, &node_config, &pool_snapshot, &self.context);
    let exec_result = if let Some(timeout_secs) = node_timeout {
        match tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            exec_future,
        ).await {
            Ok(r) => r,
            Err(_) => Err(NodeError::Timeout.with_context(
                ErrorContext::retryable(ErrorCode::Timeout,
                    format!("Node execution timed out after {}s", timeout_secs))
            )),
        }
    } else {
        exec_future.await
    };

    match exec_result {
        Ok(r) => {
            result = Some(r);
            break;
        }
        Err(e) => {
            // 判断是否应该重试
            let should_retry = if attempt < max_retries {
                if retry_on_retryable_only {
                    e.is_retryable()  // 只重试可重试错误
                } else {
                    true              // 所有错误都重试
                }
            } else {
                false
            };

            if should_retry {
                let interval = calculate_retry_interval(
                    &retry_config, attempt, &e
                );

                let _ = self.event_tx.send(GraphEngineEvent::NodeRunRetry {
                    id: exec_id.clone(),
                    node_id: node_id.clone(),
                    node_type: node_type.clone(),
                    node_title: node_title.clone(),
                    error: e.to_string(),
                    retry_index: attempt + 1,
                }).await;

                if interval > 0 {
                    tokio::time::sleep(
                        std::time::Duration::from_millis(interval)
                    ).await;
                }
            }

            last_error = Some(e);

            if !should_retry && attempt < max_retries {
                // 不可重试错误，提前跳出循环
                break;
            }
        }
    }
}
```

### 5.3 退避间隔计算

```rust
// src/core/dispatcher.rs (新增函数)

fn calculate_retry_interval(
    retry_config: &Option<RetryConfig>,
    attempt: i32,
    error: &NodeError,
) -> u64 {
    let rc = match retry_config {
        Some(rc) => rc,
        None => return 0,
    };

    // 优先使用错误自身建议的 retry_after（如 429 响应的 Retry-After header）
    if let Some(ctx) = error.error_context() {
        if let Some(retry_after) = ctx.retry_after_secs {
            return retry_after * 1000; // 转为毫秒
        }
    }

    let base = rc.retry_interval as u64;
    let interval = match rc.backoff_strategy {
        BackoffStrategy::Fixed => base,
        BackoffStrategy::Exponential => {
            let multiplied = base as f64 * rc.backoff_multiplier.powi(attempt);
            multiplied as u64
        }
        BackoffStrategy::ExponentialWithJitter => {
            let multiplied = base as f64 * rc.backoff_multiplier.powi(attempt);
            // 添加 ±10% 随机抖动，避免多个重试同时发起
            let jitter = rand::random::<f64>() * multiplied * 0.1;
            (multiplied + jitter) as u64
        }
    };

    // 受 max_retry_interval 限制
    interval.min(rc.max_retry_interval as u64)
}
```

### 5.4 错误策略使用类型化匹配

```rust
// 替换原来的字符串匹配

match result {
    Some(r) => Ok(r),
    None => {
        let last_err = last_error.unwrap_or_else(||
            NodeError::ExecutionError("Unknown error".to_string()));
        let error_type = last_err.error_code();
        let error_detail = last_err.to_structured_json();

        match error_strategy.as_ref().map(|es| &es.strategy_type) {
            Some(ErrorStrategyType::FailBranch) => {
                self.exceptions_count += 1;
                Ok(NodeRunResult {
                    status: WorkflowNodeExecutionStatus::Exception,
                    error: Some(last_err.to_string()),
                    error_type: Some(error_type),           // 现在有值了
                    error_detail: Some(error_detail),       // 结构化错误
                    edge_source_handle: "fail-branch".to_string(),
                    ..Default::default()
                })
            }
            Some(ErrorStrategyType::DefaultValue) => {
                self.exceptions_count += 1;
                let defaults = error_strategy
                    .and_then(|es| es.default_value)
                    .unwrap_or_default();
                Ok(NodeRunResult {
                    status: WorkflowNodeExecutionStatus::Exception,
                    outputs: defaults,
                    error: Some(last_err.to_string()),
                    error_type: Some(error_type),
                    error_detail: Some(error_detail),
                    edge_source_handle: "source".to_string(),
                    ..Default::default()
                })
            }
            Some(ErrorStrategyType::None) | None => {
                Err(last_err)
            }
        }
    }
}
```

### 5.5 完整错误流程图

```
┌─────────────────────────────────────────────────────────────────┐
│                     Dispatcher 错误处理流程                       │
│                                                                  │
│  executor.execute()                                              │
│       │                                                          │
│       ├── Ok(result) ──────────────────────────────→ 正常路径     │
│       │                                                          │
│       └── Err(NodeError)                                         │
│            │                                                     │
│            ├── 检查 node_timeout → 超时则包装为 Timeout + Context │
│            │                                                     │
│            ├── 检查 is_retryable() + retry_on_retryable_only     │
│            │    │                                                │
│            │    ├── 应重试 → calculate_retry_interval()           │
│            │    │    │  优先用 error.retry_after_secs             │
│            │    │    │  否则按 backoff_strategy 计算               │
│            │    │    └── 发送 NodeRunRetry 事件 → sleep → 重试     │
│            │    │                                                │
│            │    └── 不可重试 → 提前结束重试循环                     │
│            │                                                     │
│            └── 所有重试耗尽 → 匹配 ErrorStrategyType               │
│                 │                                                │
│                 ├── FailBranch                                    │
│                 │    status = Exception                           │
│                 │    error_type = error.error_code()              │
│                 │    error_detail = error.to_structured_json()    │
│                 │    edge_source_handle = "fail-branch"           │
│                 │    → NodeRunException 事件                      │
│                 │    → 路由到 fail-branch 边                      │
│                 │                                                │
│                 ├── DefaultValue                                  │
│                 │    status = Exception                           │
│                 │    outputs = default_value                      │
│                 │    error_type / error_detail 同上               │
│                 │    edge_source_handle = "source"                │
│                 │    → NodeRunException 事件                      │
│                 │    → 继续正常路径                                │
│                 │                                                │
│                 └── None (默认)                                   │
│                      → NodeRunFailed 事件                         │
│                      → GraphRunFailed 事件                        │
│                      → 终止工作流                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## 六、节点类型特定错误处理

### 6.1 HTTP 节点 — 状态码感知

当前 `HttpRequestExecutor`（`src/nodes/data_transform.rs:174-292`）对所有 HTTP 响应都返回成功，即使状态码是 4xx/5xx。需要增加可选的状态码错误检测。

新增 `fail_on_error_status` 配置（默认 false，向后兼容）：

```rust
// 在 HttpRequestExecutor::execute 中

let status_code = resp.status().as_u16();

// 如果配置了 fail_on_error_status: true 且状态码 >= 400
if fail_on_error_status && status_code >= 400 {
    let error_msg = format!("HTTP {} {}", status_code, body_preview);
    let context = match status_code {
        401 | 403 => ErrorContext::non_retryable(
            ErrorCode::HttpClientError, error_msg.clone()
        ).with_http_status(status_code),
        429 => {
            let retry_after = resp_headers.get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let mut ctx = ErrorContext::retryable(
                ErrorCode::HttpClientError, error_msg.clone()
            ).with_http_status(429);
            if let Some(ra) = retry_after {
                ctx = ctx.with_retry_after(ra);
            }
            ctx
        }
        400 | 404 | 405 | 422 => ErrorContext::non_retryable(
            ErrorCode::HttpClientError, error_msg.clone()
        ).with_http_status(status_code),
        500..=599 => ErrorContext::retryable(
            ErrorCode::HttpServerError, error_msg.clone()
        ).with_http_status(status_code),
        _ => ErrorContext::non_retryable(
            ErrorCode::HttpClientError, error_msg.clone()
        ).with_http_status(status_code),
    };

    return Err(NodeError::HttpError(error_msg).with_context(context));
}
```

HTTP 状态码 → 可重试性映射表：

| 状态码 | 分类 | 可重试 | 说明 |
|--------|------|--------|------|
| 400 | HttpClientError | 否 | 请求参数错误 |
| 401/403 | HttpClientError | 否 | 认证/权限错误 |
| 404 | HttpClientError | 否 | 资源不存在 |
| 429 | HttpClientError | 是 | 限速，尊重 Retry-After |
| 500 | HttpServerError | 是 | 服务端内部错误 |
| 502/503 | HttpServerError | 是 | 网关/服务不可用 |
| 504 | HttpServerError | 是 | 网关超时 |

### 6.2 LLM 节点 — 限速感知

当前 `LlmError::RateLimitExceeded { retry_after }` 已携带 `retry_after` 信息。通过 3.3 节的新 From 实现，此信息会保留到 `ErrorContext.retry_after_secs`。

OpenAI Provider（`src/llm/provider/openai.rs`）需要改进以从 HTTP 响应 header 提取 Retry-After：

```rust
// openai.rs — 改进错误映射

fn map_error(status: u16, body: &str, headers: &HeaderMap) -> LlmError {
    if status == 401 || status == 403 {
        return LlmError::AuthenticationError(body.to_string());
    }
    if status == 429 {
        let retry_after = headers.get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        return LlmError::RateLimitExceeded { retry_after };
    }
    LlmError::ApiError { status, message: body.to_string() }
}
```

### 6.3 Code 节点 — 沙箱崩溃 vs 逻辑错误

`SandboxError` 已有精细的 variant 区分。在 CodeNodeExecutor 中：

| SandboxError variant | ErrorCode | 可重试 | 说明 |
|---------------------|-----------|--------|------|
| ExecutionTimeout | SandboxTimeout | 是 | 执行超时，可能是临时负载 |
| MemoryLimitExceeded | SandboxMemoryLimit | 否 | 内存超限，重试无意义 |
| CompilationError | SandboxCompilationError | 否 | 编译失败，代码问题 |
| DangerousCode | SandboxDangerousCode | 否 | 危险代码，安全检查拦截 |
| ExecutionError | SandboxExecutionError | 否 | 运行时异常，用户代码逻辑错误 |

当 `result.success == false` 时（`data_transform.rs:396-403`），这是用户代码逻辑错误，标记为 `SandboxExecutionError` + `NonRetryable`。

### 6.4 Iteration 节点 — 实现 IterationErrorMode

这是最重要的缺失功能。DSL 已定义三种模式（`schema.rs:462-468`），执行代码需要实现。

#### 6.4.1 IterationNodeConfig 增加字段

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IterationNodeConfig {
    pub input_selector: Option<Value>,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub parallelism: Option<usize>,
    pub sub_graph: SubGraphDefinition,
    pub output_variable: String,
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// 新增：错误处理模式
    #[serde(default = "default_iteration_error_mode")]
    pub error_handle_mode: IterationErrorMode,
}
```

#### 6.4.2 顺序执行中的三种模式

```rust
// execute_sequential 改造

for (index, item) in items.iter().enumerate() {
    match sub_graph_executor.execute(&config.sub_graph, pool, scope_vars, context).await {
        Ok(result) => {
            results.push(result);
        }
        Err(e) => {
            match config.error_handle_mode {
                IterationErrorMode::Terminated => {
                    // 立即终止整个迭代
                    return Err(NodeError::ExecutionError(
                        format!("Iteration failed at index {}: {}", index, e)
                    ));
                }
                IterationErrorMode::RemoveAbnormal => {
                    // 跳过失败项，不加入结果
                    continue;
                }
                IterationErrorMode::ContinueOnError => {
                    // 失败项加入 null 占位，继续执行
                    results.push(Value::Null);
                }
            }
        }
    }
}
```

#### 6.4.3 并行执行中的三种模式

```rust
// execute_parallel 改造

let mut results = vec![Value::Null; items.len()];
let mut error_indices: Vec<usize> = Vec::new();

for task in tasks {
    let (index, result) = task.await
        .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

    match result {
        Ok(value) => {
            results[index] = value;
        }
        Err(e) => {
            match error_mode {
                IterationErrorMode::Terminated => {
                    // 立即终止（注意：已提交的并行任务会继续运行）
                    return Err(NodeError::ExecutionError(
                        format!("Iteration failed at index {}: {}", index, e)
                    ));
                }
                IterationErrorMode::RemoveAbnormal => {
                    error_indices.push(index);
                }
                IterationErrorMode::ContinueOnError => {
                    results[index] = Value::Null;
                }
            }
        }
    }
}

// RemoveAbnormal: 移除失败项（从大到小移除，避免索引偏移）
if matches!(error_mode, IterationErrorMode::RemoveAbnormal) {
    error_indices.sort_unstable_by(|a, b| b.cmp(a));
    for idx in error_indices {
        results.remove(idx);
    }
}
```

> 注意：RemoveAbnormal 在并行模式下通过记录错误索引后从结果中移除，而非使用 null 标记，避免与正常的 null 结果混淆。

---

## 七、WorkflowError 清理

合并冗余变体：

```rust
// src/error/workflow_error.rs

#[derive(Debug, Error)]
pub enum WorkflowError {
    // ... 保留其他变体 ...

    // 移除 NodeExecutionFailed(String, String)，统一使用 NodeExecutionError
    #[error("Node execution error: node={node_id}, error={error}")]
    NodeExecutionError {
        node_id: String,
        error: String,
        /// 新增：结构化错误详情
        error_detail: Option<serde_json::Value>,
    },

    // ... 其余不变 ...
}
```

Dispatcher 中发送 WorkflowError 时填充 error_detail：

```rust
Err(e) => {
    // ...
    return Err(WorkflowError::NodeExecutionError {
        node_id,
        error: e.to_string(),
        error_detail: Some(e.to_structured_json()),
    });
}
```

---

## 八、事件系统增强

### 8.1 to_json() 增加结构化错误字段

```rust
// src/core/event_bus.rs — to_json 修改

GraphEngineEvent::NodeRunFailed { id, node_id, node_type, node_run_result, error } => {
    serde_json::json!({
        "type": "node_run_failed",
        "data": {
            "id": id,
            "node_id": node_id,
            "node_type": node_type,
            "error": error,
            "error_type": node_run_result.error_type,       // 新增
            "error_detail": node_run_result.error_detail,   // 新增
        }
    })
}

GraphEngineEvent::NodeRunException { id, node_id, node_type, node_run_result, error } => {
    serde_json::json!({
        "type": "node_run_exception",
        "data": {
            "id": id,
            "node_id": node_id,
            "node_type": node_type,
            "error": error,
            "error_type": node_run_result.error_type,       // 新增
            "error_detail": node_run_result.error_detail,   // 新增
        }
    })
}
```

### 8.2 事件消费示例

```json
{
  "type": "node_run_exception",
  "data": {
    "id": "exec_abc123",
    "node_id": "http_1",
    "node_type": "http-request",
    "error": "HTTP 429 Too Many Requests",
    "error_type": "http_client_error",
    "error_detail": {
      "code": "http_client_error",
      "retryability": "retryable",
      "severity": "error",
      "message": "HTTP 429 Too Many Requests",
      "retry_after_secs": 30,
      "http_status": 429
    }
  }
}
```

---

## 九、文件变更清单

### 新增文件

| 文件 | 说明 |
|------|------|
| `src/error/error_context.rs` | ErrorContext, ErrorCode, ErrorRetryability, ErrorSeverity |

### 修改文件

| 文件 | 改动 |
|------|------|
| `src/error/mod.rs` | 添加 `pub mod error_context;` |
| `src/error/node_error.rs` | 新增 `WithContext` variant + `is_retryable()` / `error_code()` / `to_structured_json()` |
| `src/error/workflow_error.rs` | 合并冗余变体，`NodeExecutionError` 增加 `error_detail` |
| `src/dsl/schema.rs` | `RetryConfig` 增加退避字段，`NodeData` 增加 `timeout_secs`，`NodeRunResult` 增加 `error_detail` |
| `src/core/dispatcher.rs` | 类型化 error_strategy、可重试性感知重试、退避计算、节点级超时、填充 error_type/error_detail |
| `src/core/event_bus.rs` | `to_json()` 增加 `error_type` 和 `error_detail` 字段 |
| `src/graph/builder.rs` | `build_node_config` 传递 `timeout_secs` |
| `src/llm/error.rs` | `From<LlmError> for NodeError` 保留 ErrorContext |
| `src/llm/provider/openai.rs` | `map_error` 提取 Retry-After header |
| `src/nodes/data_transform.rs` | CodeNodeExecutor 增加 ErrorContext；HttpRequestExecutor 增加 `fail_on_error_status` 配置 |
| `src/nodes/subgraph_nodes.rs` | IterationNodeExecutor 实现三种 error_handle_mode |
| `src/plugin/error.rs` | `From<PluginError> for NodeError` 保留 ErrorContext |

---

## 十、实施阶段

### Phase 1：错误分类基础设施
1. 新增 `src/error/error_context.rs`
2. 改造 `NodeError` 增加 `WithContext` variant 和工具方法
3. 改造 `From<LlmError>` / `From<PluginError>` 保留结构化信息
4. 更新 `NodeRunResult` 增加 `error_detail` 字段
5. 单元测试：验证 ErrorContext 创建、is_retryable、序列化

### Phase 2：Dispatcher 改造
1. 类型化 error_strategy 读取
2. 可重试性感知的重试循环
3. 退避策略计算（fixed / exponential / exponential_with_jitter）
4. 填充 error_type 和 error_detail 到 NodeRunResult
5. 事件系统 to_json 增加结构化错误
6. 合并 WorkflowError 冗余变体
7. 单元测试 + 现有 dispatcher tests 回归

### Phase 3：节点级超时
1. NodeData 增加 `timeout_secs` 字段
2. builder 传递到 node config
3. dispatcher 用 `tokio::time::timeout` 包装节点执行
4. e2e 测试

### Phase 4：节点类型特定错误处理
1. HTTP 节点：状态码感知错误 + `fail_on_error_status` 配置
2. LLM 节点：OpenAI provider 提取 Retry-After header
3. Code 节点：SandboxError → ErrorContext 精确映射
4. Plugin 节点：PluginError → ErrorContext 映射
5. 各节点类型的单元测试

### Phase 5：Iteration ErrorMode 实现
1. IterationNodeConfig 增加 error_handle_mode 字段
2. execute_sequential 实现三种模式
3. execute_parallel 实现三种模式（含并行 Terminated 的处理）
4. e2e 测试覆盖三种模式

### Phase 6：RetryConfig 增强
1. BackoffStrategy 枚举和新的 RetryConfig 字段
2. dispatcher 中 calculate_retry_interval 函数
3. 向后兼容测试（旧 DSL 只有 max_retries + retry_interval）
4. e2e 测试

---

## 十一、向后兼容性保证

1. **NodeError 旧 variants 保留**：`WithContext` 是新增 variant，所有原有 variant 不变。没有 context 的旧错误通过 `default_retryability()` 提供合理默认行为。

2. **RetryConfig 新字段有默认值**：`backoff_strategy` 默认 `Fixed`，`retry_on_retryable_only` 默认 `true`，`backoff_multiplier` 默认 `2.0`。旧 DSL 只包含 `max_retries` + `retry_interval` 的情况完全兼容。

3. **NodeRunResult.error_detail 是 Option**：默认 None，不影响旧消费者。

4. **error_strategy 类型化读取兼容**：`ErrorStrategyType` 的 serde `rename_all = "kebab-case"` 已经与当前 DSL 中的 `"fail-branch"` / `"default-value"` 字符串匹配。

5. **HttpRequestExecutor 默认行为不变**：`fail_on_error_status` 默认 false，旧工作流对 4xx/5xx 仍返回成功（outputs 中包含 status_code）。

6. **IterationErrorMode 默认 Terminated**：与当前行为一致（当前实际上直接上抛错误，效果等同 Terminated）。

7. **timeout_secs 是 Option**：默认 None，不设置节点级超时时使用全局超时。

---

## 十二、验证方式

```bash
# 全量编译
cargo build

# 全量测试
cargo test

# 错误分类系统测试
cargo test error_context

# Dispatcher 重试逻辑测试
cargo test dispatcher

# Iteration ErrorMode 测试
cargo test iteration_error

# 端到端测试
cargo test e2e
```
