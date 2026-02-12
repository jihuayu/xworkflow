//! Workflow DSL schema types.
//!
//! All data structures that represent a workflow definition. These types are
//! deserialized directly from YAML/JSON DSL files and consumed by the graph
//! builder and executor registry.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::core::variable_pool::{Selector, Segment, SegmentStream};

// ================================
// Variable Selector
// ================================

/// Variable selector: e.g. ["node_id", "output_name"] or ["sys", "query"]
pub type VariableSelector = Selector;

// ================================
// Workflow DSL Schema
// ================================

/// Current supported DSL version
pub const CURRENT_DSL_VERSION: &str = "0.1.0";

/// All supported DSL versions
pub const SUPPORTED_DSL_VERSIONS: &[&str] = &["0.1.0"];

/// Top-level workflow definition.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct WorkflowSchema {
    /// DSL version string, e.g. "0.1.0"
    pub version: String,
    pub nodes: Vec<NodeSchema>,
    pub edges: Vec<EdgeSchema>,
    #[serde(default)]
    pub environment_variables: Vec<EnvironmentVariable>,
    #[serde(default)]
    pub conversation_variables: Vec<ConversationVariable>,
    #[serde(default)]
    pub error_handler: Option<ErrorHandlerConfig>,
}

/// Node definition in the DSL.
/// The `data` object embeds the type tag and all node-specific config.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct NodeSchema {
    pub id: String,
    pub data: NodeData,
}

/// Common node data.  The `type` field determines the concrete config.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct NodeData {
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub error_strategy: Option<ErrorStrategyConfig>,
    #[serde(default)]
    pub retry_config: Option<RetryConfig>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// All remaining fields are captured here as a JSON map.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// A directed edge connecting two nodes.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EdgeSchema {
    #[serde(default)]
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, alias = "sourceHandle")]
    pub source_handle: Option<String>,
}

/// An environment variable injected into the workflow at runtime.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

/// A persistent conversation variable whose value carries across runs.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ConversationVariable {
    pub name: String,
    #[serde(rename = "type")]
    pub var_type: String,
    #[serde(default)]
    pub default: Option<Value>,
}

// ================================
// Error Strategy
// ================================

/// Per-node error handling strategy.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ErrorStrategyConfig {
    #[serde(rename = "type")]
    pub strategy_type: ErrorStrategyType,
    #[serde(default)]
    pub default_value: Option<HashMap<String, Value>>,
}

/// Error strategy type for a node.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorStrategyType {
    None,
    FailBranch,
    DefaultValue,
}

/// Configuration for automatic retry on node failure.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct RetryConfig {
    #[serde(default)]
    pub max_retries: i32,
    #[serde(default)]
    pub retry_interval: i32,
    #[serde(default = "default_backoff_strategy")]
    pub backoff_strategy: BackoffStrategy,
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,
    #[serde(default = "default_max_retry_interval")]
    pub max_retry_interval: i32,
    #[serde(default = "default_retry_on_retryable_only")]
    pub retry_on_retryable_only: bool,
}

/// Retry backoff strategy.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum BackoffStrategy {
    Fixed,
    Exponential,
    ExponentialWithJitter,
}

fn default_backoff_strategy() -> BackoffStrategy { BackoffStrategy::Fixed }
fn default_backoff_multiplier() -> f64 { 2.0 }
fn default_max_retry_interval() -> i32 { 60000 }
fn default_retry_on_retryable_only() -> bool { true }

// ================================
// Workflow Error Handler
// ================================

/// How the global error handler processes a workflow-level failure.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ErrorHandlingMode {
    Recover,
    Notify,
}

impl Default for ErrorHandlingMode {
    fn default() -> Self {
        Self::Notify
    }
}

/// Global error handler configuration with an embedded recovery sub-graph.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ErrorHandlerConfig {
    pub sub_graph: crate::nodes::subgraph::SubGraphDefinition,
    #[serde(default)]
    pub mode: ErrorHandlingMode,
}

