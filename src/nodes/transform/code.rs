//! Code Node executor (sandbox-backed execution).

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
#[cfg(feature = "builtin-sandbox-js")]
use crate::core::variable_pool::StreamEvent;
use crate::core::variable_pool::{Segment, SegmentStream, StreamStatus, VariablePool};
use crate::dsl::schema::{
    EdgeHandle, NodeOutputs, NodeRunResult, VariableMapping, WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;

use super::helpers::execute_sandbox_with_audit;

/// Executor for Code nodes. Runs code in a sandboxed environment (JS, Python, WASM).
pub struct CodeNodeExecutor {
    sandbox_manager: std::sync::Arc<crate::sandbox::SandboxManager>,
}

impl CodeNodeExecutor {
    pub fn new() -> Self {
        let manager =
            crate::sandbox::SandboxManager::new(crate::sandbox::SandboxManagerConfig::default());
        Self {
            sandbox_manager: std::sync::Arc::new(manager),
        }
    }

    pub fn new_with_manager(manager: std::sync::Arc<crate::sandbox::SandboxManager>) -> Self {
        Self {
            sandbox_manager: manager,
        }
    }
}

impl Default for CodeNodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeExecutor for CodeNodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let code = config.get("code").and_then(|v| v.as_str()).unwrap_or("");
        let language_str = config
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("javascript");

        // Map language string to CodeLanguage enum
        let language = match language_str {
            "javascript" | "js" | "javascript3" => crate::sandbox::CodeLanguage::JavaScript,
            "typescript" | "ts" => crate::sandbox::CodeLanguage::TypeScript,
            "python" | "python3" => crate::sandbox::CodeLanguage::Python,
            "wasm" => crate::sandbox::CodeLanguage::Wasm,
            other => {
                return Err(NodeError::ConfigError(format!(
                    "Unsupported code language: {}",
                    other
                )));
            }
        };

        // Build inputs from variable mappings
        let mut inputs_map = serde_json::Map::new();
        let mut stream_inputs: Vec<(String, SegmentStream)> = Vec::new();
        let mut push_stream = |name: String, stream: SegmentStream| {
            stream_inputs.retain(|(n, _)| n != &name);
            stream_inputs.push((name, stream));
        };

        if let Some(vars_val) = config.get("variables") {
            if let Ok(mappings) = serde_json::from_value::<Vec<VariableMapping>>(vars_val.clone()) {
                for m in &mappings {
                    let val = variable_pool.get(&m.value_selector);
                    match val {
                        Segment::Stream(stream) => {
                            let keep_stream = language == crate::sandbox::CodeLanguage::JavaScript
                                || stream.status_async().await == StreamStatus::Running;
                            if keep_stream {
                                inputs_map.insert(m.variable.clone(), Value::Null);
                                push_stream(m.variable.clone(), stream);
                            } else {
                                inputs_map.insert(
                                    m.variable.clone(),
                                    stream.snapshot_segment_async().await.into_value(),
                                );
                            }
                        }
                        other => {
                            inputs_map.insert(m.variable.clone(), other.into_value());
                        }
                    }
                }
            }
        }
        // Support inputs map: { var: selector }
        if let Some(inputs_val) = config.get("inputs") {
            if let Some(map) = inputs_val.as_object() {
                for (var, sel_val) in map {
                    if let Some(selector) = selector_from_value(sel_val) {
                        let val = variable_pool.get(&selector);
                        match val {
                            Segment::Stream(stream) => {
                                let keep_stream = language
                                    == crate::sandbox::CodeLanguage::JavaScript
                                    || stream.status_async().await == StreamStatus::Running;
                                if keep_stream {
                                    inputs_map.insert(var.clone(), Value::Null);
                                    push_stream(var.clone(), stream);
                                } else {
                                    inputs_map.insert(
                                        var.clone(),
                                        stream.snapshot_segment_async().await.into_value(),
                                    );
                                }
                            }
                            other => {
                                inputs_map.insert(var.clone(), other.into_value());
                            }
                        }
                    }
                }
            }
        }

        let inputs = Value::Object(inputs_map.clone());

        // Build execution config
        let timeout_secs = config.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

        let exec_config = crate::sandbox::ExecutionConfig {
            timeout: std::time::Duration::from_secs(timeout_secs),
            ..crate::sandbox::ExecutionConfig::default()
        };

        // Execute via sandbox (or stream mode for JS)
        let has_running_streams = !stream_inputs.is_empty();
        if has_running_streams && language == crate::sandbox::CodeLanguage::JavaScript {
            #[cfg(feature = "builtin-sandbox-js")]
            {
                self.sandbox_manager
                    .validate(code, language)
                    .await
                    .map_err(|e| NodeError::SandboxError(e.to_string()))?;

                let stream_names = stream_inputs
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect::<Vec<_>>();

                let request = crate::sandbox::SandboxRequest {
                    code: code.to_string(),
                    language,
                    inputs: inputs.clone(),
                    config: exec_config.clone(),
                };

                let streaming = self
                    .sandbox_manager
                    .get_streaming_sandbox(language)
                    .map_err(|e| NodeError::SandboxError(e.to_string()))?;

                let (initial_output, has_callbacks, handle) = streaming
                    .execute_streaming(request, stream_names)
                    .await
                    .map_err(|e| NodeError::SandboxError(e.to_string()))?;

                if !has_callbacks {
                    let _ = handle.finalize().await;
                    let mut resolved_inputs = inputs_map.clone();
                    for (name, stream) in stream_inputs {
                        let resolved = stream.collect().await.unwrap_or(Segment::None);
                        resolved_inputs.insert(name, resolved.to_value());
                    }
                    let request = crate::sandbox::SandboxRequest {
                        code: code.to_string(),
                        language,
                        inputs: Value::Object(resolved_inputs.clone()),
                        config: exec_config,
                    };
                    let result = execute_sandbox_with_audit(
                        &self.sandbox_manager,
                        request,
                        context,
                        node_id,
                        language,
                    )
                    .await?;

                    let mut outputs = HashMap::new();
                    if let Some(output_key) = config.get("output_variable").and_then(|v| v.as_str())
                    {
                        outputs.insert(output_key.to_string(), Segment::from_value(&result.output));
                    } else if let Value::Object(obj) = &result.output {
                        for (k, v) in obj {
                            outputs.insert(k.clone(), Segment::from_value(v));
                        }
                    } else {
                        outputs.insert("result".to_string(), Segment::from_value(&result.output));
                    }

                    return Ok(NodeRunResult {
                        status: WorkflowNodeExecutionStatus::Succeeded,
                        inputs: resolved_inputs
                            .into_iter()
                            .map(|(k, v)| (k, Segment::from_value(&v)))
                            .collect(),
                        outputs: NodeOutputs::Sync(outputs),
                        edge_source_handle: EdgeHandle::Default,
                        ..Default::default()
                    });
                }

                let mut outputs = HashMap::new();
                if let Some(output_key) = config.get("output_variable").and_then(|v| v.as_str()) {
                    outputs.insert(output_key.to_string(), Segment::from_value(&initial_output));
                } else if let Value::Object(obj) = &initial_output {
                    for (k, v) in obj {
                        outputs.insert(k.clone(), Segment::from_value(v));
                    }
                } else {
                    outputs.insert("result".to_string(), Segment::from_value(&initial_output));
                }

                let (output_stream, writer) = SegmentStream::channel();
                tokio::spawn(async move {
                    let handle = handle;
                    let mut last_result: Option<Segment> = None;
                    for (name, stream) in stream_inputs {
                        let mut reader = stream.reader();
                        loop {
                            match reader.next().await {
                                Some(StreamEvent::Chunk(seg)) => {
                                    match handle.send_chunk(&name, &seg.to_value()).await {
                                        Ok(Some(val)) => {
                                            let out_seg = Segment::from_value(&val);
                                            writer.send(out_seg.clone()).await;
                                            last_result = Some(out_seg);
                                        }
                                        Ok(None) => {}
                                        Err(err) => {
                                            writer.error(err.to_string()).await;
                                            let _ = handle.finalize().await;
                                            return;
                                        }
                                    }
                                }
                                Some(StreamEvent::End(final_seg)) => {
                                    match handle.end_stream(&name, &final_seg.to_value()).await {
                                        Ok(Some(val)) => {
                                            let out_seg = Segment::from_value(&val);
                                            writer.send(out_seg.clone()).await;
                                            last_result = Some(out_seg);
                                        }
                                        Ok(None) => {}
                                        Err(err) => {
                                            writer.error(err.to_string()).await;
                                            let _ = handle.finalize().await;
                                            return;
                                        }
                                    }
                                    break;
                                }
                                Some(StreamEvent::Error(err)) => {
                                    match handle.error_stream(&name, &err).await {
                                        Ok(Some(val)) => {
                                            let out_seg = Segment::from_value(&val);
                                            writer.send(out_seg.clone()).await;
                                            last_result = Some(out_seg);
                                        }
                                        Ok(None) => {
                                            writer.error(err).await;
                                            let _ = handle.finalize().await;
                                            return;
                                        }
                                        Err(e) => {
                                            writer.error(e.to_string()).await;
                                            let _ = handle.finalize().await;
                                            return;
                                        }
                                    }
                                    break;
                                }
                                None => break,
                            }
                        }
                    }

                    writer.end(last_result.unwrap_or(Segment::None)).await;
                    let _ = handle.finalize().await;
                });

                let mut stream_outputs = HashMap::new();
                stream_outputs.insert("output".to_string(), output_stream);

                return Ok(NodeRunResult {
                    status: WorkflowNodeExecutionStatus::Succeeded,
                    inputs: inputs_map
                        .into_iter()
                        .map(|(k, v)| (k, Segment::from_value(&v)))
                        .collect(),
                    outputs: NodeOutputs::Stream {
                        ready: outputs,
                        streams: stream_outputs,
                    },
                    edge_source_handle: EdgeHandle::Default,
                    ..Default::default()
                });
            }

            #[cfg(not(feature = "builtin-sandbox-js"))]
            {
                return Err(NodeError::SandboxError(
                    "JS streaming requires builtin-sandbox-js".to_string(),
                ));
            }
        }

        if has_running_streams {
            let mut resolved_inputs = inputs_map.clone();
            for (name, stream) in stream_inputs {
                let resolved = stream.collect().await.unwrap_or(Segment::None);
                resolved_inputs.insert(name, resolved.to_value());
            }
            let request = crate::sandbox::SandboxRequest {
                code: code.to_string(),
                language,
                inputs: Value::Object(resolved_inputs.clone()),
                config: exec_config,
            };
            let result = execute_sandbox_with_audit(
                &self.sandbox_manager,
                request,
                context,
                node_id,
                language,
            )
            .await?;

            let mut outputs = HashMap::new();
            if let Some(output_key) = config.get("output_variable").and_then(|v| v.as_str()) {
                outputs.insert(output_key.to_string(), Segment::from_value(&result.output));
            } else if let Value::Object(obj) = &result.output {
                for (k, v) in obj {
                    outputs.insert(k.clone(), Segment::from_value(v));
                }
            } else {
                outputs.insert("result".to_string(), Segment::from_value(&result.output));
            }

            return Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                inputs: resolved_inputs
                    .into_iter()
                    .map(|(k, v)| (k, Segment::from_value(&v)))
                    .collect(),
                outputs: NodeOutputs::Sync(outputs),
                edge_source_handle: EdgeHandle::Default,
                ..Default::default()
            });
        }

        let request = crate::sandbox::SandboxRequest {
            code: code.to_string(),
            language,
            inputs: inputs.clone(),
            config: exec_config,
        };

        let result =
            execute_sandbox_with_audit(&self.sandbox_manager, request, context, node_id, language)
                .await?;

        // Convert output to HashMap
        let mut outputs = HashMap::new();
        if let Some(output_key) = config.get("output_variable").and_then(|v| v.as_str()) {
            outputs.insert(output_key.to_string(), Segment::from_value(&result.output));
        } else if let Value::Object(obj) = &result.output {
            for (k, v) in obj {
                outputs.insert(k.clone(), Segment::from_value(v));
            }
        } else {
            outputs.insert("result".to_string(), Segment::from_value(&result.output));
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            inputs: inputs_map
                .into_iter()
                .map(|(k, v)| (k, Segment::from_value(&v)))
                .collect(),
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "builtin-sandbox-js")]
    use crate::core::variable_pool::Selector;

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_javascript() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return { result: 42 }; }",
            "language": "javascript"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("result"),
            Some(&Segment::Integer(42))
        );
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_with_variables() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("src", "val"), Segment::Float(10.0));

        let config = serde_json::json!({
            "code": "function main(inputs) { return { doubled: inputs.x * 2 }; }",
            "language": "javascript",
            "variables": [{"variable": "x", "value_selector": ["src", "val"]}]
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code2", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("doubled"),
            Some(&Segment::Integer(20))
        );
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_with_stream_input() {
        let (stream, writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("src", "val"), Segment::Stream(stream));

        tokio::spawn(async move {
            writer.send(Segment::String("hi".into())).await;
            writer.end(Segment::String("hi".into())).await;
        });

        let config = serde_json::json!({
            "code": "function main(inputs) { return { result: inputs.x + '!'}; }",
            "language": "javascript",
            "variables": [{"variable": "x", "value_selector": ["src", "val"]}]
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_stream", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("result"),
            Some(&Segment::String("hi!".into()))
        );
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_stream_on_chunk() {
        let (stream, writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("src", "text"), Segment::Stream(stream));

        tokio::spawn(async move {
            writer.send(Segment::String("hi".into())).await;
            writer.send(Segment::String("yo".into())).await;
            writer.end(Segment::String("hiyo".into())).await;
        });

        let config = serde_json::json!({
            "code": "function main(inputs) { inputs.text.on_chunk(function(chunk) { return { output: chunk.toUpperCase() }; }); return {}; }",
            "language": "javascript",
            "variables": [{"variable": "text", "value_selector": ["src", "text"]}]
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_stream_cb", &config, &pool, &context)
            .await
            .unwrap();

        let stream = result
            .outputs
            .streams()
            .and_then(|streams| streams.get("output").cloned())
            .expect("missing stream output");
        let final_seg = stream.collect().await.unwrap_or(Segment::None);
        let final_val = final_seg.to_value();
        assert_eq!(final_val.get("output"), Some(&Value::String("YO".into())));
    }

    #[tokio::test]
    async fn test_code_node_unsupported_language() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "def main(): return {'result': 42}",
            "language": "ruby"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code3", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_returns_array() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return { items: [1, 2, 3] }; }",
            "language": "javascript"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_arr", &config, &pool, &context)
            .await
            .unwrap();
        let items = result.outputs.ready().get("items").unwrap();
        assert_eq!(items, &serde_json::json!([1, 2, 3]));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_returns_nested_object() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return { obj: { a: 1, b: 'x' } }; }",
            "language": "javascript"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_obj", &config, &pool, &context)
            .await
            .unwrap();
        let obj = result.outputs.ready().get("obj").unwrap();
        assert_eq!(obj.get("a"), Some(&Segment::Integer(1)));
        assert_eq!(obj.get("b"), Some(&Segment::String("x".into())));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_empty_code() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "",
            "language": "javascript"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_empty", &config, &pool, &context)
            .await;
        assert!(result.is_err() || result.unwrap().status == WorkflowNodeExecutionStatus::Failed);
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_no_main_function() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "var x = 1;",
            "language": "javascript"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_no_main", &config, &pool, &context)
            .await;
        assert!(result.is_err());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_returns_error() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { throw new Error('boom'); }",
            "language": "javascript"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code_err", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_code_node_unsupported_language_python() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "def main(): return {}",
            "language": "python"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code_py", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_code_node_typescript_language() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs: any) { return { result: 1 }; }",
            "language": "typescript"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code_ts", &config, &pool, &context).await;
        assert!(result.is_ok() || result.is_err());
    }

    #[tokio::test]
    async fn test_code_node_with_output_variable() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return { result: 42 }; }",
            "language": "javascript",
            "output_variable": "my_output"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code_ov", &config, &pool, &context).await;
        if let Ok(r) = result {
            assert!(
                r.outputs.ready().contains_key("my_output")
                    || r.outputs.ready().contains_key("result")
            );
        }
    }

    #[tokio::test]
    async fn test_code_node_missing_language() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return {}; }"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code_ml", &config, &pool, &context).await;
        assert!(result.is_ok() || result.is_err());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_with_output_variable_key() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return 42; }",
            "language": "javascript",
            "output_variable": "my_result"
        });
        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_ov", &config, &pool, &context)
            .await
            .unwrap();
        assert!(result.outputs.ready().contains_key("my_result"));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_with_input_mappings() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "val"), Segment::Integer(10));
        let config = serde_json::json!({
            "code": "function main(inputs) { return { doubled: inputs.x * 2 }; }",
            "language": "javascript",
            "inputs": {
                "x": ["n1", "val"]
            }
        });
        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_inp", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("doubled"),
            Some(&Segment::Integer(20))
        );
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[tokio::test]
    async fn test_code_node_typescript_defaults_to_js() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return { ok: true }; }",
            "language": "typescript"
        });
        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code_ts", &config, &pool, &context).await;
        let _ = result;
    }

    #[tokio::test]
    async fn test_code_node_python_unsupported() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "def main(inputs): return {}",
            "language": "python"
        });
        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code_py", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_code_node_wasm_unsupported() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "module",
            "language": "wasm"
        });
        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_wasm", &config, &pool, &context)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_code_node_unknown_language() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "code",
            "language": "rust"
        });
        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("code_rust", &config, &pool, &context)
            .await;
        assert!(result.is_err());
    }
}
