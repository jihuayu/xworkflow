use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

/// 工作流事件 - 通过 EventBus 传递
#[derive(Clone, Debug, Serialize)]
pub enum WorkflowEvent {
    /// 节点开始执行
    NodeStarted {
        node_id: String,
        timestamp: DateTime<Utc>,
    },

    /// 节点执行完成
    NodeFinished {
        node_id: String,
        output: Value,
        timestamp: DateTime<Utc>,
    },

    /// 节点执行失败
    NodeFailed {
        node_id: String,
        error: String,
        timestamp: DateTime<Utc>,
    },

    /// 分支选中（由 If/Else 节点产生）
    BranchSelected {
        node_id: String,
        selected_branch: String,
        timestamp: DateTime<Utc>,
    },

    /// 流式 Token（LLM 生成）
    StreamingToken {
        node_id: String,
        token: String,
        index: usize,
    },

    /// 进度更新
    ProgressUpdate {
        node_id: String,
        progress: f32,
        message: String,
    },

    /// 工作流完成
    WorkflowCompleted {
        execution_id: String,
        outputs: Value,
        timestamp: DateTime<Utc>,
    },

    /// 工作流失败
    WorkflowFailed {
        execution_id: String,
        error: String,
        timestamp: DateTime<Utc>,
    },
}

/// 事件发送器
pub type EventSender = mpsc::UnboundedSender<WorkflowEvent>;

/// 事件接收器
pub type EventReceiver = mpsc::UnboundedReceiver<WorkflowEvent>;

/// 创建事件通道
pub fn create_event_channel() -> (EventSender, EventReceiver) {
    mpsc::unbounded_channel()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_channel() {
        let (sender, mut receiver) = create_event_channel();

        sender
            .send(WorkflowEvent::NodeStarted {
                node_id: "node1".to_string(),
                timestamp: Utc::now(),
            })
            .unwrap();

        let event = receiver.recv().await.unwrap();
        match event {
            WorkflowEvent::NodeStarted { node_id, .. } => {
                assert_eq!(node_id, "node1");
            }
            _ => panic!("Unexpected event type"),
        }
    }
}
