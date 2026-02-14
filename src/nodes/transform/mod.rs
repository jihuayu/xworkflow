//! Data transformation node executors: Template, Aggregator, Assigner, HTTP, Code.
//!
//! Each executor lives in its own submodule for maintainability.

pub mod aggregator;
pub mod assigner;
pub mod code;
pub(crate) mod helpers;
pub mod http;
pub mod template;

pub use aggregator::{LegacyVariableAggregatorExecutor, VariableAggregatorExecutor};
pub use assigner::VariableAssignerExecutor;
pub use code::CodeNodeExecutor;
pub use http::HttpRequestExecutor;
pub use template::TemplateTransformExecutor;
