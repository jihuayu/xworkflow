# Error Handler Workflow 设计文档

## 1. 背景与动机

### 1.1 当前错误处理的局限性

xworkflow 目前的错误处理是**节点级**的：

- 每个节点可配置 `error_strategy`（如 `fail-branch`、`default-value`），在节点执行失败时提供局部恢复。
- 当节点错误**未被** `error_strategy` 捕获时，`WorkflowDispatcher` 会立即中止整个工作流（`dispatcher.rs:440-465`），发出 `GraphRunFailed` 事件，并返回 `ExecutionStatus::Failed(String)`。

这意味着：**一旦工作流级别失败，没有任何恢复手段**。用户无法在工作流范围内执行清理、通知或降级逻辑。

### 1.2 工作流级错误恢复的需求场景

| 场景 | 说明 |
|------|------|
| **告警/通知** | 工作流失败后自动发送 Slack/邮件/Webhook 通知 |
| **降级响应** | 主工作流失败后返回缓存结果或默认值，而非直接报错 |
| **日志/审计** | 记录结构化错误上下文到日志系统，用于后续分析 |
| **资源清理** | 释放主工作流中申请的外部资源（临时文件、锁、连接等） |

## 2. DSL Schema 变更

### 2.1 WorkflowSchema 新增字段

在 `WorkflowSchema`（`src/dsl/schema.rs:22-32`）中新增可选的 `error_handler` 字段：

```rust
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct WorkflowSchema {
    pub version: String,
    pub nodes: Vec<NodeSchema>,
    pub edges: Vec<EdgeSchema>,
    #[serde(default)]
    pub environment_variables: Vec<EnvironmentVariable>,
    #[serde(default)]
    pub conversation_variables: Vec<ConversationVariable>,
    // 新增
    #[serde(default)]
    pub error_handler: Option<ErrorHandlerConfig>,
}
```

### 2.2 ErrorHandlerConfig 定义

```rust
/// Error handling mode
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ErrorHandlingMode {
    /// 错误处理器的输出替代工作流最终输出（优雅降级）
    Recover,
    /// 错误处理器仅执行副作用（通知/日志），工作流仍返回 Failed
    Notify,
}

impl Default for ErrorHandlingMode {
    fn default() -> Self {
        Self::Notify
    }
}

/// Workflow-level error handler configuration
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ErrorHandlerConfig {
    /// 错误处理器的子图定义，复用 SubGraphDefinition
    pub sub_graph: SubGraphDefinition,
    /// 错误处理模式
    #[serde(default)]
    pub mode: ErrorHandlingMode,
}
```

### 2.3 错误上下文变量约定

错误处理器子图执行时，以下系统变量会被注入到 VariablePool 中，供子图节点通过 variable selector 读取：

| 变量选择器 | 类型 | 说明 |
|------------|------|------|
| `["sys", "error_message"]` | `String` | 错误的完整描述信息 |
| `["sys", "error_node_id"]` | `String` | 触发错误的节点 ID（如可确定） |
| `["sys", "error_node_type"]` | `String` | 触发错误的节点类型（如可确定） |
| `["sys", "error_type"]` | `String` | 错误类型分类（如 `"NodeExecutionError"`、`"Timeout"` 等） |
| `["sys", "workflow_outputs"]` | `Object` | 主工作流在失败前已经收集到的部分输出（可能为空） |

错误类型分类映射自 `WorkflowError` 的 variant 名称：

```rust
fn error_type_name(error: &WorkflowError) -> &'static str {
    match error {
        WorkflowError::NodeExecutionError { .. } => "NodeExecutionError",
        WorkflowError::NodeExecutionFailed(..) => "NodeExecutionFailed",
        WorkflowError::Timeout | WorkflowError::ExecutionTimeout => "Timeout",
        WorkflowError::MaxStepsExceeded(_) => "MaxStepsExceeded",
        WorkflowError::Aborted(_) => "Aborted",
        _ => "InternalError",
    }
}
```

从 `NodeExecutionError` 中提取 `error_node_id`：

```rust
fn extract_error_node_info(error: &WorkflowError) -> (String, String) {
    match error {
        WorkflowError::NodeExecutionError { node_id, error: _ } => {
            (node_id.clone(), /* 需从 graph 中查找 node_type */ String::new())
        }
        _ => (String::new(), String::new()),
    }
}
```

## 3. 错误处理器执行流程

### 3.1 集成点

错误处理器的触发位置在 `scheduler.rs:198-216`，即 `dispatcher.run()` 返回 `Err` 后：

