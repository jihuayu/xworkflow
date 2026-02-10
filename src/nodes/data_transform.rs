use async_trait::async_trait;
use boa_engine::{Context, Source};
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, SegmentStream, StreamEvent, StreamStatus, VariablePool};
use crate::dsl::schema::{
    NodeRunResult,
    VariableMapping, WorkflowNodeExecutionStatus, WriteMode,
};
use crate::error::{ErrorCode, ErrorContext, NodeError};
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;
use crate::sandbox::js_builtins;
use crate::template::{render_jinja2_with_functions, render_template_async};

// ================================
// Template Transform
// ================================

pub struct TemplateTransformExecutor;

#[async_trait]
impl NodeExecutor for TemplateTransformExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let template = config
            .get("template")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Build variable map from config variable mappings
        let mut static_vars: HashMap<String, Value> = HashMap::new();
        let mut stream_vars: Vec<(String, SegmentStream)> = Vec::new();
        if let Some(vars_val) = config.get("variables") {
            if let Ok(mappings) = serde_json::from_value::<Vec<VariableMapping>>(vars_val.clone()) {
                for m in &mappings {
                    let val = variable_pool.get(&m.value_selector);
                    match val {
                        Segment::Stream(stream) => {
                            if stream.status_async().await == StreamStatus::Running {
                                stream_vars.push((m.variable.clone(), stream));
                            } else {
                                static_vars.insert(
                                    m.variable.clone(),
                                    stream.snapshot_segment_async().await.to_value(),
                                );
                            }
                        }
                        other => {
                            static_vars.insert(m.variable.clone(), other.to_value());
                        }
                    }
                }
            }
        }

        if stream_vars.is_empty() {
            #[cfg(feature = "plugin-system")]
            let tmpl_functions = _context.template_functions.as_ref().map(|f| f.as_ref());
            #[cfg(not(feature = "plugin-system"))]
            let tmpl_functions: Option<&std::collections::HashMap<String, ()>> = None;

            let rendered = render_jinja2_with_functions(template, &static_vars, tmpl_functions)
                .map_err(|e| NodeError::TemplateError(e))?;

            let mut outputs = HashMap::new();
            outputs.insert("output".to_string(), Value::String(rendered));

            return Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                outputs,
                edge_source_handle: "source".to_string(),
                ..Default::default()
            });
        }

        let template_str = template.to_string();
        let base_vars = static_vars.clone();
        let (output_stream, writer) = SegmentStream::channel();

        let tmpl_functions = {
            #[cfg(feature = "plugin-system")]
            {
                _context.template_functions.as_ref().map(|f| f.clone())
            }
            #[cfg(not(feature = "plugin-system"))]
            {
                let _none: Option<std::sync::Arc<std::collections::HashMap<String, ()>>> = None;
                _none
            }
        };

        tokio::spawn(async move {
            let mut accumulated: HashMap<String, String> = HashMap::new();
            let mut last_rendered = String::new();

            for (name, stream) in stream_vars {
                let mut reader = stream.reader();
                loop {
                    match reader.next().await {
                        Some(StreamEvent::Chunk(seg)) => {
                            let entry = accumulated.entry(name.clone()).or_default();
                            entry.push_str(&seg.to_display_string());
                            let funcs_ref = tmpl_functions.as_ref().map(|f| f.as_ref());
                            match render_jinja2_with_functions(&template_str, &{
                                let mut vars = base_vars.clone();
                                for (k, v) in &accumulated {
                                    vars.insert(k.clone(), Value::String(v.clone()));
                                }
                                vars
                            }, funcs_ref) {
                                Ok(rendered) => {
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
                                Err(e) => {
                                    writer.error(e).await;
                                    return;
                                }
                            }
                        }
                        Some(StreamEvent::End(final_seg)) => {
                            accumulated.insert(name.clone(), final_seg.to_display_string());
                            let funcs_ref = tmpl_functions.as_ref().map(|f| f.as_ref());
                            match render_jinja2_with_functions(&template_str, &{
                                let mut vars = base_vars.clone();
                                for (k, v) in &accumulated {
                                    vars.insert(k.clone(), Value::String(v.clone()));
                                }
                                vars
                            }, funcs_ref) {
                                Ok(rendered) => {
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
                                Err(e) => {
                                    writer.error(e).await;
                                    return;
                                }
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
        stream_outputs.insert("output".to_string(), output_stream);

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            stream_outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

// ================================
// Variable Aggregator (returns first non-null)
// ================================

pub struct VariableAggregatorExecutor;

#[async_trait]
impl NodeExecutor for VariableAggregatorExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let selectors: Vec<Vec<String>> = config
            .get("variables")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let mut result_val = Value::Null;
        for selector in &selectors {
            let val = variable_pool.get_resolved(selector).await;
            if !val.is_none() {
                result_val = val.to_value();
                break;
            }
        }

        let mut outputs = HashMap::new();
        outputs.insert("output".to_string(), result_val);

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

// Legacy variable-assigner (same behavior as variable-aggregator)
pub struct LegacyVariableAggregatorExecutor;

#[async_trait]
impl NodeExecutor for LegacyVariableAggregatorExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        VariableAggregatorExecutor.execute(node_id, config, variable_pool, context).await
    }
}

// ================================
// Variable Assigner
// ================================

pub struct VariableAssignerExecutor;

#[async_trait]
impl NodeExecutor for VariableAssignerExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        // Parse config
        let assigned_sel: Vec<String> = config
            .get("assigned_variable_selector")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let write_mode: WriteMode = config
            .get("write_mode")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(WriteMode::Overwrite);

        // Get source value
        let source_value = if let Some(input_sel) = config.get("input_variable_selector") {
            let sel: Vec<String> = serde_json::from_value(input_sel.clone()).unwrap_or_default();
            variable_pool.get_resolved_value(&sel).await
        } else if let Some(val) = config.get("value") {
            val.clone()
        } else {
            Value::Null
        };

        // Note: actual write to pool is done by the dispatcher after execution
        let mut outputs = HashMap::new();
        outputs.insert("output".to_string(), source_value.clone());
        outputs.insert("write_mode".to_string(), serde_json::to_value(&write_mode).unwrap_or(Value::Null));
        outputs.insert("assigned_variable_selector".to_string(), serde_json::json!(assigned_sel));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

// ================================
// HTTP Request Node
// ================================

pub struct HttpRequestExecutor;

#[async_trait]
impl NodeExecutor for HttpRequestExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let method = config.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
        let url_template = config.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let timeout = config.get("timeout").and_then(|v| v.as_u64()).unwrap_or(10);
        let fail_on_error_status = config
            .get("fail_on_error_status")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Substitute variables in URL
        let url = render_template_async(url_template, variable_pool).await;

        // Build headers
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(h_arr) = config.get("headers").and_then(|v| v.as_array()) {
            for h in h_arr {
                let key = h.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let val = h.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let val = render_template_async(val, variable_pool).await;
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                    reqwest::header::HeaderValue::from_str(&val),
                ) {
                    headers.insert(name, value);
                }
            }
        }

        // Handle authorization
        if let Some(auth) = config.get("authorization") {
            match auth.get("type").and_then(|v| v.as_str()).unwrap_or("no_auth") {
                "bearer_token" => {
                    if let Some(token) = auth.get("token").and_then(|v| v.as_str()) {
                        if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token)) {
                            headers.insert(reqwest::header::AUTHORIZATION, val);
                        }
                    }
                }
                "basic_auth" => {
                    let user = auth.get("username").and_then(|v| v.as_str()).unwrap_or("");
                    let pass = auth.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", user, pass));
                    if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Basic {}", encoded)) {
                        headers.insert(reqwest::header::AUTHORIZATION, val);
                    }
                }
                _ => {}
            }
        }

        // Send request
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout))
            .build()
            .map_err(|e| NodeError::HttpError(e.to_string()))?;

        let req_builder = match method.to_uppercase().as_str() {
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            "PATCH" => client.patch(&url),
            "HEAD" => client.head(&url),
            _ => client.get(&url),
        };

        // Add body
        let req_builder = if let Some(body) = config.get("body") {
            match body.get("type").and_then(|v| v.as_str()).unwrap_or("none") {
                "raw_text" => {
                    let data = body.get("data").and_then(|v| v.as_str()).unwrap_or("");
                    let data = render_template_async(data, variable_pool).await;
                    req_builder.body(data)
                }
                "json" => {
                    let data = body.get("data").and_then(|v| v.as_str()).unwrap_or("{}");
                    let data = render_template_async(data, variable_pool).await;
                    req_builder
                        .header("Content-Type", "application/json")
                        .body(data)
                }
                _ => req_builder,
            }
        } else {
            req_builder
        };

        let resp = req_builder
            .headers(headers)
            .send()
            .await
            .map_err(|e| NodeError::HttpError(e.to_string()))?;

        let status_code = resp.status().as_u16();
        let headers_snapshot = resp.headers().clone();
        let resp_headers = format!("{:?}", headers_snapshot);
        let resp_body = resp
            .text()
            .await
            .map_err(|e| NodeError::HttpError(e.to_string()))?;

        if fail_on_error_status && status_code >= 400 {
            let body_preview: String = resp_body.chars().take(512).collect();
            let error_msg = format!("HTTP {} {}", status_code, body_preview);
            let context = match status_code {
                401 | 403 => ErrorContext::non_retryable(
                    ErrorCode::HttpClientError,
                    error_msg.clone(),
                )
                .with_http_status(status_code),
                429 => {
                    let retry_after = headers_snapshot
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok());
                    let mut ctx = ErrorContext::retryable(
                        ErrorCode::HttpClientError,
                        error_msg.clone(),
                    )
                    .with_http_status(status_code);
                    if let Some(ra) = retry_after {
                        ctx = ctx.with_retry_after(ra);
                    }
                    ctx
                }
                400 | 404 | 405 | 422 => ErrorContext::non_retryable(
                    ErrorCode::HttpClientError,
                    error_msg.clone(),
                )
                .with_http_status(status_code),
                500..=599 => ErrorContext::retryable(
                    ErrorCode::HttpServerError,
                    error_msg.clone(),
                )
                .with_http_status(status_code),
                _ => ErrorContext::non_retryable(
                    ErrorCode::HttpClientError,
                    error_msg.clone(),
                )
                .with_http_status(status_code),
            };

            return Err(NodeError::HttpError(error_msg).with_context(context));
        }

        let mut outputs = HashMap::new();
        outputs.insert("status_code".to_string(), serde_json::json!(status_code));
        outputs.insert("body".to_string(), Value::String(resp_body));
        outputs.insert("headers".to_string(), Value::String(resp_headers));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