// ================================
// Node Type Enum (Dify-compatible)
// ================================

/// Dify-compatible node type enumeration.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum NodeType {
    Start,
    End,
    Answer,
    Llm,
    KnowledgeRetrieval,
    IfElse,
    Code,
    TemplateTransform,
    QuestionClassifier,
    HttpRequest,
    Tool,
    VariableAggregator,
    Gather,
    #[serde(rename = "variable-assigner")]
    LegacyVariableAggregator,
    Loop,
    LoopStart,
    LoopEnd,
    Iteration,
    IterationStart,
    ParameterExtractor,
    #[serde(rename = "assigner")]
    VariableAssigner,
    DocumentExtractor,
    ListOperator,
    Agent,
    HumanInput,
}

impl std::fmt::Display for NodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("{:?}", self));
        write!(f, "{}", s)
    }
}

// ================================
// Node Execution Type
// ================================

/// Classifies how a node participates in execution flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeExecutionType {
    Executable,
    Response,
    Branch,
    Container,
    Root,
}

impl NodeType {
    pub fn execution_type(&self) -> NodeExecutionType {
        match self {
            NodeType::Start => NodeExecutionType::Root,
            NodeType::End | NodeType::Answer => NodeExecutionType::Response,
            NodeType::IfElse | NodeType::QuestionClassifier => NodeExecutionType::Branch,
            NodeType::Iteration | NodeType::Loop => NodeExecutionType::Container,
            _ => NodeExecutionType::Executable,
        }
    }
}

// ================================
// Start Node Config
// ================================

/// Configuration for a Start node (workflow entry point).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct StartNodeData {
    #[serde(default)]
    pub variables: Vec<StartVariable>,
}

/// A user-facing input variable declared on the Start node.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct StartVariable {
    pub variable: String,
    #[serde(default)]
    pub label: String,
    #[serde(rename = "type", default = "default_var_type")]
    pub var_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub max_length: Option<i32>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

fn default_var_type() -> String {
    "string".to_string()
}

// ================================
// End Node Config
// ================================

/// Configuration for an End node (workflow output collector).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EndNodeData {
    #[serde(default)]
    pub outputs: Vec<OutputVariable>,
}

/// A single output variable mapping in an End node.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct OutputVariable {
    pub variable: String,
    pub value_selector: VariableSelector,
}

// ================================
// Answer Node Config
// ================================

/// Configuration for an Answer node (renders a response template).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AnswerNodeData {
    pub answer: String,
}

// ================================
// IfElse Node Config (Dify-compatible: multi-case branches)
// ================================

/// Configuration for an IfElse node (conditional branching with multiple cases).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct IfElseNodeData {
    pub cases: Vec<Case>,
}

/// A single branch case in an IfElse node.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Case {
    pub case_id: String,
    pub logical_operator: LogicalOperator,
    pub conditions: Vec<Condition>,
}

/// A single condition within a case (compares a variable against a value).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Condition {
    pub variable_selector: VariableSelector,
    pub comparison_operator: ComparisonOperator,
    #[serde(default)]
    pub value: Value,
}

/// Comparison operators supported by IfElse conditions.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOperator {
    // String/Array
    Contains,
    #[serde(alias = "not_contains")]
    NotContains,
    #[serde(alias = "start_with")]
    StartWith,
    #[serde(alias = "end_with")]
    EndWith,
    Is,
    #[serde(alias = "is_not")]
    IsNot,
    Empty,
    #[serde(alias = "not_empty")]
    NotEmpty,
    In,
    #[serde(alias = "not_in")]
    NotIn,
    #[serde(alias = "all_of")]
    AllOf,
    // Numeric
    #[serde(alias = "=")]
    Equal,
    #[serde(alias = "≠")]
    NotEqual,
    #[serde(alias = ">", alias = "greater_than")]
    GreaterThan,
    #[serde(alias = "<", alias = "less_than")]
    LessThan,
    #[serde(alias = "≥", alias = "greater_than_or_equal", alias = "greater_or_equal")]
    GreaterOrEqual,
    #[serde(alias = "≤", alias = "less_than_or_equal", alias = "less_or_equal")]
    LessOrEqual,
    // Null
    Null,
    #[serde(alias = "not_null")]
    NotNull,
}

