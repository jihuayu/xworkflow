//! Jinja2 template engine implementation via minijinja.
//!
//! Provides `JinjaTemplateEngine` (implements `TemplateEngine` trait) and
//! `JinjaCompiledTemplate` (implements `CompiledTemplateHandle` trait).

use std::collections::HashMap;
use std::sync::Arc;

use minijinja::Environment;
use serde_json::Value;

use xworkflow_types::template::{CompiledTemplateHandle, TemplateEngine, TemplateFunction};

// ─── JinjaTemplateEngine ───────────────────────────────────────────

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

fn register_template_functions<'a>(
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

// ─── JinjaCompiledTemplate ─────────────────────────────────────────

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
        register_template_functions(&mut env, functions);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jinja_engine_new() {
        let engine = JinjaTemplateEngine::new();
        assert_eq!(engine.engine_name(), "jinja2");
    }

    #[test]
    fn test_jinja_engine_default() {
        let engine = JinjaTemplateEngine::default();
        assert_eq!(engine.engine_name(), "jinja2");
    }

    #[test]
    fn test_jinja_render_simple() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), Value::String("World".into()));
        
        let result = engine.render("Hello {{ name }}!", &vars, None);
        assert_eq!(result.unwrap(), "Hello World!");
    }

    #[test]
    fn test_jinja_render_multiple_vars() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("first".to_string(), Value::String("John".into()));
        vars.insert("last".to_string(), Value::String("Doe".into()));
        
        let result = engine.render("{{ first }} {{ last }}", &vars, None);
        assert_eq!(result.unwrap(), "John Doe");
    }

    #[test]
    fn test_jinja_render_with_loop() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("items".to_string(), serde_json::json!(["a", "b", "c"]));
        
        let result = engine.render("{% for item in items %}{{ item }}{% endfor %}", &vars, None);
        assert_eq!(result.unwrap(), "abc");
    }

    #[test]
    fn test_jinja_render_with_conditional() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("show".to_string(), Value::Bool(true));
        
        let result = engine.render("{% if show %}visible{% endif %}", &vars, None);
        assert_eq!(result.unwrap(), "visible");
    }

    #[test]
    fn test_jinja_render_parse_error() {
        let engine = JinjaTemplateEngine::new();
        let vars = HashMap::new();
        
        let result = engine.render("{{ unclosed", &vars, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("parse error"));
    }

    #[test]
    fn test_jinja_render_undefined_variable() {
        let engine = JinjaTemplateEngine::new();
        let vars = HashMap::new();
        
        let result = engine.render("{{ undefined_var }}", &vars, None);
        // Jinja allows undefined variables, rendering as empty
        assert!(result.is_ok());
    }

    #[test]
    fn test_jinja_render_with_filter() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), Value::String("world".into()));
        
        let result = engine.render("{{ name | upper }}", &vars, None);
        assert_eq!(result.unwrap(), "WORLD");
    }

    #[test]
    fn test_jinja_render_complex_object() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("user".to_string(), serde_json::json!({
            "name": "Alice",
            "age": 30,
            "active": true
        }));
        
        let result = engine.render("{{ user.name }} is {{ user.age }}", &vars, None);
        assert_eq!(result.unwrap(), "Alice is 30");
    }

    #[test]
    fn test_jinja_compile_simple() {
        let engine = JinjaTemplateEngine::new();
        let result = engine.compile("Hello {{ name }}!", None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_jinja_compile_error() {
        let engine = JinjaTemplateEngine::new();
        let result = engine.compile("{{ unclosed", None);
        assert!(result.is_err());
        if let Err(msg) = result {
            assert!(msg.contains("parse error") || msg.contains("error"));
        }
    }

    #[test]
    fn test_jinja_compiled_render() {
        let engine = JinjaTemplateEngine::new();
        let compiled = engine.compile("Hello {{ name }}!", None).unwrap();
        
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), Value::String("World".into()));
        
        let result = compiled.render(&vars);
        assert_eq!(result.unwrap(), "Hello World!");
    }

    #[test]
    fn test_jinja_compiled_render_multiple_times() {
        let engine = JinjaTemplateEngine::new();
        let compiled = engine.compile("Hello {{ name }}!", None).unwrap();
        
        let mut vars1 = HashMap::new();
        vars1.insert("name".to_string(), Value::String("Alice".into()));
        let result1 = compiled.render(&vars1).unwrap();
        
        let mut vars2 = HashMap::new();
        vars2.insert("name".to_string(), Value::String("Bob".into()));
        let result2 = compiled.render(&vars2).unwrap();
        
        assert_eq!(result1, "Hello Alice!");
        assert_eq!(result2, "Hello Bob!");
    }

    #[test]
    fn test_jinja_compiled_render_error() {
        let engine = JinjaTemplateEngine::new();
        let compiled = engine.compile("{{ items[0] }}", None).unwrap();
        
        let mut vars = HashMap::new();
        vars.insert("items".to_string(), Value::String("not_array".into()));
        
        let result = compiled.render(&vars);
        assert!(result.is_err());
    }

    struct TestTemplateFunction;

    impl TemplateFunction for TestTemplateFunction {
        fn name(&self) -> &str {
            "test_fn"
        }

        fn call(&self, args: &[Value]) -> Result<Value, String> {
            if args.is_empty() {
                return Err("Expected at least one argument".into());
            }
            Ok(Value::String(format!("test_{}", args[0])))
        }
    }

    #[test]
    fn test_jinja_render_with_custom_function() {
        let engine = JinjaTemplateEngine::new();
        let mut functions = HashMap::new();
        functions.insert("test_fn".to_string(), Arc::new(TestTemplateFunction) as Arc<dyn TemplateFunction>);
        
        let mut vars = HashMap::new();
        vars.insert("value".to_string(), Value::String("input".into()));
        
        let result = engine.render("{{ test_fn(value) }}", &vars, Some(&functions));
        assert_eq!(result.unwrap(), "test_input");
    }

    #[test]
    fn test_jinja_compile_with_custom_function() {
        let engine = JinjaTemplateEngine::new();
        let mut functions = HashMap::new();
        functions.insert("test_fn".to_string(), Arc::new(TestTemplateFunction) as Arc<dyn TemplateFunction>);
        
        let compiled = engine.compile("{{ test_fn(value) }}", Some(&functions)).unwrap();
        
        let mut vars = HashMap::new();
        vars.insert("value".to_string(), Value::String("data".into()));
        
        let result = compiled.render(&vars);
        assert_eq!(result.unwrap(), "test_data");
    }

    struct ErrorTemplateFunction;

    impl TemplateFunction for ErrorTemplateFunction {
        fn name(&self) -> &str {
            "error_fn"
        }

        fn call(&self, _args: &[Value]) -> Result<Value, String> {
            Err("Function error".into())
        }
    }

    #[test]
    fn test_jinja_render_with_error_function() {
        let engine = JinjaTemplateEngine::new();
        let mut functions = HashMap::new();
        functions.insert("error_fn".to_string(), Arc::new(ErrorTemplateFunction) as Arc<dyn TemplateFunction>);
        
        let vars = HashMap::new();
        
        let result = engine.render("{{ error_fn() }}", &vars, Some(&functions));
        assert!(result.is_err());
    }

    #[test]
    fn test_jinja_render_empty_template() {
        let engine = JinjaTemplateEngine::new();
        let vars = HashMap::new();
        
        let result = engine.render("", &vars, None);
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn test_jinja_render_no_variables() {
        let engine = JinjaTemplateEngine::new();
        let vars = HashMap::new();
        
        let result = engine.render("Hello World!", &vars, None);
        assert_eq!(result.unwrap(), "Hello World!");
    }

    #[test]
    fn test_jinja_render_nested_object_access() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("data".to_string(), serde_json::json!({
            "user": {
                "profile": {
                    "name": "Alice"
                }
            }
        }));
        
        let result = engine.render("{{ data.user.profile.name }}", &vars, None);
        assert_eq!(result.unwrap(), "Alice");
    }

    #[test]
    fn test_jinja_render_array_index() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("items".to_string(), serde_json::json!(["first", "second", "third"]));
        
        let result = engine.render("{{ items[1] }}", &vars, None);
        assert_eq!(result.unwrap(), "second");
    }

    #[test]
    fn test_jinja_render_with_comments() {
        let engine = JinjaTemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), Value::String("World".into()));
        
        let result = engine.render("Hello {# comment #}{{ name }}!", &vars, None);
        assert_eq!(result.unwrap(), "Hello World!");
    }

    #[test]
    fn test_compiled_template_drop() {
        let engine = JinjaTemplateEngine::new();
        let compiled = engine.compile("Test {{ x }}", None).unwrap();
        drop(compiled); // Ensure Drop is called without panic
    }
}
