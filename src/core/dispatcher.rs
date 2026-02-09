use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde_json::Value;
use tracing::{debug, error, info, warn};

use crate::core::event_bus::WorkflowEvent;
use crate::core::execution_context::NodeContext;
use crate::core::workflow_runtime::{NodeState, WorkflowRuntime};
use crate::error::WorkflowError;
use crate::graph::EdgeType;
use crate::nodes::{NodeExecutionResult, NodeRegistry};

/// 调度器 - 工作流执行的主控制循环
pub struct Dispatcher {
    runtime: Arc<WorkflowRuntime>,
    node_registry: Arc<NodeRegistry>,
}

impl Dispatcher {
    pub fn new(runtime: Arc<WorkflowRuntime>, node_registry: Arc<NodeRegistry>) -> Self {
        Dispatcher {
            runtime,
            node_registry,
        }
    }

    /// 运行工作流
    pub async fn run(&self) -> Result<Value, WorkflowError> {
        // 1. 将 Start 节点推入队列
        let start_node_id = self.runtime.definition.get_start_node_id();
        self.enqueue_node(&start_node_id).await;

        info!(
            execution_id = %self.runtime.execution_id,
            "Workflow execution started"
        );

        // 2. 主循环
        loop {
            // 从队列中取出节点
            let node_id = {
                let mut queue = self.runtime.execution_queue.lock().await;
                queue.pop_front()
            };

            match node_id {
                Some(node_id) => {
                    // 执行节点
                    self.execute_node(&node_id).await?;

                    // 检查是否完成
                    if self.is_workflow_completed().await? {
                        info!(
                            execution_id = %self.runtime.execution_id,
                            "Workflow execution completed"
                        );
                        return self.collect_outputs().await;
                    }
                }
                None => {
                    // 队列为空，检查是否有正在运行的节点
                    let states = self.runtime.node_states.lock().await;
                    let has_running = states
                        .values()
                        .any(|s| matches!(s, NodeState::Running));
                    drop(states);

                    if has_running {
                        // 等待一小段时间再检查
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    } else if self.is_workflow_completed().await? {
                        return self.collect_outputs().await;
                    } else {
                        // 没有运行中的节点，也没有就绪节点，检查是否死锁
                        warn!("No running or ready nodes, workflow may be stuck");
                        return self.collect_outputs().await;
                    }
                }
            }
        }
    }

    /// 执行单个节点
    async fn execute_node(&self, node_id: &str) -> Result<(), WorkflowError> {
        // 获取节点定义
        let node = self.runtime.definition.get_node(node_id)?;

        // 获取执行器
        let executor = self
            .node_registry
            .get(&node.node_type)
            .ok_or_else(|| WorkflowError::ExecutorNotFound(node.node_type.clone()))?;

        // 创建上下文
        let ctx = NodeContext {
            node_id: node_id.to_string(),
            node_type: node.node_type.clone(),
            config: node.config.clone(),
            execution_id: self.runtime.execution_id.clone(),
            user_id: self.runtime.user_id.clone(),
            title: node.title.clone(),
        };

        // 更新状态为 Running
        {
            let mut states = self.runtime.node_states.lock().await;
            states.insert(node_id.to_string(), NodeState::Running);
        }

        // 发送开始事件
        let _ = self.runtime.event_sender.send(WorkflowEvent::NodeStarted {
            node_id: node_id.to_string(),
            timestamp: Utc::now(),
        });

        debug!(node_id = %node_id, node_type = %node.node_type, "Executing node");

        // 执行节点
        let result = executor
            .execute(&ctx, &self.runtime.variable_pool, &self.runtime.event_sender)
            .await;

        match result {
            Ok(exec_result) => {
                self.handle_node_success(node_id, exec_result).await?;
            }
            Err(e) => {
                self.handle_node_failure(node_id, &e.to_string()).await?;
                return Err(WorkflowError::NodeExecutionFailed(
                    node_id.to_string(),
                    e.to_string(),
                ));
            }
        }

        Ok(())
    }

