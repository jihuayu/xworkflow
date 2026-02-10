use std::collections::HashMap;
use std::sync::Arc;

use minijinja::Environment;
use serde_json::Value;

use xworkflow_types::template::{CompiledTemplateHandle, TemplateFunction};

/// Pre-compiled Jinja2 template for repeated rendering.
pub struct JinjaCompiledTemplate {
    env: Environment<'static>,
    template_source: *mut str,
}

// SAFETY: JinjaCompiledTemplate owns its template source pointer and the
// environment only references that owned memory. It is safe to move
// across threads as long as it is not aliased mutably.
unsafe impl Send for JinjaCompiledTemplate {}
unsafe impl Sync for JinjaCompiledTemplate {}

impl JinjaCompiledTemplate {
    pub fn new(
        template: &str,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<Self, String> {
        let boxed: Box<str> = template.to_owned().into_boxed_str();
        let raw = Box::into_raw(boxed);
        let static_str: &'static str = unsafe { &*raw };

        let mut env = Environment::new();
        super::engine::register_template_functions(&mut env, functions);
        env.add_template("tpl", static_str)
            .map_err(|e| format!("Template parse error: {}", e))?;

        Ok(Self {
            env,
            template_source: raw,
        })
    }
}

impl CompiledTemplateHandle for JinjaCompiledTemplate {
    fn render(&self, variables: &HashMap<String, Value>) -> Result<String, String> {
        let tmpl = self
            .env
            .get_template("tpl")
            .map_err(|e| format!("Template not found: {}", e))?;
        let ctx = minijinja::Value::from_serialize(variables);
        let rendered = tmpl
            .render(ctx)
            .map_err(|e| format!("Template render error: {}", e))?;
        Ok(rendered)
    }
}

impl Drop for JinjaCompiledTemplate {
    fn drop(&mut self) {
        unsafe {
            let _ = Box::from_raw(self.template_source);
        }
    }
}
