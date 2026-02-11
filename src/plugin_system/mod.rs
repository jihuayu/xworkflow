//! Dynamic plugin system for extending xworkflow at runtime.
//!
//! Plugins can register node executors, LLM providers, template engines,
//! code sandboxes, hooks, and DSL validators. Loading happens in two phases:
//!
//! 1. **Bootstrap** — core infrastructure plugins (sandboxes, template engines).
//! 2. **Normal** — business-level plugins (custom nodes, LLM providers, hooks).
//!
//! See [`PluginRegistry`] for the central coordination point and
//! [`Plugin`] for the trait that all plugins must implement.

pub mod config;
pub mod context;
pub mod error;
pub mod extensions;
pub mod hooks;
pub mod loader;
pub mod registry;
pub mod traits;
pub mod macros;
pub mod loaders;
pub mod builtins;
#[cfg(feature = "wasm-runtime")]
pub use xworkflow_plugin_wasm as wasm;

pub use config::PluginSystemConfig;
pub use context::PluginContext;
pub use error::PluginError;
pub use extensions::{DslValidator, TemplateFunction};
pub use xworkflow_types::template::{CompiledTemplateHandle, TemplateEngine};
pub use hooks::{HookHandler, HookPayload, HookPoint};
pub use loader::{PluginLoadSource, PluginLoader};
pub use registry::{PluginPhase, PluginRegistry};
pub use traits::{Plugin, PluginCategory, PluginMetadata, PluginSource, PluginCapabilities};
