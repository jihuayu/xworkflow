use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, Selector, VariablePool};
use crate::dsl::schema::{
    McpServerConfig, McpTransport, NodeOutputs, NodeRunResult, ToolNodeData,
    WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
#[cfg(feature = "security")]
use crate::llm::executor::audit_security_event;
use crate::llm::executor::render_arguments;
use crate::mcp::pool::McpConnectionPool;
use crate::nodes::executor::NodeExecutor;
#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEventType};
#[cfg(feature = "security")]
use crate::security::network::validate_url;
#[cfg(feature = "security")]
use parking_lot::RwLock as ParkingRwLock;

pub struct ToolNodeExecutor {
    mcp_pool: Arc<RwLock<McpConnectionPool>>,
}

#[allow(clippy::result_large_err)]
impl ToolNodeExecutor {
    pub fn new(mcp_pool: Arc<RwLock<McpConnectionPool>>) -> Self {
        Self { mcp_pool }
    }

    fn resolve_server_config(
        &self,
        cfg: &ToolNodeData,
        variable_pool: &VariablePool,
    ) -> Result<McpServerConfig, Box<NodeError>> {
        if let Some(server) = &cfg.mcp_server {
            return Ok(server.clone());
        }

        let Some(server_ref) = cfg.mcp_server_ref.as_ref() else {
            return Err(Box::new(NodeError::ConfigError(
                "tool node requires mcp_server or mcp_server_ref".to_string(),
            )));
        };

        let mcp_servers = variable_pool
            .get(&Selector::new("sys", "__mcp_servers"))
            .to_value();
        let mcp_servers: HashMap<String, McpServerConfig> = serde_json::from_value(mcp_servers)
            .map_err(|e| {
                Box::new(NodeError::ConfigError(format!(
                    "invalid workflow mcp_servers: {}",
                    e
                )))
            })?;

        mcp_servers
            .get(server_ref)
            .cloned()
            .ok_or_else(|| {
                Box::new(NodeError::ConfigError(format!(
                    "mcp_server_ref '{}' not found",
                    server_ref
                )))
            })
    }

    #[cfg(feature = "security")]
    async fn check_tool_security(
        &self,
        context: &RuntimeContext,
        node_id: &str,
        tool_name: &str,
        arguments: &Value,
        variable_pool: &VariablePool,
    ) -> Result<(), NodeError> {
        let gate = crate::core::security_gate::new_security_gate(
            Arc::new(context.clone()),
            Arc::new(ParkingRwLock::new(variable_pool.clone())),
        );

        let tool_config = serde_json::json!({
            "mcp_tool": tool_name,
            "arguments": arguments,
        });
        gate.check_before_node(node_id, "mcp-tool-call", &tool_config)
            .await?;

        audit_security_event(
            context,
            SecurityEventType::ToolInvocation {
                tool_name: tool_name.to_string(),
            },
            EventSeverity::Info,
            Some(node_id.to_string()),
        )
        .await;

        Ok(())
    }
}

#[async_trait]
impl NodeExecutor for ToolNodeExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let cfg: ToolNodeData = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        let server_config = self
            .resolve_server_config(&cfg, variable_pool)
            .map_err(|e| *e)?;

        #[cfg(feature = "security")]
        if let McpTransport::Http { url, .. } = &server_config.transport {
            if let Some(policy) = context.security_policy().and_then(|p| p.network.as_ref()) {
                if let Err(err) = validate_url(url, policy).await {
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
        }

        let client = {
            let mut pool = self.mcp_pool.write().await;
            pool.get_or_connect(&server_config)
                .await
                .map_err(|e| NodeError::ExecutionError(e.to_string()))?
        };

        let arguments = render_arguments(&cfg.arguments, variable_pool, context.strict_template())?;

        #[cfg(feature = "security")]
        self.check_tool_security(context, node_id, &cfg.tool_name, &arguments, variable_pool)
            .await?;

        let result = client
            .call_tool(&cfg.tool_name, &arguments)
            .await
            .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync({
                let mut outputs = HashMap::new();
                outputs.insert("result".to_string(), Segment::String(result));
                outputs
            }),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::schema::McpTransport;
    use crate::mcp::client::{McpClient, McpToolInfo};
    use crate::mcp::error::McpError;

    struct MockMcpClient {
        response: String,
    }

    #[async_trait]
    impl McpClient for MockMcpClient {
        async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
            Ok(vec![])
        }

        async fn call_tool(&self, _name: &str, arguments: &Value) -> Result<String, McpError> {
            if let Some(id) = arguments.get("id").and_then(|v| v.as_str()) {
                return Ok(format!("{}:{}", self.response, id));
            }
            Ok(self.response.clone())
        }

        async fn close(&self) -> Result<(), McpError> {
            Ok(())
        }
    }

    fn test_server() -> McpServerConfig {
        McpServerConfig {
            name: Some("test".to_string()),
            transport: McpTransport::Stdio {
                command: "mock".to_string(),
                args: vec![],
                env: HashMap::new(),
            },
        }
    }

    #[tokio::test]
    async fn test_tool_node_basic() {
        let server = test_server();
        let pool = Arc::new(RwLock::new(McpConnectionPool::new()));
        pool.write().await.insert_connection(
            &server,
            Arc::new(MockMcpClient {
                response: "ok".to_string(),
            }),
        );

        let executor = ToolNodeExecutor::new(pool);
        let config = serde_json::json!({
            "mcp_server": serde_json::to_value(&server).unwrap(),
            "tool_name": "search",
            "arguments": {"q": "hello"}
        });

        let variable_pool = VariablePool::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("tool1", &config, &variable_pool, &context)
            .await
            .unwrap();

        assert_eq!(
            result.outputs.ready().get("result"),
            Some(&Segment::String("ok".to_string()))
        );
    }

    #[tokio::test]
    async fn test_tool_node_missing_server() {
        let executor = ToolNodeExecutor::new(Arc::new(RwLock::new(McpConnectionPool::new())));
        let config = serde_json::json!({
            "tool_name": "search",
            "arguments": {"q": "hello"}
        });

        let variable_pool = VariablePool::new();
        let context = RuntimeContext::default();
        let result = executor
            .execute("tool1", &config, &variable_pool, &context)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_node_template_args() {
        let server = test_server();
        let pool = Arc::new(RwLock::new(McpConnectionPool::new()));
        pool.write().await.insert_connection(
            &server,
            Arc::new(MockMcpClient {
                response: "ok".to_string(),
            }),
        );

        let executor = ToolNodeExecutor::new(pool);
        let config = serde_json::json!({
            "mcp_server": serde_json::to_value(&server).unwrap(),
            "tool_name": "search",
            "arguments": {"id": "{{#start.id#}}"}
        });

        let mut variable_pool = VariablePool::new();
        variable_pool.set(
            &Selector::new("start", "id"),
            Segment::String("42".to_string()),
        );

        let context = RuntimeContext::default();
        let result = executor
            .execute("tool1", &config, &variable_pool, &context)
            .await
            .unwrap();

        assert_eq!(
            result.outputs.ready().get("result"),
            Some(&Segment::String("ok:42".to_string()))
        );
    }
}
