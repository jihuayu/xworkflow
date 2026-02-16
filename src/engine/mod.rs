//! Engine layer — execution kernel.
//!
//! This module re-exports the core engine components for workflow execution:
//! dispatcher, runtime context/group, execution-phase gates, and sub-graph runner.

pub mod dispatcher;
pub mod gates;
pub mod runtime;
pub mod subgraph;
