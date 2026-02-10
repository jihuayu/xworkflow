use thiserror::Error;

use super::registry::PluginPhase;

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
