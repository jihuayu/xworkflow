use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubGraphDefinition {
    pub nodes: Vec<SubGraphNode>,
    pub edges: Vec<SubGraphEdge>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubGraphNode {
    pub id: String,
    #[serde(rename = "type", default)]
    pub node_type: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubGraphEdge {
    #[serde(default)]
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, alias = "sourceHandle", alias = "source_handle")]
    pub source_handle: Option<String>,
}
