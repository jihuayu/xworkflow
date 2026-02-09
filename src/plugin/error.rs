use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("Plugin not found: {0}")]
    NotFound(String),

    #[error("Invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("WASM compilation error: {0}")]
    CompilationError(String),

    #[error("WASM instantiation error: {0}")]
    InstantiationError(String),

    #[error("WASM execution error: {0}")]
    ExecutionError(String),

    #[error("Capability denied: {0}")]
    CapabilityDenied(String),

    #[error("Plugin timeout")]
    Timeout,

    #[error("Memory limit exceeded")]
    MemoryLimitExceeded,

    #[error("Fuel exhausted (instruction limit)")]
    FuelExhausted,

    #[error("Missing export function: {0}")]
    MissingExport(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Plugin already loaded: {0}")]
    AlreadyLoaded(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}
