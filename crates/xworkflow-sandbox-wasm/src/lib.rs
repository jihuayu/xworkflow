pub mod sandbox;

pub use sandbox::{WasmSandbox, WasmSandboxConfig};
pub use wasmtime;

pub fn parse_wat(source: &str) -> Result<Vec<u8>, xworkflow_types::sandbox::SandboxError> {
    wat::parse_str(source)
        .map_err(|e| xworkflow_types::sandbox::SandboxError::CompilationError(e.to_string()))
}
