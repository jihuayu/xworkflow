pub mod core;
pub mod dsl;
pub mod error;
pub mod evaluator;
pub mod graph;
pub mod nodes;
pub mod template;
pub mod scheduler;

pub use crate::core::{GraphEngineEvent, Segment, VariablePool, WorkflowDispatcher};
pub use crate::dsl::{parse_dsl, DslFormat, WorkflowSchema};
pub use crate::error::{NodeError, WorkflowError};
pub use crate::graph::{build_graph, Graph};
pub use crate::nodes::NodeExecutorRegistry;
pub use crate::core::dispatcher::{Command, EngineConfig};
