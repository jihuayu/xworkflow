//! Template Transform node executor.

use async_trait::async_trait;

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::compiler::CompiledNodeConfig;
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, SegmentStream, StreamEvent, StreamStatus, VariablePool};
use crate::dsl::schema::{
    EdgeHandle, NodeOutputs, NodeRunResult, VariableMapping, WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEventType};
#[cfg(not(feature = "security"))]
use crate::template::render_jinja2_with_functions;
use crate::template::render_jinja2_with_functions_and_config;
use xworkflow_types::template::{TemplateEngine, TemplateFunction};

#[cfg(feature = "security")]
use super::helpers::{audit_security_event, is_template_anomaly_error};

type TemplateRenderFn =
    dyn Fn(&str, &HashMap<String, Value>) -> Result<String, String> + Send + Sync;

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

    async fn execute_with_template(
        &self,
        node_id: &str,
        template: &str,
        variables: &[VariableMapping],
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        #[cfg(feature = "security")]
        let template_len = template.len();

        let mut static_vars: HashMap<String, Value> = HashMap::new();
        let mut stream_vars: Vec<(String, SegmentStream)> = Vec::new();
        for m in variables {
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

        if stream_vars.is_empty() {
            #[cfg(feature = "plugin-system")]
            let tmpl_functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>> =
                context.template_functions().map(|f| f.as_ref());
            #[cfg(not(feature = "plugin-system"))]
            let tmpl_functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>> = None;

            #[cfg(feature = "security")]
            let safety = context.security_policy().and_then(|p| p.template.as_ref());

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
            outputs.insert("output".to_string(), Segment::String(rendered));

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

        let funcs = tmpl_functions.clone();

        #[cfg(feature = "security")]
        let safety = context.security_policy().and_then(|p| p.template.clone());

        let render_fn: Box<TemplateRenderFn> = if let Some(engine) = &self.engine {
            #[cfg(feature = "security")]
            if let Some(cfg) = safety.as_ref() {
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

            let engine = Arc::clone(engine);
            let funcs = funcs.clone();
            Box::new(move |template: &str, vars: &HashMap<String, Value>| {
                engine.render(template, vars, funcs.as_ref().map(|f| f.as_ref()))
            })
        } else {
            #[cfg(feature = "security")]
            {
                let funcs = funcs.clone();
                let safety = safety.clone();
                Box::new(move |template: &str, vars: &HashMap<String, Value>| {
                    render_jinja2_with_functions_and_config(
                        template,
                        vars,
                        funcs.as_ref().map(|f| f.as_ref()),
                        safety.as_ref(),
                    )
                })
            }

            #[cfg(not(feature = "security"))]
            {
                let funcs = funcs.clone();
                Box::new(move |template: &str, vars: &HashMap<String, Value>| {
                    render_jinja2_with_functions(template, vars, funcs.as_ref().map(|f| f.as_ref()))
                })
            }
        };

        tokio::spawn(async move {
            let mut vars = base_vars;
            let mut last_rendered = String::new();

            for (name, stream) in stream_vars {
                let mut reader = stream.reader();
                loop {
                    match reader.next().await {
                        Some(StreamEvent::Chunk(seg)) => {
                            let entry = vars
                                .entry(name.clone())
                                .or_insert(Value::String(String::new()));
                            let val_str = entry.as_str().unwrap_or_default().to_string();
                            let new_val = format!("{}{}", val_str, seg.to_display_string());
                            *entry = Value::String(new_val);

                            let rendered = match render_fn(&template_str, &vars) {
                                Ok(v) => v,
                                Err(e) => {
                                    let _ = writer.error(e).await;
                                    return;
                                }
                            };

                            let delta = if rendered.starts_with(&last_rendered) {
                                rendered[last_rendered.len()..].to_string()
                            } else {
                                rendered.clone()
                            };
                            last_rendered = rendered;
                            let _ = writer.send(Segment::String(delta)).await;
                        }
                        Some(StreamEvent::End(seg)) => {
                            let final_val = seg.to_display_string();
                            vars.insert(name.clone(), Value::String(final_val));

                            let rendered = match render_fn(&template_str, &vars) {
                                Ok(v) => v,
                                Err(e) => {
                                    let _ = writer.error(e).await;
                                    return;
                                }
                            };

                            let delta = if rendered.starts_with(&last_rendered) {
                                rendered[last_rendered.len()..].to_string()
                            } else {
                                rendered.clone()
                            };
                            last_rendered = rendered;
                            let _ = writer.send(Segment::String(delta)).await;
                            break;
                        }
                        Some(StreamEvent::Error(err)) => {
                            let _ = writer.error(err).await;
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
        let variables = config
            .get("variables")
            .and_then(|v| serde_json::from_value::<Vec<VariableMapping>>(v.clone()).ok())
            .unwrap_or_default();

        self.execute_with_template(node_id, template, &variables, variable_pool, context)
            .await
    }

    async fn execute_compiled(
        &self,
        node_id: &str,
        compiled_config: &CompiledNodeConfig,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let CompiledNodeConfig::TemplateTransform(config) = compiled_config else {
            return self
                .execute(node_id, compiled_config.as_value(), variable_pool, context)
                .await;
        };

        self.execute_with_template(
            node_id,
            &config.parsed.template,
            &config.parsed.variables,
            variable_pool,
            context,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::Selector;

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
        let result = executor
            .execute("tt1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::String("Hello World!".into()))
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
        let result = executor
            .execute("tt_stream", &config, &pool, &context)
            .await
            .unwrap();
        let stream = result
            .outputs
            .streams()
            .and_then(|streams| streams.get("output").cloned())
            .expect("missing stream output");
        let final_seg = stream.collect().await.unwrap_or(Segment::None);
        assert_eq!(final_seg.to_display_string(), "Hello World!");
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
        let result = executor
            .execute("tt1", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::String("Hello World!".into()))
        );
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[tokio::test]
    async fn test_template_transform_multiple_variables() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("n1", "first"),
            Segment::String("John".into()),
        );
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
        let result = executor
            .execute("tt2", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::String("John Doe".into()))
        );
    }

    #[tokio::test]
    async fn test_template_transform_default() {
        let executor = TemplateTransformExecutor::default();
        assert!(executor.engine.is_none());
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
        let result = executor
            .execute("tt_int", &config, &pool, &context)
            .await
            .unwrap();
        assert_eq!(
            result.outputs.ready().get("output"),
            Some(&Segment::String("Count is 42".into()))
        );
    }
}