```
scheduler.rs::start()
  └── tokio::spawn
        └── dispatcher.run().await
              ├── Ok(outputs) → ExecutionStatus::Completed
              └── Err(e) → 【在此处插入错误处理器逻辑】
                    ├── 有 error_handler → 执行错误处理器
                    └── 无 error_handler → ExecutionStatus::Failed（现有行为）
```

### 3.2 详细执行流程

```
1. dispatcher.run() 返回 Err(workflow_error)
2. 检查 schema 是否定义了 error_handler
   ├── 无 → 直接设置 ExecutionStatus::Failed（保持现有行为）
   └── 有 →
       3. 构建错误上下文 HashMap<String, Value>
          - sys.error_message = workflow_error.to_string()
          - sys.error_node_id = extract from error
          - sys.error_node_type = extract from error + graph
          - sys.error_type = error_type_name(&workflow_error)
          - sys.workflow_outputs = partial outputs (if any)
       4. 发出 ErrorHandlerStarted 事件
       5. 使用 SubGraphExecutor 模式执行错误处理器子图：
          - 克隆主工作流的 VariablePool 作为 parent_pool
          - 将错误上下文作为 scope_vars 注入
          - 调用 build_sub_graph + WorkflowDispatcher 执行子图
       6. 根据执行结果和 mode：
          ├── 子图执行成功：
          │   ├── mode = Recover →
          │   │   - 发出 ErrorHandlerSucceeded 事件
          │   │   - 设置 ExecutionStatus::FailedWithRecovery {
          │   │       original_error, recovered_outputs
          │   │     }
          │   └── mode = Notify →
          │       - 发出 ErrorHandlerSucceeded 事件
          │       - 设置 ExecutionStatus::Failed（保持失败状态）
          └── 子图执行失败：
              - 发出 ErrorHandlerFailed 事件
              - 设置 ExecutionStatus::Failed（原始错误信息）
```

### 3.3 伪代码

```rust
// 在 scheduler.rs 的 tokio::spawn 块内
match dispatcher.run().await {
    Ok(outputs) => {
        *status_exec.lock().await = ExecutionStatus::Completed(outputs);
    }
    Err(e) => {
        // 尝试执行错误处理器
        if let Some(error_handler) = &schema.error_handler {
            let error_context = build_error_context(&e, &partial_outputs);

            let _ = event_tx.send(GraphEngineEvent::ErrorHandlerStarted {
                error: e.to_string(),
            }).await;

            let executor = SubGraphExecutor::new();
            match executor.execute(
                &error_handler.sub_graph,
                &main_pool,
                error_context,
                &context,
            ).await {
                Ok(handler_outputs) => {
                    let _ = event_tx.send(GraphEngineEvent::ErrorHandlerSucceeded {
                        outputs: /* extract from handler_outputs */,
                    }).await;

                    match error_handler.mode {
                        ErrorHandlingMode::Recover => {
                            *status_exec.lock().await =
                                ExecutionStatus::FailedWithRecovery {
                                    original_error: e.to_string(),
                                    recovered_outputs: /* from handler_outputs */,
                                };
                        }
                        ErrorHandlingMode::Notify => {
                            *status_exec.lock().await =
                                ExecutionStatus::Failed(e.to_string());
                        }
                    }
                }
                Err(handler_err) => {
                    let _ = event_tx.send(GraphEngineEvent::ErrorHandlerFailed {
                        error: handler_err.to_string(),
                    }).await;
                    *status_exec.lock().await =
                        ExecutionStatus::Failed(e.to_string());
                }
            }
        } else {
            *status_exec.lock().await = ExecutionStatus::Failed(e.to_string());
        }
    }
}
```

### 3.4 Recover 模式 vs Notify 模式

| 特性 | Recover | Notify |
|------|---------|--------|
| **最终状态** | `FailedWithRecovery` | `Failed` |
| **工作流输出** | 错误处理器子图的 end 节点输出 | 无（原始错误信息） |
| **典型用途** | 降级响应、缓存回退 | 告警通知、日志审计、资源清理 |
| **调用方感知** | 可正常获取输出，同时知道发生了降级 | 明确知道工作流失败 |

## 4. 事件系统增强

### 4.1 新增事件

在 `GraphEngineEvent`（`src/core/event_bus.rs:16-145`）中新增三个事件：

```rust
pub enum GraphEngineEvent {
    // ... 已有事件 ...

    // === Error Handler events ===
    ErrorHandlerStarted {
        error: String,
    },
    ErrorHandlerSucceeded {
        outputs: HashMap<String, Value>,
    },
    ErrorHandlerFailed {
        error: String,
    },
}
```

### 4.2 事件时序

