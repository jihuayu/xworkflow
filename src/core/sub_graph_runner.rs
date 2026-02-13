//! Sub-graph runner for container nodes (Iteration, Loop).
//!
//! A sub-graph is a self-contained mini-workflow embedded inside a container node.
//! The [`SubGraphRunner`] trait abstracts how sub-graphs are executed so that
//! tests can substitute custom implementations.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::core::dispatcher::{EngineConfig, EventEmitter, WorkflowDispatcher};
use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Segment, VariablePool};
use crate::graph::types::{Graph, GraphEdge, GraphNode, GraphTopology};
use crate::nodes::subgraph::{SubGraphDefinition, SubGraphError, SubGraphNode};
use crate::nodes::utils::selector_from_str;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Trait for executing embedded sub-graphs within container nodes.
#[async_trait]
pub trait SubGraphRunner: Send + Sync {
    /// Run the given sub-graph definition with scoped variables derived from
    /// `parent_pool` and `scope_vars`, returning the aggregated outputs.
    async fn run_sub_graph(
        &self,
        sub_graph: &SubGraphDefinition,
        parent_pool: &VariablePool,
        scope_vars: HashMap<String, Value>,
        context: &RuntimeContext,
    ) -> Result<Value, SubGraphError>;
}

/// Default [`SubGraphRunner`] implementation that builds a fresh
/// [`WorkflowDispatcher`] and runs the sub-graph to completion.
pub struct DefaultSubGraphRunner;

#[async_trait]
impl SubGraphRunner for DefaultSubGraphRunner {
    async fn run_sub_graph(
        &self,
        sub_graph: &SubGraphDefinition,
        parent_pool: &VariablePool,
        scope_vars: HashMap<String, Value>,
        context: &RuntimeContext,
    ) -> Result<Value, SubGraphError> {
        execute_sub_graph_default(sub_graph, parent_pool, scope_vars, context).await
    }
}

async fn execute_sub_graph_default(
    sub_graph: &SubGraphDefinition,
    parent_pool: &VariablePool,
    scope_vars: HashMap<String, Value>,
    context: &RuntimeContext,
) -> Result<Value, SubGraphError> {
    let mut scoped_pool = parent_pool.clone();
    inject_scope_vars(&mut scoped_pool, scope_vars)?;

    let graph = build_sub_graph(sub_graph)?;
    let registry = Arc::clone(context.node_executor_registry());
    let (tx, _rx) = mpsc::channel(16);
    let active = Arc::new(AtomicBool::new(false));
    let emitter = EventEmitter::new(tx.clone(), active);
    let sub_context = context.clone().with_event_tx(tx.clone());

    let mut dispatcher = WorkflowDispatcher::new_with_registry(
        graph,
        scoped_pool,
        registry,
        emitter,
        EngineConfig::default(),
        Arc::new(sub_context),
        #[cfg(feature = "plugin-system")]
        None,
    );

    let outputs = dispatcher
        .run()
        .await
        .map_err(|e| SubGraphError::ExecutionFailed(e.to_string()))?;

    Ok(Value::Object(outputs.into_iter().collect()))
}

fn inject_scope_vars(
    pool: &mut VariablePool,
    scope_vars: HashMap<String, Value>,
) -> Result<(), SubGraphError> {
    for (key, value) in scope_vars {
        let selector = if let Some(sel) = selector_from_str(&key) {
            sel
        } else {
            return Err(SubGraphError::ScopeError(format!(
                "Invalid scope key: {}",
                key
            )));
        };

        pool.set(&selector, Segment::from_value(&value));
    }

    Ok(())
}

fn resolve_node_type(node: &SubGraphNode) -> Option<String> {
    if let Some(nt) = &node.node_type {
        return Some(map_node_type_alias(nt));
    }
    if let Value::Object(map) = &node.data {
        if let Some(Value::String(t)) = map.get("type") {
            return Some(map_node_type_alias(t));
        }
    }
    None
}

fn map_node_type_alias(node_type: &str) -> String {
    match node_type {
        "template" => "template-transform".to_string(),
        other => other.to_string(),
    }
}

fn build_sub_graph(sub_graph: &SubGraphDefinition) -> Result<Graph, SubGraphError> {
    let mut nodes = HashMap::new();
    let mut edges = HashMap::new();
    let mut in_edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut out_edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut root_node_id = String::new();

    for (idx, n) in sub_graph.nodes.iter().enumerate() {
        let node_type = resolve_node_type(n).ok_or_else(|| {
            SubGraphError::ExecutionFailed("Sub-graph node type is missing".to_string())
        })?;
        let mut config = serde_json::Map::new();
        config.insert("type".to_string(), Value::String(node_type.clone()));
        if let Some(title) = &n.title {
            config.insert("title".to_string(), Value::String(title.clone()));
        }
        if let Value::Object(map) = &n.data {
            for (k, v) in map {
                if k == "type" {
                    continue;
                }
                config.insert(k.clone(), v.clone());
            }
        }

        let node = GraphNode {
            id: n.id.clone(),
            node_type: node_type.clone(),
            title: n.title.clone().unwrap_or_else(|| format!("node_{}", idx)),
            config: Value::Object(config),
            version: "1".to_string(),
            error_strategy: None,
            retry_config: None,
            timeout_secs: None,
        };

        if node_type == "start" {
            root_node_id = n.id.clone();
        }

        in_edges.entry(n.id.clone()).or_default();
        out_edges.entry(n.id.clone()).or_default();
        nodes.insert(n.id.clone(), node);
    }

    if root_node_id.is_empty() {
        return Err(SubGraphError::NoStartNode);
    }

    let mut has_end = false;
    for n in sub_graph.nodes.iter() {
        if let Some(nt) = resolve_node_type(n) {
            if nt == "end" {
                has_end = true;
                break;
            }
        }
    }
    if !has_end {
        return Err(SubGraphError::NoEndNode);
    }

    for (idx, e) in sub_graph.edges.iter().enumerate() {
        let edge_id = if e.id.is_empty() {
            format!("edge_{}", idx)
        } else {
            e.id.clone()
        };
        if !nodes.contains_key(&e.source) || !nodes.contains_key(&e.target) {
            return Err(SubGraphError::InvalidEdge);
        }
        let edge = GraphEdge {
            id: edge_id.clone(),
            source_node_id: e.source.clone(),
            target_node_id: e.target.clone(),
            source_handle: e.source_handle.clone(),
        };

        in_edges
            .entry(e.target.clone())
            .or_default()
            .push(edge_id.clone());
        out_edges
            .entry(e.source.clone())
            .or_default()
            .push(edge_id.clone());
        edges.insert(edge_id, edge);
    }

    let topology = GraphTopology {
        nodes,
        edges,
        in_edges,
        out_edges,
        root_node_id,
    };

    Ok(Graph::from_topology(Arc::new(topology)))
}
