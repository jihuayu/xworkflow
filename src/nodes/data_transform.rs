use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::{
    NodeRunResult,
    VariableMapping, WorkflowNodeExecutionStatus, WriteMode,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::nodes::utils::selector_from_value;
use crate::template::{render_jinja2, render_template};

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
        let mut jinja_vars: HashMap<String, Value> = HashMap::new();
        if let Some(vars_val) = config.get("variables") {
            if let Ok(mappings) = serde_json::from_value::<Vec<VariableMapping>>(vars_val.clone()) {
                for m in &mappings {
                    let val = variable_pool.get(&m.value_selector);
                    jinja_vars.insert(m.variable.clone(), val.to_value());
                }
            }
        }

        let rendered = render_jinja2(template, &jinja_vars)
            .map_err(|e| NodeError::TemplateError(e))?;

        let mut outputs = HashMap::new();
        outputs.insert("output".to_string(), Value::String(rendered));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs,
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
            let val = variable_pool.get(selector);
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
            variable_pool.get(&sel).to_value()
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

        // Substitute variables in URL
        let url = render_template(url_template, variable_pool);

        // Build headers
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(h_arr) = config.get("headers").and_then(|v| v.as_array()) {
            for h in h_arr {
                let key = h.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let val = h.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let val = render_template(val, variable_pool);
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
                    let data = render_template(data, variable_pool);
                    req_builder.body(data)
                }
                "json" => {
                    let data = body.get("data").and_then(|v| v.as_str()).unwrap_or("{}");
                    let data = render_template(data, variable_pool);
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

        let status_code = resp.status().as_u16() as i32;
        let resp_headers = format!("{:?}", resp.headers());
        let resp_body = resp
            .text()
            .await
            .map_err(|e| NodeError::HttpError(e.to_string()))?;

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
        if let Some(vars_val) = config.get("variables") {
            if let Ok(mappings) = serde_json::from_value::<Vec<VariableMapping>>(vars_val.clone()) {
                for m in &mappings {
                    let val = variable_pool.get(&m.value_selector);
                    inputs_map.insert(m.variable.clone(), val.to_value());
                }
            }
        }
        // Support inputs map: { var: selector }
        if let Some(inputs_val) = config.get("inputs") {
            if let Some(map) = inputs_val.as_object() {
                for (var, sel_val) in map {
                    if let Some(selector) = selector_from_value(sel_val) {
                        let val = variable_pool.get(&selector);
                        inputs_map.insert(var.clone(), val.to_value());
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

        // Execute via sandbox
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
            .map_err(|e| NodeError::SandboxError(e.to_string()))?;

        if !result.success {
            return Err(NodeError::ExecutionError(
                result
                    .error
                    .unwrap_or_else(|| "Unknown sandbox error".to_string()),
            ));
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
