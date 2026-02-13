use super::client::McpClient;
use super::error::McpError;
use crate::llm::types::ToolDefinition;

pub async fn discover_tools(client: &dyn McpClient) -> Result<Vec<ToolDefinition>, McpError> {
    let tools = client.list_tools().await?;

    Ok(tools
        .into_iter()
        .map(|tool| ToolDefinition {
            name: tool.name,
            description: tool.description,
            parameters: tool.input_schema,
        })
        .collect())
}
