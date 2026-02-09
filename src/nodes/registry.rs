use std::collections::HashMap;
use std::sync::Arc;

use super::NodeExecutor;

/// 节点注册表 - 管理所有节点类型的执行器
pub struct NodeRegistry {
    executors: HashMap<String, Arc<dyn NodeExecutor>>,
}

impl NodeRegistry {
    /// 创建新的注册表
    pub fn new() -> Self {
        NodeRegistry {
            executors: HashMap::new(),
        }
    }

    /// 注册节点执行器
    pub fn register(&mut self, node_type: &str, executor: Arc<dyn NodeExecutor>) {
        self.executors.insert(node_type.to_string(), executor);
    }

    /// 获取节点执行器
    pub fn get(&self, node_type: &str) -> Option<Arc<dyn NodeExecutor>> {
        self.executors.get(node_type).cloned()
    }

    /// 获取所有已注册的节点类型
    pub fn registered_types(&self) -> Vec<String> {
        self.executors.keys().cloned().collect()
    }
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 创建并初始化默认的节点注册表
pub fn create_default_registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();

    // 控制流节点
    registry.register("start", Arc::new(super::control_flow::StartNodeExecutor));
    registry.register("end", Arc::new(super::control_flow::EndNodeExecutor));
    registry.register(
        "if-else",
        Arc::new(super::control_flow::IfElseNodeExecutor::new()),
    );

    // 数据转换节点
    registry.register(
        "template",
        Arc::new(super::data_transform::TemplateNodeExecutor::new()),
    );
    registry.register(
        "variable-assigner",
        Arc::new(super::data_transform::VariableAssignerNodeExecutor),
    );
    registry.register(
        "variable-aggregator",
        Arc::new(super::data_transform::VariableAggregatorNodeExecutor),
    );
    registry.register(
        "http-request",
        Arc::new(super::data_transform::HttpRequestNodeExecutor::new()),
    );
    registry.register(
        "code",
        Arc::new(super::data_transform::CodeNodeExecutor),
    );

    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::control_flow::StartNodeExecutor;

    #[test]
    fn test_registry_register_and_get() {
        let mut registry = NodeRegistry::new();
        registry.register("start", Arc::new(StartNodeExecutor));

        assert!(registry.get("start").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_default_registry() {
        let registry = create_default_registry();

        assert!(registry.get("start").is_some());
        assert!(registry.get("end").is_some());
        assert!(registry.get("if-else").is_some());
        assert!(registry.get("template").is_some());
        assert!(registry.get("variable-assigner").is_some());
        assert!(registry.get("variable-aggregator").is_some());
        assert!(registry.get("http-request").is_some());
        assert!(registry.get("code").is_some());
    }
}
