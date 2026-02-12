use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use crate::core::variable_pool::SegmentType;
use crate::dsl::schema::{
    AnswerNodeData,
    CodeNodeData,
    DocumentExtractorNodeData,
    EndNodeData,
    HttpRequestNodeData,
    IfElseNodeData,
    LlmNodeData,
    StartNodeData,
    TemplateTransformNodeData,
    VariableAggregatorNodeData,
    VariableAssignerNodeData,
    WorkflowSchema,
};
use crate::dsl::validation::ValidationReport;
use crate::graph::GraphTopology;
use crate::nodes::subgraph_nodes::{IterationNodeConfig, ListOperatorNodeConfig, LoopNodeConfig};

use super::runner::CompiledWorkflowRunnerBuilder;

#[derive(Debug, Clone)]
pub struct CompiledConfig<T> {
    pub raw: Value,
    pub parsed: T,
}

impl<T> CompiledConfig<T> {
    pub fn new(raw: Value, parsed: T) -> Self {
        Self { raw, parsed }
    }
}

#[derive(Debug, Clone)]
pub enum CompiledNodeConfig {
    Raw(Value),
    Start(CompiledConfig<StartNodeData>),
    End(CompiledConfig<EndNodeData>),
    Answer(CompiledConfig<AnswerNodeData>),
    IfElse(CompiledConfig<IfElseNodeData>),
    Code(CompiledConfig<CodeNodeData>),
    TemplateTransform(CompiledConfig<TemplateTransformNodeData>),
    HttpRequest(CompiledConfig<HttpRequestNodeData>),
    DocumentExtractor(CompiledConfig<DocumentExtractorNodeData>),
    VariableAggregator(CompiledConfig<VariableAggregatorNodeData>),
    VariableAssigner(CompiledConfig<VariableAssignerNodeData>),
    Iteration(CompiledConfig<IterationNodeConfig>),
    Loop(CompiledConfig<LoopNodeConfig>),
    ListOperator(CompiledConfig<ListOperatorNodeConfig>),
    Llm(CompiledConfig<LlmNodeData>),
}

impl CompiledNodeConfig {
    pub fn as_value(&self) -> &Value {
        match self {
            CompiledNodeConfig::Raw(value) => value,
            CompiledNodeConfig::Start(config) => &config.raw,
            CompiledNodeConfig::End(config) => &config.raw,
            CompiledNodeConfig::Answer(config) => &config.raw,
            CompiledNodeConfig::IfElse(config) => &config.raw,
            CompiledNodeConfig::Code(config) => &config.raw,
            CompiledNodeConfig::TemplateTransform(config) => &config.raw,
            CompiledNodeConfig::HttpRequest(config) => &config.raw,
            CompiledNodeConfig::DocumentExtractor(config) => &config.raw,
            CompiledNodeConfig::VariableAggregator(config) => &config.raw,
            CompiledNodeConfig::VariableAssigner(config) => &config.raw,
            CompiledNodeConfig::Iteration(config) => &config.raw,
            CompiledNodeConfig::Loop(config) => &config.raw,
            CompiledNodeConfig::ListOperator(config) => &config.raw,
            CompiledNodeConfig::Llm(config) => &config.raw,
        }
    }
}

pub type CompiledNodeConfigMap = HashMap<String, Arc<CompiledNodeConfig>>;

/// Compiled workflow artifact containing all immutable data from DSL compilation.
#[derive(Clone)]
pub struct CompiledWorkflow {
    pub(crate) compiled_at: Instant,
    pub(crate) content_hash: u64,
    pub(crate) schema: Arc<WorkflowSchema>,
    pub(crate) graph_template: Arc<GraphTopology>,
    pub(crate) start_var_types: Arc<HashMap<String, SegmentType>>,
    pub(crate) conversation_var_types: Arc<HashMap<String, SegmentType>>,
    pub(crate) start_node_id: Arc<str>,
    pub(crate) validation_report: Arc<ValidationReport>,
    pub(crate) node_configs: Arc<CompiledNodeConfigMap>,
}

impl CompiledWorkflow {
    /// Create a new runner builder for this compiled workflow.
    pub fn runner(&self) -> CompiledWorkflowRunnerBuilder {
        CompiledWorkflowRunnerBuilder::new(self.clone())
    }

    /// Return the validation report produced at compile time.
    pub fn validation_report(&self) -> &ValidationReport {
        &self.validation_report
    }

    /// Return the content hash for this compiled workflow.
    pub fn content_hash(&self) -> u64 {
        self.content_hash
    }

    /// Return the time the workflow was compiled.
    pub fn compiled_at(&self) -> Instant {
        self.compiled_at
    }
}
