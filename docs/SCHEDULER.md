# XWorkflow - 异步并发工作流调度器使用指南

## 核心功能

`WorkflowScheduler` 是一个全局工作流调度器，支持：
- ✅ **异步执行**：所有工作流任务异步非阻塞执行
- ✅ **并发调度**：支持同时提交和执行多个工作流
- ✅ **状态查询**：实时查询任务执行状态
- ✅ **事件订阅**：可订阅工作流执行过程中的事件流
- ✅ **结果等待**：提供 `wait()` 接口等待任务完成

## 快速开始

### 1. 创建调度器

```rust
use std::sync::Arc;
use xworkflow::WorkflowScheduler;

#[tokio::main]
async fn main() {
    // 创建全局调度器
    let scheduler = Arc::new(WorkflowScheduler::new());
    
    // 启动后台服务
    let _service_handle = scheduler.clone().start();
    
    // 现在可以提交任务了...
}
```

### 2. 提交工作流任务

```rust
use serde_json::json;
use xworkflow::dsl::DslFormat;

let dsl = r#"
nodes:
  - id: start_1
    type: start
    data: {}
  - id: template_1
    type: template
    data:
      template: "Hello {{#input.name#}}!"
  - id: end_1
    type: end
    data:
      outputs:
        - name: greeting
          variable_selector: "template_1.output"
edges:
  - source: start_1
    target: template_1
  - source: template_1
    target: end_1
"#;

// 提交任务
let handle = scheduler.submit(
    dsl, 
    DslFormat::Yaml, 
    json!({"name": "World"}),
    "user_123"
).await.unwrap();

println!("任务已提交: {}", handle.execution_id);
```

### 3. 等待任务完成

```rust
// 方式 1: 等待并获取结果
let output = handle.wait().await.unwrap();
println!("结果: {}", output);

// 方式 2: 查询状态
let status = handle.status().await.unwrap();
match status {
    ExecutionStatus::Completed(output) => println!("完成: {}", output),
    ExecutionStatus::Running => println!("运行中..."),
    ExecutionStatus::Failed(err) => eprintln!("失败: {}", err),
    _ => {}
}
```

## 并发执行示例

```rust
// 并发提交 10 个工作流
let mut handles = Vec::new();
for i in 0..10 {
    let handle = scheduler.submit(
        dsl,
        DslFormat::Yaml,
        json!({"task_id": i}),
        "user_123"
    ).await.unwrap();
    handles.push(handle);
}

// 等待所有任务完成
for handle in handles {
    match handle.wait().await {
        Ok(output) => println!("✅ 任务完成: {}", output),
        Err(e) => eprintln!("❌ 任务失败: {}", e),
    }
}
```

## API 文档

### `WorkflowScheduler`

#### `new() -> Self`
创建新的调度器实例。

#### `start(self: Arc<Self>) -> JoinHandle<()>`
启动后台状态查询服务。必须在提交任务前调用。

#### `async submit(...) -> Result<WorkflowHandle>`
提交工作流任务到调度器。

**参数：**
- `dsl_content: &str` - 工作流 DSL（YAML 或 JSON）
- `format: DslFormat` - DSL 格式
- `inputs: Value` - 输入参数
- `user_id: &str` - 用户标识

**返回：** `WorkflowHandle` - 任务句柄

#### `async list_executions() -> Vec<String>`
获取所有正在执行的任务 ID 列表。

#### `async cleanup_completed()`
清理已完成的任务记录。

### `WorkflowHandle`

#### `async status() -> Result<ExecutionStatus>`
查询任务当前状态。

#### `async wait() -> Result<Value>`
阻塞等待任务完成并返回结果。

#### `async subscribe_events() -> Result<EventReceiver>`
订阅任务的事件流（节点开始、完成、失败等）。

### `ExecutionStatus`

```rust
pub enum ExecutionStatus {
    Pending,              // 等待执行
    Running,              // 正在运行
    Completed(Value),     // 执行成功
    Failed(String),       // 执行失败
}
```

## 运行示例

```bash
# 运行调度器演示
cargo run --example scheduler_demo

# 运行测试
cargo test scheduler::tests
```

## 架构说明

```
┌─────────────────────────────────────┐
│     WorkflowScheduler (全局)        │
│  ┌───────────────────────────────┐  │
│  │  任务注册表 (HashMap)         │  │
│  │  - execution_id -> Task       │  │
│  └───────────────────────────────┘  │
│  ┌───────────────────────────────┐  │
│  │  状态查询服务 (tokio task)    │  │
│  └───────────────────────────────┘  │
└──────────┬──────────────────────────┘
           │ submit()
           ▼
    ┌─────────────┐
    │ WorkflowHandle │ ◄── 返回给调用方
    │ - execution_id │
    │ - status()     │
    │ - wait()       │
    └─────────────┘
           │
           ▼
    每个任务独立执行
    (tokio::spawn)
```

## 性能特点

- **非阻塞**：所有任务异步执行，互不阻塞
- **内存高效**：使用 Arc 共享不可变数据（NodeRegistry、WorkflowDefinition）
- **线程安全**：使用 RwLock + DashMap 保证并发安全
- **事件驱动**：基于 tokio mpsc channel 的事件总线

## 注意事项

1. **必须启动服务**：调用 `scheduler.start()` 后才能查询状态
2. **资源清理**：长期运行建议定期调用 `cleanup_completed()`
3. **并发限制**：当前无并发限制，可根据需要添加信号量控制
4. **事件订阅**：当前事件订阅为简化实现，后续可改为广播模式

## 后续优化建议

- [ ] 添加并发限制（信号量）
- [ ] 支持任务取消
- [ ] 支持任务优先级
- [ ] 改进事件订阅为广播模式
- [ ] 添加任务执行超时控制
- [ ] 持久化任务状态（可选）
