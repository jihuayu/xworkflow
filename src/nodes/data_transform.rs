use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::{
    NodeRunResult,
    VariableMapping, WorkflowNodeExecutionStatus, WriteMode,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
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
    ) -> Result<NodeRunResult, NodeError> {
        VariableAggregatorExecutor.execute(node_id, config, variable_pool).await
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
// Code Node (stub – requires external sandbox)
// ================================

pub struct CodeNodeExecutor;

#[async_trait]
impl NodeExecutor for CodeNodeExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
    ) -> Result<NodeRunResult, NodeError> {
        let _code = config.get("code").and_then(|v| v.as_str()).unwrap_or("");
        let language = config.get("language").and_then(|v| v.as_str()).unwrap_or("python3");

        // Build inputs from variable mappings
        let mut inputs = HashMap::new();
        if let Some(vars_val) = config.get("variables") {
            if let Ok(mappings) = serde_json::from_value::<Vec<VariableMapping>>(vars_val.clone()) {
                for m in &mappings {
                    let val = variable_pool.get(&m.value_selector);
                    inputs.insert(m.variable.clone(), val.to_value());
                }
            }
        }

        // In a real implementation, this would POST to a sandbox.
        // For now, return a stub result.
        let mut outputs = HashMap::new();
        outputs.insert(
            "result".to_string(),
            Value::String(format!("[Code execution stub: {} code not sandboxed]", language)),
        );

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            inputs,
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
        let result = executor.execute("tt1", &config, &pool).await.unwrap();
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
        let result = executor.execute("agg1", &config, &pool).await.unwrap();
        assert_eq!(result.outputs.get("output"), Some(&Value::String("found".into())));
    }

    #[tokio::test]
    async fn test_variable_aggregator_all_null() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "variables": [["n1", "out"], ["n2", "out"]]
        });

        let executor = VariableAggregatorExecutor;
        let result = executor.execute("agg1", &config, &pool).await.unwrap();
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
        let result = executor.execute("va1", &config, &pool).await.unwrap();
        assert_eq!(result.outputs.get("output"), Some(&Value::String("data".into())));
    }

    #[tokio::test]
    async fn test_code_node_stub() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "code": "def main(): return {'result': 42}",
            "language": "python3"
        });

        let executor = CodeNodeExecutor;
        let result = executor.execute("code1", &config, &pool).await.unwrap();
        assert!(result.outputs.contains_key("result"));
    }
}
