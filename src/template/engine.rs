//! Template rendering implementations.
//!
//! Two distinct template notations are supported:
//!
//! 1. **Dify Answer-node** templates — `{{#node_id.var#}}` placeholders
//!    resolved against a [`VariablePool`].
//! 2. **Jinja2** templates — powered by `minijinja` with custom functions
//!    and optional security sanitisation.

use crate::core::variable_pool::{Selector, VariablePool};
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock};

#[cfg(feature = "security")]
use crate::security::validation::TemplateSafetyConfig;

use xworkflow_types::template::{CompiledTemplateHandle, TemplateEngine, TemplateFunction};

static TEMPLATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{#([^#]+)#\}\}").unwrap()
});

/// Render a template string with `{{#node_id.var_name#}}` variable references.
/// This is the Dify Answer node template syntax.
pub fn render_template(template: &str, pool: &VariablePool) -> String {
    render_template_with_config(template, pool, false).unwrap_or_default()
}

/// Error returned when a referenced variable is missing in strict mode.
#[derive(Debug, Clone)]
pub struct TemplateRenderError {
    pub selector: String,
}

impl std::fmt::Display for TemplateRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "missing template variable: {}", self.selector)
    }
}

impl std::error::Error for TemplateRenderError {}

/// Synchronous render with configurable strict mode.
///
/// In strict mode an error is returned on the first missing variable.
/// In lenient mode missing variables are replaced with empty strings.
pub fn render_template_with_config(
    template: &str,
    pool: &VariablePool,
    strict: bool,
) -> Result<String, TemplateRenderError> {
    let mut missing: Vec<String> = Vec::new();
    let rendered = TEMPLATE_RE
        .replace_all(template, |caps: &regex::Captures| {
            let selector_str = &caps[1]; // e.g. "node_abc.text"
            if let Some(selector) = Selector::parse_str(selector_str) {
                if strict && !pool.has(&selector) {
                    missing.push(selector_str.to_string());
                    return String::new();
                }
                if !strict && !pool.has(&selector) {
                    missing.push(selector_str.to_string());
                }
                let val = pool.get(&selector);
                val.to_display_string()
            } else {
                missing.push(selector_str.to_string());
                String::new()
            }
        })
        .into_owned();

    if !missing.is_empty() {
        if strict {
            return Err(TemplateRenderError {
                selector: missing[0].clone(),
            });
        }
        tracing::warn!(
            missing = ?missing,
            "template variable(s) missing"
        );
    }
    Ok(rendered)
}

/// Async render template with stream resolution.
pub async fn render_template_async(template: &str, pool: &VariablePool) -> String {
    render_template_async_with_config(template, pool, false)
        .await
        .unwrap_or_default()
}

/// Async-capable render that resolves streaming variables before substitution.
pub async fn render_template_async_with_config(
    template: &str,
    pool: &VariablePool,
    strict: bool,
) -> Result<String, TemplateRenderError> {
    let mut result = String::new();
    let mut last_index = 0;
    let mut missing: Vec<String> = Vec::new();

    for caps in TEMPLATE_RE.captures_iter(template) {
        let mat = caps.get(0).unwrap();
        result.push_str(&template[last_index..mat.start()]);
        let selector_str = &caps[1];
        if let Some(selector) = Selector::parse_str(selector_str) {
            if !pool.has(&selector) {
                missing.push(selector_str.to_string());
                if strict {
                    return Err(TemplateRenderError {
                        selector: selector_str.to_string(),
                    });
                }
            }
            let val = pool.get_resolved(&selector).await;
            result.push_str(&val.to_display_string());
        } else {
            missing.push(selector_str.to_string());
            if strict {
                return Err(TemplateRenderError {
                    selector: selector_str.to_string(),
                });
            }
        }
        last_index = mat.end();
    }

    result.push_str(&template[last_index..]);
    if !missing.is_empty() {
        tracing::warn!(
            missing = ?missing,
            "template variable(s) missing"
        );
    }
    Ok(result)
}

type TemplateFunctionMap = HashMap<String, Arc<dyn TemplateFunction>>;

fn builtin_template_engine() -> Result<&'static Arc<dyn TemplateEngine>, String> {
    #[cfg(feature = "builtin-template-jinja")]
    {
        static ENGINE: OnceLock<Arc<dyn TemplateEngine>> = OnceLock::new();
        Ok(ENGINE.get_or_init(|| {
            Arc::new(crate::template::jinja::JinjaTemplateEngine::new())
                as Arc<dyn TemplateEngine>
        }))
    }
    #[cfg(not(feature = "builtin-template-jinja"))]
    {
        Err("Template engine not available (builtin-template-jinja disabled)".into())
    }
}

