//! Infrastructure layer — pluggable implementations.
//!
//! This layer holds concrete implementations of infrastructure concerns:
//! plugin system, sandbox engines, security providers, and LLM adapters.
//! These are accessed via traits/ports defined in the engine or domain layers.

/// Plugin system — dynamic plugin loading and lifecycle management.
#[cfg(feature = "plugin-system")]
pub use crate::plugin_system;

/// Sandbox engines — JavaScript and WASM sandboxed execution.
pub use crate::sandbox;

/// Security subsystem — policies, governors, audit, credentials.
#[cfg(feature = "security")]
pub use crate::security;

/// LLM provider adapters.
pub use crate::llm;
