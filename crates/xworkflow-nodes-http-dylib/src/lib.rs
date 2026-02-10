use xworkflow::plugin_system::Plugin;
use xworkflow_nodes_http::HttpNodePlugin;

#[no_mangle]
pub static XWORKFLOW_RUST_PLUGIN: bool = true;

#[no_mangle]
pub extern "C" fn xworkflow_plugin_abi_version() -> u32 {
    1
}

#[no_mangle]
pub fn xworkflow_rust_plugin_create() -> Box<dyn Plugin> {
    Box::new(HttpNodePlugin::default())
}
