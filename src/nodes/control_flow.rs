use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::{
    Case, NodeRunResult, OutputVariable, StartVariable, WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::evaluator::evaluate_cases;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;
use crate::template::render_template;

// ================================
// Start Node
// ================================

pub struct StartNodeExecutor;

#[async_trait]
impl NodeExecutor for StartNodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let mut outputs = HashMap::new();

        // Parse start variables from config
        if let Some(vars_val) = config.get("variables") {
            if let Ok(vars) = serde_json::from_value::<Vec<StartVariable>>(vars_val.clone()) {
                for var in &vars {
                    let sel = vec![node_id.to_string(), var.variable.clone()];
                    let val = variable_pool.get(&sel);
                    outputs.insert(var.variable.clone(), val.to_value());
                }
            }
        }

        // System variables
        let sys_query = variable_pool.get(&["sys".to_string(), "query".to_string()]);
        outputs.insert("sys.query".to_string(), sys_query.to_value());
        let sys_files = variable_pool.get(&["sys".to_string(), "files".to_string()]);
        outputs.insert("sys.files".to_string(), sys_files.to_value());

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

// ================================
// End Node
// ================================

pub struct EndNodeExecutor;

#[async_trait]
impl NodeExecutor for EndNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let mut outputs = HashMap::new();
        let mut inputs = HashMap::new();

        if let Some(outputs_val) = config.get("outputs") {
            if let Some(arr) = outputs_val.as_array() {
                for ov in arr {
                    let variable = ov.get("variable").and_then(|v| v.as_str()).unwrap_or("");
                    let selector_val = ov.get("value_selector").or_else(|| ov.get("variable_selector"));
                    let selector = selector_val.and_then(selector_from_value);

                    if !variable.is_empty() {
                        if let Some(sel) = selector {
                            let val = variable_pool.get(&sel);
                            let json_val = val.to_value();
                            outputs.insert(variable.to_string(), json_val.clone());
                            inputs.insert(variable.to_string(), json_val);
                        }
                    }
                }
            } else if let Ok(output_vars) = serde_json::from_value::<Vec<OutputVariable>>(outputs_val.clone()) {
                for ov in &output_vars {
                    let val = variable_pool.get(&ov.value_selector);
                    let json_val = val.to_value();
                    outputs.insert(ov.variable.clone(), json_val.clone());
                    inputs.insert(ov.variable.clone(), json_val);
                }
            }
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            inputs,
            outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

// ================================
// Answer Node
// ================================

pub struct AnswerNodeExecutor;

#[async_trait]
impl NodeExecutor for AnswerNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let answer_template = config
            .get("answer")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let rendered = render_template(answer_template, variable_pool);

        let mut outputs = HashMap::new();
        outputs.insert("answer".to_string(), Value::String(rendered));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

// ================================
// IfElse Node (Dify-compatible multi-case)
// ================================

pub struct IfElseNodeExecutor;

#[async_trait]
impl NodeExecutor for IfElseNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        // Parse cases from config
        let cases: Vec<Case> = config
            .get("cases")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let selected = evaluate_cases(&cases, variable_pool);

        let mut outputs = HashMap::new();
        outputs.insert("selected_case".to_string(), Value::String(selected.clone()));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: selected,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::Segment;

    #[tokio::test]
    async fn test_start_node() {
        let mut pool = VariablePool::new();
        pool.set(
            &["start1".to_string(), "query".to_string()],
            Segment::String("hello".into()),
        );
        pool.set(
            &["sys".to_string(), "query".to_string()],
            Segment::String("sys_query".into()),
        );

        let config = serde_json::json!({
            "variables": [{"variable": "query", "label": "Q", "type": "string", "required": true}]
        });

        let executor = StartNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("start1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("query"), Some(&Value::String("hello".into())));
        assert_eq!(result.outputs.get("sys.query"), Some(&Value::String("sys_query".into())));
    }

    #[tokio::test]
    async fn test_end_node() {
        let mut pool = VariablePool::new();
        pool.set(
            &["node_llm".to_string(), "text".to_string()],
            Segment::String("result text".into()),
        );

        let config = serde_json::json!({
            "outputs": [{"variable": "result", "value_selector": ["node_llm", "text"]}]
        });

        let executor = EndNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("end1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("result"), Some(&Value::String("result text".into())));
    }

    #[tokio::test]
    async fn test_answer_node() {
        let mut pool = VariablePool::new();
        pool.set(
            &["n1".to_string(), "name".to_string()],
            Segment::String("Alice".into()),
        );

        let config = serde_json::json!({
            "answer": "Hello {{#n1.name#}}!"
        });

        let executor = AnswerNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("ans1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("answer"), Some(&Value::String("Hello Alice!".into())));
    }

    #[tokio::test]
    async fn test_ifelse_true_branch() {
        let mut pool = VariablePool::new();
        pool.set(
            &["n".to_string(), "x".to_string()],
            Segment::Integer(10),
        );

        let config = serde_json::json!({
            "cases": [{
                "case_id": "case1",
                "logical_operator": "and",
                "conditions": [{
                    "variable_selector": ["n", "x"],
                    "comparison_operator": "greater_than",
                    "value": 5
                }]
            }]
        });

        let executor = IfElseNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("if1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.edge_source_handle, "case1");
    }

    #[tokio::test]
    async fn test_ifelse_else_branch() {
        let mut pool = VariablePool::new();
        pool.set(
            &["n".to_string(), "x".to_string()],
            Segment::Integer(3),
        );

        let config = serde_json::json!({
            "cases": [{
                "case_id": "case1",
                "logical_operator": "and",
                "conditions": [{
                    "variable_selector": ["n", "x"],
                    "comparison_operator": "greater_than",
                    "value": 5
                }]
            }]
        });

        let executor = IfElseNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("if1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.edge_source_handle, "false");
    }

    #[tokio::test]
    async fn test_ifelse_multi_case() {
        let mut pool = VariablePool::new();
        pool.set(
            &["n".to_string(), "x".to_string()],
            Segment::Integer(15),
        );

        let config = serde_json::json!({
            "cases": [
                {
                    "case_id": "case1",
                    "logical_operator": "and",
                    "conditions": [{
                        "variable_selector": ["n", "x"],
                        "comparison_operator": "less_than",
                        "value": 10
                    }]
                },
                {
                    "case_id": "case2",
                    "logical_operator": "and",
                    "conditions": [{
                        "variable_selector": ["n", "x"],
                        "comparison_operator": "greater_than",
                        "value": 10
                    }]
                }
            ]
        });

        let executor = IfElseNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("if1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.edge_source_handle, "case2");
    }
}
