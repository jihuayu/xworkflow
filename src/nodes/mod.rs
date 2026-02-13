//! Node executor registry and built-in node implementations.
//!
//! Each workflow node type (Start, End, IfElse, Code, HTTP, etc.) is implemented
//! as a [`NodeExecutor`] trait object registered in the [`NodeExecutorRegistry`].
//!
//! Sub-modules:
//! - [`control_flow`] — Start, End, Answer, IfElse executors.
//! - [`data_transform`] — Template, Aggregator, Assigner, HTTP, Code executors.
//! - [`subgraph`] — Sub-graph definition and executor for embedded mini-workflows.
//! - [`subgraph_nodes`] — Iteration, Loop, ListOperator container executors.

#[cfg(feature = "builtin-agent-node")]
pub mod agent;
pub mod control_flow;
pub mod data_transform;
pub mod document_extract;
pub mod executor;
pub mod gather;
pub mod human_input;
pub mod subgraph;
pub mod subgraph_nodes;
#[cfg(feature = "builtin-agent-node")]
#[allow(clippy::result_large_err)]
pub mod tool;
pub mod utils;

pub use executor::*;
