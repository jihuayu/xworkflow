//! Node executor trait and registry.

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
#[cfg(any(feature = "builtin-docextract-node", feature = "builtin-agent-node"))]
use std::sync::Arc;
use std::sync::OnceLock;
#[cfg(feature = "builtin-agent-node")]
use tokio::sync::RwLock;

use crate::compiler::CompiledNodeConfig;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::NodeRunResult;
use crate::error::NodeError;
use crate::llm::LlmNodeExecutor;
use crate::llm::LlmProviderRegistry;
use crate::llm::QuestionClassifierExecutor;

type ExecutorFactory = dyn Fn() -> Box<dyn NodeExecutor> + Send + Sync;

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

    /// Execute using a compiled node config when available.
    async fn execute_compiled(
        &self,
        node_id: &str,
        compiled_config: &CompiledNodeConfig,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        self.execute(node_id, compiled_config.as_value(), variable_pool, context)
            .await
    }
}

/// Registry of node executors by node type string
pub struct NodeExecutorRegistry {
    executors: HashMap<String, OnceLock<Box<dyn NodeExecutor>>>,
    factories: Mutex<HashMap<String, Box<ExecutorFactory>>>,
    #[cfg(feature = "builtin-agent-node")]
    llm_registry: Option<std::sync::Arc<LlmProviderRegistry>>,
}

impl NodeExecutorRegistry {
    /// Create a new registry pre-populated with all built-in executors.
    pub fn new() -> Self {
        Self::with_builtins()
    }

    /// Create an empty registry with no executors registered.
    pub fn empty() -> Self {
        NodeExecutorRegistry {
            executors: HashMap::new(),
            factories: Mutex::new(HashMap::new()),
            #[cfg(feature = "builtin-agent-node")]
            llm_registry: None,
        }
    }

    /// Create a registry with all built-in (feature-gated) node executors.
    pub fn with_builtins() -> Self {
        let mut registry = NodeExecutorRegistry::empty();
        // Register built-in executors
        #[cfg(feature = "builtin-core-nodes")]
        {
            registry.register("start", Box::new(super::control_flow::StartNodeExecutor));
            registry.register("end", Box::new(super::control_flow::EndNodeExecutor));
            registry.register("answer", Box::new(super::control_flow::AnswerNodeExecutor));
            registry.register("if-else", Box::new(super::control_flow::IfElseNodeExecutor));
        }

        #[cfg(feature = "builtin-transform-nodes")]
        {
            registry.register(
                "template-transform",
                Box::new(super::transform::TemplateTransformExecutor::new()),
            );
            registry.register(
                "variable-aggregator",
                Box::new(super::transform::VariableAggregatorExecutor),
            );
            registry.register("gather", Box::new(super::gather::GatherExecutor));
            registry.register(
                "variable-assigner",
                Box::new(super::transform::LegacyVariableAggregatorExecutor),
            );
            registry.register(
                "assigner",
                Box::new(super::transform::VariableAssignerExecutor),
            );
        }

        #[cfg(feature = "builtin-http-node")]
        {
            registry.register(
                "http-request",
                Box::new(super::transform::HttpRequestExecutor),
            );
        }

        #[cfg(feature = "builtin-code-node")]
        {
            registry.register_lazy(
                "code",
                Box::new(|| Box::new(super::transform::CodeNodeExecutor::new())),
            );
        }

        #[cfg(feature = "builtin-subgraph-nodes")]
        {
            registry.register(
                "list-operator",
                Box::new(super::flow::ListOperatorNodeExecutor::new()),
            );
            registry.register(
                "iteration",
                Box::new(super::flow::IterationNodeExecutor::new()),
            );
            registry.register("loop", Box::new(super::flow::LoopNodeExecutor::new()));
        }

        #[cfg(feature = "builtin-memory-nodes")]
        {
            registry.register("memory-recall", Box::new(super::memory::MemoryRecallExecutor));
            registry.register("memory-store", Box::new(super::memory::MemoryStoreExecutor));
        }

        // Stub executors for types that need external services
        // LLM executor is injected via set_llm_provider_registry
        registry.register(
            "knowledge-retrieval",
            Box::new(StubExecutor("knowledge-retrieval")),
        );
        registry.register(
            "question-classifier",
            Box::new(StubExecutor("question-classifier")),
        );
        registry.register(
            "parameter-extractor",
            Box::new(StubExecutor("parameter-extractor")),
        );
        registry.register("tool", Box::new(StubExecutor("tool")));
        #[cfg(feature = "builtin-docextract-node")]
        {
            let provider = Arc::new(xworkflow_docextract_builtin::BuiltinDocExtractProvider::new());
            let router = Arc::new(super::document_extract::ExtractorRouter::from_providers(&[
                provider,
            ]));
            registry.register_lazy(
                "document-extractor",
                Box::new(move || {
                    Box::new(super::document_extract::DocumentExtractorExecutor::new(
                        router.clone(),
                    ))
                }),
            );
        }

        #[cfg(not(feature = "builtin-docextract-node"))]
        {
            registry.register(
                "document-extractor",
                Box::new(StubExecutor("document-extractor")),
            );
        }
        registry.register("agent", Box::new(StubExecutor("agent")));
        registry.register(
            "human-input",
            Box::new(super::human_input::HumanInputExecutor),
        );
        registry
    }

