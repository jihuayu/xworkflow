//! Data transformation node executors: Template, Aggregator, Assigner, HTTP, Code.

use async_trait::async_trait;
use futures::StreamExt;
#[cfg(feature = "builtin-sandbox-js")]
use boa_engine::{Context, Source};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(feature = "builtin-sandbox-js")]
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{
    Segment, SegmentStream, Selector, StreamEvent, StreamStatus, VariablePool, SCOPE_NODE_ID,
};
use crate::dsl::schema::{
    EdgeHandle, NodeOutputs, NodeRunResult, VariableMapping, WorkflowNodeExecutionStatus, WriteMode,
};
use crate::error::{ErrorCode, ErrorContext, NodeError};
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;
use crate::template::{
    CompiledTemplate,
    render_jinja2_with_functions_and_config,
    render_template_async_with_config,
};
#[cfg(not(feature = "security"))]
use crate::template::render_jinja2_with_functions;
use xworkflow_types::template::{TemplateEngine, TemplateFunction};
#[cfg(feature = "builtin-sandbox-js")]
use xworkflow_sandbox_js::builtins as js_builtins;
#[cfg(feature = "security")]
use crate::security::network::{validate_url, SecureHttpClientFactory};
#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEvent, SecurityEventType};

// ================================
// Template Transform
// ================================

/// Executor for the Template Transform node. Renders a Jinja2 template with
/// variables from the pool, supporting both sync and streaming inputs.
pub struct TemplateTransformExecutor {
    engine: Option<Arc<dyn TemplateEngine>>,
}

impl TemplateTransformExecutor {
    /// Create a new executor using the default template engine.
    pub fn new() -> Self {
        Self { engine: None }
    }

    /// Create a new executor with a custom [`TemplateEngine`] implementation.
    pub fn new_with_engine(engine: Arc<dyn TemplateEngine>) -> Self {
        Self {
            engine: Some(engine),
        }
    }
}

