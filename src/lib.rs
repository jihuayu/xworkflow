//! # XWorkflow — A Dify-compatible Workflow Engine
//!
//! `xworkflow` is a high-performance, extensible workflow engine written in Rust.
//! It implements a Dify-compatible DSL for defining directed-acyclic-graph (DAG)
//! based workflows with support for:
//!
//! - **Node execution**: Start, End, Answer, IfElse, Code, Template, HTTP, LLM,
//!   Variable Aggregator/Assigner, Iteration, Loop, List Operator, and more.
//! - **Streaming**: First-class support for LLM streaming responses that propagate
//!   through downstream nodes.
//! - **Plugin system**: Dynamic loading of node executors, LLM providers, sandboxes,
//!   template engines, and lifecycle hooks via a two-phase plugin architecture.
//! - **Sandboxed code execution**: JavaScript (via Boa / V8) and WASM sandboxes with
//!   configurable resource limits.
//! - **Security**: Optional security layer with resource governors, credential
//!   providers, network policies, audit logging, and selector validation.
//! - **Debugger mode**: Interactive breakpoint-based debugging with step/continue
//!   controls and variable inspection.
//! - **DSL validation**: Three-layer validation (structure, topology, semantics)
//!   with rich diagnostics.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use xworkflow::{WorkflowRunner, parse_dsl, DslFormat};
//!
//! #[tokio::main]
//! async fn main() {
//!     let yaml = std::fs::read_to_string("workflow.yaml").unwrap();
//!     let schema = parse_dsl(&yaml, DslFormat::Yaml).unwrap();
//!     let handle = WorkflowRunner::builder(schema)
//!         .run()
//!         .await
//!         .unwrap();
//!     let status = handle.wait().await;
//!     println!("{:?}", status);
//! }
//! ```
//!
//! # Feature Flags
//!
//! | Flag | Description |
//! |------|-------------|
//! | `security` | Enables the security subsystem (policies, governors, audit) |
//! | `plugin-system` | Enables dynamic plugin loading via `libloading` |
//! | `builtin-sandbox-js` | Bundles the JavaScript sandbox (Boa engine) |
//! | `builtin-sandbox-wasm` | Bundles the WASM sandbox |
//! | `builtin-template-jinja` | Bundles the Jinja2 template engine |
//! | `builtin-core-nodes` | Registers Start, End, Answer, IfElse executors |
//! | `builtin-transform-nodes` | Registers Template, Aggregator, Assigner executors |
//! | `builtin-http-node` | Registers the HTTP Request executor |
//! | `builtin-code-node` | Registers the Code (JS sandbox) executor |
//! | `builtin-subgraph-nodes` | Registers Iteration, Loop, ListOperator executors |
//! | `builtin-llm-node` | Registers the LLM executor |

pub mod compiler;
pub mod core;
pub mod dsl;
pub mod error;
pub mod evaluator;
pub mod graph;
pub mod llm;
#[cfg(feature = "memory")]
pub mod memory;
#[cfg(feature = "builtin-agent-node")]
pub mod mcp;
pub mod nodes;
#[cfg(feature = "plugin-system")]
pub mod plugin_system;
pub mod sandbox;
#[cfg(feature = "security")]
pub mod security;
pub mod template;

// New architecture layers (see docs/architecture-reorganization-design.md)
pub mod api;
pub mod application;
pub mod domain;
pub mod engine;
pub mod infrastructure;

pub use crate::api::{WorkflowHandle, WorkflowRunner, WorkflowRunnerBuilder};
#[cfg(feature = "workflow-cache")]
pub use crate::compiler::{
    CacheKey, CacheStats, GroupCacheStats, WorkflowCache, WorkflowCacheConfig,
};
pub use crate::compiler::{
    CompiledConfig, CompiledNodeConfig, CompiledNodeConfigMap, CompiledWorkflow,
    CompiledWorkflowRunnerBuilder, WorkflowCompiler,
};
pub use crate::core::dispatcher::{Command, EngineConfig};
#[cfg(feature = "checkpoint")]
pub use crate::core::{
    ChangeSeverity, Checkpoint, CheckpointError, CheckpointStore, ContextFingerprint,
    EnvironmentChange, FileCheckpointStore, MemoryCheckpointStore, ResumeDiagnostic, ResumePolicy,
    SerializableEdgeState,
};
pub use crate::core::{
    DefaultSandboxPool, DefaultSubGraphRunner, FakeIdGenerator, FakeTimeProvider, GraphEngineEvent,
    HttpClientProvider, HttpPoolConfig, IdGenerator, RealIdGenerator, RealTimeProvider,
    RuntimeContext, RuntimeGroup, RuntimeGroupBuilder, SafeStopSignal, SandboxPool, Segment,
    SegmentType, SubGraphRunner, TimeProvider, VariablePool, WorkflowContext, WorkflowDispatcher,
};
pub use crate::domain::execution::ExecutionStatus;
pub use crate::dsl::{
    parse_dsl, validate_dsl, validate_schema, Diagnostic, DiagnosticLevel, DslFormat,
    ValidationReport, WorkflowSchema,
};
pub use crate::error::{NodeError, WorkflowError};
pub use crate::graph::{build_graph, Graph};
pub use crate::nodes::NodeExecutorRegistry;
#[cfg(feature = "memory")]
pub use crate::memory::*;
#[cfg(feature = "plugin-system")]
pub use crate::plugin_system::{
    Plugin, PluginCategory, PluginContext, PluginError, PluginLoadSource, PluginLoader,
    PluginMetadata, PluginPhase, PluginRegistry, PluginSource, PluginSystemConfig,
};
#[cfg(feature = "security")]
pub use crate::security::*;