// ================================
// Code Node (sandbox-backed execution)
// ================================

#[derive(Debug)]
struct CallbackInvoke {
    var_name: String,
    callback: String,
    arg: Option<Value>,
    resp: oneshot::Sender<Result<Option<Value>, String>>,
}

#[derive(Debug)]
enum RuntimeCommand {
    Invoke(CallbackInvoke),
    Shutdown,
}

#[derive(Clone, Debug)]
struct JsStreamRuntime {
    tx: mpsc::Sender<RuntimeCommand>,
}

impl JsStreamRuntime {
    async fn invoke(
        &self,
        var_name: String,
        callback: &str,
        arg: Option<Value>,
    ) -> Result<Option<Value>, String> {
        let (resp_tx, resp_rx) = oneshot::channel();
        let cmd = RuntimeCommand::Invoke(CallbackInvoke {
            var_name,
            callback: callback.to_string(),
            arg,
            resp: resp_tx,
        });
        self.tx.send(cmd).await.map_err(|_| "runtime closed".to_string())?;
        resp_rx.await.map_err(|_| "runtime dropped".to_string())?
    }

    async fn shutdown(&self) {
        let _ = self.tx.send(RuntimeCommand::Shutdown).await;
    }
}

fn escape_js_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('\'', "\\'")
}

