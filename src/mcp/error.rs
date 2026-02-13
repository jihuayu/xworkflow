use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP invalid config: {0}")]
    InvalidConfig(String),
    #[error("MCP connection error: {0}")]
    ConnectionError(String),
    #[error("MCP protocol error: {0}")]
    ProtocolError(String),
    #[error("MCP tool not found: {0}")]
    ToolNotFound(String),
    #[error("MCP tool execution error: {0}")]
    ToolError(String),
    #[error("MCP transport not supported: {0}")]
    UnsupportedTransport(String),
}

impl From<serde_json::Error> for McpError {
    fn from(value: serde_json::Error) -> Self {
        Self::ProtocolError(value.to_string())
    }
}

impl From<std::io::Error> for McpError {
    fn from(value: std::io::Error) -> Self {
        Self::ConnectionError(value.to_string())
    }
}
