//! FFI helper macros for declaring plugins as dynamic libraries.

/// Declare a plugin type for C-ABI dynamic loading.
///
/// Generates `xworkflow_plugin_create`, `xworkflow_plugin_destroy`, and
/// `xworkflow_plugin_abi_version` extern functions.
#[macro_export]
macro_rules! xworkflow_declare_plugin {
    ($plugin_type:ty) => {
        #[no_mangle]
        pub extern "C" fn xworkflow_plugin_create() -> *mut std::ffi::c_void {
            let plugin: Box<dyn $crate::plugin_system::Plugin> =
                Box::new(<$plugin_type>::default());
            Box::into_raw(plugin) as *mut std::ffi::c_void
        }

        #[no_mangle]
        pub extern "C" fn xworkflow_plugin_destroy(ptr: *mut std::ffi::c_void) {
            if !ptr.is_null() {
                unsafe { let _ = Box::from_raw(ptr as *mut dyn $crate::plugin_system::Plugin); }
            }
        }

        #[no_mangle]
        pub extern "C" fn xworkflow_plugin_abi_version() -> u32 { 1 }
    };
}

/// Declare a Rust-native plugin (no C-ABI tear-down needed).
///
/// Generates `XWORKFLOW_RUST_PLUGIN`, `XWORKFLOW_RUST_ABI_VERSION`, and
/// `xworkflow_rust_plugin_create` symbols.
#[macro_export]
macro_rules! xworkflow_declare_rust_plugin {
    ($plugin_type:ty) => {
        #[no_mangle]
        pub static XWORKFLOW_RUST_PLUGIN: bool = true;

        #[no_mangle]
        pub static XWORKFLOW_RUST_ABI_VERSION: u32 = 1;

        #[no_mangle]
        pub fn xworkflow_rust_plugin_create() -> Box<dyn $crate::plugin_system::Plugin> {
            Box::new(<$plugin_type>::default())
        }
    };
}
