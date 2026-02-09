use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::core::variable_pool::SegmentStream;

// ================================
// Variable Selector (Dify-compatible)
// ================================

/// Variable selector: e.g. ["node_id", "output_name"] or ["sys", "query"]
pub type VariableSelector = Vec<String>;

// ================================
// Workflow DSL Schema
// ================================

/// Current supported DSL version
pub const CURRENT_DSL_VERSION: &str = "0.1.0";

/// All supported DSL versions
pub const SUPPORTED_DSL_VERSIONS: &[&str] = &["0.1.0"];

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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EdgeSchema {
    #[serde(default)]
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, alias = "sourceHandle")]
    pub source_handle: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ErrorStrategyConfig {
    #[serde(rename = "type")]
    pub strategy_type: ErrorStrategyType,
    #[serde(default)]
    pub default_value: Option<HashMap<String, Value>>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorStrategyType {
    None,
    FailBranch,
    DefaultValue,
}

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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ErrorHandlerConfig {
    pub sub_graph: crate::nodes::subgraph::SubGraphDefinition,
    #[serde(default)]
    pub mode: ErrorHandlingMode,
}

// ================================
// Node Type Enum (Dify-compatible)
// ================================

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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct StartNodeData {
    #[serde(default)]
    pub variables: Vec<StartVariable>,
}

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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EndNodeData {
    #[serde(default)]
    pub outputs: Vec<OutputVariable>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct OutputVariable {
    pub variable: String,
    pub value_selector: VariableSelector,
}

// ================================
// Answer Node Config
// ================================

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AnswerNodeData {
    pub answer: String,
}

// ================================
// IfElse Node Config (Dify-compatible: multi-case branches)
// ================================

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct IfElseNodeData {
    pub cases: Vec<Case>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Case {
    pub case_id: String,
    pub logical_operator: LogicalOperator,
    pub conditions: Vec<Condition>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Condition {
    pub variable_selector: VariableSelector,
    pub comparison_operator: ComparisonOperator,
    #[serde(default)]
    pub value: Value,
}

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

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum LogicalOperator {
    And,
    Or,
}

// ================================
// Template Transform Node Config
// ================================

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TemplateTransformNodeData {
    pub template: String,
    #[serde(default)]
    pub variables: Vec<VariableMapping>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableMapping {
    pub variable: String,
    pub value_selector: VariableSelector,
}

// ================================
// Code Node Config
// ================================

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CodeNodeData {
    pub code: String,
    pub language: CodeLanguage,
    #[serde(default)]
    pub variables: Vec<VariableMapping>,
    #[serde(default)]
    pub outputs: HashMap<String, Value>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum CodeLanguage {
    Python3,
    Javascript,
}

// ================================
// HTTP Request Node Config
// ================================

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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct KeyValuePair {
    pub key: String,
    pub value: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpBody {
    None,
    FormData { data: Vec<KeyValuePair> },
    XWwwFormUrlencoded { data: Vec<KeyValuePair> },
    RawText { data: String },
    Json { data: String },
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Authorization {
    NoAuth,
    ApiKey { key: String, value: String, position: ApiKeyPosition },
    BearerToken { token: String },
    BasicAuth { username: String, password: String },
    CustomHeaders { headers: Vec<KeyValuePair> },
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyPosition { Header, Query, Body }

fn default_timeout() -> u64 { 10 }

// ================================
// Variable Aggregator (Dify: returns first non-null)
// ================================

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableAggregatorNodeData {
    pub variables: Vec<VariableSelector>,
    #[serde(default)]
    pub output_type: Option<String>,
}

// ================================
// Variable Assigner
// ================================

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

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CompletionParams {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i32>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PromptMessage {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub edition_type: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ContextConfig {
    pub enabled: bool,
    #[serde(default)]
    pub variable_selector: Option<VariableSelector>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VisionConfig {
    pub enabled: bool,
    #[serde(default)]
    pub variable_selector: Option<VariableSelector>,
    #[serde(default)]
    pub detail: Option<String>,
}

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

#[derive(Debug, Clone)]
pub struct NodeRunResult {
    pub status: WorkflowNodeExecutionStatus,
    pub inputs: HashMap<String, Value>,
    pub process_data: HashMap<String, Value>,
    pub outputs: HashMap<String, Value>,
    pub stream_outputs: HashMap<String, SegmentStream>,
    pub metadata: HashMap<String, Value>,
    pub llm_usage: Option<LlmUsage>,
    pub edge_source_handle: String,
    pub error: Option<String>,
    pub error_type: Option<String>,
    pub retry_index: i32,
    pub error_detail: Option<Value>,
}

impl Default for NodeRunResult {
    fn default() -> Self {
        NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            inputs: HashMap::new(),
            process_data: HashMap::new(),
            outputs: HashMap::new(),
            stream_outputs: HashMap::new(),
            metadata: HashMap::new(),
            llm_usage: None,
            edge_source_handle: "source".to_string(),
            error: None,
            error_type: None,
            retry_index: 0,
            error_detail: None,
        }
    }
}

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
