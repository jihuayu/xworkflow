use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::graph::builder::WorkflowDefinition;

use super::event_bus::EventSender;
use super::variable_pool::VariablePool;

/// 节点状态
#[derive(Clone, Debug)]
pub enum NodeState {
    /// 等待执行
    Pending,
    /// 正在执行
    Running,
    /// 执行完成
    Completed,
    /// 执行失败（包含错误信息）
    Failed(String),
    /// 被跳过（如分支未选中）
    Skipped,
}

/// 工作流运行时 - 维护执行过程中的可变状态
pub struct WorkflowRuntime {
    /// 执行 ID（每次运行唯一）
    pub execution_id: String,

    /// 工作流定义（不可变，使用 Arc 共享）
    pub definition: Arc<WorkflowDefinition>,

    /// 变量池（线程安全）
    pub variable_pool: Arc<VariablePool>,

    /// 节点状态映射
    pub node_states: Arc<Mutex<HashMap<String, NodeState>>>,

    /// 执行队列（就绪节点）
    pub execution_queue: Arc<Mutex<VecDeque<String>>>,

    /// 事件发送器
    pub event_sender: EventSender,

    /// 开始时间
    pub start_time: DateTime<Utc>,

    /// 用户 ID
    pub user_id: String,
}

impl WorkflowRuntime {
    /// 创建新的工作流运行时
    pub fn new(
        definition: Arc<WorkflowDefinition>,
        inputs: Value,
        user_id: String,
        event_sender: EventSender,
    ) -> Self {
        let variable_pool = Arc::new(VariablePool::with_inputs(inputs));

        // 初始化所有节点状态为 Pending
        let mut node_states = HashMap::new();
        for idx in definition.graph.node_indices() {
            if let Some(node) = definition.graph.node_weight(idx) {
                node_states.insert(node.id.clone(), NodeState::Pending);
            }
        }

        WorkflowRuntime {
            execution_id: uuid::Uuid::new_v4().to_string(),
            definition,
            variable_pool,
            node_states: Arc::new(Mutex::new(node_states)),
            execution_queue: Arc::new(Mutex::new(VecDeque::new())),
            event_sender,
            start_time: Utc::now(),
            user_id,
        }
    }
}
