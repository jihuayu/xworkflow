use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::graph::types::EdgeTraversalState;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ConsumedResources {
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_llm_cost: f64,
    pub total_tool_calls: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SerializableEdgeState {
    Pending,
    Taken,
    Skipped,
    Cancelled,
}

impl From<EdgeTraversalState> for SerializableEdgeState {
    fn from(state: EdgeTraversalState) -> Self {
        match state {
            EdgeTraversalState::Pending => Self::Pending,
            EdgeTraversalState::Taken => Self::Taken,
            EdgeTraversalState::Skipped => Self::Skipped,
            EdgeTraversalState::Cancelled => Self::Cancelled,
        }
    }
}

impl From<SerializableEdgeState> for EdgeTraversalState {
    fn from(state: SerializableEdgeState) -> Self {
        match state {
            SerializableEdgeState::Pending => Self::Pending,
            SerializableEdgeState::Taken => Self::Taken,
            SerializableEdgeState::Skipped => Self::Skipped,
            SerializableEdgeState::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContextFingerprint {
    pub security_level: Option<String>,
    pub llm_providers: Vec<String>,
    pub registered_node_types: Vec<String>,
    pub network_policy_hash: Option<String>,
    pub schema_hash: String,
    pub credential_groups: Vec<String>,
    pub engine_config_hash: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Checkpoint {
    pub workflow_id: String,
    pub execution_id: String,
    pub created_at: i64,

    pub completed_node_id: String,
    pub node_states: HashMap<String, SerializableEdgeState>,
    pub edge_states: HashMap<String, SerializableEdgeState>,
    pub ready_queue: Vec<String>,
    pub ready_predecessor: HashMap<String, String>,

    pub variables: HashMap<String, Value>,

    pub step_count: i32,
    pub exceptions_count: i32,
    pub final_outputs: HashMap<String, Value>,
    pub elapsed_secs: u64,

    pub consumed_resources: Option<ConsumedResources>,
    pub context_fingerprint: Option<ContextFingerprint>,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum ResumePolicy {
    #[default]
    Normal,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSeverity {
    Info,
    Warning,
    Danger,
}

#[derive(Debug, Clone)]
pub struct EnvironmentChange {
    pub component: String,
    pub description: String,
    pub severity: ChangeSeverity,
}

#[derive(Debug, Clone, Default)]
pub struct ResumeDiagnostic {
    pub changes: Vec<EnvironmentChange>,
}

impl ResumeDiagnostic {
    pub fn has_danger(&self) -> bool {
        self.changes
            .iter()
            .any(|change| change.severity == ChangeSeverity::Danger)
    }

    pub fn has_warnings(&self) -> bool {
        self.changes
            .iter()
            .any(|change| matches!(change.severity, ChangeSeverity::Warning | ChangeSeverity::Danger))
    }

    pub fn report(&self) -> String {
        let mut lines = Vec::new();
        for change in &self.changes {
            let icon = match change.severity {
                ChangeSeverity::Info => "ℹ️",
                ChangeSeverity::Warning => "⚠️",
                ChangeSeverity::Danger => "🔴",
            };
            lines.push(format!("{} [{}] {}", icon, change.component, change.description));
        }
        lines.join("\n")
    }
}
