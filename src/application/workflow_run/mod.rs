//! Workflow run orchestration shared by schema-run and compiled-run flows.

mod handle;
mod orchestrator;
mod value_conversion;

pub use handle::WorkflowHandle;
pub(crate) use orchestrator::{
    run_workflow, run_workflow_debug, WorkflowGraphSpec, WorkflowRunOptions, WorkflowRunSpec,
};

pub(crate) use value_conversion::segment_from_type;
