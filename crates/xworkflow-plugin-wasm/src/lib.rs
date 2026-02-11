pub mod error;
pub mod host_functions;
pub mod manifest;
pub mod runtime;

use std::path::Path;

pub use error::PluginError;
pub use host_functions::{EventEmitter, PluginState, VariableAccess, HOST_MODULE};
pub use manifest::{
    AllowedCapabilities,
    PluginCapabilities,
    PluginHook,
    PluginHookType,
    PluginManifest,
    PluginNodeType,
};
pub use runtime::{PluginRuntime, PluginStatus, WasmEngine};

pub fn parse_wat_str(source: &str) -> Result<Vec<u8>, PluginError> {
    wat::parse_str(source).map_err(|e| PluginError::CompilationError(e.to_string()))
}

pub fn read_wasm_bytes(path: &Path) -> Result<Vec<u8>, PluginError> {
    let bytes = std::fs::read(path).map_err(PluginError::IoError)?;
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("wat") {
            let text = String::from_utf8_lossy(&bytes);
            return parse_wat_str(&text);
        }
    }
    Ok(bytes)
}
