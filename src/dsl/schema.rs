use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 工作流 DSL 模式
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct WorkflowSchema {
    /// 节点列表
    pub nodes: Vec<NodeSchema>,

    /// 边列表
    pub edges: Vec<EdgeSchema>,

    /// 变量定义
    #[serde(default)]
    pub variables: Vec<VariableSchema>,

    /// 环境变量
    #[serde(default)]
    pub environment_variables: Vec<EnvironmentVariable>,

    /// 对话变量
    #[serde(default)]
    pub conversation_variables: Vec<ConversationVariable>,
}

/// 节点模式
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct NodeSchema {
    /// 节点 ID
    pub id: String,

    /// 节点类型（llm, code, if-else 等）
    #[serde(rename = "type")]
    pub node_type: String,

    /// 节点数据（配置）
    #[serde(default = "default_data")]
    pub data: Value,

    /// 节点标题
    #[serde(default)]
    pub title: String,

    /// 位置信息（用于可视化，可选）
    #[serde(default)]
    pub position: Option<Position>,
}

fn default_data() -> Value {
    Value::Object(serde_json::Map::new())
}

/// 边模式
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EdgeSchema {
    /// 边 ID
    #[serde(default)]
    pub id: String,

    /// 源节点 ID
    pub source: String,

    /// 目标节点 ID
    pub target: String,

    /// 源句柄（用于分支节点）
    #[serde(default)]
    pub source_handle: Option<String>,
}

/// 变量模式
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableSchema {
    /// 变量名
    pub name: String,

    /// 变量类型
    #[serde(rename = "type")]
    pub var_type: String,

    /// 是否必需
    #[serde(default)]
    pub required: bool,

    /// 默认值
    #[serde(default)]
    pub default: Option<Value>,
}

/// 环境变量
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

/// 对话变量
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ConversationVariable {
    pub name: String,
    #[serde(rename = "type")]
    pub var_type: String,
    #[serde(default)]
    pub default: Option<Value>,
}

/// 位置信息
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

// ================================
// 节点配置数据结构
// ================================

/// End 节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EndNodeConfig {
    #[serde(default)]
    pub outputs: Vec<OutputVariable>,
}

/// 输出变量定义
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct OutputVariable {
    pub name: String,
    pub variable_selector: String,
}

/// If/Else 节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct IfElseNodeConfig {
    /// 条件列表
    pub conditions: Vec<Condition>,

    /// 逻辑运算符（and, or）
    pub logical_operator: LogicalOperator,
}

/// 条件
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Condition {
    /// 变量选择器（如 "llm_node_1.text"）
    pub variable_selector: String,

    /// 比较运算符
    pub comparison_operator: ComparisonOperator,

    /// 比较值
    #[serde(default)]
    pub value: Value,
}

/// 比较运算符
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOperator {
    Contains,
    NotContains,
    StartWith,
    EndWith,
    IsEmpty,
    IsNotEmpty,
    Equal,
    NotEqual,
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
}

/// 逻辑运算符
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum LogicalOperator {
    And,
    Or,
}

/// Iteration 节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct IterationNodeConfig {
    /// 输入数组的变量选择器
    pub input_selector: String,

    /// 是否并行执行
    #[serde(default)]
    pub parallel: bool,

    /// 并行度（最大并发数）
    #[serde(default = "default_parallelism")]
    pub parallelism: usize,

    /// 子图定义（嵌套的节点和边）
    pub sub_workflow: SubWorkflowSchema,
}

fn default_parallelism() -> usize {
    10
}

/// 子工作流模式
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SubWorkflowSchema {
    pub nodes: Vec<NodeSchema>,
    pub edges: Vec<EdgeSchema>,
}

/// Template 节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TemplateNodeConfig {
    /// 模板内容
    pub template: String,
}

/// LLM 节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct LlmNodeConfig {
    /// 模型提供商
    pub provider: String,

    /// 模型名称
    pub model: String,

    /// Prompt 模板
    pub prompt_template: Vec<PromptMessage>,

    /// 模型参数
    #[serde(default)]
    pub model_parameters: ModelParameters,

    /// 是否启用流式输出
    #[serde(default)]
    pub stream: bool,
}

/// Prompt 消息
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
}

/// 模型参数
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ModelParameters {
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    #[serde(default = "default_top_p")]
    pub top_p: f32,

    #[serde(default)]
    pub max_tokens: Option<u32>,
}

impl Default for ModelParameters {
    fn default() -> Self {
        ModelParameters {
            temperature: default_temperature(),
            top_p: default_top_p(),
            max_tokens: None,
        }
    }
}

fn default_temperature() -> f32 {
    0.7
}
fn default_top_p() -> f32 {
    1.0
}

/// Code 节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CodeNodeConfig {
    pub language: CodeLanguage,
    pub code: String,
    #[serde(default)]
    pub inputs: std::collections::HashMap<String, String>,
    pub output_variable: String,
}

/// 代码语言
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum CodeLanguage {
    Python,
    Javascript,
}

/// HTTP Request 节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct HttpRequestNodeConfig {
    pub method: HttpMethod,
    pub url: String,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

/// HTTP 方法
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

fn default_timeout() -> u64 {
    30
}

/// 变量赋值节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableAssignerNodeConfig {
    /// 要赋值的变量列表
    pub assignments: Vec<VariableAssignment>,
}

/// 变量赋值
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableAssignment {
    /// 目标变量名
    pub variable: String,
    /// 源值（可以是变量选择器或常量）
    pub value: Value,
    /// 是否从变量池解析
    #[serde(default)]
    pub from_variable: bool,
    /// 变量选择器
    #[serde(default)]
    pub variable_selector: Option<String>,
}

/// 变量聚合器节点配置
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VariableAggregatorNodeConfig {
    /// 要聚合的变量选择器列表
    pub variables: Vec<String>,
    /// 输出变量名
    pub output_variable: String,
}
