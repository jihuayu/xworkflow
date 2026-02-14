//! Application layer — use-case orchestration.
//!
//! This layer coordinates high-level workflows such as:
//! - Running a workflow from a schema or compiled artifact.
//! - Plugin and security bootstrap (startup-phase gates).
//! - Compilation pipeline.
//!
//! It depends on [`engine`](crate::engine) for execution and
//! [`domain`](crate::domain) for shared types.

pub mod bootstrap;
pub mod compile;
pub mod workflow_run;
