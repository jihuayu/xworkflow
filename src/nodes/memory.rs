use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::dsl::schema::{EdgeHandle, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::NodeError;
use crate::memory::{
    resolve_namespace,
    MemoryQuery,
    MemoryStoreNodeData,
    MemoryStoreParams,
    MemoryRecallNodeData,
};
use crate::nodes::executor::NodeExecutor;
use crate::template::render_template_async_with_config;

#[cfg(feature = "security")]
use crate::security::audit::{EventSeverity, SecurityEvent, SecurityEventType};

pub struct MemoryRecallExecutor;
pub struct MemoryStoreExecutor;

#[async_trait]
impl NodeExecutor for MemoryRecallExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let data: MemoryRecallNodeData = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::SerializationError(e.to_string()))?;

        let provider = context
            .memory_provider()
            .ok_or_else(|| NodeError::ExecutionError("MemoryProvider is not configured".to_string()))?;

        let namespace = resolve_namespace(&data.scope, variable_pool, context, node_id)
            .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

        let query_text = if let Some(sel) = &data.query_selector {
            variable_pool.get_resolved(sel).await.as_string()
        } else {
            None
        };

        let query = MemoryQuery {
            namespace: namespace.clone(),
            key: data.key,
            query_text,
            filter: data.filter,
            top_k: Some(data.top_k),
        };

        #[cfg(feature = "security")]
        audit_memory_access(context, node_id, &namespace, "recall", true).await;

        let entries = provider
            .recall(query)
            .await
            .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

        let entries_len = entries.len() as i64;
        let mut outputs = HashMap::new();
        outputs.insert(
            "results".to_string(),
            Segment::from_value(&serde_json::to_value(entries)
                .map_err(|e| NodeError::SerializationError(e.to_string()))?),
        );
        outputs.insert("count".to_string(), Segment::Integer(entries_len));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

#[async_trait]
impl NodeExecutor for MemoryStoreExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let data: MemoryStoreNodeData = serde_json::from_value(config.clone())
            .map_err(|e| NodeError::SerializationError(e.to_string()))?;

        let provider = context
            .memory_provider()
            .ok_or_else(|| NodeError::ExecutionError("MemoryProvider is not configured".to_string()))?;

        let namespace = resolve_namespace(&data.scope, variable_pool, context, node_id)
            .map_err(|e| NodeError::ExecutionError(e.to_string()))?;

        let rendered_key = render_template_async_with_config(
            &data.key,
            variable_pool,
            context.strict_template(),
        )
        .await
        .map_err(|e| NodeError::VariableNotFound(e.selector))?;

        let value = variable_pool.get_resolved(&data.value_selector).await.to_value();
        let metadata = merge_metadata(data.metadata, data.metadata_selectors, variable_pool).await;

        #[cfg(feature = "security")]
        if let Some(policy) = context.security_policy().and_then(|p| p.memory.as_ref()) {
            check_value_safety(&value, policy).map_err(|e| NodeError::ExecutionError(e.to_string()))?;
        }

        let params = MemoryStoreParams {
            namespace: namespace.clone(),
            key: rendered_key.clone(),
            value,
            metadata,
        };

        let result = provider.store(params).await;

        #[cfg(feature = "security")]
        audit_memory_access(context, node_id, &namespace, "store", result.is_ok()).await;

        result.map_err(|e| NodeError::ExecutionError(e.to_string()))?;

        let mut outputs = HashMap::new();
        outputs.insert("stored".to_string(), Segment::Boolean(true));
        outputs.insert("key".to_string(), Segment::String(rendered_key));

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}

async fn merge_metadata(
    metadata: Option<HashMap<String, Value>>,
    metadata_selectors: Option<HashMap<String, crate::core::variable_pool::Selector>>,
    variable_pool: &VariablePool,
) -> Option<Value> {
    let mut merged = Map::new();
    if let Some(meta) = metadata {
        for (k, v) in meta {
            merged.insert(k, v);
        }
    }
    if let Some(selectors) = metadata_selectors {
        for (k, selector) in selectors {
            merged.insert(k, variable_pool.get_resolved(&selector).await.to_value());
        }
    }

    if merged.is_empty() {
        None
    } else {
        Some(Value::Object(merged))
    }
}

#[cfg(feature = "security")]
fn check_value_safety(
    value: &Value,
    policy: &crate::security::policy::MemorySecurityPolicy,
) -> Result<(), crate::memory::MemoryError> {
    if let Some(max_bytes) = policy.max_value_bytes {
        let size = serde_json::to_vec(value)
            .map_err(|e| crate::memory::MemoryError::ProviderError(e.to_string()))?
            .len();
        if size > max_bytes {
            return Err(crate::memory::MemoryError::QuotaExceeded(format!(
                "Value size {} exceeds max {}",
                size, max_bytes
            )));
        }
    }
    Ok(())
}

#[cfg(feature = "security")]
async fn audit_memory_access(
    context: &RuntimeContext,
    node_id: &str,
    namespace: &str,
    operation: &str,
    success: bool,
) {
    let logger = match context.audit_logger() {
        Some(l) => l,
        None => return,
    };
    let group_id = match context.resource_group() {
        Some(g) => g.group_id.clone(),
        None => return,
    };

    logger
        .log_event(SecurityEvent {
            timestamp: context.time_provider.now_timestamp(),
            group_id,
            workflow_id: context.workflow_id.clone(),
            node_id: Some(node_id.to_string()),
            event_type: SecurityEventType::MemoryAccess {
                namespace: namespace.to_string(),
                operation: operation.to_string(),
                node_id: node_id.to_string(),
                success,
            },
            details: Value::Null,
            severity: EventSeverity::Info,
        })
        .await;
}