    /// 处理节点执行成功
    async fn handle_node_success(
        &self,
        node_id: &str,
        result: NodeExecutionResult,
    ) -> Result<(), WorkflowError> {
        match result {
            NodeExecutionResult::Completed(output) => {
                // 更新状态
                {
                    let mut states = self.runtime.node_states.lock().await;
                    states.insert(node_id.to_string(), NodeState::Completed);
                }

                // 写入变量池
                self.runtime.variable_pool.set_node_output(node_id, output.clone());

                // 发送完成事件
                let _ = self
                    .runtime
                    .event_sender
                    .send(WorkflowEvent::NodeFinished {
                        node_id: node_id.to_string(),
                        output,
                        timestamp: Utc::now(),
                    });

                // 查找并入队后继节点
                self.enqueue_successors(node_id).await?;
            }

            NodeExecutionResult::BranchSelected(selected_branch) => {
                // 更新状态
                {
                    let mut states = self.runtime.node_states.lock().await;
                    states.insert(node_id.to_string(), NodeState::Completed);
                }

                // 写入空输出
                self.runtime
                    .variable_pool
                    .set_node_output(node_id, serde_json::json!({"branch": &selected_branch}));

                // 发送分支选择事件
                let _ = self
                    .runtime
                    .event_sender
                    .send(WorkflowEvent::BranchSelected {
                        node_id: node_id.to_string(),
                        selected_branch: selected_branch.clone(),
                        timestamp: Utc::now(),
                    });

                // 根据分支选择入队对应的后继节点
                self.enqueue_branch_successor(node_id, &selected_branch)
                    .await?;
            }

            NodeExecutionResult::IterationCompleted(results) => {
                // 更新状态
                {
                    let mut states = self.runtime.node_states.lock().await;
                    states.insert(node_id.to_string(), NodeState::Completed);
                }

                // 写入迭代结果
                let output = serde_json::json!({ "outputs": results });
                self.runtime.variable_pool.set_node_output(node_id, output.clone());

                let _ = self
                    .runtime
                    .event_sender
                    .send(WorkflowEvent::NodeFinished {
                        node_id: node_id.to_string(),
                        output,
                        timestamp: Utc::now(),
                    });

                self.enqueue_successors(node_id).await?;
            }

            NodeExecutionResult::Failed(error) => {
                self.handle_node_failure(node_id, &error).await?;
                return Err(WorkflowError::NodeExecutionFailed(
                    node_id.to_string(),
                    error,
                ));
            }

            NodeExecutionResult::Skipped => {
                let mut states = self.runtime.node_states.lock().await;
                states.insert(node_id.to_string(), NodeState::Skipped);
            }
        }

        Ok(())
    }

    /// 处理节点执行失败
    async fn handle_node_failure(
        &self,
        node_id: &str,
        error: &str,
    ) -> Result<(), WorkflowError> {
        // 更新状态
        {
            let mut states = self.runtime.node_states.lock().await;
            states.insert(node_id.to_string(), NodeState::Failed(error.to_string()));
        }

        // 发送失败事件
        let _ = self.runtime.event_sender.send(WorkflowEvent::NodeFailed {
            node_id: node_id.to_string(),
            error: error.to_string(),
            timestamp: Utc::now(),
        });

        error!(node_id = %node_id, error = %error, "Node execution failed");

        Ok(())
    }

    /// 查找并入队所有满足依赖的后继节点
    async fn enqueue_successors(&self, node_id: &str) -> Result<(), WorkflowError> {
        let successors = self.runtime.definition.get_successors(node_id)?;

        for successor_id in successors {
            if self.are_dependencies_met(&successor_id).await? {
                self.enqueue_node(&successor_id).await;
            }
        }

        Ok(())
    }