fn eval_js_to_string(context: &mut Context, code: &str) -> Result<String, String> {
    let result = context
        .eval(Source::from_bytes(code))
        .map_err(|e| format!("JS eval error: {}", e))?;
    let s = result
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .ok_or_else(|| "JS result is not string".to_string())?;
    Ok(s)
}

fn parse_json_result(result_str: &str) -> Result<Option<Value>, String> {
    if result_str == "__undefined__" {
        return Ok(None);
    }
    let val: Value = serde_json::from_str(result_str)
        .map_err(|e| format!("Failed to parse JSON: {}", e))?;
    if val.is_null() {
        return Ok(None);
    }
    Ok(Some(val))
}

async fn spawn_js_stream_runtime(
    code: String,
    inputs: Value,
    stream_vars: Vec<String>,
) -> Result<(Value, bool, JsStreamRuntime), NodeError> {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<RuntimeCommand>(64);
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(Value, bool), String>>();

    tokio::task::spawn_blocking(move || {
        let mut context = Context::default();
        if let Err(e) = js_builtins::register_all(&mut context) {
            let _ = ready_tx.send(Err(format!("Failed to register builtins: {}", e)));
            return;
        }

        if let Err(e) = context.eval(Source::from_bytes(&code)) {
            let _ = ready_tx.send(Err(format!("JS code eval error: {}", e)));
            return;
        }

        let inputs_json = serde_json::to_string(&inputs).unwrap_or("{}".into());
        let inputs_json_escaped = escape_js_string(&inputs_json);

        let mut stream_setup = String::new();
        stream_setup.push_str("var __inputs = JSON.parse('");
        stream_setup.push_str(&inputs_json_escaped);
        stream_setup.push_str("');\n");
        stream_setup.push_str("globalThis.__stream_callbacks__ = globalThis.__stream_callbacks__ || {};\n");

        for var_name in &stream_vars {
            let var_escaped = escape_js_string(var_name);
            stream_setup.push_str(&format!(
                "globalThis.__stream_callbacks__['{}'] = {{ on_chunk: null, on_end: null, on_error: null }};\n",
                var_escaped
            ));
            stream_setup.push_str(&format!(
                "__inputs['{}'] = {{\n",
                var_escaped
            ));
            stream_setup.push_str(&format!(
                "  on_chunk: function(fn) {{ globalThis.__stream_callbacks__['{}'].on_chunk = fn; return this; }},\n",
                var_escaped
            ));
            stream_setup.push_str(&format!(
                "  on_end: function(fn) {{ globalThis.__stream_callbacks__['{}'].on_end = fn; return this; }},\n",
                var_escaped
            ));
            stream_setup.push_str(&format!(
                "  on_error: function(fn) {{ globalThis.__stream_callbacks__['{}'].on_error = fn; return this; }}\n",
                var_escaped
            ));
            stream_setup.push_str("};\n");
        }
        stream_setup.push_str("globalThis.__stream_initial_output__ = main(__inputs);\n");
        stream_setup.push_str("globalThis.__stream_has_callbacks__ = false;\n");
        for var_name in &stream_vars {
            let var_escaped = escape_js_string(var_name);
            stream_setup.push_str(&format!(
                "if (typeof globalThis.__stream_callbacks__['{}'].on_chunk === 'function') {{ globalThis.__stream_has_callbacks__ = true; }}\n",
                var_escaped
            ));
        }

        if let Err(e) = context.eval(Source::from_bytes(&stream_setup)) {
            let _ = ready_tx.send(Err(format!("JS stream setup error: {}", e)));
            return;
        }

        let output_json = match eval_js_to_string(
            &mut context,
            "(function(){ var v = globalThis.__stream_initial_output__; if (v === undefined) return '__undefined__'; try { return JSON.stringify(v); } catch (e) { return '__undefined__'; } })()",
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        let initial_output = match parse_json_result(&output_json) {
            Ok(Some(v)) => v,
            Ok(None) => Value::Null,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };

        let has_callbacks = match eval_js_to_string(
            &mut context,
            "(function(){ return globalThis.__stream_has_callbacks__ ? 'true' : 'false'; })()",
        ) {
            Ok(v) => v == "true",
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };

        let _ = ready_tx.send(Ok((initial_output, has_callbacks)));

        loop {
            match cmd_rx.blocking_recv() {
                Some(RuntimeCommand::Invoke(inv)) => {
                    let arg_json = inv
                        .arg
                        .map(|v| serde_json::to_string(&v).unwrap_or("null".into()))
                        .unwrap_or_else(|| "null".to_string());
                    let arg_escaped = escape_js_string(&arg_json);
                    let var_escaped = escape_js_string(&inv.var_name);
                    let cb_escaped = escape_js_string(&inv.callback);
                    let js = format!(
                        "(function() {{ var cb = globalThis.__stream_callbacks__ && globalThis.__stream_callbacks__['{}'] && globalThis.__stream_callbacks__['{}']['{}']; if (typeof cb !== 'function') return '__undefined__'; var arg = JSON.parse('{}'); var res = cb(arg); if (res === undefined) return '__undefined__'; try {{ return JSON.stringify(res); }} catch (e) {{ return '__undefined__'; }} }})()",
                        var_escaped, var_escaped, cb_escaped, arg_escaped
                    );
                    let result = match eval_js_to_string(&mut context, &js) {
                        Ok(s) => parse_json_result(&s),
                        Err(e) => Err(e),
                    };
                    let _ = inv.resp.send(result);
                }
                Some(RuntimeCommand::Shutdown) | None => break,
            }
        }
    });

    let (initial_output, has_callbacks) = ready_rx
        .await
        .map_err(|_| NodeError::ExecutionError("JS runtime setup failed".into()))?
        .map_err(NodeError::ExecutionError)?;

    Ok((initial_output, has_callbacks, JsStreamRuntime { tx: cmd_tx }))
}

pub struct CodeNodeExecutor {
    sandbox_manager: std::sync::Arc<crate::sandbox::SandboxManager>,
}

impl CodeNodeExecutor {
    pub fn new() -> Self {
        let manager = crate::sandbox::SandboxManager::new(
            crate::sandbox::SandboxManagerConfig::default(),
        );
        Self {
            sandbox_manager: std::sync::Arc::new(manager),
        }
    }

    pub fn new_with_manager(manager: std::sync::Arc<crate::sandbox::SandboxManager>) -> Self {
        Self { sandbox_manager: manager }
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
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
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
                            if stream.status_async().await == StreamStatus::Running {
                                inputs_map.insert(m.variable.clone(), Value::Null);
                                push_stream(m.variable.clone(), stream);
                            } else {
                                inputs_map.insert(
                                    m.variable.clone(),
                                    stream.snapshot_segment_async().await.to_value(),
                                );
                            }
                        }
                        other => {
                            inputs_map.insert(m.variable.clone(), other.to_value());
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
                                if stream.status_async().await == StreamStatus::Running {
                                    inputs_map.insert(var.clone(), Value::Null);
                                    push_stream(var.clone(), stream);
                                } else {
                                    inputs_map.insert(
                                        var.clone(),
                                        stream.snapshot_segment_async().await.to_value(),
                                    );
                                }
                            }
                            other => {
                                inputs_map.insert(var.clone(), other.to_value());
                            }
                        }
                    }
                }
            }
        }

        let inputs = Value::Object(inputs_map.clone());

        // Build execution config
        let timeout_secs = config
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);

        let exec_config = crate::sandbox::ExecutionConfig {
            timeout: std::time::Duration::from_secs(timeout_secs),
            ..crate::sandbox::ExecutionConfig::default()
        };

        // Execute via sandbox (or stream mode for JS)
        let has_running_streams = !stream_inputs.is_empty();
        if has_running_streams && language == crate::sandbox::CodeLanguage::JavaScript {
            self.sandbox_manager
                .validate(code, language)
                .await
                .map_err(|e| NodeError::SandboxError(e.to_string()))?;

            let stream_names = stream_inputs.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
            let (initial_output, has_callbacks, runtime) = spawn_js_stream_runtime(
                code.to_string(),
                inputs.clone(),
                stream_names,
            )
            .await?;

            if !has_callbacks {
                runtime.shutdown().await;
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
                let result = self
                    .sandbox_manager
                    .execute(request)
                    .await
                    .map_err(|e| {
                        let context = match &e {
                            crate::sandbox::SandboxError::ExecutionTimeout => ErrorContext::retryable(
                                ErrorCode::SandboxTimeout,
                                e.to_string(),
                            ),
                            crate::sandbox::SandboxError::MemoryLimitExceeded => ErrorContext::non_retryable(
                                ErrorCode::SandboxMemoryLimit,
                                e.to_string(),
                            ),
                            crate::sandbox::SandboxError::CompilationError(_) => ErrorContext::non_retryable(
                                ErrorCode::SandboxCompilationError,
                                e.to_string(),
                            ),
                            crate::sandbox::SandboxError::DangerousCode(_) => ErrorContext::non_retryable(
                                ErrorCode::SandboxDangerousCode,
                                e.to_string(),
                            ),
                            _ => ErrorContext::non_retryable(
                                ErrorCode::SandboxExecutionError,
                                e.to_string(),
                            ),
                        };
                        NodeError::SandboxError(e.to_string()).with_context(context)
                    })?;

                if !result.success {
                    let message = result
                        .error
                        .unwrap_or_else(|| "Unknown sandbox error".to_string());
                    let context = ErrorContext::non_retryable(ErrorCode::SandboxExecutionError, &message);
                    return Err(NodeError::ExecutionError(message).with_context(context));
                }

                let mut outputs = HashMap::new();
                if let Some(output_key) = config.get("output_variable").and_then(|v| v.as_str()) {
                    outputs.insert(output_key.to_string(), result.output);
                } else if let Value::Object(obj) = &result.output {
                    for (k, v) in obj {
                        outputs.insert(k.clone(), v.clone());
                    }
                } else {
                    outputs.insert("result".to_string(), result.output);
                }

                return Ok(NodeRunResult {
                    status: WorkflowNodeExecutionStatus::Succeeded,
                    inputs: resolved_inputs.into_iter().map(|(k, v)| (k, v)).collect(),
                    outputs,
                    edge_source_handle: "source".to_string(),
                    ..Default::default()
                });
            }

            let mut outputs = HashMap::new();
            if let Some(output_key) = config.get("output_variable").and_then(|v| v.as_str()) {
                outputs.insert(output_key.to_string(), initial_output.clone());
            } else if let Value::Object(obj) = &initial_output {
                for (k, v) in obj {
                    outputs.insert(k.clone(), v.clone());
                }
            } else {
                outputs.insert("result".to_string(), initial_output.clone());
            }

            let (output_stream, writer) = SegmentStream::channel();
            let runtime_clone = runtime.clone();
            tokio::spawn(async move {
                let mut last_result: Option<Segment> = None;
                for (name, stream) in stream_inputs {
                    let mut reader = stream.reader();
                    loop {
                        match reader.next().await {
                            Some(StreamEvent::Chunk(seg)) => {
                                match runtime_clone
                                    .invoke(name.clone(), "on_chunk", Some(seg.to_value()))
                                    .await
                                {
                                    Ok(Some(val)) => {
                                        let out_seg = Segment::from_value(&val);
                                        writer.send(out_seg.clone()).await;
                                        last_result = Some(out_seg);
                                    }
                                    Ok(None) => {}
                                    Err(err) => {
                                        writer.error(err).await;
                                        runtime_clone.shutdown().await;
                                        return;
                                    }
                                }
                            }
                            Some(StreamEvent::End(final_seg)) => {
                                match runtime_clone
                                    .invoke(name.clone(), "on_end", Some(final_seg.to_value()))
                                    .await
                                {
                                    Ok(Some(val)) => {
                                        let out_seg = Segment::from_value(&val);
                                        writer.send(out_seg.clone()).await;
                                        last_result = Some(out_seg);
                                    }
                                    Ok(None) => {}
                                    Err(err) => {
                                        writer.error(err).await;
                                        runtime_clone.shutdown().await;
                                        return;
                                    }
                                }
                                break;
                            }
                            Some(StreamEvent::Error(err)) => {
                                match runtime_clone
                                    .invoke(name.clone(), "on_error", Some(Value::String(err.clone())))
                                    .await
                                {
                                    Ok(Some(val)) => {
                                        let out_seg = Segment::from_value(&val);
                                        writer.send(out_seg.clone()).await;
                                        last_result = Some(out_seg);
                                    }
                                    Ok(None) => {
                                        writer.error(err).await;
                                        runtime_clone.shutdown().await;
                                        return;
                                    }
                                    Err(e) => {
                                        writer.error(e).await;
                                        runtime_clone.shutdown().await;
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
                runtime_clone.shutdown().await;
            });

            let mut stream_outputs = HashMap::new();
            stream_outputs.insert("output".to_string(), output_stream);

            return Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                inputs: inputs_map.into_iter().map(|(k, v)| (k, v)).collect(),
                outputs,
                stream_outputs,
                edge_source_handle: "source".to_string(),
                ..Default::default()
            });
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
            let result = self
                .sandbox_manager
                .execute(request)
                .await
                .map_err(|e| {
                    let context = match &e {
                        crate::sandbox::SandboxError::ExecutionTimeout => ErrorContext::retryable(
                            ErrorCode::SandboxTimeout,
                            e.to_string(),
                        ),
                        crate::sandbox::SandboxError::MemoryLimitExceeded => ErrorContext::non_retryable(
                            ErrorCode::SandboxMemoryLimit,
                            e.to_string(),
                        ),
                        crate::sandbox::SandboxError::CompilationError(_) => ErrorContext::non_retryable(
                            ErrorCode::SandboxCompilationError,
                            e.to_string(),
                        ),
                        crate::sandbox::SandboxError::DangerousCode(_) => ErrorContext::non_retryable(
                            ErrorCode::SandboxDangerousCode,
                            e.to_string(),
                        ),
                        _ => ErrorContext::non_retryable(
                            ErrorCode::SandboxExecutionError,
                            e.to_string(),
                        ),
                    };
                    NodeError::SandboxError(e.to_string()).with_context(context)
                })?;

            if !result.success {
                let message = result
                    .error
                    .unwrap_or_else(|| "Unknown sandbox error".to_string());
                let context = ErrorContext::non_retryable(ErrorCode::SandboxExecutionError, &message);
                return Err(NodeError::ExecutionError(message).with_context(context));
            }

            let mut outputs = HashMap::new();
            if let Some(output_key) = config.get("output_variable").and_then(|v| v.as_str()) {
                outputs.insert(output_key.to_string(), result.output);
            } else if let Value::Object(obj) = &result.output {
                for (k, v) in obj {
                    outputs.insert(k.clone(), v.clone());
                }
            } else {
                outputs.insert("result".to_string(), result.output);
            }

            return Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                inputs: resolved_inputs.into_iter().map(|(k, v)| (k, v)).collect(),
                outputs,
                edge_source_handle: "source".to_string(),
                ..Default::default()
            });
        }

        let request = crate::sandbox::SandboxRequest {
            code: code.to_string(),
            language,
            inputs: inputs.clone(),
            config: exec_config,
        };

        let result = self
            .sandbox_manager
            .execute(request)
            .await
            .map_err(|e| {
                let context = match &e {
                    crate::sandbox::SandboxError::ExecutionTimeout => ErrorContext::retryable(
                        ErrorCode::SandboxTimeout,
                        e.to_string(),
                    ),
                    crate::sandbox::SandboxError::MemoryLimitExceeded => ErrorContext::non_retryable(
                        ErrorCode::SandboxMemoryLimit,
                        e.to_string(),
                    ),
                    crate::sandbox::SandboxError::CompilationError(_) => ErrorContext::non_retryable(
                        ErrorCode::SandboxCompilationError,
                        e.to_string(),
                    ),
                    crate::sandbox::SandboxError::DangerousCode(_) => ErrorContext::non_retryable(
                        ErrorCode::SandboxDangerousCode,
                        e.to_string(),
                    ),
                    _ => ErrorContext::non_retryable(
                        ErrorCode::SandboxExecutionError,
                        e.to_string(),
                    ),
                };
                NodeError::SandboxError(e.to_string()).with_context(context)
            })?;

        if !result.success {
            let message = result
                .error
                .unwrap_or_else(|| "Unknown sandbox error".to_string());
            let context = ErrorContext::non_retryable(ErrorCode::SandboxExecutionError, &message);
            return Err(NodeError::ExecutionError(message).with_context(context));
        }

        // Convert output to HashMap
        let mut outputs = HashMap::new();
        if let Some(output_key) = config.get("output_variable").and_then(|v| v.as_str()) {
            outputs.insert(output_key.to_string(), result.output);
        } else if let Value::Object(obj) = &result.output {
            for (k, v) in obj {
                outputs.insert(k.clone(), v.clone());
            }
        } else {
            outputs.insert("result".to_string(), result.output);
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            inputs: inputs_map
                .into_iter()
                .map(|(k, v)| (k, v))
                .collect(),
            outputs,
            edge_source_handle: "source".to_string(),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::Segment;

    #[tokio::test]
    async fn test_template_transform() {
        let mut pool = VariablePool::new();
        pool.set(
            &["n1".to_string(), "name".to_string()],
            Segment::String("World".into()),
        );

        let config = serde_json::json!({
            "template": "Hello {{ name }}!",
            "variables": [{"variable": "name", "value_selector": ["n1", "name"]}]
        });

        let executor = TemplateTransformExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("tt1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("output"), Some(&Value::String("Hello World!".into())));
    }

    #[tokio::test]
    async fn test_variable_aggregator() {
        let mut pool = VariablePool::new();
        // First selector has no value, second has a value
        pool.set(
            &["n2".to_string(), "out".to_string()],
            Segment::String("found".into()),
        );

        let config = serde_json::json!({
            "variables": [["n1", "out"], ["n2", "out"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("agg1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("output"), Some(&Value::String("found".into())));
    }

    #[tokio::test]
    async fn test_variable_aggregator_all_null() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "variables": [["n1", "out"], ["n2", "out"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("agg1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("output"), Some(&Value::Null));
    }

    #[tokio::test]
    async fn test_variable_assigner() {
        let mut pool = VariablePool::new();
        pool.set(
            &["src".to_string(), "val".to_string()],
            Segment::String("data".into()),
        );

        let config = serde_json::json!({
            "assigned_variable_selector": ["target", "result"],
            "input_variable_selector": ["src", "val"],
            "write_mode": "overwrite"
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("va1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("output"), Some(&Value::String("data".into())));
    }

    #[tokio::test]
    async fn test_code_node_javascript() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "function main(inputs) { return { result: 42 }; }",
            "language": "javascript"
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("result"), Some(&Value::Number(serde_json::Number::from(42))));
    }

    #[tokio::test]
    async fn test_code_node_with_variables() {
        let mut pool = VariablePool::new();
        pool.set(
            &["src".to_string(), "val".to_string()],
            Segment::Float(10.0),
        );

        let config = serde_json::json!({
            "code": "function main(inputs) { return { doubled: inputs.x * 2 }; }",
            "language": "javascript",
            "variables": [{"variable": "x", "value_selector": ["src", "val"]}]
        });

        let executor = CodeNodeExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("code2", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("doubled"), Some(&Value::Number(serde_json::Number::from(20))));
    }

    #[tokio::test]
    async fn test_template_transform_with_stream() {
        let (stream, writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut pool = VariablePool::new();
        pool.set(
            &["n1".to_string(), "text".to_string()],
            Segment::Stream(stream),
        );

        tokio::spawn(async move {
            writer.send(Segment::String("World".into())).await;
            writer.end(Segment::String("World".into())).await;
        });

        let config = serde_json::json!({
            "template": "Hello {{ name }}!",
            "variables": [{"variable": "name", "value_selector": ["n1", "text"]}]
        });

        let executor = TemplateTransformExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("tt_stream", &config, &pool, &context).await.unwrap();
        let stream = result.stream_outputs.get("output").cloned().expect("missing stream output");
        let final_seg = stream.collect().await.unwrap_or(Segment::None);
        assert_eq!(final_seg.to_display_string(), "Hello World!");
    }

    #[tokio::test]
    async fn test_code_node_with_stream_input() {
        let (stream, writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut pool = VariablePool::new();
        pool.set(
            &["src".to_string(), "val".to_string()],
            Segment::Stream(stream),
        );

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
        let result = executor.execute("code_stream", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.get("result"), Some(&Value::String("hi!".into())));
    }

    #[tokio::test]
    async fn test_code_node_stream_on_chunk() {
        let (stream, writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut pool = VariablePool::new();
        pool.set(
            &["src".to_string(), "text".to_string()],
            Segment::Stream(stream),
        );

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
        let result = executor.execute("code_stream_cb", &config, &pool, &context).await.unwrap();

        let stream = result.stream_outputs.get("output").cloned().expect("missing stream output");
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
}
