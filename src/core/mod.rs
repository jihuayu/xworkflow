//! Core engine components for workflow execution.
//!
//! This module contains the fundamental building blocks of the XWorkflow engine:
//!
//! - [`variable_pool`] — The Dify-compatible variable type system (`Segment`, `VariablePool`,
//!   `SegmentStream`) used including streaming support.
//! - [`dispatcher`] — The main workflow dispatcher that drives DAG-based graph execution.
//! - [`event_bus`] — Event types emitted during workflow execution for observability.
//! - [`runtime_group`] — Shared runtime resources for multiple workflows.
//! - [`workflow_context`] — Per-workflow execution context referencing a runtime group.
//! - [`runtime_context`] — Compatibility shim for legacy `RuntimeContext` usage.
//! - [`sub_graph_runner`] — Sub-graph execution for iteration/loop containers.
//! - [`debug`] — Interactive debugger gate/hook traits and implementations.
//! - [`security_gate`] — Security enforcement gate applied before/after node execution.
//! - [`plugin_gate`] — Plugin hook gate applied around workflow/node lifecycle.

#[cfg(feature = "checkpoint")]
pub mod checkpoint;
pub mod debug;
pub mod dispatcher;
pub mod event_bus;
pub mod http_client;
pub mod plugin_gate;
pub mod runtime_context;
pub mod runtime_group;
pub mod safe_stop;
pub mod security_gate;
pub mod sub_graph_runner;
pub mod variable_pool;
pub mod workflow_context;

#[cfg(feature = "checkpoint")]
pub use checkpoint::{
    ChangeSeverity, Checkpoint, CheckpointError, CheckpointStore, ContextFingerprint,
    EnvironmentChange, FileCheckpointStore, MemoryCheckpointStore, ResumeDiagnostic, ResumePolicy,
    SerializableEdgeState,
};
pub use debug::{
    DebugAction, DebugCommand, DebugConfig, DebugError, DebugEvent, DebugGate, DebugHandle,
    DebugHook, DebugState, InteractiveDebugGate, InteractiveDebugHook, NoopGate, NoopHook,
    PauseLocation, PauseReason as DebugPauseReason,
};
pub use dispatcher::WorkflowDispatcher;
pub use event_bus::{GraphEngineEvent, PauseReason};
pub use http_client::{HttpClientProvider, HttpPoolConfig};
pub use runtime_context::RuntimeContext;
pub use runtime_group::{DefaultSandboxPool, RuntimeGroup, RuntimeGroupBuilder, SandboxPool};
pub use safe_stop::SafeStopSignal;
pub use sub_graph_runner::{DefaultSubGraphRunner, SubGraphRunner};
pub use variable_pool::{
    FileSegment, FileTransferMethod, Segment, SegmentStream, SegmentType, Selector, StreamEvent,
    StreamReader, StreamStatus, StreamWriter, VariablePool, SCOPE_NODE_ID,
};
pub use workflow_context::{
    FakeIdGenerator, FakeTimeProvider, IdGenerator, RealIdGenerator, RealTimeProvider,
    TimeProvider, WorkflowContext,
};
