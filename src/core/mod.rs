//! Core engine components for workflow execution.
//!
//! This module contains the fundamental building blocks of the XWorkflow engine:
//!
//! - [`variable_pool`] — The Dify-compatible variable type system (`Segment`, `VariablePool`,
//!   `SegmentStream`) used including streaming support.
//! - [`dispatcher`] — The main workflow dispatcher that drives DAG-based graph execution.
//! - [`event_bus`] — Event types emitted during workflow execution for observability.
//! - [`runtime_context`] — Runtime context providing time, ID generation, and extension points.
//! - [`sub_graph_runner`] — Sub-graph execution for iteration/loop containers.
//! - [`debug`] — Interactive debugger gate/hook traits and implementations.
//! - [`security_gate`] — Security enforcement gate applied before/after node execution.
//! - [`plugin_gate`] — Plugin hook gate applied around workflow/node lifecycle.

pub mod event_bus;
pub mod variable_pool;
pub mod dispatcher;
pub mod runtime_context;
pub mod sub_graph_runner;
pub mod debug;
pub mod security_gate;
pub mod plugin_gate;

pub use variable_pool::{
	FileSegment,
	Segment,
	SegmentStream,
	SegmentType,
	Selector,
	SCOPE_NODE_ID,
	StreamEvent,
	StreamReader,
	StreamStatus,
	StreamWriter,
	VariablePool,
};
pub use event_bus::{GraphEngineEvent, PauseReason};
pub use dispatcher::WorkflowDispatcher;
pub use runtime_context::{RuntimeContext, TimeProvider, IdGenerator, RealTimeProvider, RealIdGenerator, FakeTimeProvider, FakeIdGenerator};
pub use sub_graph_runner::{DefaultSubGraphRunner, SubGraphRunner};
pub use debug::{
	DebugAction,
	DebugCommand,
	DebugConfig,
	DebugEvent,
	DebugHandle,
	DebugHook,
	DebugGate,
	DebugState,
	DebugError,
	NoopGate,
	NoopHook,
	InteractiveDebugGate,
	InteractiveDebugHook,
	PauseLocation,
	PauseReason as DebugPauseReason,
};
