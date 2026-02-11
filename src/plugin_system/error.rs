//! Error types for the plugin system.

use thiserror::Error;

use super::registry::PluginPhase;

/// Errors that can occur during plugin loading, registration, or execution.
#[derive(Error, Debug)]
pub enum PluginError {
    #[error("Invalid config: {0}")]
    InvalidConfig(String),
    #[error("Load error: {0}")]
    LoadError(String),
    #[error("Missing export: {0}")]
    MissingExport(String),
    #[error("ABI version mismatch: expected {expected}, actual {actual}")]
    AbiVersionMismatch { expected: u32, actual: u32 },
    #[error("Plugin conflict: {0}")]
    ConflictError(String),
    #[error("Wrong phase: expected {expected:?}, actual {actual:?}")]
    WrongPhase { expected: PluginPhase, actual: PluginPhase },
    #[error("Capability denied: {0}")]
    CapabilityDenied(String),
    #[error("Plugin not found: {0}")]
    NotFound(String),
    #[error("Register error: {0}")]
    RegisterError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_error_display() {
        assert!(PluginError::InvalidConfig("bad".into()).to_string().contains("bad"));
        assert!(PluginError::LoadError("fail".into()).to_string().contains("fail"));
        assert!(PluginError::MissingExport("fn".into()).to_string().contains("fn"));
        assert!(PluginError::ConflictError("dup".into()).to_string().contains("dup"));
        assert!(PluginError::CapabilityDenied("http".into()).to_string().contains("http"));
        assert!(PluginError::NotFound("p1".into()).to_string().contains("p1"));
        assert!(PluginError::RegisterError("err".into()).to_string().contains("err"));
    }

    #[test]
    fn test_plugin_error_abi_mismatch() {
        let err = PluginError::AbiVersionMismatch { expected: 1, actual: 2 };
        let msg = err.to_string();
        assert!(msg.contains("1"));
        assert!(msg.contains("2"));
    }

    #[test]
    fn test_plugin_error_wrong_phase() {
        let err = PluginError::WrongPhase {
            expected: PluginPhase::Bootstrap,
            actual: PluginPhase::Normal,
        };
        let msg = err.to_string();
        assert!(msg.contains("Bootstrap"));
        assert!(msg.contains("Normal"));
    }
}