```
主工作流执行阶段：
  GraphRunStarted
  NodeRunStarted { ... }
  NodeRunSucceeded/NodeRunFailed { ... }
  ...
  GraphRunFailed { error, exceptions_count }  ← 主工作流失败

错误处理器执行阶段：
  ErrorHandlerStarted { error }               ← 开始执行错误处理器
  （错误处理器内部的节点事件通过独立的 event channel 传递，
    不会混入主工作流的事件流中）
  ErrorHandlerSucceeded { outputs }           ← 处理器执行成功
  或
  ErrorHandlerFailed { error }                ← 处理器执行也失败
```

> **注意**：错误处理器子图内部使用独立的 `mpsc::channel` 传递事件（与 `SubGraphExecutor` 现有模式一致，见 `subgraph.rs:87`）。`ErrorHandlerStarted/Succeeded/Failed` 三个事件通过主工作流的 event channel 发出，保证调用方可以追踪错误处理器的整体状态。

### 4.3 to_json 序列化

新增事件需要在 `GraphEngineEvent::to_json()`（`src/core/event_bus.rs:147+`）中添加对应的序列化分支：

```rust
GraphEngineEvent::ErrorHandlerStarted { error } => json!({
    "event": "error_handler_started",
    "error": error,
}),
GraphEngineEvent::ErrorHandlerSucceeded { outputs } => json!({
    "event": "error_handler_succeeded",
    "outputs": outputs,
}),
GraphEngineEvent::ErrorHandlerFailed { error } => json!({
    "event": "error_handler_failed",
    "error": error,
}),
```

## 5. ExecutionStatus 增强

### 5.1 新增 FailedWithRecovery variant

在 `ExecutionStatus`（`src/scheduler.rs:18-24`）中新增：

```rust
#[derive(Debug, Clone)]
pub enum ExecutionStatus {
    Running,
    Completed(HashMap<String, Value>),
    Failed(String),
    // 新增
    FailedWithRecovery {
        original_error: String,
        recovered_outputs: HashMap<String, Value>,
    },
}
```

### 5.2 调用方使用示例

```rust
match handle.status().await {
    ExecutionStatus::Completed(outputs) => {
        // 正常完成
    }
    ExecutionStatus::FailedWithRecovery { original_error, recovered_outputs } => {
        // 主工作流失败，但错误处理器提供了降级输出
        log::warn!("Workflow failed with recovery: {}", original_error);
        // recovered_outputs 可作为降级响应使用
    }
    ExecutionStatus::Failed(error) => {
        // 工作流失败，无恢复
    }
    ExecutionStatus::Running => {
        // 仍在执行中
    }
}
```

## 6. DSL 示例

### 6.1 Recover 模式 — 降级响应

```yaml
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: http_call
    data:
      type: http-request
      title: Call External API
      method: GET
      url: "https://api.example.com/data"
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["http_call", "body"]

edges:
  - source: start
    target: http_call
  - source: http_call
    target: end

error_handler:
  mode: recover
  sub_graph:
    nodes:
      - id: eh_start
        type: start
        title: Error Handler Start
        data: {}
      - id: log_error
        type: template-transform
        title: Build Fallback Response
        data:
          template: |
            Error occurred: {{ sys.error_message }}
            Error node: {{ sys.error_node_id }}
            Returning fallback response.
          variables:
            - variable: error_msg
              value_selector: ["sys", "error_message"]
            - variable: error_node
              value_selector: ["sys", "error_node_id"]
      - id: eh_end
        type: end
        title: Error Handler End
        data:
          outputs:
            - variable: result
              value_selector: ["log_error", "output"]
    edges:
      - source: eh_start
        target: log_error
      - source: log_error
        target: eh_end
```

### 6.2 Notify 模式 — 告警通知

```yaml
version: "0.1.0"
nodes:
  - id: start
    data:
      type: start
      title: Start
  - id: process
    data:
      type: code
      title: Process Data
      language: javascript
      code: |
        throw new Error("Something went wrong");
  - id: end
    data:
      type: end
      title: End
      outputs:
        - variable: result
          value_selector: ["process", "output"]

edges:
  - source: start
    target: process
  - source: process
    target: end

error_handler:
  mode: notify
  sub_graph:
    nodes:
      - id: eh_start
        type: start
        title: Error Handler Start
        data: {}
      - id: notify
        type: http-request
        title: Send Alert
        data:
          method: POST
          url: "https://hooks.slack.com/services/xxx/yyy/zzz"
          headers:
            Content-Type: "application/json"
          body:
            type: json
            data: |
              {
                "text": "Workflow failed: {{ sys.error_message }}, node: {{ sys.error_node_id }}"
              }
      - id: eh_end
        type: end
        title: Error Handler End
        data:
          outputs: []
    edges:
      - source: eh_start
        target: notify
      - source: notify
        target: eh_end
```

