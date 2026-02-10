use std::sync::Arc;

use crate::sandbox::{CodeLanguage, CodeSandbox, ExecutionConfig};

/// Service key for language providers.
pub const LANG_PROVIDE_KEY: &str = "code.lang-provide";

/// Language provider interface.
pub trait LanguageProvider: Send + Sync + 'static {
    /// Supported language.
    fn language(&self) -> CodeLanguage;

    /// Display name.
    fn display_name(&self) -> &str;

    /// Sandbox implementation.
    fn sandbox(&self) -> Arc<dyn CodeSandbox>;

    /// Default execution config.
    fn default_config(&self) -> ExecutionConfig;

    /// Supported file extensions.
    fn file_extensions(&self) -> Vec<&str>;

    /// Whether streaming is supported.
    fn supports_streaming(&self) -> bool {
        false
    }
}

/// Wrapper for Any downcast when using service registry.
pub struct LanguageProviderWrapper(pub Arc<dyn LanguageProvider>);
