//! Code Sandbox Module
//!
//! Provides a unified interface for executing user code in sandboxed environments.
//! Supports multiple implementations: built-in JS evaluator, V8 (feature-gated), WASM, remote, etc.

pub mod error;
pub mod manager;
pub mod types;

pub use error::SandboxError;
pub use manager::{SandboxManager, SandboxManagerConfig};
pub use types::*;

#[cfg(feature = "builtin-sandbox-js")]
pub use xworkflow_sandbox_js::builtins as js_builtins;
#[cfg(feature = "builtin-sandbox-js")]
pub use xworkflow_sandbox_js::{BuiltinSandbox, BuiltinSandboxConfig};

#[cfg(feature = "builtin-sandbox-wasm")]
pub use xworkflow_sandbox_wasm::{WasmSandbox, WasmSandboxConfig};
