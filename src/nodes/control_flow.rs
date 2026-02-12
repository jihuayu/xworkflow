//! Control flow node executors: Start, End, Answer, IfElse.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, SegmentStream, StreamEvent, StreamStatus, VariablePool};
use crate::core::variable_pool::Selector;
use crate::compiler::CompiledNodeConfig;
use crate::dsl::schema::{
    Case, EdgeHandle, NodeOutputs, NodeRunResult, OutputVariable, StartVariable,
    WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::evaluator::evaluate_cases;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;
// render_template_async no longer used in this module
use regex::Regex;
#[cfg(feature = "security")]
use crate::security::SecurityLevel;

// ================================
// Start Node
// ================================

/// Executor for the Start node. Copies input variables into the pool.
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
                    let sel = Selector::new(node_id, var.variable.clone());
                    let val = variable_pool.get(&sel);
                    outputs.insert(var.variable.clone(), val.to_value());
                }
            }
        }

        // System variables
        let sys_query = variable_pool.get(&Selector::new("sys", "query"));
        outputs.insert("sys.query".to_string(), sys_query.to_value());
        let sys_files = variable_pool.get(&Selector::new("sys", "files"));
        outputs.insert("sys.files".to_string(), sys_files.to_value());

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }

    async fn execute_compiled(
        &self,
        node_id: &str,
        compiled_config: &CompiledNodeConfig,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let CompiledNodeConfig::Start(config) = compiled_config else {
            return self.execute(node_id, compiled_config.as_value(), variable_pool, _context).await;
        };

        let mut outputs = HashMap::new();
        for var in &config.parsed.variables {
            let sel = Selector::new(node_id, var.variable.clone());
            let val = variable_pool.get(&sel);
            outputs.insert(var.variable.clone(), val.to_value());
        }

        let sys_query = variable_pool.get(&Selector::new("sys", "query"));
        outputs.insert("sys.query".to_string(), sys_query.to_value());
        let sys_files = variable_pool.get(&Selector::new("sys", "files"));
        outputs.insert("sys.files".to_string(), sys_files.to_value());

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

// ================================
// End Node
// ================================

/// Executor for the End node. Collects final workflow outputs.
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
                            let val = variable_pool.get_resolved(&sel).await;
                            let json_val = val.to_value();
                            outputs.insert(variable.to_string(), json_val.clone());
                            inputs.insert(variable.to_string(), json_val);
                        }
                    }
                }
            } else if let Ok(output_vars) = serde_json::from_value::<Vec<OutputVariable>>(outputs_val.clone()) {
                for ov in &output_vars {
                    let val = variable_pool.get_resolved(&ov.value_selector).await;
                    let json_val = val.to_value();
                    outputs.insert(ov.variable.clone(), json_val.clone());
                    inputs.insert(ov.variable.clone(), json_val);
                }
            }
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            inputs,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }

    async fn execute_compiled(
        &self,
        _node_id: &str,
        compiled_config: &CompiledNodeConfig,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let CompiledNodeConfig::End(config) = compiled_config else {
            return self.execute(_node_id, compiled_config.as_value(), variable_pool, _context).await;
        };

        let mut outputs = HashMap::new();
        let mut inputs = HashMap::new();
        for ov in &config.parsed.outputs {
            let val = variable_pool.get_resolved(&ov.value_selector).await;
            let json_val = val.to_value();
            outputs.insert(ov.variable.clone(), json_val.clone());
            inputs.insert(ov.variable.clone(), json_val);
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            inputs,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
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

        let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut static_values: HashMap<String, String> = HashMap::new();
        let mut stream_vars: Vec<(String, SegmentStream)> = Vec::new();

        for caps in re.captures_iter(answer_template) {
            let selector_str = &caps[1];
            if !seen.insert(selector_str.to_string()) {
                continue;
            }
            let Some(selector) = Selector::parse_str(selector_str) else {
                continue;
            };
            let seg = variable_pool.get(&selector);
            match seg {
                Segment::Stream(stream) => {
                    if stream.status_async().await == StreamStatus::Running {
                        stream_vars.push((selector_str.to_string(), stream));
                        static_values.insert(selector_str.to_string(), String::new());
                    } else {
                        let snap = stream.snapshot_segment_async().await;
                        static_values.insert(selector_str.to_string(), snap.to_display_string());
                    }
                }
                other => {
                    static_values.insert(selector_str.to_string(), other.to_display_string());
                }
            }
        }

        if stream_vars.is_empty() {
            let rendered = render_answer_with_map(answer_template, &static_values);
            let mut outputs = HashMap::new();
            outputs.insert("answer".to_string(), Value::String(rendered));
            return Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                outputs: NodeOutputs::Sync(outputs),
                edge_source_handle: EdgeHandle::Default,
                ..Default::default()
            });
        }

        let template_str = answer_template.to_string();
        let (answer_stream, writer) = SegmentStream::channel();

        tokio::spawn(async move {
            let mut values = static_values;
            let mut last_rendered = String::new();

            for (selector, stream) in stream_vars {
                let mut reader = stream.reader();
                loop {
                    match reader.next().await {
                        Some(StreamEvent::Chunk(seg)) => {
                            let entry = values.entry(selector.clone()).or_default();
                            entry.push_str(&seg.to_display_string());
                            let rendered = render_answer_with_map(&template_str, &values);
                            let delta = if rendered.starts_with(&last_rendered) {
                                rendered[last_rendered.len()..].to_string()
                            } else if rendered != last_rendered {
                                rendered.clone()
                            } else {
                                String::new()
                            };
                            last_rendered = rendered;
                            if !delta.is_empty() {
                                writer.send(Segment::String(delta)).await;
                            }
                        }
                        Some(StreamEvent::End(final_seg)) => {
                            values.insert(selector.clone(), final_seg.to_display_string());
                            let rendered = render_answer_with_map(&template_str, &values);
                            let delta = if rendered.starts_with(&last_rendered) {
                                rendered[last_rendered.len()..].to_string()
                            } else if rendered != last_rendered {
                                rendered.clone()
                            } else {
                                String::new()
                            };
                            last_rendered = rendered;
                            if !delta.is_empty() {
                                writer.send(Segment::String(delta)).await;
                            }
                            break;
                        }
                        Some(StreamEvent::Error(err)) => {
                            writer.error(err).await;
                            return;
                        }
                        None => break,
                    }
                }
            }

            writer.end(Segment::String(last_rendered)).await;
        });

        let mut stream_outputs = HashMap::new();
        stream_outputs.insert("answer".to_string(), answer_stream);

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Stream {
                ready: HashMap::new(),
                streams: stream_outputs,
            },
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }

    async fn execute_compiled(
        &self,
        _node_id: &str,
        compiled_config: &CompiledNodeConfig,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let CompiledNodeConfig::Answer(config) = compiled_config else {
            return self.execute(_node_id, compiled_config.as_value(), variable_pool, _context).await;
        };

        let answer_template = &config.parsed.answer;
        let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut static_values: HashMap<String, String> = HashMap::new();
        let mut stream_vars: Vec<(String, SegmentStream)> = Vec::new();

        for caps in re.captures_iter(answer_template) {
            let selector_str = &caps[1];
            if !seen.insert(selector_str.to_string()) {
                continue;
            }
            let Some(selector) = Selector::parse_str(selector_str) else {
                continue;
            };
            let seg = variable_pool.get(&selector);
            match seg {
                Segment::Stream(stream) => {
                    if stream.status_async().await == StreamStatus::Running {
                        stream_vars.push((selector_str.to_string(), stream));
                        static_values.insert(selector_str.to_string(), String::new());
                    } else {
                        let snap = stream.snapshot_segment_async().await;
                        static_values.insert(selector_str.to_string(), snap.to_display_string());
                    }
                }
                other => {
                    static_values.insert(selector_str.to_string(), other.to_display_string());
                }
            }
        }

        if stream_vars.is_empty() {
            let rendered = render_answer_with_map(answer_template, &static_values);
            let mut outputs = HashMap::new();
            outputs.insert("answer".to_string(), Value::String(rendered));
            return Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                outputs: NodeOutputs::Sync(outputs),
                edge_source_handle: EdgeHandle::Default,
                ..Default::default()
            });
        }

        let template_str = answer_template.to_string();
        let (answer_stream, writer) = SegmentStream::channel();

        tokio::spawn(async move {
            let mut values = static_values;
            let mut last_rendered = String::new();

            for (selector, stream) in stream_vars {
                let mut reader = stream.reader();
                loop {
                    match reader.next().await {
                        Some(StreamEvent::Chunk(seg)) => {
                            let entry = values.entry(selector.clone()).or_default();
                            entry.push_str(&seg.to_display_string());
                            let rendered = render_answer_with_map(&template_str, &values);
                            let delta = if rendered.starts_with(&last_rendered) {
                                rendered[last_rendered.len()..].to_string()
                            } else {
                                rendered.clone()
                            };
                            last_rendered = rendered;
                            let _ = writer.send(Segment::String(delta)).await;
                        }
                        Some(StreamEvent::End(_)) | None => break,
                        Some(StreamEvent::Error(err)) => {
                            let _ = writer.error(err).await;
                            break;
                        }
                    }
                }
            }

            let _ = writer.finish().await;
        });

        let mut stream_outputs = HashMap::new();
        stream_outputs.insert("answer".to_string(), answer_stream);

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Stream {
                ready: HashMap::new(),
                streams: stream_outputs,
            },
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