/// Logical operator for combining conditions within a case.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum LogicalOperator {
    And,
    Or,
}

// ================================
// Template Transform Node Config
// ================================

/// Configuration for a Template Transform node (Jinja2 rendering).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TemplateTransformNodeData {
    pub template: String,
    #[serde(default)]
    pub variables: Vec<VariableMapping>,
}

/// Maps a named variable to a [`VariableSelector`] source.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableMapping {
    pub variable: String,
    pub value_selector: VariableSelector,
}

// ================================
// Code Node Config
// ================================

/// Configuration for a Code node (JavaScript/Python3 sandbox execution).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CodeNodeData {
    pub code: String,
    pub language: CodeLanguage,
    #[serde(default)]
    pub variables: Vec<VariableMapping>,
    #[serde(default)]
    pub outputs: HashMap<String, Value>,
}

/// Supported programming languages for Code nodes.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum CodeLanguage {
    Python3,
    Javascript,
}

// ================================
// HTTP Request Node Config
// ================================

/// Configuration for an HTTP Request node.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct HttpRequestNodeData {
    pub method: HttpMethod,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KeyValuePair>,
    #[serde(default)]
    pub params: Vec<KeyValuePair>,
    #[serde(default)]
    pub body: Option<HttpBody>,
    #[serde(default)]
    pub authorization: Option<Authorization>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub fail_on_error_status: Option<bool>,
}

// ================================
// Document Extractor Node Config
// ================================

/// Configuration for a Document Extractor node.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DocumentExtractorNodeData {
    pub variables: Vec<VariableMapping>,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default)]
    pub extract_options: Option<Value>,
}

/// HTTP method for requests.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
}

/// A generic key-value pair used for headers, query params, and form data.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct KeyValuePair {
    pub key: String,
    pub value: String,
}

/// HTTP request body variants.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpBody {
    None,
    FormData { data: Vec<KeyValuePair> },
    XWwwFormUrlencoded { data: Vec<KeyValuePair> },
    RawText { data: String },
    Json { data: String },
}

/// HTTP authorization schemes.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Authorization {
    NoAuth,
    ApiKey { key: String, value: String, position: ApiKeyPosition },
    BearerToken { token: String },
    BasicAuth { username: String, password: String },
    CustomHeaders { headers: Vec<KeyValuePair> },
}

/// Where to place an API key in the request.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyPosition { Header, Query, Body }

fn default_timeout() -> u64 { 10 }

// ================================
// Variable Aggregator (Dify: returns first non-null)
// ================================

/// Configuration for a Variable Aggregator node (returns first non-null).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableAggregatorNodeData {
    pub variables: Vec<VariableSelector>,
    #[serde(default)]
    pub output_type: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct GatherNodeData {
    #[serde(default)]
    pub variables: Vec<VariableSelector>,
    #[serde(default)]
    pub join_mode: JoinMode,
    #[serde(default = "default_cancel_remaining")]
    pub cancel_remaining: bool,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub timeout_strategy: TimeoutStrategy,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JoinMode {
    All,
    Any,
    NOfM { n: usize },
}

impl Default for JoinMode {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutStrategy {
    ProceedWithAvailable,
    Fail,
}

impl Default for TimeoutStrategy {
    fn default() -> Self {
        Self::ProceedWithAvailable
    }
}

fn default_cancel_remaining() -> bool {
    true
}

fn default_parallel_enabled() -> bool {
    true
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ParallelConfig {
    #[serde(default = "default_parallel_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub max_concurrency: usize,
}

// ================================
// Variable Assigner
// ================================

/// Configuration for a Variable Assigner node (writes to a conversation variable).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableAssignerNodeData {
    pub assigned_variable_selector: VariableSelector,
    #[serde(default)]
    pub input_variable_selector: Option<VariableSelector>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default = "default_write_mode")]
    pub write_mode: WriteMode,
}