    /// 入队分支节点的后继（只入队选中的分支）
    async fn enqueue_branch_successor(
        &self,
        node_id: &str,
        selected_branch: &str,
    ) -> Result<(), WorkflowError> {
        let edge_type = match selected_branch {
            "true" => EdgeType::TrueBranch,
            "false" => EdgeType::FalseBranch,
            _ => EdgeType::Normal,
        };

        if let Some(target_id) = self
            .runtime
            .definition
            .get_successor_by_edge_type(node_id, &edge_type)?
        {
            self.enqueue_node(&target_id).await;
        }

        // 标记未选中的分支节点为 Skipped
        let other_edge_type = match selected_branch {
            "true" => EdgeType::FalseBranch,
            "false" => EdgeType::TrueBranch,
            _ => return Ok(()),
        };

        if let Some(skipped_id) = self
            .runtime
            .definition
            .get_successor_by_edge_type(node_id, &other_edge_type)?
        {
            self.skip_branch(&skipped_id).await?;
        }

        Ok(())
    }

    /// 递归标记分支节点为 Skipped
    async fn skip_branch(&self, node_id: &str) -> Result<(), WorkflowError> {
        {
            let mut states = self.runtime.node_states.lock().await;
            states.insert(node_id.to_string(), NodeState::Skipped);
        }

        // 查找该节点的后继节点
        let successors = self.runtime.definition.get_successors(node_id)?;
        for successor_id in &successors {
            // 检查该后继节点是否只有一个前驱（被跳过的节点），或者是否是 end 节点
            let predecessors = self.runtime.definition.get_predecessors(successor_id)?;

            // 如果这个后继节点的所有前驱都在 skipped 分支中，也标记为 skipped
            // 但如果它有其他非 skipped 前驱（如从其他分支汇合），则不标记
            if predecessors.len() == 1 {
                // 只有一个前驱，可以安全标记为 skipped
                let node = self.runtime.definition.get_node(successor_id)?;
                if node.node_type != "end" {
                    // 不要跳过 end 节点，使用 Box::pin 处理递归 async
                    Box::pin(self.skip_branch(successor_id)).await?;
                }
            }
        }

        Ok(())
    }

    /// 检查节点的所有依赖是否满足
    async fn are_dependencies_met(&self, node_id: &str) -> Result<bool, WorkflowError> {
        let predecessors = self.runtime.definition.get_predecessors(node_id)?;

        let states = self.runtime.node_states.lock().await;
        for pred_id in &predecessors {
            match states.get(pred_id) {
                Some(NodeState::Completed) => continue,
                Some(NodeState::Skipped) => continue,
                _ => return Ok(false),
            }
        }

        Ok(true)
    }

    /// 检查工作流是否完成
    async fn is_workflow_completed(&self) -> Result<bool, WorkflowError> {
        let end_node_id = self.runtime.definition.get_end_node_id()?;
        let states = self.runtime.node_states.lock().await;

        match states.get(&end_node_id) {
            Some(NodeState::Completed) => Ok(true),
            Some(NodeState::Skipped) => Ok(true),
            _ => Ok(false),
        }
    }

    /// 收集工作流输出
    async fn collect_outputs(&self) -> Result<Value, WorkflowError> {
        let end_node_id = self.runtime.definition.get_end_node_id()?;

        let outputs = self
            .runtime
            .variable_pool
            .get_node_output(&end_node_id)
            .unwrap_or(serde_json::json!({}));

        // 发送工作流完成事件
        let _ = self
            .runtime
            .event_sender
            .send(WorkflowEvent::WorkflowCompleted {
                execution_id: self.runtime.execution_id.clone(),
                outputs: outputs.clone(),
                timestamp: Utc::now(),
            });

        Ok(outputs)
    }

    /// 将节点推入执行队列
    async fn enqueue_node(&self, node_id: &str) {
        let mut queue = self.runtime.execution_queue.lock().await;
        // 避免重复入队
        if !queue.contains(&node_id.to_string()) {
            queue.push_back(node_id.to_string());
            debug!(node_id = %node_id, "Node enqueued");
        }
    }
}
