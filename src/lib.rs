//! XWorkflow - Dify 工作流执行引擎
//!
//! 一个基于队列的事件驱动图引擎，用于执行 Dify 风格的 DAG 工作流。
//!
//! # 架构概览
//!
//! - **dsl**: DSL 解析与验证（YAML/JSON → WorkflowSchema）
//! - **graph**: 图结构构建与遍历（petgraph）
//! - **core**: 核心运行时（调度器、事件总线、变量池）
//! - **nodes**: 节点执行器（Strategy 模式）
//! - **template**: Jinja2 兼容模板引擎
//! - **evaluator**: 条件评估引擎
//! - **error**: 错误类型定义

pub mod core;
pub mod dsl;
pub mod error;
pub mod evaluator;
pub mod graph;
pub mod nodes;
pub mod scheduler;
pub mod template;

use std::sync::Arc;

use serde_json::Value;

use crate::core::dispatcher::Dispatcher;
use crate::core::event_bus::{create_event_channel, EventReceiver};
use crate::core::workflow_runtime::WorkflowRuntime;
use crate::dsl::{parse_dsl, validate_workflow_schema, DslFormat};
use crate::error::WorkflowError;
use crate::graph::builder::build_graph;
use crate::graph::validator::validate_graph;
use crate::nodes::registry::create_default_registry;

// 导出调度器
pub use scheduler::{ExecutionStatus, WorkflowHandle, WorkflowScheduler};

/// 工作流执行结果
pub struct WorkflowExecutionHandle {
    /// 执行结果的 JoinHandle
    pub result_handle: tokio::task::JoinHandle<Result<Value, WorkflowError>>,
    /// 事件接收器（用于监听执行过程中的事件）
    pub event_receiver: EventReceiver,
    /// 执行 ID
    pub execution_id: String,
}

/// 执行工作流（高层 API）
///
/// # Arguments
///
/// * `dsl_content` - DSL 内容（YAML 或 JSON）
/// * `format` - DSL 格式
/// * `inputs` - 工作流输入参数
/// * `user_id` - 用户 ID
///
/// # Returns
///
/// 返回 `WorkflowExecutionHandle`，包含异步结果和事件接收器。
pub fn execute_workflow(
    dsl_content: &str,
    format: DslFormat,
    inputs: Value,
    user_id: &str,
) -> Result<WorkflowExecutionHandle, WorkflowError> {
    // 1. 解析 DSL
    let schema = parse_dsl(dsl_content, format)
        .map_err(|e| WorkflowError::DslParseError(e.to_string()))?;

    // 2. 验证 schema
    validate_workflow_schema(&schema)
        .map_err(|e| WorkflowError::GraphValidationError(e.to_string()))?;

    // 3. 构建图
    let definition = build_graph(&schema)?;

    // 4. 验证图
    validate_graph(&definition.graph)?;

    // 5. 创建事件通道
    let (event_sender, event_receiver) = create_event_channel();

    // 6. 创建运行时
    let definition = Arc::new(definition);
    let runtime = Arc::new(WorkflowRuntime::new(
        definition,
        inputs,
        user_id.to_string(),
        event_sender,
    ));

    let execution_id = runtime.execution_id.clone();

    // 7. 创建调度器并运行
    let node_registry = Arc::new(create_default_registry());
    let dispatcher = Dispatcher::new(runtime, node_registry);

    let result_handle = tokio::spawn(async move { dispatcher.run().await });

    Ok(WorkflowExecutionHandle {
        result_handle,
        event_receiver,
        execution_id,
    })
}

/// 同步执行工作流（等待完成后返回结果）
pub async fn execute_workflow_sync(
    dsl_content: &str,
    format: DslFormat,
    inputs: Value,
    user_id: &str,
) -> Result<Value, WorkflowError> {
    let handle = execute_workflow(dsl_content, format, inputs, user_id)?;
    handle
        .result_handle
        .await
        .map_err(|e| WorkflowError::InternalError(format!("Task join error: {}", e)))?
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use serde_json::json;

    fn simple_linear_workflow_yaml() -> &'static str {
        r#"
nodes:
  - id: start_1
    type: start
    data: {}
    title: Start
  - id: template_1
    type: template
    data:
      template: "Hello {{#input.name#}}!"
    title: Greeting Template
  - id: end_1
    type: end
    data:
      outputs:
        - name: result
          variable_selector: "template_1.output"
    title: End

edges:
  - source: start_1
    target: template_1
  - source: template_1
    target: end_1
"#
    }

    fn branching_workflow_yaml() -> &'static str {
        r#"
nodes:
  - id: start_1
    type: start
    data: {}
    title: Start
  - id: ifelse_1
    type: if-else
    data:
      conditions:
        - variable_selector: "input.age"
          comparison_operator: greater_than
          value: 18
      logical_operator: and
    title: Check Age
  - id: template_true
    type: template
    data:
      template: "Adult user: {{#input.name#}}"
    title: Adult Template
  - id: template_false
    type: template
    data:
      template: "Minor user: {{#input.name#}}"
    title: Minor Template
  - id: end_1
    type: end
    data:
      outputs:
        - name: result
          variable_selector: "template_true.output"
        - name: result2
          variable_selector: "template_false.output"
    title: End

edges:
  - source: start_1
    target: ifelse_1
  - source: ifelse_1
    target: template_true
    source_handle: "true"
  - source: ifelse_1
    target: template_false
    source_handle: "false"
  - source: template_true
    target: end_1
  - source: template_false
    target: end_1
"#
    }

    #[tokio::test]
    async fn test_simple_linear_workflow() {
        let inputs = json!({"name": "World"});
        let result =
            execute_workflow_sync(simple_linear_workflow_yaml(), DslFormat::Yaml, inputs, "user1")
                .await;
        assert!(result.is_ok(), "Workflow failed: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output["result"], "Hello World!");
    }

    #[tokio::test]
    async fn test_branching_workflow_true_branch() {
        let inputs = json!({"name": "Alice", "age": 25});
        let result =
            execute_workflow_sync(branching_workflow_yaml(), DslFormat::Yaml, inputs, "user1")
                .await;
        assert!(result.is_ok(), "Workflow failed: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output["result"], "Adult user: Alice");
    }

    #[tokio::test]
    async fn test_branching_workflow_false_branch() {
        let inputs = json!({"name": "Bob", "age": 15});
        let result =
            execute_workflow_sync(branching_workflow_yaml(), DslFormat::Yaml, inputs, "user1")
                .await;
        assert!(result.is_ok(), "Workflow failed: {:?}", result.err());
        let output = result.unwrap();
        assert_eq!(output["result2"], "Minor user: Bob");
    }

    #[tokio::test]
    async fn test_invalid_dsl() {
        let result =
            execute_workflow_sync("invalid dsl content {{{", DslFormat::Yaml, json!({}), "user1")
                .await;
        assert!(result.is_err());
    }
}
