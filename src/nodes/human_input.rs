use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::VariablePool;
use crate::dsl::schema::{
    FormFieldDefinition,
    HumanInputNodeData,
    HumanInputResumeMode,
    HumanInputTimeoutAction,
    NodeRunResult,
    WorkflowNodeExecutionStatus,
};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;
use crate::template::render_template_async_with_config;

pub(crate) const HUMAN_INPUT_REQUEST_KEY: &str = "human_input_request";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HumanInputPauseRequest {
    pub node_id: String,
    pub node_title: String,
    pub resume_token: String,
    pub resume_mode: HumanInputResumeMode,
    pub form_fields: Vec<FormFieldDefinition>,
    pub prompt_text: Option<String>,
    pub timeout_at: Option<i64>,
    pub timeout_action: HumanInputTimeoutAction,
    pub timeout_default_values: Option<HashMap<String, Value>>,
}

pub struct HumanInputExecutor;

impl HumanInputExecutor {
    async fn resolve_form_fields(
        fields: &[FormFieldDefinition],
        variable_pool: &VariablePool,
    ) -> Vec<FormFieldDefinition> {
        let mut resolved = Vec::with_capacity(fields.len());
        for field in fields {
            let mut item = field.clone();
            if item.default_value.is_none() {
                if let Some(selector) = item.default_value_selector.as_ref() {
                    let val = variable_pool.get_resolved_value(selector).await;
                    if !val.is_null() {
                        item.default_value = Some(val);
                    }
                }
            }
            resolved.push(item);
        }
        resolved
    }
}

#[async_trait]
impl NodeExecutor for HumanInputExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let cfg: HumanInputNodeData =
            serde_json::from_value(config.clone()).map_err(|e| NodeError::ConfigError(e.to_string()))?;

        let prompt_text = match cfg.prompt_text.as_deref() {
            Some(template) => Some(
                render_template_async_with_config(template, variable_pool, context.strict_template())
                    .await
                    .map_err(|e| NodeError::TemplateError(e.to_string()))?,
            ),
            None => None,
        };

        let resume_token = context.id_generator.next_id();
        let timeout_at = cfg
            .timeout_secs
            .map(|secs| context.time_provider.now_timestamp().saturating_add(secs as i64));
        let node_title = config
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let form_fields = Self::resolve_form_fields(&cfg.form_fields, variable_pool).await;

        let request = HumanInputPauseRequest {
            node_id: _node_id.to_string(),
            node_title: node_title.clone(),
            resume_token: resume_token.clone(),
            resume_mode: cfg.resume_mode,
            form_fields,
            prompt_text: prompt_text.clone(),
            timeout_at,
            timeout_action: cfg.timeout_action,
            timeout_default_values: cfg.timeout_default_values,
        };

        let mut metadata = HashMap::new();
        metadata.insert(
            HUMAN_INPUT_REQUEST_KEY.to_string(),
            serde_json::to_value(&request).map_err(|e| NodeError::SerializationError(e.to_string()))?,
        );
        metadata.insert("resume_token".to_string(), Value::String(resume_token));
        metadata.insert("node_title".to_string(), Value::String(node_title));
        if let Some(prompt) = prompt_text {
            metadata.insert("prompt".to_string(), Value::String(prompt));
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Paused,
            metadata,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::{Segment, Selector};
    use crate::dsl::schema::HumanInputTimeoutAction;

    #[tokio::test]
    async fn test_human_input_executor_paused() {
        let executor = HumanInputExecutor;
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("start", "name"), Segment::String("alice".into()));
        let context = RuntimeContext::default();

        let config = serde_json::json!({
            "type": "human-input",
            "title": "Approval",
            "prompt_text": "Hello {{#start.name#}}",
            "resume_mode": "form",
            "form_fields": [
                {
                    "variable": "name",
                    "label": "Name",
                    "field_type": "text",
                    "required": true
                }
            ],
            "timeout_secs": 10,
            "timeout_action": "fail"
        });

        let result = executor
            .execute("approval", &config, &pool, &context)
            .await
            .unwrap();

        assert_eq!(result.status, WorkflowNodeExecutionStatus::Paused);
        assert_eq!(result.metadata.get("prompt"), Some(&Value::String("Hello alice".to_string())));
        assert_eq!(result.metadata.get("node_title"), Some(&Value::String("Approval".to_string())));

        let request_value = result.metadata.get(HUMAN_INPUT_REQUEST_KEY).expect("missing request metadata");
        let request: HumanInputPauseRequest = serde_json::from_value(request_value.clone()).unwrap();
        assert_eq!(request.node_id, "approval");
        assert_eq!(request.node_title, "Approval");
        assert_eq!(request.resume_mode, HumanInputResumeMode::Form);
        assert_eq!(request.form_fields.len(), 1);
        assert_eq!(request.timeout_action, HumanInputTimeoutAction::Fail);
        assert!(request.timeout_at.is_some());
        assert!(!request.resume_token.is_empty());
    }

    #[tokio::test]
    async fn test_human_input_executor_resolve_default_selector() {
        let executor = HumanInputExecutor;
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("start", "city"), Segment::String("Shanghai".into()));
        let context = RuntimeContext::default();

        let config = serde_json::json!({
            "type": "human-input",
            "title": "Form",
            "resume_mode": "form",
            "form_fields": [
                {
                    "variable": "city",
                    "label": "City",
                    "field_type": "text",
                    "default_value_selector": ["start", "city"]
                }
            ]
        });

        let result = executor.execute("human", &config, &pool, &context).await.unwrap();
        let request_value = result.metadata.get(HUMAN_INPUT_REQUEST_KEY).unwrap();
        let request: HumanInputPauseRequest = serde_json::from_value(request_value.clone()).unwrap();
        assert_eq!(request.form_fields[0].default_value, Some(Value::String("Shanghai".into())));
    }
}