    #[cfg(feature = "plugin-system")]
    pub fn apply_plugin_executors(&mut self, executors: HashMap<String, Box<dyn NodeExecutor>>) {
        for (node_type, executor) in executors {
            self.register(&node_type, executor);
        }
    }

    /// Register an eagerly-constructed executor for the given node type.
    pub fn register(&mut self, node_type: &str, executor: Box<dyn NodeExecutor>) {
        let cell = OnceLock::new();
        let _ = cell.set(executor);
        self.executors.insert(node_type.to_string(), cell);
        self.factories.lock().remove(node_type);
    }

    /// Register a lazily-constructed executor. The factory is called on first access.
    pub fn register_lazy(
        &mut self,
        node_type: &str,
        factory: Box<dyn Fn() -> Box<dyn NodeExecutor> + Send + Sync>,
    ) {
        self.executors.entry(node_type.to_string()).or_default();
        self.factories.lock().insert(node_type.to_string(), factory);
    }

    #[cfg(feature = "builtin-llm-node")]
    pub fn set_llm_provider_registry(&mut self, registry: std::sync::Arc<LlmProviderRegistry>) {
        #[cfg(feature = "builtin-agent-node")]
        {
            self.llm_registry = Some(registry.clone());
        }
        self.register("llm", Box::new(LlmNodeExecutor::new(registry.clone())));
        self.register(
            "question-classifier",
            Box::new(QuestionClassifierExecutor::new(registry)),
        );
    }

    #[cfg(not(feature = "builtin-llm-node"))]
    pub fn set_llm_provider_registry(&mut self, _registry: std::sync::Arc<LlmProviderRegistry>) {}

    #[cfg(feature = "builtin-agent-node")]
    pub fn set_mcp_pool(&mut self, pool: Arc<RwLock<crate::mcp::pool::McpConnectionPool>>) {
        self.register(
            "tool",
            Box::new(super::tool::ToolNodeExecutor::new(pool.clone())),
        );

        if let Some(llm_registry) = &self.llm_registry {
            self.register(
                "agent",
                Box::new(super::agent::AgentNodeExecutor::new(
                    llm_registry.clone(),
                    pool,
                )),
            );
        }
    }

    /// Look up an executor for the given node type string.
    pub fn get(&self, node_type: &str) -> Option<&dyn NodeExecutor> {
        let cell = self.executors.get(node_type)?;
        if cell.get().is_none() {
            let factory = self.factories.lock().remove(node_type);
            if let Some(factory) = factory {
                let _ = cell.set(factory());
            }
        }
        cell.get().map(|e| e.as_ref())
    }

