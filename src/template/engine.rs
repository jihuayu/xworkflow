use crate::core::variable_pool::VariablePool;
use regex::Regex;

/// Render a template string with `{{#node_id.var_name#}}` variable references.
/// This is the Dify Answer node template syntax.
pub fn render_template(template: &str, pool: &VariablePool) -> String {
    let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();
    re.replace_all(template, |caps: &regex::Captures| {
        let selector_str = &caps[1]; // e.g. "node_abc.text"
        let parts: Vec<String> = selector_str.split('.').map(|s| s.to_string()).collect();
        let val = pool.get(&parts);
        val.to_display_string()
    })
    .into_owned()
}

/// Async render template with stream resolution.
pub async fn render_template_async(template: &str, pool: &VariablePool) -> String {
    let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();
    let mut result = String::new();
    let mut last_index = 0;

    for caps in re.captures_iter(template) {
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

#[cfg(feature = "plugin-system")]
type TemplateFunctionMap = std::collections::HashMap<
    String,
    std::sync::Arc<dyn crate::plugin_system::TemplateFunction>,
>;

#[cfg(not(feature = "plugin-system"))]
type TemplateFunctionMap = std::collections::HashMap<String, ()>;

/// Render a Jinja2 template using minijinja with provided variables and optional functions
pub fn render_jinja2_with_functions(
    template: &str,
    variables: &std::collections::HashMap<String, serde_json::Value>,
    _functions: Option<&TemplateFunctionMap>,
) -> Result<String, String> {
    let mut env = minijinja::Environment::new();

    #[cfg(feature = "plugin-system")]
    if let Some(funcs) = _functions {
        for (name, func) in funcs {
            let func = func.clone();
            env.add_function(name.as_str(), move |args: Vec<minijinja::Value>| {
                let json_args = args
                    .iter()
                    .map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null))
                    .collect::<Vec<_>>();
                let result = func.call(&json_args).map_err(|e| {
                    minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e)
                })?;
                Ok(minijinja::Value::from_serialize(result))
            });
        }
    }

    env.add_template("tpl", template)
        .map_err(|e| format!("Template parse error: {}", e))?;
    let tmpl = env
        .get_template("tpl")
        .map_err(|e| format!("Template not found: {}", e))?;
    let ctx = minijinja::Value::from_serialize(variables);
    tmpl.render(ctx)
        .map_err(|e| format!("Template render error: {}", e))
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
}
