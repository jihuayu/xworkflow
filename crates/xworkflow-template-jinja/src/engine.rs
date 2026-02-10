use std::collections::HashMap;
use std::sync::Arc;

use minijinja::Environment;
use serde_json::Value;

use xworkflow_types::template::{CompiledTemplateHandle, TemplateEngine, TemplateFunction};

use crate::compiled::JinjaCompiledTemplate;

pub struct JinjaTemplateEngine;

impl JinjaTemplateEngine {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JinjaTemplateEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateEngine for JinjaTemplateEngine {
    fn render(
        &self,
        template: &str,
        variables: &HashMap<String, Value>,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<String, String> {
        let mut env = Environment::new();
        register_template_functions(&mut env, functions);

        env.add_template("tpl", template)
            .map_err(|e| format!("Template parse error: {}", e))?;
        let tmpl = env
            .get_template("tpl")
            .map_err(|e| format!("Template not found: {}", e))?;
        let ctx = minijinja::Value::from_serialize(variables);
        let rendered = tmpl
            .render(ctx)
            .map_err(|e| format!("Template render error: {}", e))?;

        Ok(rendered)
    }

    fn compile(
        &self,
        template: &str,
        functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
    ) -> Result<Box<dyn CompiledTemplateHandle>, String> {
        let compiled = JinjaCompiledTemplate::new(template, functions)?;
        Ok(Box::new(compiled))
    }

    fn engine_name(&self) -> &str {
        "jinja2"
    }
}

pub(crate) fn register_template_functions<'a>(
    env: &mut Environment<'a>,
    functions: Option<&HashMap<String, Arc<dyn TemplateFunction>>>,
) {
    if let Some(funcs) = functions {
        let owned = funcs
            .iter()
            .map(|(name, func)| (name.clone(), func.clone()))
            .collect::<Vec<_>>();
        for (name, func) in owned {
            env.add_function(name, move |args: Vec<minijinja::Value>| {
                let json_args = args
                    .iter()
                    .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                let result = func.call(&json_args).map_err(|e| {
                    minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e)
                })?;
                Ok(minijinja::Value::from_serialize(result))
            });
        }
    }
}
