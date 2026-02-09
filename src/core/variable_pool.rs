use std::collections::HashMap;

use dashmap::DashMap;
use regex::Regex;
use serde_json::Value;

use crate::error::NodeError;

/// 变量池 - 线程安全的变量存储
#[derive(Debug)]
pub struct VariablePool {
    /// 节点输出存储（node_id -> output_value）
    node_outputs: DashMap<String, Value>,

    /// 系统变量（sys.query, sys.files 等）
    system_vars: DashMap<String, Value>,

    /// 对话变量（跨轮次持久化）
    conversation_vars: DashMap<String, Value>,

    /// 作用域栈（用于迭代节点的变量隔离）
    scope_stack: parking_lot::RwLock<Vec<Scope>>,
}

/// 作用域
#[derive(Debug, Clone)]
pub struct Scope {
    /// 作用域 ID（通常是迭代节点 ID + 索引）
    pub scope_id: String,

    /// 作用域内的局部变量
    pub variables: HashMap<String, Value>,
}

impl VariablePool {
    /// 创建新的变量池
    pub fn new() -> Self {
        VariablePool {
            node_outputs: DashMap::new(),
            system_vars: DashMap::new(),
            conversation_vars: DashMap::new(),
            scope_stack: parking_lot::RwLock::new(Vec::new()),
        }
    }

    /// 使用初始输入创建变量池
    pub fn with_inputs(inputs: Value) -> Self {
        let pool = Self::new();
        pool.set_node_output("input", inputs);
        pool
    }

    /// 设置节点输出
    pub fn set_node_output(&self, node_id: &str, output: Value) {
        self.node_outputs.insert(node_id.to_string(), output);
    }

    /// 获取节点输出
    pub fn get_node_output(&self, node_id: &str) -> Option<Value> {
        self.node_outputs.get(node_id).map(|v| v.value().clone())
    }

    /// 获取变量值（支持路径解析）
    ///
    /// 示例:
    /// - "llm_node_1.text" -> 获取 llm_node_1 输出的 text 字段
    /// - "input.name" -> 获取输入的 name 字段
    /// - "sys.query" -> 获取系统变量 query
    pub fn get_value(&self, selector: &str) -> Option<Value> {
        let parts: Vec<&str> = selector.splitn(2, '.').collect();

        if parts.is_empty() {
            return None;
        }

        let root = parts[0];

        // 1. 先检查当前作用域栈
        {
            let scopes = self.scope_stack.read();
            for scope in scopes.iter().rev() {
                if let Some(value) = scope.variables.get(root) {
                    if parts.len() == 1 {
                        return Some(value.clone());
                    } else {
                        return Self::resolve_path(value, parts[1]);
                    }
                }
            }
        }

        // 2. 检查系统变量
        if root == "sys" {
            if parts.len() > 1 {
                return self.system_vars.get(parts[1]).map(|v| v.value().clone());
            }
            return None;
        }

        // 3. 检查节点输出
        if let Some(node_output) = self.node_outputs.get(root) {
            if parts.len() == 1 {
                return Some(node_output.value().clone());
            } else {
                return Self::resolve_path(node_output.value(), parts[1]);
            }
        }

        // 4. 检查对话变量
        if let Some(conv_var) = self.conversation_vars.get(root) {
            if parts.len() == 1 {
                return Some(conv_var.value().clone());
            } else {
                return Self::resolve_path(conv_var.value(), parts[1]);
            }
        }

        None
    }

    /// 解析 JSON 路径
    fn resolve_path(value: &Value, path: &str) -> Option<Value> {
        let mut current = value;
        for part in path.split('.') {
            // 检查是否是数组索引 (e.g., "items[0]")
            if let Some(bracket_pos) = part.find('[') {
                let key = &part[..bracket_pos];
                let index_str = &part[bracket_pos + 1..part.len() - 1];

                if !key.is_empty() {
                    current = current.get(key)?;
                }

                let index: usize = index_str.parse().ok()?;
                current = current.get(index)?;
            } else {
                current = current.get(part)?;
            }
        }
        Some(current.clone())
    }

    /// 解析模板字符串中的所有变量
    ///
    /// 支持两种语法：
    /// - `{{#node_id.field#}}` - Dify 风格
    /// - 返回替换后的字符串
    pub fn resolve_template(&self, text: &str) -> Result<String, NodeError> {
        let re = Regex::new(r"\{\{#([^#]+)#\}\}")
            .map_err(|e| NodeError::TemplateError(e.to_string()))?;

        let mut result = text.to_string();

        for cap in re.captures_iter(text) {
            let full_match = cap.get(0).unwrap().as_str();
            let selector = cap.get(1).unwrap().as_str().trim();

            if let Some(value) = self.get_value(selector) {
                let replacement = match &value {
                    Value::String(s) => s.clone(),
                    Value::Null => "".to_string(),
                    other => other.to_string(),
                };
                result = result.replace(full_match, &replacement);
            } else {
                // 未找到变量时替换为空字符串
                result = result.replace(full_match, "");
            }
        }

        Ok(result)
    }

