use regex::Regex;

use crate::core::variable_pool::VariablePool;
use crate::error::NodeError;

/// 解析文本中的 {{#...#}} 变量引用
///
/// # 参数
/// - `text`: 包含变量引用的文本
/// - `pool`: 变量池
///
/// # 返回
/// - 解析后的文本
pub fn resolve_variables(text: &str, pool: &VariablePool) -> Result<String, NodeError> {
    pool.resolve_template(text)
}

/// 提取文本中所有变量选择器
pub fn extract_selectors(text: &str) -> Vec<String> {
    let re = Regex::new(r"\{\{#([^#]+)#\}\}").unwrap();
    re.captures_iter(text)
        .map(|cap| cap[1].trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_resolve_variables() {
        let pool = VariablePool::new();
        pool.set_node_output("node1", json!({"text": "hello"}));

        let result = resolve_variables("Result: {{#node1.text#}}", &pool).unwrap();
        assert_eq!(result, "Result: hello");
    }

    #[test]
    fn test_extract_selectors() {
        let text = "Hello {{#input.name#}}, result is {{#llm.text#}}";
        let selectors = extract_selectors(text);
        assert_eq!(selectors, vec!["input.name", "llm.text"]);
    }

    #[test]
    fn test_resolve_multiple_variables() {
        let pool = VariablePool::new();
        pool.set_node_output("input", json!({"name": "Alice"}));
        pool.set_node_output("llm", json!({"text": "World"}));

        let result = resolve_variables(
            "Hello {{#input.name#}}, says {{#llm.text#}}",
            &pool,
        )
        .unwrap();
        assert_eq!(result, "Hello Alice, says World");
    }
}
