//! Compatibility shim for legacy `RuntimeContext`.
//!
//! New code should use [`WorkflowContext`] and [`RuntimeGroup`] instead.

pub use crate::core::workflow_context::{
    FakeIdGenerator,
    FakeTimeProvider,
    IdGenerator,
    RealIdGenerator,
    RealTimeProvider,
    TimeProvider,
    WorkflowContext,
};

pub type RuntimeContext = WorkflowContext;
