//! HTTP Request node executor.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{EdgeHandle, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::{ErrorCode, ErrorContext, NodeError};
use crate::nodes::executor::NodeExecutor;
#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEventType};
#[cfg(feature = "security")]
use crate::security::network::validate_url;
use crate::template::render_template_async_with_config;

#[cfg(feature = "security")]
use super::helpers::audit_security_event;
use super::helpers::read_response_with_limit;

/// Executor for HTTP Request nodes.
pub struct HttpRequestExecutor;

#[async_trait]
impl NodeExecutor for HttpRequestExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let method = config
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET");
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
            context.strict_template(),
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
                    context.strict_template(),
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
            match auth
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("no_auth")
            {
                "bearer_token" => {
                    if let Some(token) = auth.get("token").and_then(|v| v.as_str()) {
                        if let Ok(val) =
                            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))
                        {
                            headers.insert(reqwest::header::AUTHORIZATION, val);
                        }
                    }
                }
                "basic_auth" => {
                    let user = auth.get("username").and_then(|v| v.as_str()).unwrap_or("");
                    let pass = auth.get("password").and_then(|v| v.as_str()).unwrap_or("");
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(format!("{}:{}", user, pass));
                    if let Ok(val) =
                        reqwest::header::HeaderValue::from_str(&format!("Basic {}", encoded))
                    {
                        headers.insert(reqwest::header::AUTHORIZATION, val);
                    }
                }
                _ => {}
            }
        }

        // Send request
        #[cfg(feature = "security")]
        if let Some(policy) = context.security_policy().and_then(|p| p.network.as_ref()) {
            if let Err(err) = validate_url(&url, policy).await {
                audit_security_event(
                    context,
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
        }

        let client = if let Some(provider) = context.http_client() {
            provider.client_for_context(context)?
        } else {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout))
                .build()
                .map_err(|e| NodeError::HttpError(e.to_string()))?
        };

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
                        context.strict_template(),
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
                        context.strict_template(),
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
            .timeout(std::time::Duration::from_secs(timeout))
            .headers(headers)
            .send()
            .await
            .map_err(|e| NodeError::HttpError(e.to_string()))?;

        let status_code = resp.status().as_u16();
        let headers_snapshot = resp.headers().clone();
        let resp_headers = format!("{:?}", headers_snapshot);
        #[cfg(feature = "security")]
        let max_response_bytes = {
            let mut max = context
                .resource_group()
                .map(|g| g.quota.http_max_response_bytes);
            if let Some(policy) = context.security_policy() {
                if let Some(limit) = policy.node_limits.get("http-request") {
                    max = Some(
                        max.map(|m| m.min(limit.max_output_bytes))
                            .unwrap_or(limit.max_output_bytes),
                    );
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
                401 | 403 => {
                    ErrorContext::non_retryable(ErrorCode::HttpClientError, error_msg.clone())
                        .with_http_status(status_code)
                }
                429 => {
                    let retry_after = headers_snapshot
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok());
                    let mut ctx =
                        ErrorContext::retryable(ErrorCode::HttpClientError, error_msg.clone())
                            .with_http_status(status_code);
                    if let Some(ra) = retry_after {
                        ctx = ctx.with_retry_after(ra);
                    }
                    ctx
                }
                400 | 404 | 405 | 422 => {
                    ErrorContext::non_retryable(ErrorCode::HttpClientError, error_msg.clone())
                        .with_http_status(status_code)
                }
                500..=599 => ErrorContext::retryable(ErrorCode::HttpServerError, error_msg.clone())
                    .with_http_status(status_code),
                _ => ErrorContext::non_retryable(ErrorCode::HttpClientError, error_msg.clone())
                    .with_http_status(status_code),
            };

            return Err(NodeError::HttpError(error_msg).with_context(context));
        }

        let mut outputs = HashMap::new();
        outputs.insert(
            "status_code".to_string(),
            Segment::Integer(status_code as i64),
        );
        outputs.insert("body".to_string(), Segment::String(resp_body));
        outputs.insert("headers".to_string(), Segment::String(resp_headers));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::Selector;

    #[tokio::test]
    async fn test_http_request_missing_url() {
        let pool = VariablePool::new();
        let config = serde_json::json!({
            "method": "GET"
        });

        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let result = executor
            .execute("http_no_url", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_bearer", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_basic", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_post_raw", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_post_json", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_delete", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_patch", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_head", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_fail", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_params", &config, &pool, &context)
            .await;
    }

    #[tokio::test]
    async fn test_http_request_with_template_url() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("start", "domain"),
            Segment::String("example.com".into()),
        );
        let config = serde_json::json!({
            "url": "http://{{#start.domain#}}/api",
            "method": "GET",
            "timeout": 2
        });
        let executor = HttpRequestExecutor;
        let context = RuntimeContext::default();
        let _ = executor
            .execute("http_tmpl", &config, &pool, &context)
            .await;
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
        let _ = executor
            .execute("http_no_auth", &config, &pool, &context)
            .await;
    }
}