/// Write mode for variable assignment.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum WriteMode {
    Overwrite,
    Append,
    Clear,
}

fn default_write_mode() -> WriteMode { WriteMode::Overwrite }

// ================================
// Iteration Node Config
// ================================

/// Configuration for an Iteration node (loops over a list with a sub-graph).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct IterationNodeData {
    pub iterator_selector: VariableSelector,
    pub output_selector: VariableSelector,
    #[serde(default)]
    pub is_parallel: bool,
    #[serde(default)]
    pub parallel_nums: Option<u32>,
    #[serde(default = "default_iteration_error_mode")]
    pub error_handle_mode: IterationErrorMode,
}

/// Error handling mode for iteration failures.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum IterationErrorMode {
    Terminated,
    RemoveAbnormal,
    ContinueOnError,
}

fn default_iteration_error_mode() -> IterationErrorMode { IterationErrorMode::Terminated }

// ================================
// LLM Node Config (stub)
// ================================

/// Configuration for an LLM node (calls an LLM provider).
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct LlmNodeData {
    pub model: ModelConfig,
    #[serde(default)]
    pub prompt_template: Vec<PromptMessage>,
    #[serde(default)]
    pub context: Option<ContextConfig>,
    #[serde(default)]
    pub vision: Option<VisionConfig>,
    #[serde(default)]
    pub memory: Option<MemoryConfig>,
    #[serde(default)]
    pub stream: Option<bool>,
}

/// LLM model selection and parameters.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ModelConfig {
    pub provider: String,
    pub name: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub completion_params: Option<CompletionParams>,
    #[serde(default)]
    pub credentials: Option<HashMap<String, String>>,
}

/// LLM completion sampling parameters.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CompletionParams {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i32>,
}

/// A prompt message in the LLM prompt template.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PromptMessage {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub edition_type: Option<String>,
}

/// Context retrieval configuration for the LLM node.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ContextConfig {
    pub enabled: bool,
    #[serde(default)]
    pub variable_selector: Option<VariableSelector>,
}

/// Vision (multi-modal) configuration for the LLM node.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VisionConfig {
    pub enabled: bool,
    #[serde(default)]
    pub variable_selector: Option<VariableSelector>,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Conversation memory configuration for the LLM node.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MemoryConfig {
    pub enabled: bool,
    #[serde(default)]
    pub window_size: Option<i32>,
    #[serde(default)]
    pub variable_selector: Option<VariableSelector>,
}

// ================================
// Node Run Result (Dify-compatible)
// ================================

/// Execution status of a single node run.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeExecutionStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Exception,
    Stopped,
    Paused,
    Retry,
}

/// Result of executing a single node, including outputs, metadata, and status.
#[derive(Debug, Clone)]
pub struct NodeRunResult {
    pub status: WorkflowNodeExecutionStatus,
    pub inputs: HashMap<String, Segment>,
    pub outputs: NodeOutputs,
    pub metadata: HashMap<String, Value>,
    pub llm_usage: Option<LlmUsage>,
    pub edge_source_handle: EdgeHandle,
    pub error: Option<NodeErrorInfo>,
    pub retry_index: i32,
}