/// Render a Jinja2 template using minijinja with provided variables and optional functions
pub fn render_jinja2_with_functions(
    template: &str,
    variables: &std::collections::HashMap<String, serde_json::Value>,
    functions: Option<&TemplateFunctionMap>,
) -> Result<String, String> {
    render_jinja2_with_functions_and_config(template, variables, functions, None)
}

pub fn render_jinja2_with_functions_and_config(
    template: &str,
    variables: &std::collections::HashMap<String, serde_json::Value>,
    functions: Option<&TemplateFunctionMap>,
    #[cfg(feature = "security")] safety: Option<&TemplateSafetyConfig>,
    #[cfg(not(feature = "security"))] _safety: Option<&()>,
) -> Result<String, String> {
    #[cfg(feature = "security")]
    if let Some(cfg) = safety {
        if template.len() > cfg.max_template_length {
            return Err(format!(
                "Template too large (max {}, got {})",
                cfg.max_template_length,
                template.len()
            ));
        }
    }

    let engine = builtin_template_engine()?;
    let rendered = engine.render(template, variables, functions)?;

    #[cfg(feature = "security")]
    if let Some(cfg) = safety {
        if rendered.len() > cfg.max_output_length {
            return Err(format!(
                "Template output too large (max {}, got {})",
                cfg.max_output_length,
                rendered.len()
            ));
        }
    }

    Ok(rendered)
}

/// Pre-compiled Jinja2 template for repeated rendering.
pub struct CompiledTemplate {
    handle: Box<dyn CompiledTemplateHandle>,
    max_output_length: Option<usize>,
}

impl CompiledTemplate {
    pub fn new(
        template: &str,
        functions: Option<&TemplateFunctionMap>,
    ) -> Result<Self, String> {
        Self::new_with_config(template, functions, None)
    }

    pub fn new_with_config(
        template: &str,
        functions: Option<&TemplateFunctionMap>,
        #[cfg(feature = "security")] safety: Option<&TemplateSafetyConfig>,
        #[cfg(not(feature = "security"))] _safety: Option<&()>,
    ) -> Result<Self, String> {
        #[cfg(feature = "security")]
        if let Some(cfg) = safety {
            if template.len() > cfg.max_template_length {
                return Err(format!(
                    "Template too large (max {}, got {})",
                    cfg.max_template_length,
                    template.len()
                ));
            }
        }

        let engine = builtin_template_engine()?;
        let handle = engine.compile(template, functions)?;

        Ok(Self {
            handle,
            max_output_length: {
                #[cfg(feature = "security")]
                {
                    safety.map(|cfg| cfg.max_output_length)
                }
                #[cfg(not(feature = "security"))]
                {
                    None
                }
            },
        })
    }

    pub fn render(
        &self,
        variables: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<String, String> {
        let rendered = self.handle.render(variables)?;

        if let Some(limit) = self.max_output_length {
            if rendered.len() > limit {
                return Err(format!(
                    "Template output too large (max {}, got {})",
                    limit,
                    rendered.len()
                ));
            }
        }

        Ok(rendered)
    }
}

/// Render a Jinja2 template using minijinja with provided variables
pub fn render_jinja2(
    template: &str,
    variables: &std::collections::HashMap<String, serde_json::Value>,
) -> Result<String, String> {
    render_jinja2_with_functions(template, variables, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::variable_pool::{Segment, Selector};

    #[test]
    fn test_render_template_basic() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("node1", "name"),
            Segment::String("Alice".to_string()),
        );
        let result = render_template("Hello {{#node1.name#}}!", &pool);
        assert_eq!(result, "Hello Alice!");
    }