    /// List all currently registered node type names.
    pub fn list_registered_types(&self) -> Vec<String> {
        self.executors.keys().cloned().collect()
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
        let message = format!("Node type '{}' is not implemented", self.0);
        Ok(NodeRunResult {
            status: crate::dsl::schema::WorkflowNodeExecutionStatus::Exception,
            error: Some(crate::dsl::schema::NodeErrorInfo {
                message: message.clone(),
                error_type: Some("not_implemented".to_string()),
                detail: None,
            }),
            outputs: crate::dsl::schema::NodeOutputs::Sync({
                let mut m = HashMap::new();
                m.insert(
                    "text".to_string(),
                    Segment::String(format!(
                        "[Stub: {} node {} not implemented]",
                        self.0, node_id
                    )),
                );
                m
            }),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_executor_registry_empty() {
        let registry = NodeExecutorRegistry::empty();
        assert!(registry.get("start").is_none());
        assert!(registry.get("code").is_none());
    }

    #[test]
    fn test_node_executor_registry_with_builtins() {
        let registry = NodeExecutorRegistry::with_builtins();
        // built-in core nodes
        assert!(registry.get("start").is_some());
        assert!(registry.get("end").is_some());
        assert!(registry.get("answer").is_some());
        assert!(registry.get("if-else").is_some());
        // built-in transform nodes
        assert!(registry.get("template-transform").is_some());
        assert!(registry.get("variable-aggregator").is_some());
        assert!(registry.get("gather").is_some());
        assert!(registry.get("variable-assigner").is_some());
        assert!(registry.get("assigner").is_some());
        // built-in http node
        assert!(registry.get("http-request").is_some());
        // code node (optional, only with builtin-code-node feature)
        #[cfg(feature = "builtin-code-node")]
        assert!(registry.get("code").is_some());
        // subgraph nodes
        assert!(registry.get("list-operator").is_some());
        assert!(registry.get("iteration").is_some());
        assert!(registry.get("loop").is_some());
        #[cfg(feature = "builtin-memory-nodes")]
        {
            assert!(registry.get("memory-recall").is_some());
            assert!(registry.get("memory-store").is_some());
        }
        // stub nodes
        assert!(registry.get("knowledge-retrieval").is_some());
        assert!(registry.get("question-classifier").is_some());
        assert!(registry.get("parameter-extractor").is_some());
        assert!(registry.get("tool").is_some());
        assert!(registry.get("document-extractor").is_some());
        assert!(registry.get("agent").is_some());
        assert!(registry.get("human-input").is_some());
    }

    #[test]
    fn test_node_executor_registry_new_is_with_builtins() {
        let registry = NodeExecutorRegistry::new();
        assert!(registry.get("start").is_some());
    }

    #[test]
    fn test_node_executor_registry_default() {
        let registry = NodeExecutorRegistry::default();
        assert!(registry.get("start").is_some());
    }

    #[test]
    fn test_node_executor_registry_get_nonexistent() {
        let registry = NodeExecutorRegistry::with_builtins();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_register_custom_executor() {
        let mut registry = NodeExecutorRegistry::empty();
        registry.register("custom", Box::new(StubExecutor("custom")));
        assert!(registry.get("custom").is_some());
    }

    #[test]
    fn test_register_lazy() {
        let mut registry = NodeExecutorRegistry::empty();
        registry.register_lazy("lazy", Box::new(|| Box::new(StubExecutor("lazy"))));
        // Before first access, lazy factory should be used
        let executor = registry.get("lazy");
        assert!(executor.is_some());
        // Second access should return same executor
        assert!(registry.get("lazy").is_some());
    }

    #[tokio::test]
    async fn test_stub_executor_execute() {
        let executor = StubExecutor("test-type");
        let vp = VariablePool::new();
        let ctx = RuntimeContext::default();
        let result = executor
            .execute("node1", &serde_json::json!({}), &vp, &ctx)
            .await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(matches!(
            result.status,
            crate::dsl::schema::WorkflowNodeExecutionStatus::Exception
        ));
        assert!(result.error.is_some());
        assert!(result.error.unwrap().message.contains("test-type"));
    }
}
