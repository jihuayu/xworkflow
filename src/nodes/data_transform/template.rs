use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::event_bus::EventSender;
use crate::core::execution_context::NodeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::TemplateNodeConfig;
use crate::error::NodeError;
use crate::nodes::{NodeExecutionResult, NodeExecutor};
use crate::template::TemplateEngine;

/// 模板节点执行器
pub struct TemplateNodeExecutor {
    template_engine: TemplateEngine,
}

impl TemplateNodeExecutor {
    pub fn new() -> Self {
        TemplateNodeExecutor {
            template_engine: TemplateEngine::new(),
        }
    }
}

impl Default for TemplateNodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeExecutor for TemplateNodeExecutor {
    fn validate(&self, config: &Value) -> Result<(), NodeError> {
        let _config: TemplateNodeConfig = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::ConfigError(format!("Invalid template config: {}", e)))?;
        Ok(())
    }

    async fn execute(
        &self,
        ctx: &NodeContext,
        pool: &Arc<VariablePool>,
        _event_sender: &EventSender,
    ) -> Result<NodeExecutionResult, NodeError> {
        let config: TemplateNodeConfig = serde_json::from_value(ctx.config.clone())
            .map_err(|e| NodeError::ConfigError(format!("Invalid template config: {}", e)))?;

        // 先解析 Dify 风格变量 {{#node.field#}}
        let resolved_template = pool.resolve_template(&config.template)?;

        // 再使用 Jinja2 模板引擎渲染
        let variables = pool.get_all_variables();
        let rendered = self
            .template_engine
            .render_template(&resolved_template, &variables)?;

        Ok(NodeExecutionResult::Completed(serde_json::json!({
            "output": rendered,
        })))
    }

    fn node_type(&self) -> &str {
        "template"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::event_bus::create_event_channel;
    use serde_json::json;

    #[tokio::test]
    async fn test_template_with_dify_variables() {
        let executor = TemplateNodeExecutor::new();
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("input", json!({"name": "World"}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "template1".to_string(),
            node_type: "template".to_string(),
            config: json!({
                "template": "Hello {{#input.name#}}"
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "Template".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::Completed(value) => {
                assert_eq!(value["output"], "Hello World");
            }
            _ => panic!("Expected Completed"),
        }
    }

    #[tokio::test]
    async fn test_template_with_jinja2() {
        let executor = TemplateNodeExecutor::new();
        let pool = Arc::new(VariablePool::new());
        pool.set_node_output("input", json!({"items": ["a", "b", "c"]}));

        let (sender, _receiver) = create_event_channel();

        let ctx = NodeContext {
            node_id: "template1".to_string(),
            node_type: "template".to_string(),
            config: json!({
                "template": "{% for item in input.items %}{{ item }}{% if not loop.last %},{% endif %}{% endfor %}"
            }),
            execution_id: "test".to_string(),
            user_id: "user1".to_string(),
            title: "Template".to_string(),
        };

        let result = executor.execute(&ctx, &pool, &sender).await.unwrap();
        match result {
            NodeExecutionResult::Completed(value) => {
                assert_eq!(value["output"], "a,b,c");
            }
            _ => panic!("Expected Completed"),
        }
    }
}
