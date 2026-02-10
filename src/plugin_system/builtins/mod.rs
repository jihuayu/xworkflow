pub mod wasm_bootstrap;

#[cfg(feature = "builtin-sandbox-js")]
pub mod sandbox_js;
#[cfg(feature = "builtin-sandbox-wasm")]
pub mod sandbox_wasm;
#[cfg(feature = "builtin-template-jinja")]
pub mod template_jinja;

pub use wasm_bootstrap::{WasmBootstrapPlugin, WasmPluginConfig, WasmPluginLoader};

#[cfg(feature = "builtin-sandbox-js")]
pub use sandbox_js::{create_js_sandbox_plugins, JsSandboxBootPlugin, JsSandboxLangPlugin};
#[cfg(feature = "builtin-sandbox-wasm")]
pub use sandbox_wasm::{create_wasm_sandbox_plugins, WasmSandboxBootPlugin, WasmSandboxLangPlugin};
#[cfg(feature = "builtin-template-jinja")]
pub use template_jinja::JinjaTemplatePlugin;
