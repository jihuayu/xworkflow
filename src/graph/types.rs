//! Graph data structures.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::dsl::schema::{EdgeHandle, ErrorStrategyConfig, RetryConfig};

// ================================
// Edge Traversal State
// ================================

/// Traversal state of a graph edge during execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeTraversalState {
    /// Not yet resolved.
    Pending,
    /// The edge was taken (data flows through).
    Taken,
    /// The edge was skipped (e.g. IfElse branch not selected).
    Skipped,
}

// ================================
// Graph Node
// ================================

/// A node in the execution graph, carrying type and config.
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: String,
    pub node_type: String,
    pub title: String,
    pub config: Value,
    pub version: String,
    pub error_strategy: Option<ErrorStrategyConfig>,
    pub retry_config: Option<RetryConfig>,
    pub timeout_secs: Option<u64>,
}

// ================================
// Graph Edge
// ================================

/// A directed edge in the execution graph.
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub id: String,
    pub source_node_id: String,
    pub target_node_id: String,
    pub source_handle: Option<String>,
}

// ================================
// Graph
// ================================

/// Immutable topology shared across workflow executions.
#[derive(Debug, Clone)]
pub struct GraphTopology {
    pub nodes: HashMap<String, GraphNode>,
    pub edges: HashMap<String, GraphEdge>,
    pub in_edges: HashMap<String, Vec<String>>,   // node_id -> [edge_id]
    pub out_edges: HashMap<String, Vec<String>>,  // node_id -> [edge_id]
    pub root_node_id: String,
}

/// Execution DAG state for a single workflow run.
#[derive(Debug, Clone)]
pub struct Graph {
    pub topology: Arc<GraphTopology>,
    pub node_states: HashMap<String, EdgeTraversalState>,
    pub edge_states: HashMap<String, EdgeTraversalState>,
}

impl Graph {
    /// Create a new execution graph from a shared topology.
    pub fn from_topology(topology: Arc<GraphTopology>) -> Self {
        let node_states = topology
            .nodes
            .keys()
            .map(|id| (id.clone(), EdgeTraversalState::Pending))
            .collect();
        let edge_states = topology
            .edges
            .keys()
            .map(|id| (id.clone(), EdgeTraversalState::Pending))
            .collect();
        Self {
            topology,
            node_states,
            edge_states,
        }
    }

    /// Access the shared topology.
    pub fn topology(&self) -> &GraphTopology {
        &self.topology
    }

    /// Return the root (start) node id.
    pub fn root_node_id(&self) -> &str {
        &self.topology.root_node_id
    }

    /// Get node by ID.
    pub fn get_node(&self, node_id: &str) -> Option<&GraphNode> {
        self.topology.nodes.get(node_id)
    }

    /// Get edge by ID.
    pub fn get_edge(&self, edge_id: &str) -> Option<&GraphEdge> {
        self.topology.edges.get(edge_id)
    }

    /// Get node traversal state.
    pub fn node_state(&self, node_id: &str) -> Option<EdgeTraversalState> {
        self.node_states.get(node_id).copied()
    }

    /// Set node traversal state.
    pub fn set_node_state(&mut self, node_id: &str, state: EdgeTraversalState) {
        if let Some(entry) = self.node_states.get_mut(node_id) {
            *entry = state;
        }
    }

    /// Get edge traversal state.
    pub fn edge_state(&self, edge_id: &str) -> Option<EdgeTraversalState> {
        self.edge_states.get(edge_id).copied()
    }

    /// Set edge traversal state.
    pub fn set_edge_state(&mut self, edge_id: &str, state: EdgeTraversalState) {
        if let Some(entry) = self.edge_states.get_mut(edge_id) {
            *entry = state;
        }
    }