## 7. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src/dsl/schema.rs` | 修改 | `WorkflowSchema` 新增 `error_handler` 字段；新增 `ErrorHandlerConfig`、`ErrorHandlingMode` 类型 |
| `src/nodes/subgraph.rs` | 只读复用 | 复用 `SubGraphDefinition`、`SubGraphExecutor::execute`、`build_sub_graph` 等，无需修改 |
| `src/scheduler.rs` | 修改 | `ExecutionStatus` 新增 `FailedWithRecovery` variant；`start()` 方法中 `Err` 分支添加错误处理器调用逻辑 |
| `src/core/event_bus.rs` | 修改 | `GraphEngineEvent` 新增 `ErrorHandlerStarted`、`ErrorHandlerSucceeded`、`ErrorHandlerFailed`；`to_json()` 新增对应序列化 |
| `src/core/dispatcher.rs` | 无修改 | 错误处理器内部通过 `SubGraphExecutor` 间接使用，无需直接修改 |
| `src/dsl/validator.rs` | 修改 | 新增错误处理器子图的验证逻辑（start/end 节点检查、边引用检查） |
| `src/error/workflow_error.rs` | 可选修改 | 可新增 `error_type_name()` 辅助函数用于错误分类 |

## 8. 实施阶段

### Phase 1: 核心类型定义
- 在 `src/dsl/schema.rs` 中新增 `ErrorHandlerConfig`、`ErrorHandlingMode`
- 在 `WorkflowSchema` 中新增 `error_handler` 字段
- 在 `ExecutionStatus` 中新增 `FailedWithRecovery` variant
- 确保 YAML/JSON 反序列化正常工作

### Phase 2: 事件系统扩展
- 在 `GraphEngineEvent` 中新增三个错误处理器事件
- 实现 `to_json()` 序列化

### Phase 3: 执行逻辑集成
- 在 `scheduler.rs` 的 `start()` 方法中集成错误处理器调用逻辑
- 实现错误上下文构建函数 `build_error_context()`
- 实现错误类型名称映射函数 `error_type_name()`
- 复用 `SubGraphExecutor` 执行错误处理器子图

### Phase 4: 验证与测试
- 在 `src/dsl/validator.rs` 中新增错误处理器子图的验证
- 编写单元测试：error handler 子图执行、recover 模式输出替代、notify 模式保持 Failed
- 编写集成测试：完整工作流 + 错误处理器的端到端测试

## 9. 向后兼容性

- `error_handler` 字段使用 `#[serde(default)]` + `Option<T>`，**不填时为 `None`**，完全兼容已有 DSL 文件。
- `ExecutionStatus::FailedWithRecovery` 是新增 variant，不影响已有的 `Completed` / `Failed` 匹配逻辑。但调用方如果对 `ExecutionStatus` 做了穷尽匹配（`match`），需要新增分支处理——这属于编译时即可发现的 breaking change，影响可控。
- 事件系统新增三个事件，不影响已有事件的发出与消费。
- **DSL 版本**：此变更属于向后兼容的功能扩展，不需要升级 DSL 版本号。如果需要更严格的版本管理，可考虑升级到 `0.2.0` 并同时支持 `0.1.0`。

## 10. 设计决策记录

### Q: 为什么复用 SubGraphDefinition 而不是引用独立的工作流文件？

复用 `SubGraphDefinition`（inline sub-graph）的好处：
1. **零依赖**：错误处理器定义自包含在同一份 DSL 文件中，不需要额外的文件引用机制。
2. **代码复用**：直接复用 `SubGraphExecutor`、`build_sub_graph()` 等已有基础设施，实现成本低。
3. **一致性**：与 iteration/loop 节点中使用 sub-graph 的模式一致。

### Q: 为什么错误处理器本身失败时不递归调用另一个处理器？

避免递归是刻意的设计约束：
1. 防止无限递归（错误处理器的错误处理器的错误处理器……）。
2. 错误处理器应尽量简单可靠（通知、日志、返回默认值），而非复杂业务逻辑。
3. 如果错误处理器也失败，说明系统存在更严重的问题，应直接暴露原始错误。

### Q: 错误处理器是否能访问主工作流的 VariablePool？

**是**。错误处理器子图会克隆主工作流的 VariablePool 作为 parent_pool（与 `SubGraphExecutor::execute` 现有行为一致，见 `subgraph.rs:82`）。这意味着错误处理器可以读取主工作流中已经计算出的中间变量，用于构建更丰富的错误上下文。
