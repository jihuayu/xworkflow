pub mod core;
pub mod dsl;
pub mod error;
pub mod evaluator;
pub mod graph;
pub mod nodes;
pub mod sandbox;
pub mod template;
pub mod scheduler;
pub mod llm;
#[cfg(feature = "security")]
pub mod security;
#[cfg(feature = "plugin-system")]
pub mod plugin_system;

pub use crate::core::{
	GraphEngineEvent,
	Segment,
	SegmentType,
	VariablePool,
	WorkflowDispatcher,
	RuntimeContext,
	SubGraphRunner,
	DefaultSubGraphRunner,
	TimeProvider,
	IdGenerator,
	RealTimeProvider,
	RealIdGenerator,
	FakeTimeProvider,
	FakeIdGenerator,
};
pub use crate::dsl::{
	parse_dsl,
	validate_dsl,
	validate_schema,
	Diagnostic,
	DiagnosticLevel,
	ValidationReport,
	DslFormat,
	WorkflowSchema,
};
pub use crate::error::{NodeError, WorkflowError};
pub use crate::graph::{build_graph, Graph};
pub use crate::nodes::NodeExecutorRegistry;
pub use crate::core::dispatcher::{Command, EngineConfig};
pub use crate::scheduler::{ExecutionStatus, WorkflowHandle, WorkflowRunner, WorkflowRunnerBuilder};
#[cfg(feature = "security")]
pub use crate::security::*;
#[cfg(feature = "plugin-system")]
pub use crate::plugin_system::{
	Plugin,
	PluginCategory,
	PluginContext,
	PluginError,
	PluginLoadSource,
	PluginLoader,
	PluginMetadata,
	PluginPhase,
	PluginRegistry,
	PluginSource,
	PluginSystemConfig,
};