fn render_answer_with_map(template: &str, values: &HashMap<String, String>) -> String {
    let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();
    re.replace_all(template, |caps: &regex::Captures| {
        let selector_str = &caps[1];
        values.get(selector_str).cloned().unwrap_or_default()
    })
    .into_owned()
}

// ================================
// IfElse Node (Dify-compatible multi-case)
// ================================

pub struct IfElseNodeExecutor;

const ELSE_BRANCH_HANDLE: &str = "false";

#[async_trait]
impl NodeExecutor for IfElseNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        // Parse cases from config
        let cases: Vec<Case> = config
            .get("cases")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        #[cfg(not(feature = "security"))]
        let _ = context;

        let selected = match evaluate_cases(&cases, variable_pool).await {
            Ok(selected) => selected,
            Err(mismatch) => {
                let message = format!(
                    "Condition type mismatch: expected {}, got {} for {:?}",
                    mismatch.expected_type, mismatch.actual_type, mismatch.operator
                );

                #[cfg(feature = "security")]
                let is_strict = context
                    .security_policy()
                    .map(|policy| policy.level == SecurityLevel::Strict)
                    .or_else(|| {
                        context
                            .resource_group()
                            .map(|group| group.security_level == SecurityLevel::Strict)
                    })
                    .unwrap_or(false);

                #[cfg(not(feature = "security"))]
                let is_strict = false;

                if is_strict {
                    return Err(NodeError::TypeError(message));
                }

                tracing::warn!("{}; falling back to else branch", message);
                None
            }
        };

        let selected_case = selected.unwrap_or_else(|| ELSE_BRANCH_HANDLE.to_string());
        let mut outputs = HashMap::new();
        outputs.insert("selected_case".to_string(), Value::String(selected_case.clone()));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Branch(selected_case),
            ..Default::default()
        })
    }

    async fn execute_compiled(
        &self,
        _node_id: &str,
        compiled_config: &CompiledNodeConfig,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let CompiledNodeConfig::IfElse(config) = compiled_config else {
            return self.execute(_node_id, compiled_config.as_value(), variable_pool, context).await;
        };

        let cases = &config.parsed.cases;

        #[cfg(not(feature = "security"))]
        let _ = context;

        let selected = match evaluate_cases(cases, variable_pool).await {
            Ok(selected) => selected,
            Err(mismatch) => {
                let message = format!(
                    "Condition type mismatch: expected {}, got {} for {:?}",
                    mismatch.expected_type, mismatch.actual_type, mismatch.operator
                );

                #[cfg(feature = "security")]
                let is_strict = context
                    .security_policy()
                    .map(|policy| policy.level == SecurityLevel::Strict)
                    .or_else(|| {
                        context
                            .resource_group()
                            .map(|group| group.security_level == SecurityLevel::Strict)
                    })
                    .unwrap_or(false);

                #[cfg(not(feature = "security"))]
                let is_strict = false;

                if is_strict {
                    return Err(NodeError::TypeError(message));
                }

                tracing::warn!("{}; falling back to else branch", message);
                None
            }
        };

        let selected_case = selected.unwrap_or_else(|| ELSE_BRANCH_HANDLE.to_string());
        let mut outputs = HashMap::new();
        outputs.insert("selected_case".to_string(), Value::String(selected_case.clone()));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Branch(selected_case),
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
            &crate::core::variable_pool::Selector::new("start1", "query"),
            Segment::String("hello".into()),
        );
        pool.set(
            &crate::core::variable_pool::Selector::new("sys", "query"),
            Segment::String("sys_query".into()),
        );

        let config = serde_json::json!({
            "variables": [{"variable": "query", "label": "Q", "type": "string", "required": true}]
        });

        let executor = StartNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("start1", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("query"),
            Some(&Value::String("hello".into()))
        );
        assert_eq!(
            result.outputs.ready().get("sys.query"),
            Some(&Value::String("sys_query".into()))
        );
    }

    #[tokio::test]
    async fn test_end_node() {
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("node_llm", "text"),
            Segment::String("result text".into()),
        );

        let config = serde_json::json!({
            "outputs": [{"variable": "result", "value_selector": ["node_llm", "text"]}]
        });

        let executor = EndNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("end1", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("result"),
            Some(&Value::String("result text".into()))
        );
    }

    #[tokio::test]
    async fn test_answer_node() {
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("n1", "name"),
            Segment::String("Alice".into()),
        );

        let config = serde_json::json!({
            "answer": "Hello {{#n1.name#}}!"
        });

        let executor = AnswerNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("ans1", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("answer"),
            Some(&Value::String("Hello Alice!".into()))
        );
    }

    #[tokio::test]
    async fn test_answer_node_with_stream() {
        let (stream, writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("n1", "name"),
            Segment::Stream(stream),
        );

        tokio::spawn(async move {
            writer.send(Segment::String("Bob".into())).await;
            writer.end(Segment::String("Bob".into())).await;
        });

        let config = serde_json::json!({
            "answer": "Hello {{#n1.name#}}!"
        });

        let executor = AnswerNodeExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("ans_stream", &config, &pool, &context).await.unwrap();
        let stream = result
            .outputs
            .streams()
            .and_then(|streams| streams.get("answer").cloned())
            .expect("missing answer stream");
        let final_seg = stream.collect().await.unwrap_or(Segment::None);
        assert_eq!(final_seg.to_display_string(), "Hello Bob!");
    }

    #[tokio::test]
    async fn test_ifelse_true_branch() {
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("n", "x"),
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
        assert_eq!(
            result.edge_source_handle,
            EdgeHandle::Branch("case1".to_string())
        );
    }

    #[tokio::test]
    async fn test_ifelse_else_branch() {
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("n", "x"),
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
        assert_eq!(
            result.edge_source_handle,
            EdgeHandle::Branch("false".to_string())
        );
    }

    #[tokio::test]
    async fn test_ifelse_multi_case() {
        let mut pool = VariablePool::new();
        pool.set(
            &crate::core::variable_pool::Selector::new("n", "x"),
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
        assert_eq!(
            result.edge_source_handle,
            EdgeHandle::Branch("case2".to_string())
        );
    }
}
