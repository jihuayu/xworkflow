use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, RwLock};

use crate::core::dispatcher::Dispatcher;
use crate::core::event_bus::{create_event_channel, EventReceiver, EventSender, WorkflowEvent};
use crate::core::workflow_runtime::WorkflowRuntime;
use crate::dsl::{parse_dsl, validate_workflow_schema, DslFormat};
use crate::error::WorkflowError;
use crate::graph::builder::build_graph;
use crate::graph::validator::validate_graph;
use crate::nodes::registry::{create_default_registry, NodeRegistry};

/// 工作流执行状态
#[derive(Debug, Clone)]
pub enum ExecutionStatus {
    /// 等待执行
    Pending,
    /// 正在运行
    Running,
    /// 执行成功
    Completed(Value),
    /// 执行失败
    Failed(String),
}

/// 工作流任务句柄
#[derive(Clone)]
pub struct WorkflowHandle {
    /// 执行 ID
    pub execution_id: String,
    /// 状态查询通道
    status_tx: mpsc::UnboundedSender<StatusRequest>,
}

impl WorkflowHandle {
    /// 获取执行状态
    pub async fn status(&self) -> Result<ExecutionStatus, WorkflowError> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.status_tx
            .send(StatusRequest::GetStatus {
                execution_id: self.execution_id.clone(),
                response: tx,
            })
            .map_err(|_| WorkflowError::InternalError("Scheduler stopped".to_string()))?;

        rx.recv()
            .await
            .ok_or_else(|| WorkflowError::InternalError("No response".to_string()))?
    }

    /// 等待执行完成并返回结果
    pub async fn wait(&self) -> Result<Value, WorkflowError> {
        loop {
            match self.status().await? {
                ExecutionStatus::Completed(output) => return Ok(output),
                ExecutionStatus::Failed(error) => {
                    return Err(WorkflowError::InternalError(error))
                }
                _ => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// 订阅工作流事件流
    pub async fn subscribe_events(&self) -> Result<EventReceiver, WorkflowError> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.status_tx
            .send(StatusRequest::SubscribeEvents {
                execution_id: self.execution_id.clone(),
                response: tx,
            })
            .map_err(|_| WorkflowError::InternalError("Scheduler stopped".to_string()))?;

        rx.recv()
            .await
            .ok_or_else(|| WorkflowError::InternalError("No response".to_string()))?
    }
}

/// 状态查询请求
enum StatusRequest {
    GetStatus {
        execution_id: String,
        response: mpsc::UnboundedSender<Result<ExecutionStatus, WorkflowError>>,
    },
    SubscribeEvents {
        execution_id: String,
        response: mpsc::UnboundedSender<Result<EventReceiver, WorkflowError>>,
    },
}

/// 执行任务信息
struct ExecutionTask {
    #[allow(dead_code)]
    execution_id: String,
    status: ExecutionStatus,
    #[allow(dead_code)]
    event_tx: EventSender,
}

/// 工作流调度器 - 支持并发执行多个工作流
pub struct WorkflowScheduler {
    /// 节点注册表（共享）
    node_registry: Arc<NodeRegistry>,
    /// 正在执行的任务
    tasks: Arc<RwLock<HashMap<String, ExecutionTask>>>,
    /// 状态查询通道
    status_rx: Arc<RwLock<mpsc::UnboundedReceiver<StatusRequest>>>,
    status_tx: mpsc::UnboundedSender<StatusRequest>,
}

impl WorkflowScheduler {
    /// 创建新的调度器
    pub fn new() -> Self {
        let (status_tx, status_rx) = mpsc::unbounded_channel();

        WorkflowScheduler {
            node_registry: Arc::new(create_default_registry()),
            tasks: Arc::new(RwLock::new(HashMap::new())),
            status_rx: Arc::new(RwLock::new(status_rx)),
            status_tx,
        }
    }

    /// 启动调度器后台服务
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run_status_service().await;
        })
    }

    /// 运行状态查询服务
    async fn run_status_service(&self) {
        let mut rx = self.status_rx.write().await;
        while let Some(request) = rx.recv().await {
            match request {
                StatusRequest::GetStatus {
                    execution_id,
                    response,
                } => {
                    let tasks = self.tasks.read().await;
                    let status = tasks
                        .get(&execution_id)
                        .map(|task| task.status.clone())
                        .unwrap_or(ExecutionStatus::Failed(
                            "Execution not found".to_string(),
                        ));
                    let _ = response.send(Ok(status));
                }
                StatusRequest::SubscribeEvents {
                    execution_id,
                    response,
                } => {
                    let tasks = self.tasks.read().await;
                    let result = tasks
                        .get(&execution_id)
                        .map(|_task| {
                            // 创建新的事件接收器（广播模式需要改造，这里简化处理）
                            let (_tx, rx) = create_event_channel();
                            Ok(rx)
                        })
                        .unwrap_or_else(|| {
                            Err(WorkflowError::InternalError(
                                "Execution not found".to_string(),
                            ))
                        });
                    let _ = response.send(result);
                }
            }
        }
    }

    /// 提交工作流执行任务
    pub async fn submit(
        &self,
        dsl_content: &str,
        format: DslFormat,
        inputs: Value,
        user_id: &str,
    ) -> Result<WorkflowHandle, WorkflowError> {
        // 1. 解析和验证 DSL
        let schema = parse_dsl(dsl_content, format)
            .map_err(|e| WorkflowError::DslParseError(e.to_string()))?;
        validate_workflow_schema(&schema)
            .map_err(|e| WorkflowError::GraphValidationError(e.to_string()))?;

        // 2. 构建图
        let definition = build_graph(&schema)?;
        validate_graph(&definition.graph)?;
        let definition = Arc::new(definition);

        // 3. 创建运行时
        let (event_sender, mut event_receiver) = create_event_channel();
        let runtime = Arc::new(WorkflowRuntime::new(
            definition,
            inputs,
            user_id.to_string(),
            event_sender.clone(),
        ));

        let execution_id = runtime.execution_id.clone();

        // 4. 注册任务
        {
            let mut tasks = self.tasks.write().await;
            tasks.insert(
                execution_id.clone(),
                ExecutionTask {
                    execution_id: execution_id.clone(),
                    status: ExecutionStatus::Pending,
                    event_tx: event_sender,
                },
            );
        }

        // 5. 创建调度器并异步执行
        let dispatcher = Dispatcher::new(runtime, self.node_registry.clone());
        let tasks = self.tasks.clone();
        let exec_id = execution_id.clone();

        tokio::spawn(async move {
            // 标记为运行中
            {
                let mut tasks_guard = tasks.write().await;
                if let Some(task) = tasks_guard.get_mut(&exec_id) {
                    task.status = ExecutionStatus::Running;
                }
            }

            // 执行工作流
            let result = dispatcher.run().await;

            // 更新最终状态
            {
                let mut tasks_guard = tasks.write().await;
                if let Some(task) = tasks_guard.get_mut(&exec_id) {
                    task.status = match result {
                        Ok(output) => ExecutionStatus::Completed(output),
                        Err(e) => ExecutionStatus::Failed(e.to_string()),
                    };
                }
            }
        });

        // 6. 启动事件监听器（更新任务状态）
        let tasks_clone = self.tasks.clone();
        let exec_id_clone = execution_id.clone();
        tokio::spawn(async move {
            while let Some(event) = event_receiver.recv().await {
                match event {
                    WorkflowEvent::WorkflowCompleted { outputs, .. } => {
                        let mut tasks = tasks_clone.write().await;
                        if let Some(task) = tasks.get_mut(&exec_id_clone) {
                            task.status = ExecutionStatus::Completed(outputs);
                        }
                        break;
                    }
                    WorkflowEvent::WorkflowFailed { error, .. } => {
                        let mut tasks = tasks_clone.write().await;
                        if let Some(task) = tasks.get_mut(&exec_id_clone) {
                            task.status = ExecutionStatus::Failed(error);
                        }
                        break;
                    }
                    _ => {}
                }
            }
        });

        // 7. 返回句柄
        Ok(WorkflowHandle {
            execution_id,
            status_tx: self.status_tx.clone(),
        })
    }

    /// 获取所有正在执行的任务列表
    pub async fn list_executions(&self) -> Vec<String> {
        let tasks = self.tasks.read().await;
        tasks.keys().cloned().collect()
    }

    /// 清理已完成的任务（可选）
    pub async fn cleanup_completed(&self) {
        let mut tasks = self.tasks.write().await;
        tasks.retain(|_, task| {
            !matches!(
                task.status,
                ExecutionStatus::Completed(_) | ExecutionStatus::Failed(_)
            )
        });
    }
}