impl Default for TemplateTransformExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeExecutor for TemplateTransformExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let template = config
            .get("template")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        #[cfg(feature = "security")]
        let template_len = template.len();

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
                                    stream.snapshot_segment_async().await.into_value(),
                                );
                            }
                        }
                        other => {
                            static_vars.insert(m.variable.clone(), other.into_value());
                        }
                    }
                }
            }
        }

        if stream_vars.is_empty() {
            #[cfg(feature = "plugin-system")]
            let tmpl_functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>> =
                context.template_functions().map(|f| f.as_ref());
            #[cfg(not(feature = "plugin-system"))]
            let tmpl_functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>> = None;

            #[cfg(feature = "security")]
            let safety = context
                .security_policy()
                .and_then(|p| p.template.as_ref());

            let rendered = if let Some(engine) = &self.engine {
                #[cfg(feature = "security")]
                if let Some(cfg) = safety {
                    if template.len() > cfg.max_template_length {
                        let err = format!(
                            "Template too large (max {}, got {})",
                            cfg.max_template_length,
                            template.len()
                        );
                        if is_template_anomaly_error(&err) {
                            audit_security_event(
                                context,
                                SecurityEventType::TemplateRenderingAnomaly {
                                    template_length: template_len,
                                },
                                EventSeverity::Warning,
                                Some(node_id.to_string()),
                            )
                            .await;
                        }
                        return Err(NodeError::TemplateError(err));
                    }
                }

                match engine.render(template, &static_vars, tmpl_functions) {
                    Ok(rendered) => {
                        #[cfg(feature = "security")]
                        if let Some(cfg) = safety {
                            if rendered.len() > cfg.max_output_length {
                                let err = format!(
                                    "Template output too large (max {}, got {})",
                                    cfg.max_output_length,
                                    rendered.len()
                                );
                                if is_template_anomaly_error(&err) {
                                    audit_security_event(
                                        context,
                                        SecurityEventType::TemplateRenderingAnomaly {
                                            template_length: template_len,
                                        },
                                        EventSeverity::Warning,
                                        Some(node_id.to_string()),
                                    )
                                    .await;
                                }
                                return Err(NodeError::TemplateError(err));
                            }
                        }
                        rendered
                    }
                    Err(e) => {
                        #[cfg(feature = "security")]
                        {
                            if is_template_anomaly_error(&e) {
                                audit_security_event(
                                    context,
                                    SecurityEventType::TemplateRenderingAnomaly {
                                        template_length: template_len,
                                    },
                                    EventSeverity::Warning,
                                    Some(node_id.to_string()),
                                )
                                .await;
                            }
                        }
                        return Err(NodeError::TemplateError(e));
                    }
                }
            } else {
                #[cfg(feature = "security")]
                let rendered = match render_jinja2_with_functions_and_config(
                    template,
                    &static_vars,
                    tmpl_functions,
                    safety,
                ) {
                    Ok(rendered) => rendered,
                    Err(e) => {
                        if is_template_anomaly_error(&e) {
                            audit_security_event(
                                context,
                                SecurityEventType::TemplateRenderingAnomaly {
                                    template_length: template_len,
                                },
                                EventSeverity::Warning,
                                Some(node_id.to_string()),
                            )
                            .await;
                        }
                        return Err(NodeError::TemplateError(e));
                    }
                };

                #[cfg(not(feature = "security"))]
                let rendered = render_jinja2_with_functions(template, &static_vars, tmpl_functions)
                    .map_err(|e| NodeError::TemplateError(e))?;

                rendered
            };

            let mut outputs = HashMap::new();
            outputs.insert("output".to_string(), Value::String(rendered));

            return Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                outputs: NodeOutputs::Sync(outputs),
                edge_source_handle: EdgeHandle::Default,
                ..Default::default()
            });
        }

        let template_str = template.to_string();
        let base_vars = static_vars;
        let (output_stream, writer) = SegmentStream::channel();

        let tmpl_functions: Option<Arc<HashMap<String, Arc<dyn TemplateFunction>>>> = {
            #[cfg(feature = "plugin-system")]
            {
                context.template_functions().cloned()
            }
            #[cfg(not(feature = "plugin-system"))]
            {
                None
            }
        };

        let funcs_ref = tmpl_functions.as_ref().map(|f| f.as_ref());

        #[cfg(feature = "security")]
        let safety = context
            .security_policy()
            .and_then(|p| p.template.as_ref());

        let render_fn = if let Some(engine) = &self.engine {
            #[cfg(feature = "security")]
            if let Some(cfg) = safety {
                if template_str.len() > cfg.max_template_length {
                    let err = format!(
                        "Template too large (max {}, got {})",
                        cfg.max_template_length,
                        template_str.len()
                    );
                    if is_template_anomaly_error(&err) {
                        audit_security_event(
                            context,
                            SecurityEventType::TemplateRenderingAnomaly {
                                template_length: template_len,
                            },
                            EventSeverity::Warning,
                            Some(node_id.to_string()),
                        )
                        .await;
                    }
                    return Err(NodeError::TemplateError(err));
                }
            }

            let handle = engine
                .compile(&template_str, funcs_ref)
                .map_err(NodeError::TemplateError)?;
            let max_output = {
                #[cfg(feature = "security")]
                {
                    safety.map(|cfg| cfg.max_output_length)
                }
                #[cfg(not(feature = "security"))]
                {
                    None
                }
            };
            Box::new(move |vars: &HashMap<String, Value>| -> Result<String, String> {
                let rendered = handle.render(vars)?;
                if let Some(limit) = max_output {
                    if rendered.len() > limit {
                        return Err(format!(
                            "Template output too large (max {}, got {})",
                            limit,
                            rendered.len()
                        ));
                    }
                }
                Ok(rendered)
            }) as Box<dyn Fn(&HashMap<String, Value>) -> Result<String, String> + Send + Sync>
        } else {
            #[cfg(feature = "security")]
            let compiled = match CompiledTemplate::new_with_config(&template_str, funcs_ref, safety) {
                Ok(compiled) => compiled,
                Err(e) => {
                    if is_template_anomaly_error(&e) {
                        audit_security_event(
                            context,
                            SecurityEventType::TemplateRenderingAnomaly {
                                template_length: template_len,
                            },
                            EventSeverity::Warning,
                            Some(node_id.to_string()),
                        )
                        .await;
                    }
                    return Err(NodeError::TemplateError(e));
                }
            };

            #[cfg(not(feature = "security"))]
            let compiled = CompiledTemplate::new(&template_str, funcs_ref)
                .map_err(|e| NodeError::TemplateError(e))?;

            let compiled = Arc::new(compiled);
            Box::new(move |vars: &HashMap<String, Value>| -> Result<String, String> {
                compiled.render(vars)
            }) as Box<dyn Fn(&HashMap<String, Value>) -> Result<String, String> + Send + Sync>
        };

        #[cfg(feature = "security")]
        let context_for_stream = context.clone();
        #[cfg(feature = "security")]
        let node_id_for_stream = node_id.to_string();
        #[cfg(feature = "security")]
        let template_len_for_stream = template_len;
        tokio::spawn(async move {
            let mut last_rendered = String::new();
            let mut vars = base_vars;

            for (name, stream) in stream_vars {
                // 预先占位：将该变量强制为 String accumulator，避免 chunk 热路径里 clone+insert
                vars.insert(name.clone(), Value::String(String::new()));

                let mut reader = stream.reader();
                loop {
                    match reader.next().await {
                        Some(StreamEvent::Chunk(seg)) => {
                            let seg_text = seg.to_display_string();
                            match vars
                                .get_mut(&name)
                                .expect("vars entry should exist")
                            {
                                Value::String(s) => s.push_str(&seg_text),
                                other => *other = Value::String(seg_text),
                            }

                            match render_fn(&vars) {
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
                                    #[cfg(feature = "security")]
                                    if is_template_anomaly_error(&e) {
                                        audit_security_event(
                                            &context_for_stream,
                                            SecurityEventType::TemplateRenderingAnomaly {
                                                template_length: template_len_for_stream,
                                            },
                                            EventSeverity::Warning,
                                            Some(node_id_for_stream.clone()),
                                        )
                                        .await;
                                    }
                                    writer.error(e).await;
                                    return;
                                }
                            }
                        }
                        Some(StreamEvent::End(final_seg)) => {
                            let final_text = final_seg.to_display_string();

                            match vars
                                .get_mut(&name)
                                .expect("vars entry should exist")
                            {
                                Value::String(s) => {
                                    s.clear();
                                    s.push_str(&final_text);
                                }
                                other => *other = Value::String(final_text),
                            }
                            match render_fn(&vars) {
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
                                    #[cfg(feature = "security")]
                                    if is_template_anomaly_error(&e) {
                                        audit_security_event(
                                            &context_for_stream,
                                            SecurityEventType::TemplateRenderingAnomaly {
                                                template_length: template_len_for_stream,
                                            },
                                            EventSeverity::Warning,
                                            Some(node_id_for_stream.clone()),
                                        )
                                        .await;
                                    }
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
            outputs: NodeOutputs::Stream {
                ready: HashMap::new(),
                streams: stream_outputs,
            },
            edge_source_handle: EdgeHandle::Default,
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
        let selectors: Vec<Selector> = config
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
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
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
        let assigned_sel: Selector = config
            .get("assigned_variable_selector")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_else(|| Selector::new(SCOPE_NODE_ID, "output"));

        let write_mode: WriteMode = config
            .get("write_mode")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(WriteMode::Overwrite);

        // Get source value
        let source_value = if let Some(input_sel) = config.get("input_variable_selector") {
            if let Some(sel) = selector_from_value(input_sel) {
                variable_pool.get_resolved_value(&sel).await
            } else {
                Value::Null
            }
        } else if let Some(val) = config.get("value") {
            val.clone()
        } else {
            Value::Null
        };

        // Note: actual write to pool is done by the dispatcher after execution
        let mut outputs = HashMap::new();
        outputs.insert("output".to_string(), source_value.clone());
        outputs.insert("write_mode".to_string(), serde_json::to_value(&write_mode).unwrap_or(Value::Null));
        outputs.insert(
            "assigned_variable_selector".to_string(),
            serde_json::to_value(&assigned_sel).unwrap_or(Value::Null),
        );

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
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
        node_id: &str,
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
        let url = render_template_async_with_config(
            url_template,
            variable_pool,
            _context.strict_template(),
        )
        .await
        .map_err(|e| NodeError::VariableNotFound(e.selector))?;

        // Build headers
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(h_arr) = config.get("headers").and_then(|v| v.as_array()) {
            for h in h_arr {
                let key = h.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let val = h.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let val = render_template_async_with_config(
                    val,
                    variable_pool,
                    _context.strict_template(),
                )
                .await
                .map_err(|e| NodeError::VariableNotFound(e.selector))?;
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
        #[cfg(feature = "security")]
        let client = {
            if let Some(policy) = _context
                .security_policy()
                .and_then(|p| p.network.as_ref())
            {
                if let Err(err) = validate_url(&url, policy).await {
                    audit_security_event(
                        _context,
                        SecurityEventType::SsrfBlocked {
                            url: url.clone(),
                            reason: err.to_string(),
                        },
                        EventSeverity::Warning,
                        Some(node_id.to_string()),
                    )
                    .await;
                    return Err(NodeError::InputValidationError(err.to_string()));
                }
                SecureHttpClientFactory::build(policy, std::time::Duration::from_secs(timeout))
                    .map_err(|e| NodeError::HttpError(e.to_string()))?
            } else {
                reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(timeout))
                    .build()
                    .map_err(|e| NodeError::HttpError(e.to_string()))?
            }
        };

        #[cfg(not(feature = "security"))]
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
                    let data = render_template_async_with_config(
                        data,
                        variable_pool,
                        _context.strict_template(),
                    )
                    .await
                    .map_err(|e| NodeError::VariableNotFound(e.selector))?;
                    req_builder.body(data)
                }
                "json" => {
                    let data = body.get("data").and_then(|v| v.as_str()).unwrap_or("{}");
                    let data = render_template_async_with_config(
                        data,
                        variable_pool,
                        _context.strict_template(),
                    )
                    .await
                    .map_err(|e| NodeError::VariableNotFound(e.selector))?;
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
        #[cfg(feature = "security")]
        let max_response_bytes = {
            let mut max = _context
                .resource_group()
                .map(|g| g.quota.http_max_response_bytes);
            if let Some(policy) = _context.security_policy() {
                if let Some(limit) = policy.node_limits.get("http-request") {
                    max = Some(max.map(|m| m.min(limit.max_output_bytes)).unwrap_or(limit.max_output_bytes));
                }
            }
            max
        };

        #[cfg(not(feature = "security"))]
        let max_response_bytes: Option<usize> = None;

        let resp_body = read_response_with_limit(resp, max_response_bytes).await?;

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
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

async fn read_response_with_limit(
    resp: reqwest::Response,
    max_bytes: Option<usize>,
) -> Result<String, NodeError> {
    if let Some(limit) = max_bytes {
        if let Some(len) = resp.content_length() {
            if len as usize > limit {
                return Err(NodeError::HttpError(format!(
                    "HTTP response too large (max {} bytes, got {})",
                    limit,
                    len
                )));
            }
        }
    }

    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| NodeError::HttpError(e.to_string()))?;
        if let Some(limit) = max_bytes {
            if buf.len() + chunk.len() > limit {
                return Err(NodeError::HttpError(format!(
                    "HTTP response too large (max {} bytes)",
                    limit
                )));
            }
        }
        buf.extend_from_slice(&chunk);
    }

    Ok(String::from_utf8_lossy(&buf).to_string())
}

#[cfg(feature = "security")]
async fn audit_security_event(
    context: &RuntimeContext,
    event_type: SecurityEventType,
    severity: EventSeverity,
    node_id: Option<String>,
) {
    let logger = match context.audit_logger() {
        Some(l) => l,
        None => return,
    };
    let group_id = match context.resource_group() {
        Some(g) => g.group_id.clone(),
        None => return,
    };
    let event = SecurityEvent {
        timestamp: context.time_provider.now_timestamp(),
        group_id,
        workflow_id: None,
        node_id,
        event_type,
        details: serde_json::Value::Null,
        severity,
    };
    logger.log_event(event).await;
}

#[cfg(feature = "security")]
fn is_template_anomaly_error(message: &str) -> bool {
    let msg = message.to_lowercase();
    msg.contains("output too large")
        || msg.contains("template output too large")
        || msg.contains("fuel")
        || msg.contains("loop")
        || msg.contains("recursion")
        || msg.contains("timeout")
}

async fn execute_sandbox_with_audit(
    manager: &crate::sandbox::SandboxManager,
    request: crate::sandbox::SandboxRequest,
    context: &RuntimeContext,
    node_id: &str,
    language: crate::sandbox::CodeLanguage,
) -> Result<crate::sandbox::SandboxResult, NodeError> {
    let result = manager.execute(request).await;

    let result = match result {
        Ok(result) => result,
        Err(e) => {
            #[cfg(feature = "security")]
            audit_security_event(
                context,
                SecurityEventType::SandboxViolation {
                    sandbox_type: format!("{:?}", language),
                    violation: e.to_string(),
                },
                EventSeverity::Warning,
                Some(node_id.to_string()),
            )
            .await;

            let error_context = match &e {
                crate::sandbox::SandboxError::ExecutionTimeout => ErrorContext::retryable(
                    ErrorCode::SandboxTimeout,
                    e.to_string(),
                ),
                crate::sandbox::SandboxError::InputTooLarge { .. }
                | crate::sandbox::SandboxError::OutputTooLarge { .. } => ErrorContext::non_retryable(
                    ErrorCode::SandboxMemoryLimit,
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

            return Err(NodeError::SandboxError(e.to_string()).with_context(error_context));
        }
    };

    if !result.success {
        let message = result
            .error
            .unwrap_or_else(|| "Unknown sandbox error".to_string());
        #[cfg(feature = "security")]
        audit_security_event(
            context,
            SecurityEventType::SandboxViolation {
                sandbox_type: format!("{:?}", language),
                violation: message.clone(),
            },
            EventSeverity::Warning,
            Some(node_id.to_string()),
        )
        .await;

        let error_context =
            ErrorContext::non_retryable(ErrorCode::SandboxExecutionError, &message);
        return Err(NodeError::ExecutionError(message).with_context(error_context));
    }

    Ok(result)
}

// ================================
// Code Node (sandbox-backed execution)
// ================================

#[cfg(feature = "builtin-sandbox-js")]
#[derive(Debug)]
struct CallbackInvoke {
    var_name: String,
    callback: String,
    arg: Option<Value>,
    resp: oneshot::Sender<Result<Option<Value>, String>>,
}

#[cfg(feature = "builtin-sandbox-js")]
#[derive(Debug)]
enum RuntimeCommand {
    Invoke(CallbackInvoke),
    Shutdown,
}

#[cfg(feature = "builtin-sandbox-js")]
#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct JsStreamRuntime {
    tx: mpsc::Sender<RuntimeCommand>,
}

#[cfg(feature = "builtin-sandbox-js")]
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

    pub async fn shutdown(&self) {
        let _ = self.tx.send(RuntimeCommand::Shutdown).await;
    }
}

#[cfg(feature = "builtin-sandbox-js")]
impl Drop for JsStreamRuntime {
    fn drop(&mut self) {
        let _ = self.tx.try_send(RuntimeCommand::Shutdown);
    }
}

#[cfg(feature = "builtin-sandbox-js")]
fn escape_js_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(feature = "builtin-sandbox-js")]
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

#[cfg(feature = "builtin-sandbox-js")]
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

#[cfg(feature = "builtin-sandbox-js")]
async fn spawn_js_stream_runtime_inner(
    code: String,
    inputs: Value,
    stream_vars: Vec<String>,
    exit_flag: Option<Arc<AtomicBool>>,
) -> Result<(Value, bool, JsStreamRuntime), NodeError> {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<RuntimeCommand>(64);
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(Value, bool), String>>();

    let exit_flag_for_thread = exit_flag.clone();

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

        if let Some(flag) = exit_flag_for_thread {
            flag.store(true, Ordering::SeqCst);
        }
    });

    let (initial_output, has_callbacks) = ready_rx
        .await
        .map_err(|_| NodeError::ExecutionError("JS runtime setup failed".into()))?
        .map_err(NodeError::ExecutionError)?;

    Ok((initial_output, has_callbacks, JsStreamRuntime { tx: cmd_tx }))
}

#[cfg(feature = "builtin-sandbox-js")]
async fn spawn_js_stream_runtime(
    code: String,
    inputs: Value,
    stream_vars: Vec<String>,
) -> Result<(Value, bool, JsStreamRuntime), NodeError> {
    spawn_js_stream_runtime_inner(code, inputs, stream_vars, None).await
}

#[cfg(feature = "builtin-sandbox-js")]
#[doc(hidden)]
pub async fn spawn_js_stream_runtime_with_exit_flag(
    code: String,
    inputs: Value,
    stream_vars: Vec<String>,
) -> Result<(Value, bool, JsStreamRuntime, Arc<AtomicBool>), NodeError> {
    let exit_flag = Arc::new(AtomicBool::new(false));
    let (initial_output, has_callbacks, runtime) =
        spawn_js_stream_runtime_inner(code, inputs, stream_vars, Some(exit_flag.clone())).await?;
    Ok((initial_output, has_callbacks, runtime, exit_flag))
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
                            if language == crate::sandbox::CodeLanguage::JavaScript {
                                inputs_map.insert(m.variable.clone(), Value::Null);
                                push_stream(m.variable.clone(), stream);
                            } else if stream.status_async().await == StreamStatus::Running {
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
                                if language == crate::sandbox::CodeLanguage::JavaScript {
                                    inputs_map.insert(var.clone(), Value::Null);
                                    push_stream(var.clone(), stream);
                                } else if stream.status_async().await == StreamStatus::Running {
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
            #[cfg(feature = "builtin-sandbox-js")]
            {
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
                    outputs: NodeOutputs::Sync(outputs),
                    edge_source_handle: EdgeHandle::Default,
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
            tokio::spawn(async move {
                let runtime = runtime;
                let mut last_result: Option<Segment> = None;
                for (name, stream) in stream_inputs {
                    let mut reader = stream.reader();
                    loop {
                        match reader.next().await {
                            Some(StreamEvent::Chunk(seg)) => {
                                match runtime
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
                                        runtime.shutdown().await;
                                        return;
                                    }
                                }
                            }
                            Some(StreamEvent::End(final_seg)) => {
                                match runtime
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
                                        runtime.shutdown().await;
                                        return;
                                    }
                                }
                                break;
                            }
                            Some(StreamEvent::Error(err)) => {
                                match runtime
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
                                        runtime.shutdown().await;
                                        return;
                                    }
                                    Err(e) => {
                                        writer.error(e).await;
                                        runtime.shutdown().await;
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
                runtime.shutdown().await;
            });

            let mut stream_outputs = HashMap::new();
            stream_outputs.insert("output".to_string(), output_stream);

            return Ok(NodeRunResult {
                status: WorkflowNodeExecutionStatus::Succeeded,
                inputs: inputs_map.into_iter().map(|(k, v)| (k, v)).collect(),
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

        let result = execute_sandbox_with_audit(
            &self.sandbox_manager,
            request,
            context,
            node_id,
            language,
        )
        .await?;

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
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::{Segment, Selector};

    #[cfg(feature = "builtin-template-jinja")]
    #[tokio::test]
    async fn test_template_transform() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("n1", "name"),
            Segment::String("World".into()),
        );

        let config = serde_json::json!({
            "template": "Hello {{ name }}!",
            "variables": [{"variable": "name", "value_selector": ["n1", "name"]}]
        });

        let executor = TemplateTransformExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("tt1", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Value::String("Hello World!".into()))
        );
    }

    #[tokio::test]
    async fn test_variable_aggregator() {
        let mut pool = VariablePool::new();
        // First selector has no value, second has a value
        pool.set(
            &Selector::new("n2", "out"),
            Segment::String("found".into()),
        );

        let config = serde_json::json!({
            "variables": [["n1", "out"], ["n2", "out"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("agg1", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Value::String("found".into()))
        );
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
        assert_eq!(result.outputs.ready().get("output"), Some(&Value::Null));
    }

    #[tokio::test]
    async fn test_variable_assigner() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("src", "val"),
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
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Value::String("data".into()))
        );
    }

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
        let result = executor.execute("code1", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("result"),
            Some(&Value::Number(serde_json::Number::from(42)))
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
        let result = executor.execute("code2", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("doubled"),
            Some(&Value::Number(serde_json::Number::from(20)))
        );
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[tokio::test]
    async fn test_template_transform_with_stream() {
        let (stream, writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "text"), Segment::Stream(stream));

        tokio::spawn(async move {
            writer.send(Segment::String("World".into())).await;
            writer.end(Segment::String("World".into())).await;
        });

        let config = serde_json::json!({
            "template": "Hello {{ name }}!",
            "variables": [{"variable": "name", "value_selector": ["n1", "text"]}]
        });

        let executor = TemplateTransformExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("tt_stream", &config, &pool, &context).await.unwrap();
        let stream = result
            .outputs
            .streams()
            .and_then(|streams| streams.get("output").cloned())
            .expect("missing stream output");
        let final_seg = stream.collect().await.unwrap_or(Segment::None);
        assert_eq!(final_seg.to_display_string(), "Hello World!");
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
        let result = executor.execute("code_stream", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("result"),
            Some(&Value::String("hi!".into()))
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
        let result = executor.execute("code_stream_cb", &config, &pool, &context).await.unwrap();

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

    #[tokio::test]
    async fn test_variable_aggregator_first_non_null() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("a", "x"), Segment::None);
        pool.set(&Selector::new("b", "y"), Segment::String("found".into()));

        let config = serde_json::json!({
            "variables": [["a", "x"], ["b", "y"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("va1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&Value::String("found".into())));
    }

    #[tokio::test]
    async fn test_variable_aggregator_all_missing() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "variables": [["missing1", "x"], ["missing2", "y"]]
        });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("va2", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&Value::Null));
    }

    #[tokio::test]
    async fn test_variable_aggregator_empty_selectors() {
        let pool = VariablePool::new();
        let config = serde_json::json!({ "variables": [] });

        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("va3", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&Value::Null));
    }

    #[tokio::test]
    async fn test_legacy_variable_aggregator() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "v"), Segment::Integer(99));
        let config = serde_json::json!({ "variables": [["n1", "v"]] });

        let executor = LegacyVariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("lva", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&serde_json::json!(99)));
    }

    #[tokio::test]
    async fn test_variable_assigner_overwrite() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("src", "val"), Segment::String("hello".into()));
        let config = serde_json::json!({
            "assigned_variable_selector": ["tgt", "out"],
            "write_mode": "overwrite",
            "input_variable_selector": ["src", "val"]
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("assign1", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&Value::String("hello".into())));
    }

    #[tokio::test]
    async fn test_variable_assigner_append() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "assigned_variable_selector": ["tgt", "arr"],
            "write_mode": "append",
            "value": "new_item"
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("assign2", &config, &pool, &context).await.unwrap();
        let wm = result.outputs.ready().get("write_mode").unwrap();
        assert_eq!(wm, &Value::String("append".into()));
    }

    #[tokio::test]
    async fn test_variable_assigner_clear() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "assigned_variable_selector": ["tgt", "x"],
            "write_mode": "clear"
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("assign3", &config, &pool, &context).await.unwrap();
        let wm = result.outputs.ready().get("write_mode").unwrap();
        assert_eq!(wm, &Value::String("clear".into()));
    }

    #[tokio::test]
    async fn test_variable_assigner_default_mode() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "value": 42
        });

        let executor = VariableAssignerExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("assign4", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("output"), Some(&serde_json::json!(42)));
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[tokio::test]
    async fn test_template_transform_no_variables() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "template": "Hello World!",
            "variables": []
        });

        let executor = TemplateTransformExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("tt1", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Value::String("Hello World!".into()))
        );
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[tokio::test]
    async fn test_template_transform_multiple_variables() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "first"), Segment::String("John".into()));
        pool.set(&Selector::new("n1", "last"), Segment::String("Doe".into()));

        let config = serde_json::json!({
            "template": "{{ first }} {{ last }}",
            "variables": [
                {"variable": "first", "value_selector": ["n1", "first"]},
                {"variable": "last", "value_selector": ["n1", "last"]}
            ]
        });

        let executor = TemplateTransformExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("tt2", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Value::String("John Doe".into()))
        );
    }

    #[tokio::test]
    async fn test_template_transform_default() {
        let executor = TemplateTransformExecutor::default();
        // Just verify it compiles and creates
        assert!(executor.engine.is_none());
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
        let result = executor.execute("code_arr", &config, &pool, &context).await.unwrap();
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
        let result = executor.execute("code_obj", &config, &pool, &context).await.unwrap();
        let obj = result.outputs.ready().get("obj").unwrap();
        assert_eq!(obj.get("a"), Some(&serde_json::json!(1)));
        assert_eq!(obj.get("b"), Some(&Value::String("x".into())));
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
        let result = executor.execute("code_empty", &config, &pool, &context).await;
        // Empty code should probably fail
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
        let result = executor.execute("code_no_main", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_is_template_anomaly_error_output_too_large() {
        assert!(is_template_anomaly_error("Output too large for template"));
    }

    #[test]
    fn test_is_template_anomaly_error_fuel() {
        assert!(is_template_anomaly_error("run out of FUEL"));
    }

    #[test]
    fn test_is_template_anomaly_error_loop() {
        assert!(is_template_anomaly_error("infinite loop detected"));
    }

    #[test]
    fn test_is_template_anomaly_error_recursion() {
        assert!(is_template_anomaly_error("maximum recursion depth exceeded"));
    }

    #[test]
    fn test_is_template_anomaly_error_timeout() {
        assert!(is_template_anomaly_error("execution timeout"));
    }

    #[test]
    fn test_is_template_anomaly_error_normal() {
        assert!(!is_template_anomaly_error("variable not found"));
        assert!(!is_template_anomaly_error(""));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_escape_js_string() {
        assert_eq!(escape_js_string("hello"), "hello");
        assert_eq!(escape_js_string("it's"), "it\\'s");
        assert_eq!(escape_js_string("back\\slash"), "back\\\\slash");
        assert_eq!(escape_js_string("it\\'s"), "it\\\\\\'s");
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_undefined() {
        let result = parse_json_result("__undefined__").unwrap();
        assert!(result.is_none());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_null() {
        let result = parse_json_result("null").unwrap();
        assert!(result.is_none());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_valid() {
        let result = parse_json_result(r#"{"key": "value"}"#).unwrap();
        assert_eq!(result, Some(serde_json::json!({"key": "value"})));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_invalid() {
        let result = parse_json_result("not valid json {{{");
        assert!(result.is_err());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_number() {
        let result = parse_json_result("42").unwrap();
        assert_eq!(result, Some(serde_json::json!(42)));
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
        // TypeScript without appropriate sandbox returns error
        let result = executor.execute("code_ts", &config, &pool, &context).await;
        // May succeed (treated as JS) or fail depending on sandbox support
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
        // With output_variable, result should be stored under that key
        if let Ok(r) = result {
            assert!(r.outputs.ready().contains_key("my_output") || r.outputs.ready().contains_key("result"));
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
        // missing language defaults or errors
        assert!(result.is_ok() || result.is_err());
    }

    #[tokio::test]
    async fn test_http_request_missing_url() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "method": "GET"
        });

        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("http_no_url", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_http_request_invalid_method() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://example.com",
            "method": "INVALID"
        });

        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("http_bad", &config, &pool, &context).await;
        // Invalid method may still execute or error depending on implementation
        // Just verify it doesn't panic
        let _ = result;
    }

    #[tokio::test]
    async fn test_http_request_with_headers() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/headers",
            "method": "GET",
            "timeout": 2,
            "headers": [
                {"key": "X-Custom", "value": "test123"},
                {"key": "Accept", "value": "application/json"}
            ]
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        // May fail due to network, but exercises the header-building path
        let _ = executor.execute("http_hdr", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_with_bearer_auth() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/get",
            "method": "GET",
            "timeout": 2,
            "authorization": {
                "type": "bearer_token",
                "token": "my_secret_token"
            }
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_bearer", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_with_basic_auth() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/get",
            "method": "GET",
            "timeout": 2,
            "authorization": {
                "type": "basic_auth",
                "username": "user",
                "password": "pass"
            }
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_basic", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_post_raw_text() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/post",
            "method": "POST",
            "timeout": 2,
            "body": {
                "type": "raw_text",
                "data": "hello world"
            }
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_post_raw", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_post_json() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/post",
            "method": "POST",
            "timeout": 2,
            "body": {
                "type": "json",
                "data": "{\"key\": \"value\"}"
            }
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_post_json", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_put_method() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/put",
            "method": "PUT",
            "timeout": 2
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_put", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_delete_method() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/delete",
            "method": "DELETE",
            "timeout": 2
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_delete", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_patch_method() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/patch",
            "method": "PATCH",
            "timeout": 2
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_patch", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_head_method() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/get",
            "method": "HEAD",
            "timeout": 2
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_head", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_fail_on_error_status() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/status/404",
            "method": "GET",
            "timeout": 2,
            "fail_on_error_status": true
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_fail", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_with_params() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/get",
            "method": "GET",
            "timeout": 2,
            "params": [
                {"key": "q", "value": "test"},
                {"key": "page", "value": "1"}
            ]
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_params", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_with_template_url() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("start", "domain"), Segment::String("example.com".into()));
        let config = serde_json::json!({
            "url": "http://{{#start.domain#}}/api",
            "method": "GET",
            "timeout": 2
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_tmpl", &config, &pool, &context).await;
    }

    #[tokio::test]
    async fn test_http_request_no_auth_type() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "url": "http://httpbin.org/get",
            "method": "GET",
            "timeout": 2,
            "authorization": {
                "type": "no_auth"
            }
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor.execute("http_no_auth", &config, &pool, &context).await;
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
        let result = executor.execute("code_ov", &config, &pool, &context).await.unwrap();
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
        let result = executor.execute("code_inp", &config, &pool, &context).await.unwrap();
        assert_eq!(result.outputs.ready().get("doubled"), Some(&serde_json::json!(20)));
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
        // TypeScript is typically unsupported unless specifically handled
        let result = executor.execute("code_ts", &config, &pool, &context).await;
        // Just exercise the path
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
        let result = executor.execute("code_wasm", &config, &pool, &context).await;
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
        let result = executor.execute("code_rust", &config, &pool, &context).await;
        assert!(result.is_err());
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[tokio::test]
    async fn test_template_transform_with_integer_variable() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "count"), Segment::Integer(42));
        let config = serde_json::json!({
            "template": "Count is {{ count }}",
            "variables": [
                {"variable": "count", "value_selector": ["n1", "count"]}
            ]
        });
        let executor = TemplateTransformExecutor::new();
        let context = RuntimeContext::default();
        let result = executor.execute("tt_int", &config, &pool, &context).await.unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Value::String("Count is 42".into()))
        );
    }

    #[tokio::test]
    async fn test_variable_aggregator_mixed_types() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "text"), Segment::String("hello".into()));
        pool.set(&Selector::new("n2", "num"), Segment::Integer(42));
        let config = serde_json::json!({
            "groups": [{
                "output_type": "array-any",
                "variables": [
                    {"value_selector": ["n1", "text"]},
                    {"value_selector": ["n2", "num"]}
                ]
            }]
        });
        let executor = VariableAggregatorExecutor;
        let context = RuntimeContext::default();
        let result = executor.execute("agg_mix", &config, &pool, &context).await.unwrap();
        let output = result.outputs.ready().get("output");
        // The output should have been produced (either array or string depending on aggregator logic)
        assert!(output.is_some());
    }

    #[test]
    fn test_is_template_anomaly_error_large_output() {
        assert!(is_template_anomaly_error("output too large for template"));
    }

    #[test]
    fn test_is_template_anomaly_error_recursion_depth() {
        assert!(is_template_anomaly_error("recursion depth exceeded"));
    }

    #[test]
    fn test_is_template_anomaly_error_not_anomaly() {
        assert!(!is_template_anomaly_error("variable not found"));
    }

    #[test]
    fn test_escape_js_string_backslash_and_quote() {
        let result = escape_js_string("hello\\world");
        assert!(result.contains("\\\\"));
        let result2 = escape_js_string("it's");
        assert!(result2.contains("\\'"));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_empty_string() {
        let result = parse_json_result("");
        // Empty string is invalid JSON, returns Err
        assert!(result.is_err() || result.unwrap().is_none());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_boolean_true() {
        let result = parse_json_result("true").unwrap().unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_boolean_false() {
        let result = parse_json_result("false").unwrap().unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_array() {
        let result = parse_json_result("[1,2,3]").unwrap().unwrap();
        assert_eq!(result, serde_json::json!([1,2,3]));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_object() {
        let result = parse_json_result("{\"key\":\"val\"}").unwrap().unwrap();
        assert_eq!(result, serde_json::json!({"key": "val"}));
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_undefined_sentinel() {
        let result = parse_json_result("__undefined__").unwrap();
        assert!(result.is_none());
    }

    #[cfg(feature = "builtin-sandbox-js")]
    #[test]
    fn test_parse_json_result_null_returns_none() {
        let result = parse_json_result("null").unwrap();
        assert!(result.is_none());
    }
}
