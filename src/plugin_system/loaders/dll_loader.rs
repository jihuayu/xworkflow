use async_trait::async_trait;
use libloading::Library;

use super::super::error::PluginError;
use super::super::loader::{PluginLoadSource, PluginLoader};
use super::super::traits::Plugin;

const CURRENT_ABI_VERSION: u32 = 1;

pub struct DllPluginLoader {
    abi_version: u32,
}

impl Default for DllPluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl DllPluginLoader {
    pub fn new() -> Self {
        Self {
            abi_version: CURRENT_ABI_VERSION,
        }
    }

    unsafe fn load_c_abi(&self, library: Library) -> Result<Box<dyn Plugin>, PluginError> {
        if let Ok(version_fn) =
            library.get::<extern "C" fn() -> u32>(b"xworkflow_plugin_abi_version\0")
        {
            let version = version_fn();
            if version != self.abi_version {
                return Err(PluginError::AbiVersionMismatch {
                    expected: self.abi_version,
                    actual: version,
                });
            }
        }

        let create_fn = library
            .get::<extern "C" fn() -> *mut std::ffi::c_void>(b"xworkflow_plugin_create\0")
            .map_err(|e| PluginError::MissingExport(e.to_string()))?;
        let destroy_fn = *library
            .get::<extern "C" fn(*mut std::ffi::c_void)>(b"xworkflow_plugin_destroy\0")
            .map_err(|e| PluginError::MissingExport(e.to_string()))?;

        let raw_ptr = create_fn();
        if raw_ptr.is_null() {
            return Err(PluginError::LoadError("Plugin create returned null".into()));
        }

        // The C ABI plugin returns a *mut c_void that we treat as an opaque handle.
        // We store metadata returned via C FFI functions and delegate via FFI calls.
        // For now, wrap in a CPluginWrapper that holds the raw pointer and destroy fn.
        Ok(Box::new(CPluginWrapper {
            _library: library,
            destroy_fn: SendDestroyFn(destroy_fn),
            raw_ptr: SendRawPtr(raw_ptr),
        }))
    }

    unsafe fn load_rust_abi(&self, library: Library) -> Result<Box<dyn Plugin>, PluginError> {
        let version = *library
            .get::<*const u32>(b"XWORKFLOW_RUST_ABI_VERSION\0")
            .map_err(|e| PluginError::MissingExport(e.to_string()))?;
        if *version != self.abi_version {
            return Err(PluginError::AbiVersionMismatch {
                expected: self.abi_version,
                actual: *version,
            });
        }

        let create_fn = library
            .get::<fn() -> Box<dyn Plugin>>(b"xworkflow_rust_plugin_create\0")
            .map_err(|e| PluginError::MissingExport(e.to_string()))?;

        let inner = create_fn();
        Ok(Box::new(RustDllPluginWrapper {
            inner,
            _library: library,
        }))
    }
}

#[async_trait]
impl PluginLoader for DllPluginLoader {
    fn loader_type(&self) -> &str {
        "dll"
    }

    async fn load(&self, source: &PluginLoadSource) -> Result<Box<dyn Plugin>, PluginError> {
        let path = source
            .params
            .get("path")
            .ok_or_else(|| PluginError::InvalidConfig("DLL path not specified".into()))?;

        unsafe {
            let library = Library::new(path)
                .map_err(|e| PluginError::LoadError(format!("Failed to load DLL: {}", e)))?;

            let is_rust_abi = library
                .get::<*const bool>(b"XWORKFLOW_RUST_PLUGIN\0")
                .is_ok();
            if is_rust_abi {
                self.load_rust_abi(library)
            } else {
                self.load_c_abi(library)
            }
        }
    }
}

// Newtype wrappers to allow Send + Sync for raw pointer and fn pointer
struct SendRawPtr(*mut std::ffi::c_void);
// SAFETY: The raw pointer is only used behind the Library's lifetime and
// the plugin contract guarantees single-ownership semantics.
unsafe impl Send for SendRawPtr {}
unsafe impl Sync for SendRawPtr {}

struct SendDestroyFn(unsafe extern "C" fn(*mut std::ffi::c_void));
unsafe impl Send for SendDestroyFn {}
unsafe impl Sync for SendDestroyFn {}

/// Wrapper for C ABI plugins. Does NOT try to cast the raw pointer to
/// `dyn Plugin` — instead it is an opaque handle that will be freed via
/// the `destroy_fn` on drop.  C ABI plugins are a low-level extension
/// point; full Plugin trait delegation would require additional FFI
/// function pointers for metadata/register/shutdown.  For now the wrapper
/// returns stub metadata indicating it is a C ABI plugin.
struct CPluginWrapper {
    _library: Library,
    destroy_fn: SendDestroyFn,
    raw_ptr: SendRawPtr,
}

impl Drop for CPluginWrapper {
    fn drop(&mut self) {
        unsafe {
            (self.destroy_fn.0)(self.raw_ptr.0);
        }
    }
}

// Static metadata for C ABI plugins
fn c_plugin_metadata() -> super::super::traits::PluginMetadata {
    super::super::traits::PluginMetadata {
        id: "c_abi_plugin".to_string(),
        name: "C ABI Plugin".to_string(),
        version: "0.0.0".to_string(),
        category: super::super::traits::PluginCategory::Normal,
        description: "Loaded via C ABI".to_string(),
        source: super::super::traits::PluginSource::Dll {
            path: std::path::PathBuf::new(),
        },
        capabilities: None,
    }
}

#[async_trait]
impl Plugin for CPluginWrapper {
    fn metadata(&self) -> &super::super::traits::PluginMetadata {
        // Return a leaked static ref – acceptable for the rare C ABI path.
        // In a real implementation, metadata would be fetched via FFI.
        Box::leak(Box::new(c_plugin_metadata()))
    }

    async fn register(
        &self,
        _context: &mut super::super::context::PluginContext,
    ) -> Result<(), PluginError> {
        // C ABI plugins do their registration inside `xworkflow_plugin_create`.
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        // Cleanup happens in Drop via destroy_fn.
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct RustDllPluginWrapper {
    inner: Box<dyn Plugin>,
    _library: Library,
}

#[async_trait]
impl Plugin for RustDllPluginWrapper {
    fn metadata(&self) -> &super::super::traits::PluginMetadata {
        self.inner.metadata()
    }

    async fn register(
        &self,
        context: &mut super::super::context::PluginContext,
    ) -> Result<(), PluginError> {
        self.inner.register(context).await
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        self.inner.shutdown().await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self.inner.as_any()
    }
}
