use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::NodeRunResult;
use crate::error::NodeError;
use crate::llm::LlmProviderRegistry;
#[cfg(not(feature = "plugin-system"))]
use crate::plugin::{PluginManager, PluginNodeExecutor};
use crate::llm::LlmNodeExecutor;

/// Trait for node execution. Each node type implements this.
#[async_trait]
pub trait NodeExecutor: Send + Sync {
    /// Execute the node, returning a NodeRunResult
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError>;
}

/// Registry of node executors by node type string
pub struct NodeExecutorRegistry {
    executors: HashMap<String, Box<dyn NodeExecutor>>,
}

impl NodeExecutorRegistry {
    pub fn new() -> Self {
        let mut registry = NodeExecutorRegistry {
            executors: HashMap::new(),
        };
        // Register built-in executors
        registry.register("start", Box::new(super::control_flow::StartNodeExecutor));
        registry.register("end", Box::new(super::control_flow::EndNodeExecutor));
        registry.register("answer", Box::new(super::control_flow::AnswerNodeExecutor));
        registry.register("if-else", Box::new(super::control_flow::IfElseNodeExecutor));
        registry.register("template-transform", Box::new(super::data_transform::TemplateTransformExecutor));
        registry.register("variable-aggregator", Box::new(super::data_transform::VariableAggregatorExecutor));
        registry.register("variable-assigner", Box::new(super::data_transform::LegacyVariableAggregatorExecutor));
        registry.register("assigner", Box::new(super::data_transform::VariableAssignerExecutor));
        registry.register("http-request", Box::new(super::data_transform::HttpRequestExecutor));
        registry.register("code", Box::new(super::data_transform::CodeNodeExecutor::new()));
        // Stub executors for types that need external services
        // LLM executor is injected via set_llm_provider_registry
        registry.register("knowledge-retrieval", Box::new(StubExecutor("knowledge-retrieval")));
        registry.register("question-classifier", Box::new(StubExecutor("question-classifier")));
        registry.register("parameter-extractor", Box::new(StubExecutor("parameter-extractor")));
        registry.register("tool", Box::new(StubExecutor("tool")));
        registry.register("document-extractor", Box::new(StubExecutor("document-extractor")));
        registry.register("list-operator", Box::new(super::subgraph_nodes::ListOperatorNodeExecutor::new()));
        registry.register("agent", Box::new(StubExecutor("agent")));
        registry.register("human-input", Box::new(StubExecutor("human-input")));
        registry.register("iteration", Box::new(super::subgraph_nodes::IterationNodeExecutor::new()));
        registry.register("loop", Box::new(super::subgraph_nodes::LoopNodeExecutor::new()));
        registry
    }

    #[cfg(not(feature = "plugin-system"))]
    pub fn new_with_plugins(plugin_manager: std::sync::Arc<PluginManager>) -> Self {
        let mut registry = Self::new();
        for (node_type, _) in plugin_manager.get_plugin_node_types() {
            registry.register(
                &node_type,
                Box::new(PluginNodeExecutor::new(
                    plugin_manager.clone(),
                    Some(node_type.clone()),
                )),
            );
        }
        registry
    }

    #[cfg(feature = "plugin-system")]
    pub fn apply_plugin_executors(
        &mut self,
        executors: HashMap<String, Box<dyn NodeExecutor>>,
    ) {
        for (node_type, executor) in executors {
            self.register(&node_type, executor);
        }
    }

    pub fn register(&mut self, node_type: &str, executor: Box<dyn NodeExecutor>) {
        self.executors.insert(node_type.to_string(), executor);
    }

    pub fn set_llm_provider_registry(&mut self, registry: std::sync::Arc<LlmProviderRegistry>) {
        self.executors.insert(
            "llm".to_string(),
            Box::new(LlmNodeExecutor::new(registry)),
        );
    }

    pub fn get(&self, node_type: &str) -> Option<&dyn NodeExecutor> {
        self.executors.get(node_type).map(|e| e.as_ref())
    }
}

impl Default for NodeExecutorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Stub executor for node types that require external services
/// Returns a placeholder result
struct StubExecutor(&'static str);

#[async_trait]
impl NodeExecutor for StubExecutor {
    async fn execute(
        &self,
        node_id: &str,
        _config: &Value,
        _variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        Ok(NodeRunResult {
            outputs: {
                let mut m = HashMap::new();
                m.insert(
                    "text".to_string(),
                    Value::String(format!("[Stub: {} node {} not implemented]", self.0, node_id)),
                );
                m
            },
            ..Default::default()
        })
    }
}
