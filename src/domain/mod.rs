//! Domain layer — pure domain model and shared types.
//!
//! This layer contains types that are used across multiple layers of the system
//! but do not depend on any runtime implementation details.
//!
//! Submodules:
//! - [`execution`] — Execution status and output models.
//! - [`model`] — Protocol-stable types (selectors, segment types, sub-graph definitions).

pub mod execution;
pub mod model;