    /// Check if a node is ready (all in-edges resolved, at least one taken)
    pub fn is_node_ready(&self, node_id: &str) -> bool {
        let edge_ids = match self.topology.in_edges.get(node_id) {
            Some(ids) if !ids.is_empty() => ids,
            None => return node_id == self.root_node_id(), // root has no in-edges
            _ => return node_id == self.root_node_id(),
        };
        let (mut all_resolved, mut any_taken) = (true, false);
        for eid in edge_ids {
            match self.edge_state(eid) {
                Some(EdgeTraversalState::Pending) => {
                    all_resolved = false;
                    break;
                }
                Some(EdgeTraversalState::Taken) => {
                    any_taken = true;
                }
                _ => {}
            }
        }
        all_resolved && any_taken
    }

    /// Process normal (non-branch) edges after node completion: mark all out-edges as Taken
    pub fn process_normal_edges(&mut self, node_id: &str) {
        let edge_ids = self
            .topology
            .out_edges
            .get(node_id)
            .map(|ids| ids.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for eid in edge_ids {
            self.set_edge_state(&eid, EdgeTraversalState::Taken);
        }
    }

    /// Process branch edges: mark the selected branch as Taken, others as Skipped
    pub fn process_branch_edges(&mut self, node_id: &str, selected_handle: &EdgeHandle) {
        let selected_handle = match selected_handle {
            EdgeHandle::Branch(handle) => handle.as_str(),
            EdgeHandle::Default => "source",
        };
        let edge_ids = match self.topology.out_edges.get(node_id) {
            Some(ids) => ids.iter().cloned().collect::<Vec<_>>(),
            None => return,
        };
        let mut skipped_targets = Vec::new();
        for eid in &edge_ids {
            let (handle, target) = match self.topology.edges.get(eid) {
                Some(edge) => (edge.source_handle.clone(), edge.target_node_id.clone()),
                None => continue,
            };
            let handle = handle.as_deref().unwrap_or("source");
            if handle == selected_handle {
                self.set_edge_state(eid, EdgeTraversalState::Taken);
            } else {
                self.set_edge_state(eid, EdgeTraversalState::Skipped);
                skipped_targets.push(target);
            }
        }
        // Propagate skip to non-selected branches
        for target in skipped_targets {
            self.propagate_skip(&target);
        }
    }

    /// Recursively skip downstream nodes if all their in-edges are skipped
    pub fn propagate_skip(&mut self, node_id: &str) {
        // Only skip if ALL in-edges are skipped
        let all_skipped = self.topology.in_edges.get(node_id)
            .map(|eids| {
                !eids.is_empty() && eids.iter().all(|eid| {
                    matches!(self.edge_state(eid), Some(EdgeTraversalState::Skipped))
                })
            })
            .unwrap_or(false);

        if !all_skipped {
            return;
        }

        self.set_node_state(node_id, EdgeTraversalState::Skipped);

        let out_edges = self
            .topology
            .out_edges
            .get(node_id)
            .map(|ids| ids.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut targets = Vec::new();
        for eid in &out_edges {
            let target = match self.topology.edges.get(eid) {
                Some(edge) => edge.target_node_id.clone(),
                None => continue,
            };
            self.set_edge_state(eid, EdgeTraversalState::Skipped);
            targets.push(target);
        }
        for target in targets {
            self.propagate_skip(&target);
        }
    }

    /// Get downstream node IDs
    pub fn downstream_node_ids(&self, node_id: &str) -> impl Iterator<Item = &str> {
        self.topology
            .out_edges
            .get(node_id)
            .into_iter()
            .flat_map(|eids| eids.iter())
            .filter_map(|eid| self.topology.edges.get(eid).map(|e| e.target_node_id.as_str()))
    }

    /// Check if a node type is a branch type
    pub fn is_branch_node(&self, node_id: &str) -> bool {
        self.topology
            .out_edges
            .get(node_id)
            .map(|eids| {
                eids.iter().any(|eid| {
                    self.topology
                        .edges
                        .get(eid)
                        .and_then(|e| e.source_handle.as_deref())
                        .map(|h| h != "source")
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }

    /// Check if a branch handle exists for the given node.
    pub fn has_branch_handle(&self, node_id: &str, handle: &str) -> bool {
        self.topology
            .out_edges
            .get(node_id)
            .map(|eids| {
                eids.iter().any(|eid| {
                    self.topology
                        .edges
                        .get(eid)
                        .and_then(|e| e.source_handle.as_deref())
                        == Some(handle)
                })
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_graph() -> Graph {
        let mut nodes = HashMap::new();
        nodes.insert("start".into(), GraphNode {
            id: "start".into(), node_type: "start".into(), title: "Start".into(),
            config: Value::Null, version: "1".into(),
            error_strategy: None, retry_config: None, timeout_secs: None,
        });
        nodes.insert("end".into(), GraphNode {
            id: "end".into(), node_type: "end".into(), title: "End".into(),
            config: Value::Null, version: "1".into(),
            error_strategy: None, retry_config: None, timeout_secs: None,
        });

        let mut edges = HashMap::new();
        edges.insert("e1".into(), GraphEdge {
            id: "e1".into(), source_node_id: "start".into(), target_node_id: "end".into(),
            source_handle: Some("source".into()),
        });

        let mut in_edges = HashMap::new();
        in_edges.insert("end".into(), vec!["e1".into()]);

        let mut out_edges = HashMap::new();
        out_edges.insert("start".into(), vec!["e1".into()]);

        let topology = GraphTopology { nodes, edges, in_edges, out_edges, root_node_id: "start".into() };
        Graph::from_topology(Arc::new(topology))
    }

    #[test]
    fn test_root_ready() {
        let g = make_graph();
        assert!(g.is_node_ready("start"));
    }

    #[test]
    fn test_end_not_ready_initially() {
        let g = make_graph();
        assert!(!g.is_node_ready("end"));
    }

    #[test]
    fn test_process_normal_edges() {
        let mut g = make_graph();
        g.process_normal_edges("start");
        assert!(g.is_node_ready("end"));
    }

    #[test]
    fn test_branch_edges() {
        let mut nodes = HashMap::new();
        nodes.insert("if".into(), GraphNode {
            id: "if".into(), node_type: "if-else".into(), title: "IF".into(),
            config: Value::Null, version: "1".into(),
            error_strategy: None, retry_config: None, timeout_secs: None,
        });
        nodes.insert("a".into(), GraphNode {
            id: "a".into(), node_type: "code".into(), title: "A".into(),
            config: Value::Null, version: "1".into(),
            error_strategy: None, retry_config: None, timeout_secs: None,
        });
        nodes.insert("b".into(), GraphNode {
            id: "b".into(), node_type: "code".into(), title: "B".into(),
            config: Value::Null, version: "1".into(),
            error_strategy: None, retry_config: None, timeout_secs: None,
        });

        let mut edges = HashMap::new();
        edges.insert("e_true".into(), GraphEdge {
            id: "e_true".into(), source_node_id: "if".into(), target_node_id: "a".into(),
            source_handle: Some("case1".into()),
        });
        edges.insert("e_false".into(), GraphEdge {
            id: "e_false".into(), source_node_id: "if".into(), target_node_id: "b".into(),
            source_handle: Some("false".into()),
        });

        let mut in_edges = HashMap::new();
        in_edges.insert("a".into(), vec!["e_true".into()]);
        in_edges.insert("b".into(), vec!["e_false".into()]);

        let mut out_edges = HashMap::new();
        out_edges.insert("if".into(), vec!["e_true".into(), "e_false".into()]);

        let topology = GraphTopology { nodes, edges, in_edges, out_edges, root_node_id: "if".into() };
        let mut g = Graph::from_topology(Arc::new(topology));

        g.process_branch_edges("if", &EdgeHandle::Branch("case1".to_string()));

        assert!(g.is_node_ready("a"));
        assert!(!g.is_node_ready("b"));
        assert_eq!(g.node_state("b"), Some(EdgeTraversalState::Skipped));
    }
}
