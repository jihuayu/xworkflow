use crate::core::variable_pool::VariablePool;
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
    TEMPLATE_RE.replace_all(template, |caps: &regex::Captures| {
        let selector_str = &caps[1]; // e.g. "node_abc.text"
        let parts: Vec<String> = selector_str.split('.').map(|s| s.to_string()).collect();
        let val = pool.get(&parts);
        val.to_display_string()
    })
    .into_owned()
}

/// Async render template with stream resolution.
pub async fn render_template_async(template: &str, pool: &VariablePool) -> String {
    let mut result = String::new();
    let mut last_index = 0;

    for caps in TEMPLATE_RE.captures_iter(template) {
        let mat = caps.get(0).unwrap();
        result.push_str(&template[last_index..mat.start()]);
        let selector_str = &caps[1];
        let parts: Vec<String> = selector_str.split('.').map(|s| s.to_string()).collect();
        let val = pool.get_resolved(&parts).await;
        result.push_str(&val.to_display_string());
        last_index = mat.end();
    }

    result.push_str(&template[last_index..]);
    result
}

type TemplateFunctionMap = HashMap<String, Arc<dyn TemplateFunction>>;

fn builtin_template_engine() -> Result<&'static Arc<dyn TemplateEngine>, String> {
    #[cfg(feature = "builtin-template-jinja")]
    {
        static ENGINE: OnceLock<Arc<dyn TemplateEngine>> = OnceLock::new();
        Ok(ENGINE.get_or_init(|| {
            Arc::new(xworkflow_template_jinja::JinjaTemplateEngine::new())
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
    use crate::core::variable_pool::Segment;

    #[test]
    fn test_render_template_basic() {
        let mut pool = VariablePool::new();
        pool.set(
            &["node1".to_string(), "name".to_string()],
            Segment::String("Alice".to_string()),
        );
        let result = render_template("Hello {{#node1.name#}}!", &pool);
        assert_eq!(result, "Hello Alice!");
    }

    #[test]
    fn test_render_template_multiple() {
        let mut pool = VariablePool::new();
        pool.set(
            &["n1".to_string(), "a".to_string()],
            Segment::String("X".to_string()),
        );
        pool.set(
            &["n2".to_string(), "b".to_string()],
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
            &["sys".to_string(), "query".to_string()],
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
            &["n1".to_string(), "text".to_string()],
            Segment::Stream(stream),
        );

        tokio::spawn(async move {
            writer.send(Segment::String("hi".into())).await;
            writer.end(Segment::String("hi".into())).await;
        });

        let result = render_template_async("Say {{#n1.text#}}", &pool).await;
        assert_eq!(result, "Say hi");
    }

    #[test]
    fn test_render_jinja2() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("name".to_string(), serde_json::json!("World"));
        let result = render_jinja2("Hello {{ name }}!", &vars).unwrap();
        assert_eq!(result, "Hello World!");
    }

    #[test]
    fn test_render_jinja2_loop() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("items".to_string(), serde_json::json!(["a", "b", "c"]));
        let result = render_jinja2("{% for i in items %}{{i}} {% endfor %}", &vars).unwrap();
        assert_eq!(result.trim(), "a b c");
    }

    #[test]
    fn test_compiled_template_render() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("name".to_string(), serde_json::json!("Rust"));
        let compiled = CompiledTemplate::new("Hello {{ name }}!", None).unwrap();
        let result = compiled.render(&vars).unwrap();
        assert_eq!(result, "Hello Rust!");
    }
}