    #[test]
    fn test_render_template_multiple() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("n1", "a"),
            Segment::String("X".to_string()),
        );
        pool.set(
            &Selector::new("n2", "b"),
            Segment::Integer(42),
        );
        let result = render_template("{{#n1.a#}} and {{#n2.b#}}", &pool);
        assert_eq!(result, "X and 42");
    }

    #[test]
    fn test_render_template_missing() {
        let pool = VariablePool::new();
        let result = render_template("Hello {{#missing.var#}}!", &pool);
        assert_eq!(result, "Hello !");
    }

    #[test]
    fn test_render_template_sys() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("sys", "query"),
            Segment::String("test".to_string()),
        );
        let result = render_template("Q: {{#sys.query#}}", &pool);
        assert_eq!(result, "Q: test");
    }

    #[tokio::test]
    async fn test_render_template_async_stream() {
        let (stream, writer) = crate::core::variable_pool::SegmentStream::channel();
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("n1", "text"),
            Segment::Stream(stream),
        );

        tokio::spawn(async move {
            writer.send(Segment::String("hi".into())).await;
            writer.end(Segment::String("hi".into())).await;
        });

        let result = render_template_async("Say {{#n1.text#}}", &pool).await;
        assert_eq!(result, "Say hi");
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[test]
    fn test_render_jinja2() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("name".to_string(), serde_json::json!("World"));
        let result = render_jinja2("Hello {{ name }}!", &vars).unwrap();
        assert_eq!(result, "Hello World!");
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[test]
    fn test_render_jinja2_loop() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("items".to_string(), serde_json::json!(["a", "b", "c"]));
        let result = render_jinja2("{% for i in items %}{{i}} {% endfor %}", &vars).unwrap();
        assert_eq!(result.trim(), "a b c");
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[test]
    fn test_compiled_template_render() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("name".to_string(), serde_json::json!("Rust"));
        let compiled = CompiledTemplate::new("Hello {{ name }}!", None).unwrap();
        let result = compiled.render(&vars).unwrap();
        assert_eq!(result, "Hello Rust!");
    }

    #[test]
    fn test_render_template_with_config_strict_missing() {
        let pool = VariablePool::new();
        let result = render_template_with_config("Hello {{#missing.var#}}!", &pool, true);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("missing.var"));
    }

    #[test]
    fn test_render_template_with_config_strict_present() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("n1", "x"),
            Segment::String("ok".into()),
        );
        let result = render_template_with_config("{{#n1.x#}}", &pool, true);
        assert_eq!(result.unwrap(), "ok");
    }

    #[test]
    fn test_render_template_with_config_non_strict_missing() {
        let pool = VariablePool::new();
        let result = render_template_with_config("foo {{#miss.x#}} bar", &pool, false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "foo  bar");
    }

    #[test]
    fn test_render_template_no_placeholders() {
        let pool = VariablePool::new();
        let result = render_template("No placeholders here", &pool);
        assert_eq!(result, "No placeholders here");
    }

    #[test]
    fn test_render_template_bad_selector() {
        let pool = VariablePool::new();
        // A selector without a dot is invalid
        let result = render_template("{{#nodot#}}", &pool);
        assert_eq!(result, "");
    }

    #[test]
    fn test_template_render_error_display() {
        let err = TemplateRenderError {
            selector: "n1.x".to_string(),
        };
        assert_eq!(err.to_string(), "missing template variable: n1.x");
        // Test the Error trait
        let _source = std::error::Error::source(&err);
    }

    #[tokio::test]
    async fn test_render_template_async_basic() {
        let mut pool = VariablePool::new();
        pool.set(
            &Selector::new("n1", "a"),
            Segment::String("hello".into()),
        );
        let result = render_template_async("{{#n1.a#}} world", &pool).await;
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn test_render_template_async_missing() {
        let pool = VariablePool::new();
        let result = render_template_async("missing {{#m.x#}}", &pool).await;
        assert_eq!(result, "missing ");
    }

    #[tokio::test]
    async fn test_render_template_async_strict_missing() {
        let pool = VariablePool::new();
        let result = render_template_async_with_config("{{#m.x#}}", &pool, true).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_render_template_async_strict_bad_selector() {
        let pool = VariablePool::new();
        let result = render_template_async_with_config("{{#nodot#}}", &pool, true).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_render_template_async_multiple() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("a", "x"), Segment::Integer(1));
        pool.set(&Selector::new("b", "y"), Segment::String("z".into()));
        let result = render_template_async("{{#a.x#}}-{{#b.y#}}", &pool).await;
        assert_eq!(result, "1-z");
    }

    #[test]
    fn test_render_template_integer_value() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "count"), Segment::Integer(99));
        let result = render_template("Count: {{#n1.count#}}", &pool);
        assert_eq!(result, "Count: 99");
    }

    #[test]
    fn test_render_template_float_value() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "pi"), Segment::Float(3.14));
        let result = render_template("Pi: {{#n1.pi#}}", &pool);
        assert!(result.starts_with("Pi: 3.14"));
    }

    #[test]
    fn test_render_template_bool_value() {
        let mut pool = VariablePool::new();
        pool.set(&Selector::new("n1", "flag"), Segment::Boolean(true));
        let result = render_template("Flag: {{#n1.flag#}}", &pool);
        assert_eq!(result, "Flag: true");
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[test]
    fn test_render_jinja2_multiple_vars() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("a".to_string(), serde_json::json!(1));
        vars.insert("b".to_string(), serde_json::json!("two"));
        let result = render_jinja2("{{ a }} and {{ b }}", &vars).unwrap();
        assert_eq!(result, "1 and two");
    }

    #[cfg(feature = "builtin-template-jinja")]
    #[test]
    fn test_compiled_template_multiple_renders() {
        let compiled = CompiledTemplate::new("Hello {{ name }}!", None).unwrap();
        let mut v1 = std::collections::HashMap::new();
        v1.insert("name".to_string(), serde_json::json!("A"));
        assert_eq!(compiled.render(&v1).unwrap(), "Hello A!");

        let mut v2 = std::collections::HashMap::new();
        v2.insert("name".to_string(), serde_json::json!("B"));
        assert_eq!(compiled.render(&v2).unwrap(), "Hello B!");
    }
}
