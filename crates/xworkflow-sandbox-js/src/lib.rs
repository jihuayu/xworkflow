pub mod builtins;
#[cfg(feature = "security")]
pub mod security;
pub mod sandbox;

pub use sandbox::{BuiltinSandbox, BuiltinSandboxConfig};

// Re-export boa_engine for consumers that need direct access (e.g., streaming runtime)
pub use boa_engine;
