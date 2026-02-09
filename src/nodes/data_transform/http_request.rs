use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::execution_context::NodeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::HttpRequestNodeConfig;
use crate::error::NodeError;
use crate::nodes::{NodeExecutionResult, NodeExecutor};

/// HTTP 请求节点执行器
pub struct HttpRequestNodeExecutor {
    http_client: reqwest::Client,
}

impl HttpRequestNodeExecutor {
    pub fn new() -> Self {
        HttpRequestNodeExecutor {
            http_client: reqwest::Client::builder()
                .pool_max_idle_per_host(10)
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for HttpRequestNodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeExecutor for HttpRequestNodeExecutor {
    fn validate(&self, config: &Value) -> Result<(), NodeError> {
        let _config: HttpRequestNodeConfig = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(format!("Invalid HTTP request config: {}", e)))?;
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &NodeContext,
        pool: &Arc<VariablePool>,
        _event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError> {
        let config: HttpRequestNodeConfig = serde_json::from_value(ctx.config.clone())
            .map_err(|e| NodeError::ConfigError(format!("Invalid HTTP request config: {}", e)))?;

        // 解析 URL 中的变量
        let url = pool.resolve_template(&config.url)?;

        // 解析请求体中的变量
        let body = if let Some(body_template) = &config.body {
            Some(pool.resolve_template(body_template)?)
        } else {
            None
        };

        // 构建请求
        let mut request = match config.method {
            crate::dsl::HttpMethod::Get => self.http_client.get(&url),
            crate::dsl::HttpMethod::Post => self.http_client.post(&url),
            crate::dsl::HttpMethod::Put => self.http_client.put(&url),
            crate::dsl::HttpMethod::Delete => self.http_client.delete(&url),
            crate::dsl::HttpMethod::Patch => self.http_client.patch(&url),
        };

        // 添加请求头
        for (key, value) in &config.headers {
            request = request.header(key, value);
        }

        // 添加请求体
        if let Some(body) = body {
            request = request.body(body);
        }

        // 发送请求
        let response = request
            .timeout(std::time::Duration::from_secs(config.timeout))
            .send()
            .await
            .map_err(|e| NodeError::HttpError(format!("HTTP request failed: {}", e)))?;

        // 处理响应
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| NodeError::HttpError(format!("Failed to read response body: {}", e)))?;

        // 尝试解析为 JSON
        let body_json = serde_json::from_str::<Value>(&body).ok();

        Ok(NodeExecutionResult::Completed(serde_json::json!({
            "status": status,
            "body": body_json.unwrap_or_else(|| serde_json::json!(body)),
        })))
    }

    fn node_type(&self) -> &str {
        "http-request"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_config() {
        let executor = HttpRequestNodeExecutor::new();
        let config = serde_json::json!({
            "method": "GET",
            "url": "https://example.com",
            "timeout": 30
        });

        assert!(executor.validate(&config).is_ok());
    }

    #[test]
    fn test_validate_invalid_config() {
        let executor = HttpRequestNodeExecutor::new();
        let config = serde_json::json!({
            "invalid": true
        });

        assert!(executor.validate(&config).is_err());
    }
}
