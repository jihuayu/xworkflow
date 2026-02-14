//! Container node executors: Iteration, Loop, ListOperator.
//!
//! These nodes contain embedded sub-graphs that are executed via
//! [`SubGraphRunner`](crate::core::SubGraphRunner).

pub mod iteration;
pub mod list_operator;
pub mod loop_node;

pub use crate::domain::model::{
    IterationNodeConfig, ListOperation, ListOperatorNodeConfig, LoopConditionConfig,
    LoopNodeConfig, SortOrder,
};
pub use iteration::IterationNodeExecutor;
pub use list_operator::ListOperatorNodeExecutor;
pub use loop_node::LoopNodeExecutor;

use std::sync::Arc;

use crate::core::runtime_context::RuntimeContext;
use crate::core::sub_graph_runner::{DefaultSubGraphRunner, SubGraphRunner};

pub(crate) fn resolve_sub_graph_runner(context: &RuntimeContext) -> Arc<dyn SubGraphRunner> {
    context
        .sub_graph_runner()
        .cloned()
        .unwrap_or_else(|| Arc::new(DefaultSubGraphRunner))
}
