//! Bootstrap gates — startup-phase plugin and security orchestration.
//!
//! These gates are invoked during the workflow builder phase (before execution
//! starts) to initialize plugins, validate schemas with security policies,
//! and configure the runtime.

pub mod plugin_bootstrap;
pub mod security_bootstrap;

pub use plugin_bootstrap::{new_scheduler_plugin_gate, SchedulerPluginGate};
pub use security_bootstrap::{new_scheduler_security_gate, SchedulerSecurityGate};