impl Default for WorkflowScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn simple_workflow_yaml() -> &'static str {
        r#"
nodes:
  - id: start_1
    type: start
    data: {}
  - id: template_1
    type: template
    data:
      template: "Result: {{#input.value#}}"
  - id: end_1
    type: end
    data:
      outputs:
        - name: result
          variable_selector: "template_1.output"
edges:
  - source: start_1
    target: template_1
  - source: template_1
    target: end_1
"#
    }

    #[tokio::test]
    async fn test_scheduler_single_workflow() {
        let scheduler = Arc::new(WorkflowScheduler::new());
        let _service_handle = scheduler.clone().start();

        let handle = scheduler
            .submit(
                simple_workflow_yaml(),
                DslFormat::Yaml,
                json!({"value": "test123"}),
                "user1",
            )
            .await
            .unwrap();

        let result = handle.wait().await.unwrap();
        assert_eq!(result["result"], "Result: test123");
    }

    #[tokio::test]
    async fn test_scheduler_concurrent_workflows() {
        let scheduler = Arc::new(WorkflowScheduler::new());
        let _service_handle = scheduler.clone().start();

        // 提交 5 个并发工作流
        let mut handles = Vec::new();
        for i in 0..5 {
            let handle = scheduler
                .submit(
                    simple_workflow_yaml(),
                    DslFormat::Yaml,
                    json!({"value": format!("task_{}", i)}),
                    "user1",
                )
                .await
                .unwrap();
            handles.push(handle);
        }

        // 等待所有完成
        for (i, handle) in handles.into_iter().enumerate() {
            let result = handle.wait().await.unwrap();
            assert_eq!(result["result"], format!("Result: task_{}", i));
        }
    }

    #[tokio::test]
    async fn test_scheduler_status_query() {
        let scheduler = Arc::new(WorkflowScheduler::new());
        let _service_handle = scheduler.clone().start();

        let handle = scheduler
            .submit(
                simple_workflow_yaml(),
                DslFormat::Yaml,
                json!({"value": "status_test"}),
                "user1",
            )
            .await
            .unwrap();

        // 检查状态变化
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let status = handle.status().await.unwrap();
        assert!(
            matches!(status, ExecutionStatus::Running | ExecutionStatus::Completed(_))
        );

        // 等待完成
        let _ = handle.wait().await;
        let final_status = handle.status().await.unwrap();
        assert!(matches!(final_status, ExecutionStatus::Completed(_)));
    }
}
