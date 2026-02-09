use serde_json::Value;

/// 节点执行上下文 - 传递给 NodeExecutor 的参数
#[derive(Debug, Clone)]
pub struct NodeContext {
    /// 节点 ID
    pub node_id: String,

    /// 节点类型
    pub node_type: String,

    /// 节点配置（从 DSL 解析）
    pub config: Value,

    /// 执行 ID
    pub execution_id: String,

    /// 用户 ID
    pub user_id: String,

    /// 节点标题
    pub title: String,
}
