//! Execution-phase gates — plugin and security enforcement during node execution.

pub use crate::core::plugin_gate::{NoopPluginGate, PluginGate};
pub use crate::core::security_gate::{NoopSecurityGate, SecurityGate};

#[cfg(feature = "plugin-system")]
pub use crate::core::plugin_gate::new_plugin_gate_with_registry;
pub use crate::core::security_gate::new_security_gate;
