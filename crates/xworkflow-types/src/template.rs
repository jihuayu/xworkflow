use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Template function extension point.
pub trait TemplateFunction: Send + Sync {
    fn name(&self) -> &str;
    fn call(&self, args: &[Value]) -> Result<Value, String>;
}

/// Template engine interface.
pub trait TemplateEngine: Send + Sync {
    /// Render a template string.
    fn render(
        &self,
        template: &str,
        variables: &HashMap<String, Value>,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<String, String>;

    /// Pre-compile a template for repeated rendering.
    fn compile(
        &self,
        template: &str,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<Box<dyn CompiledTemplateHandle>, String>;

    /// Engine name.
    fn engine_name(&self) -> &str;
}

/// Pre-compiled template handle.
pub trait CompiledTemplateHandle: Send + Sync {
    fn render(&self, variables: &HashMap<String, Value>) -> Result<String, String>;
}
