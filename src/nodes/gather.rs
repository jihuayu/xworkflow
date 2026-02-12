use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, SegmentArray, VariablePool};
use crate::dsl::schema::{EdgeHandle, GatherNodeData, NodeOutputs, NodeRunResult, WorkflowNodeExecutionStatus};
use crate::error::NodeError;
use crate::nodes::executor::NodeExecutor;

pub struct GatherExecutor;

#[async_trait]
impl NodeExecutor for GatherExecutor {
    async fn execute(
        &self,
        _node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        _context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        let gather = serde_json::from_value::<GatherNodeData>(config.clone()).unwrap_or(GatherNodeData {
            variables: Vec::new(),
            join_mode: crate::dsl::schema::JoinMode::All,
            cancel_remaining: true,
            timeout_secs: None,
            timeout_strategy: crate::dsl::schema::TimeoutStrategy::ProceedWithAvailable,
        });

        let mut results: Vec<Segment> = Vec::new();
        for selector in &gather.variables {
            let value = variable_pool.get_resolved(selector).await;
            if !value.is_none() {
                results.push(value);
            }
        }

        let completed_count = results.len() as i64;
        let first_result = results.first().cloned();

        let mut outputs = HashMap::new();
        outputs.insert(
            "results".to_string(),
            Segment::Array(Arc::new(SegmentArray::new(results))),
        );
        outputs.insert("completed_count".to_string(), Segment::Integer(completed_count));
        if let Some(first) = first_result {
            outputs.insert("first_result".to_string(), first);
        }

        Ok(NodeRunResult {
            status: WorkflowNodeExecutionStatus::Succeeded,
            outputs: NodeOutputs::Sync(outputs),
            edge_source_handle: EdgeHandle::Default,
            ..Default::default()
        })
    }
}
