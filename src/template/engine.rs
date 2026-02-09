use std::collections::HashMap;

use minijinja::Environment;
use serde_json::Value;

use crate::error::NodeError;

/// 模板引擎 - 封装 Minijinja
pub struct TemplateEngine {
    env: Environment<'static>,
}

impl TemplateEngine {
    /// 创建新的模板引擎实例
    pub fn new() -> Self {
        let mut env = Environment::new();

        // 注册自定义过滤器
        env.add_filter("default", minijinja_default_filter);
        env.add_filter("upper", minijinja_upper_filter);
        env.add_filter("lower", minijinja_lower_filter);
        env.add_filter("trim", minijinja_trim_filter);

        TemplateEngine { env }
    }

    /// 渲染模板
    ///
    /// # 参数
    /// - `template`: 模板字符串
    /// - `variables`: 变量映射
    ///
    /// # 返回
    /// - 渲染后的字符串
    pub fn render_template(
        &self,
        template: &str,
        variables: &HashMap<String, Value>,
    ) -> Result<String, NodeError> {
        let tmpl = self
            .env
            .template_from_str(template)
            .map_err(|e| NodeError::TemplateError(format!("Template compile error: {}", e)))?;

        // 将 variables 转换为 minijinja::Value
        let ctx = convert_to_minijinja_value(variables);

        tmpl.render(ctx)
            .map_err(|e| NodeError::TemplateError(format!("Template render error: {}", e)))
    }
}

impl Default for TemplateEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// 将 HashMap<String, serde_json::Value> 转换为 minijinja::Value
fn convert_to_minijinja_value(
    variables: &HashMap<String, Value>,
) -> minijinja::Value {
    let json_value = serde_json::to_value(variables).unwrap_or(Value::Object(Default::default()));
    minijinja::Value::from_serialize(&json_value)
}

// 自定义过滤器实现

fn minijinja_default_filter(
    value: minijinja::Value,
    default: Option<minijinja::Value>,
) -> minijinja::Value {
    if value.is_undefined() || value.is_none() {
        default.unwrap_or(minijinja::Value::from(""))
    } else {
        value
    }
}

fn minijinja_upper_filter(value: String) -> String {
    value.to_uppercase()
}

fn minijinja_lower_filter(value: String) -> String {
    value.to_lowercase()
}

fn minijinja_trim_filter(value: String) -> String {
    value.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_simple_template() {
        let engine = TemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), json!("World"));

        let result = engine.render_template("Hello {{ name }}!", &vars).unwrap();
        assert_eq!(result, "Hello World!");
    }

    #[test]
    fn test_nested_template() {
        let engine = TemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert(
            "user".to_string(),
            json!({"name": "Alice", "age": 30}),
        );

        let result = engine
            .render_template("Name: {{ user.name }}, Age: {{ user.age }}", &vars)
            .unwrap();
        assert_eq!(result, "Name: Alice, Age: 30");
    }

    #[test]
    fn test_loop_template() {
        let engine = TemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("items".to_string(), json!(["a", "b", "c"]));

        let result = engine
            .render_template(
                "{% for item in items %}{{ item }}{% if not loop.last %},{% endif %}{% endfor %}",
                &vars,
            )
            .unwrap();
        assert_eq!(result, "a,b,c");
    }

    #[test]
    fn test_conditional_template() {
        let engine = TemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("show".to_string(), json!(true));

        let result = engine
            .render_template(
                "{% if show %}visible{% else %}hidden{% endif %}",
                &vars,
            )
            .unwrap();
        assert_eq!(result, "visible");
    }

    #[test]
    fn test_filter_upper() {
        let engine = TemplateEngine::new();
        let mut vars = HashMap::new();
        vars.insert("text".to_string(), json!("hello"));

        let result = engine
            .render_template("{{ text | upper }}", &vars)
            .unwrap();
        assert_eq!(result, "HELLO");
    }
}
