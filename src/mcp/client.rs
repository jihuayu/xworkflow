use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;

use super::error::McpError;

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[async_trait]
pub trait McpClient: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError>;
    async fn call_tool(&self, name: &str, arguments: &Value) -> Result<String, McpError>;
    async fn close(&self) -> Result<(), McpError>;
}

pub struct RmcpClient {
    inner: tokio::sync::Mutex<rmcp::service::RunningService<rmcp::RoleClient, ()>>,
}

impl RmcpClient {
    pub async fn connect_stdio(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        use rmcp::transport::TokioChildProcess;
        use rmcp::ServiceExt;

        let mut process = tokio::process::Command::new(command);
        process.args(args);
        process.envs(env);

        let transport = TokioChildProcess::new(&mut process)
            .map_err(|e| McpError::ConnectionError(e.to_string()))?;
        let inner =
            ().serve(transport)
                .await
                .map_err(|e| McpError::ConnectionError(e.to_string()))?;
        Ok(Self {
            inner: tokio::sync::Mutex::new(inner),
        })
    }

    pub async fn connect_http(
        url: &str,
        headers: &HashMap<String, String>,
    ) -> Result<Self, McpError> {
        use rmcp::transport::SseTransport;
        use rmcp::ServiceExt;

        let mut header_map = reqwest::header::HeaderMap::new();
        for (key, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|e| McpError::InvalidConfig(e.to_string()))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|e| McpError::InvalidConfig(e.to_string()))?;
            header_map.insert(name, value);
        }
        let http_client = reqwest::Client::builder()
            .default_headers(header_map)
            .build()
            .map_err(|e| McpError::ConnectionError(e.to_string()))?;

        let transport = SseTransport::start_with_client(url, http_client)
            .await
            .map_err(|e| McpError::ConnectionError(e.to_string()))?;
        let inner =
            ().serve(transport)
                .await
                .map_err(|e| McpError::ConnectionError(e.to_string()))?;
        Ok(Self {
            inner: tokio::sync::Mutex::new(inner),
        })
    }
}

#[async_trait]
impl McpClient for RmcpClient {
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let response = self
            .inner
            .lock()
            .await
            .list_all_tools()
            .await
            .map_err(|e| McpError::ProtocolError(e.to_string()))?;

        Ok(response
            .into_iter()
            .map(|tool| McpToolInfo {
                name: tool.name.to_string(),
                description: tool.description.to_string(),
                input_schema: Value::Object(tool.input_schema.as_ref().clone()),
            })
            .collect())
    }

    async fn call_tool(&self, name: &str, arguments: &Value) -> Result<String, McpError> {
        use rmcp::model::CallToolRequestParam;

        let args = arguments.as_object().cloned().unwrap_or_default();

        let response = self
            .inner
            .lock()
            .await
            .call_tool(CallToolRequestParam {
                name: name.to_string().into(),
                arguments: Some(args),
            })
            .await
            .map_err(|e| McpError::ToolError(e.to_string()))?;

        let text = response
            .content
            .iter()
            .filter_map(|content| {
                if let Some(text) = content.raw.as_text() {
                    return Some(text.text.clone());
                }
                content
                    .raw
                    .as_resource()
                    .map(|resource| serde_json::to_string(&resource.resource).unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join("\n");

        if response.is_error.unwrap_or(false) {
            return Err(McpError::ToolError(text));
        }

        Ok(text)
    }

    async fn close(&self) -> Result<(), McpError> {
        // RunningService::cancel(self) consumes self; pool lifecycle relies on Drop.
        Ok(())
    }
}
