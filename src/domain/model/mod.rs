//! Protocol-stable model types shared across layers.

mod condition;
mod flow;
mod segment_type;
mod selector;
mod subgraph;

pub use condition::{ComparisonOperator, IterationErrorMode};
pub use flow::{
    IterationNodeConfig, ListOperation, ListOperatorNodeConfig, LoopConditionConfig,
    LoopNodeConfig, SortOrder,
};
pub use segment_type::SegmentType;
pub use selector::{Selector, SCOPE_NODE_ID};
pub use subgraph::{SubGraphDefinition, SubGraphEdge, SubGraphNode};
