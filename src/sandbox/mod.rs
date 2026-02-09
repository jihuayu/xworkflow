//! Code Sandbox Module
//!
//! Provides a unified interface for executing user code in sandboxed environments.
//! Supports multiple implementations: built-in JS evaluator, V8 (feature-gated), WASM, remote, etc.

pub mod types;
pub mod error;
pub mod manager;
pub mod builtin;
pub mod js_builtins;
pub mod wasm_sandbox;

pub use types::*;
pub use error::SandboxError;
pub use manager::{SandboxManager, SandboxManagerConfig};
pub use builtin::BuiltinSandbox;
pub use wasm_sandbox::{WasmSandbox, WasmSandboxConfig};