    /// 设置系统变量
    pub fn set_system_var(&self, key: &str, value: Value) {
        self.system_vars.insert(key.to_string(), value);
    }

    /// 设置对话变量
    pub fn set_conversation_var(&self, key: &str, value: Value) {
        self.conversation_vars.insert(key.to_string(), value);
    }

    /// 获取所有变量（用于模板渲染）
    pub fn get_all_variables(&self) -> HashMap<String, Value> {
        let mut vars = HashMap::new();

        // 添加节点输出
        for entry in self.node_outputs.iter() {
            vars.insert(entry.key().clone(), entry.value().clone());
        }

        // 添加系统变量
        let mut sys_vars = serde_json::Map::new();
        for entry in self.system_vars.iter() {
            sys_vars.insert(entry.key().clone(), entry.value().clone());
        }
        if !sys_vars.is_empty() {
            vars.insert("sys".to_string(), Value::Object(sys_vars));
        }

        // 添加作用域变量
        let scopes = self.scope_stack.read();
        for scope in scopes.iter() {
            for (k, v) in &scope.variables {
                vars.insert(k.clone(), v.clone());
            }
        }

        vars
    }

    /// 进入新作用域（迭代开始时调用）
    pub fn push_scope(&self, scope_id: String, variables: HashMap<String, Value>) {
        let mut scopes = self.scope_stack.write();
        scopes.push(Scope {
            scope_id,
            variables,
        });
    }

    /// 退出作用域（迭代结束时调用）
    pub fn pop_scope(&self) -> Option<Scope> {
        let mut scopes = self.scope_stack.write();
        scopes.pop()
    }
}

impl Default for VariablePool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_set_and_get_node_output() {
        let pool = VariablePool::new();
        pool.set_node_output("node1", json!({"text": "hello"}));

        let value = pool.get_value("node1.text");
        assert_eq!(value, Some(json!("hello")));
    }

    #[test]
    fn test_get_full_node_output() {
        let pool = VariablePool::new();
        pool.set_node_output("node1", json!({"text": "hello", "count": 42}));

        let value = pool.get_value("node1");
        assert_eq!(value, Some(json!({"text": "hello", "count": 42})));
    }

    #[test]
    fn test_nested_path() {
        let pool = VariablePool::new();
        pool.set_node_output(
            "node1",
            json!({
                "data": {
                    "items": [1, 2, 3]
                }
            }),
        );

        let value = pool.get_value("node1.data.items");
        assert_eq!(value, Some(json!([1, 2, 3])));
    }

    #[test]
    fn test_array_index() {
        let pool = VariablePool::new();
        pool.set_node_output(
            "node1",
            json!({
                "data": {
                    "items": [10, 20, 30]
                }
            }),
        );

        let value = pool.get_value("node1.data.items[0]");
        assert_eq!(value, Some(json!(10)));
    }

    #[test]
    fn test_resolve_template() {
        let pool = VariablePool::new();
        pool.set_node_output("input", json!({"name": "World"}));
        pool.set_node_output("llm", json!({"text": "Hello!"}));

        let result = pool
            .resolve_template("Hello {{#input.name#}}, LLM says: {{#llm.text#}}")
            .unwrap();
        assert_eq!(result, "Hello World, LLM says: Hello!");
    }

    #[test]
    fn test_resolve_template_missing_var() {
        let pool = VariablePool::new();

        let result = pool
            .resolve_template("Hello {{#missing.var#}}!")
            .unwrap();
        assert_eq!(result, "Hello !");
    }

    #[test]
    fn test_system_vars() {
        let pool = VariablePool::new();
        pool.set_system_var("query", json!("test query"));

        let value = pool.get_value("sys.query");
        assert_eq!(value, Some(json!("test query")));
    }

    #[test]
    fn test_scope_management() {
        let pool = VariablePool::new();

        // 设置节点输出
        pool.set_node_output("node1", json!({"text": "global"}));

        // 进入作用域
        let mut scope_vars = HashMap::new();
        scope_vars.insert("item".to_string(), json!("item1"));
        scope_vars.insert("index".to_string(), json!(0));
        pool.push_scope("iter_0".to_string(), scope_vars);

        // 在作用域内可以访问局部变量
        assert_eq!(pool.get_value("item"), Some(json!("item1")));
        assert_eq!(pool.get_value("index"), Some(json!(0)));

        // 仍然可以访问全局变量
        assert_eq!(pool.get_value("node1.text"), Some(json!("global")));

        // 退出作用域
        pool.pop_scope();

        // 局部变量不再可访问
        assert_eq!(pool.get_value("item"), None);
    }

    #[test]
    fn test_conversation_vars() {
        let pool = VariablePool::new();
        pool.set_conversation_var("history", json!(["msg1", "msg2"]));

        let value = pool.get_value("history");
        assert_eq!(value, Some(json!(["msg1", "msg2"])));
    }

    #[test]
    fn test_get_all_variables() {
        let pool = VariablePool::new();
        pool.set_node_output("node1", json!({"text": "hello"}));
        pool.set_system_var("query", json!("test"));

        let vars = pool.get_all_variables();
        assert!(vars.contains_key("node1"));
        assert!(vars.contains_key("sys"));
    }
}
