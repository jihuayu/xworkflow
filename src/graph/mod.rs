//! Graph construction and representation.
//!
//! The [`Graph`] is built from a validated [`WorkflowSchema`](crate::dsl::WorkflowSchema)
//! by [`build_graph`]. It contains nodes, edges, and adjacency lists used by
//! the [`WorkflowDispatcher`](crate::core::WorkflowDispatcher) to traverse the DAG.

pub mod types;
pub mod builder;

pub use types::*;
pub use builder::*;