impl Default for NodeRunResult {
    fn default() -> Self {
        NodeRunResult {
            status: WorkflowNodeExecutionStatus::Pending,
            inputs: HashMap::new(),
            outputs: NodeOutputs::Sync(HashMap::new()),
            metadata: HashMap::new(),
            llm_usage: None,
            edge_source_handle: EdgeHandle::Default,
            error: None,
            retry_index: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeHandle {
    /// Non-branch nodes: all edges are taken
    Default,
    /// Branch nodes: select specific edge handle
    Branch(String),
}

impl Default for EdgeHandle {
    fn default() -> Self {
        EdgeHandle::Default
    }
}

/// Node output container, supporting both synchronous and streaming outputs.
#[derive(Debug, Clone)]
pub enum NodeOutputs {
    /// All outputs are ready
    Sync(HashMap<String, Segment>),
    /// Some outputs are streaming; ready holds non-stream values
    Stream {
        ready: HashMap<String, Segment>,
        streams: HashMap<String, SegmentStream>,
    },
}

impl NodeOutputs {
    pub fn ready(&self) -> &HashMap<String, Segment> {
        match self {
            NodeOutputs::Sync(map) => map,
            NodeOutputs::Stream { ready, .. } => ready,
        }
    }

    pub fn streams(&self) -> Option<&HashMap<String, SegmentStream>> {
        match self {
            NodeOutputs::Stream { streams, .. } => Some(streams),
            _ => None,
        }
    }

    pub fn into_parts(self) -> (HashMap<String, Segment>, HashMap<String, SegmentStream>) {
        match self {
            NodeOutputs::Sync(map) => (map, HashMap::new()),
            NodeOutputs::Stream { ready, streams } => (ready, streams),
        }
    }

    pub fn to_value_map(&self) -> HashMap<String, Value> {
        self
            .ready()
            .iter()
            .map(|(k, v)| (k.clone(), v.to_value()))
            .collect()
    }
}

/// Structured error information attached to a failed [`NodeRunResult`].
#[derive(Debug, Clone)]
pub struct NodeErrorInfo {
    pub message: String,
    pub error_type: Option<String>,
    pub detail: Option<Value>,
}

/// LLM token usage and cost metrics.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct LlmUsage {
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
    #[serde(default)]
    pub total_price: f64,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub latency: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_type_display() {
        assert_eq!(NodeType::Start.to_string(), "start");
        assert_eq!(NodeType::End.to_string(), "end");
        assert_eq!(NodeType::IfElse.to_string(), "if-else");
        assert_eq!(NodeType::Code.to_string(), "code");
        assert_eq!(NodeType::HttpRequest.to_string(), "http-request");
        assert_eq!(NodeType::TemplateTransform.to_string(), "template-transform");
        assert_eq!(NodeType::VariableAssigner.to_string(), "assigner");
    }

    #[test]
    fn test_node_type_execution_type() {
        assert_eq!(NodeType::Start.execution_type(), NodeExecutionType::Root);
        assert_eq!(NodeType::End.execution_type(), NodeExecutionType::Response);
        assert_eq!(NodeType::Answer.execution_type(), NodeExecutionType::Response);
        assert_eq!(NodeType::IfElse.execution_type(), NodeExecutionType::Branch);
        assert_eq!(NodeType::QuestionClassifier.execution_type(), NodeExecutionType::Branch);
        assert_eq!(NodeType::Iteration.execution_type(), NodeExecutionType::Container);
        assert_eq!(NodeType::Loop.execution_type(), NodeExecutionType::Container);
        assert_eq!(NodeType::Code.execution_type(), NodeExecutionType::Executable);
        assert_eq!(NodeType::Llm.execution_type(), NodeExecutionType::Executable);
    }

    #[test]
    fn test_node_run_result_default() {
        let r = NodeRunResult::default();
        assert_eq!(r.status, WorkflowNodeExecutionStatus::Pending);
        assert!(r.inputs.is_empty());
        assert!(r.outputs.ready().is_empty());
        assert!(r.metadata.is_empty());
        assert!(r.llm_usage.is_none());
        assert_eq!(r.edge_source_handle, EdgeHandle::Default);
        assert!(r.error.is_none());
        assert_eq!(r.retry_index, 0);
    }

    #[test]
    fn test_edge_handle_default() {
        let h = EdgeHandle::default();
        assert_eq!(h, EdgeHandle::Default);
    }

    #[test]
    fn test_edge_handle_branch() {
        let h = EdgeHandle::Branch("case1".into());
        assert_ne!(h, EdgeHandle::Default);
    }

    #[test]
    fn test_node_outputs_sync() {
        let mut m = HashMap::new();
        m.insert("key".into(), Segment::Integer(42));
        let out = NodeOutputs::Sync(m);
        assert_eq!(out.ready()["key"], Segment::Integer(42));
        assert!(out.streams().is_none());
        let (ready, streams) = out.into_parts();
        assert_eq!(ready["key"], Segment::Integer(42));
        assert!(streams.is_empty());
    }

    #[test]
    fn test_node_outputs_stream() {
        let (stream, _writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut ready = HashMap::new();
        ready.insert("r".into(), Segment::Boolean(true));
        let mut streams = HashMap::new();
        streams.insert("s".into(), stream);
        let out = NodeOutputs::Stream { ready, streams };
        assert_eq!(out.ready()["r"], Segment::Boolean(true));
        assert!(out.streams().is_some());
        assert_eq!(out.streams().unwrap().len(), 1);
    }

    #[test]
    fn test_error_handling_mode_default() {
        let mode = ErrorHandlingMode::default();
        let json = serde_json::to_string(&mode).unwrap();
        assert!(json.contains("notify"));
    }

    #[test]
    fn test_comparison_operator_serde_aliases() {
        let op: ComparisonOperator = serde_json::from_str("\"not_contains\"").unwrap();
        assert_eq!(op, ComparisonOperator::NotContains);
        let op: ComparisonOperator = serde_json::from_str("\"start_with\"").unwrap();
        assert_eq!(op, ComparisonOperator::StartWith);
        let op: ComparisonOperator = serde_json::from_str("\">\"").unwrap();
        assert_eq!(op, ComparisonOperator::GreaterThan);
    }

    #[test]
    fn test_workflow_schema_serde_roundtrip() {
        let json = serde_json::json!({
            "version": "0.1.0",
            "nodes": [],
            "edges": []
        });
        let schema: WorkflowSchema = serde_json::from_value(json).unwrap();
        assert_eq!(schema.version, "0.1.0");
        assert!(schema.nodes.is_empty());
    }

    #[test]
    fn test_llm_usage_default() {
        let u = LlmUsage::default();
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
        assert_eq!(u.total_tokens, 0);
        assert_eq!(u.total_price, 0.0);
        assert_eq!(u.currency, "");
        assert_eq!(u.latency, 0.0);
    }

    #[test]
    fn test_node_type_serde_roundtrip() {
        let node_types = vec![
            NodeType::Start,
            NodeType::End,
            NodeType::Answer,
            NodeType::Llm,
            NodeType::IfElse,
            NodeType::Code,
            NodeType::HttpRequest,
            NodeType::TemplateTransform,
            NodeType::VariableAggregator,
            NodeType::Gather,
            NodeType::VariableAssigner,
            NodeType::Iteration,
            NodeType::Loop,
            NodeType::ListOperator,
        ];
        for nt in node_types {
            let json = serde_json::to_value(&nt).unwrap();
            let de: NodeType = serde_json::from_value(json).unwrap();
            assert_eq!(de, nt);
        }
    }

    #[test]
    fn test_workflow_execution_status_variants() {
        let statuses = vec![
            ("\"pending\"", WorkflowNodeExecutionStatus::Pending),
            ("\"running\"", WorkflowNodeExecutionStatus::Running),
            ("\"succeeded\"", WorkflowNodeExecutionStatus::Succeeded),
            ("\"failed\"", WorkflowNodeExecutionStatus::Failed),
        ];
        for (s, expected) in statuses {
            let de: WorkflowNodeExecutionStatus = serde_json::from_str(s).unwrap();
            assert_eq!(de, expected);
        }
    }
}
