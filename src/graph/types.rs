use petgraph::stable_graph::NodeIndex;
use serde_json::Value;

/// 图节点
#[derive(Debug, Clone)]
pub struct GraphNode {
    /// 节点 ID（对应 DSL 中的 id）
    pub id: String,

    /// 节点类型（llm, code, if-else 等）
    pub node_type: String,

    /// 节点配置（对应 DSL 中的 data 字段）
    pub config: Value,

    /// 节点标题
    pub title: String,
}

/// 图边
#[derive(Debug, Clone)]
pub struct GraphEdge {
    /// 边 ID
    pub id: String,

    /// 源节点 ID
    pub source: String,

    /// 目标节点 ID
    pub target: String,

    /// 边类型
    pub edge_type: EdgeType,
}

/// 边类型
#[derive(Debug, Clone, PartialEq)]
pub enum EdgeType {
    /// 普通边
    Normal,
    /// If/Else 的 true 分支
    TrueBranch,
    /// If/Else 的 false 分支
    FalseBranch,
    /// 迭代节点的循环体
    IterationBody,
}

impl EdgeType {
    /// 从 source_handle 字符串解析边类型
    pub fn from_source_handle(handle: &Option<String>) -> Self {
        match handle.as_deref() {
            Some("true") => EdgeType::TrueBranch,
            Some("false") => EdgeType::FalseBranch,
            Some("iteration") => EdgeType::IterationBody,
            _ => EdgeType::Normal,
        }
    }
}

/// 工作流元数据
#[derive(Debug, Clone)]
pub struct WorkflowMetadata {
    pub variables: Vec<crate::dsl::VariableSchema>,
    pub environment_variables: Vec<crate::dsl::EnvironmentVariable>,
    pub conversation_variables: Vec<crate::dsl::ConversationVariable>,
}

/// 节点 ID 到 petgraph NodeIndex 的映射
pub type NodeIndexMap = std::collections::HashMap<String, NodeIndex>;
