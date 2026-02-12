use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::dsl::{parse_dsl, validate_schema, DslFormat, WorkflowSchema};
use crate::error::WorkflowError;
use crate::graph::{build_graph, Graph};
use crate::scheduler::{collect_conversation_variable_types, collect_start_variable_types};

use super::compiled_workflow::{
    CompiledConfig,
    CompiledNodeConfig,
    CompiledNodeConfigMap,
    CompiledWorkflow,
};

pub struct WorkflowCompiler;

impl WorkflowCompiler {
    /// Compile from DSL text content.
    pub fn compile(content: &str, format: DslFormat) -> Result<CompiledWorkflow, WorkflowError> {
        let schema = parse_dsl(content, format)?;
        let content_hash = Self::hash_bytes(content.as_bytes());
        Self::compile_schema_with_hash(schema, content_hash)
    }

    /// Compile from a pre-parsed schema.
    pub fn compile_schema(schema: WorkflowSchema) -> Result<CompiledWorkflow, WorkflowError> {
        let content_hash = Self::hash_schema(&schema);
        Self::compile_schema_with_hash(schema, content_hash)
    }

    fn compile_schema_with_hash(
        schema: WorkflowSchema,
        content_hash: u64,
    ) -> Result<CompiledWorkflow, WorkflowError> {
        let report = validate_schema(&schema);
        if !report.is_valid {
            return Err(WorkflowError::ValidationFailed(report));
        }

        let graph = build_graph(&schema)?;
        let graph_template = Arc::clone(&graph.topology);

        let start_var_types = collect_start_variable_types(&schema);
        let conversation_var_types = collect_conversation_variable_types(&schema);
        let start_node_id = graph.root_node_id().to_string();

        let node_configs = Self::compile_node_configs(&graph);

        Ok(CompiledWorkflow {
            compiled_at: Instant::now(),
            content_hash,
            schema: Arc::new(schema),
            graph_template,
            start_var_types: Arc::new(start_var_types),
            conversation_var_types: Arc::new(conversation_var_types),
            start_node_id: Arc::from(start_node_id),
            validation_report: Arc::new(report),
            node_configs: Arc::new(node_configs),
        })
    }

    fn compile_node_configs(graph: &Graph) -> CompiledNodeConfigMap {
        let mut map: CompiledNodeConfigMap = HashMap::new();
        for (node_id, node) in graph.topology.nodes.iter() {
            let compiled = Self::compile_node_config(&node.node_type, &node.config);
            map.insert(node_id.clone(), Arc::new(compiled));
        }
        map
    }

    fn compile_node_config(node_type: &str, raw: &Value) -> CompiledNodeConfig {
        match node_type {
            "start" => Self::compile_typed(raw, CompiledNodeConfig::Start),
            "end" => Self::compile_typed(raw, CompiledNodeConfig::End),
            "answer" => Self::compile_typed(raw, CompiledNodeConfig::Answer),
            "if-else" => Self::compile_typed(raw, CompiledNodeConfig::IfElse),
            "template-transform" => Self::compile_typed(raw, CompiledNodeConfig::TemplateTransform),
            "code" => Self::compile_typed(raw, CompiledNodeConfig::Code),
            "http-request" => Self::compile_typed(raw, CompiledNodeConfig::HttpRequest),
            "document-extractor" => Self::compile_typed(raw, CompiledNodeConfig::DocumentExtractor),
            "variable-aggregator" => Self::compile_typed(raw, CompiledNodeConfig::VariableAggregator),
            "assigner" | "variable-assigner" => {
                Self::compile_typed(raw, CompiledNodeConfig::VariableAssigner)
            }
            "iteration" => Self::compile_typed(raw, CompiledNodeConfig::Iteration),
            "loop" => Self::compile_typed(raw, CompiledNodeConfig::Loop),
            "list-operator" => Self::compile_typed(raw, CompiledNodeConfig::ListOperator),
            "llm" => Self::compile_typed(raw, CompiledNodeConfig::Llm),
            _ => CompiledNodeConfig::Raw(raw.clone()),
        }
    }

    fn compile_typed<T>(raw: &Value, wrap: fn(CompiledConfig<T>) -> CompiledNodeConfig) -> CompiledNodeConfig
    where
        T: DeserializeOwned,
    {
        match serde_json::from_value::<T>(raw.clone()) {
            Ok(parsed) => wrap(CompiledConfig::new(raw.clone(), parsed)),
            Err(_) => CompiledNodeConfig::Raw(raw.clone()),
        }
    }

    fn hash_schema(schema: &WorkflowSchema) -> u64 {
        let value = serde_json::to_value(schema).unwrap_or(Value::Null);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        Self::hash_value(&value, &mut hasher);
        hasher.finish()
    }

    fn hash_bytes(bytes: &[u8]) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    }

    fn hash_value<H: Hasher>(value: &Value, state: &mut H) {
        match value {
            Value::Null => {
                0u8.hash(state);
            }
            Value::Bool(b) => {
                1u8.hash(state);
                b.hash(state);
            }
            Value::Number(n) => {
                2u8.hash(state);
                if let Some(i) = n.as_i64() {
                    i.hash(state);
                } else if let Some(u) = n.as_u64() {
                    u.hash(state);
                } else if let Some(f) = n.as_f64() {
                    state.write(&f.to_le_bytes());
                }
            }
            Value::String(s) => {
                3u8.hash(state);
                s.hash(state);
            }
            Value::Array(items) => {
                4u8.hash(state);
                items.len().hash(state);
                for item in items {
                    Self::hash_value(item, state);
                }
            }
            Value::Object(map) => {
                5u8.hash(state);
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                keys.len().hash(state);
                for key in keys {
                    key.hash(state);
                    if let Some(value) = map.get(key) {
                        Self::hash_value(value, state);
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "builtin-core-nodes"))]
mod tests {
        use super::*;
        use crate::scheduler::ExecutionStatus;
        use serde_json::Value;
        use std::collections::HashMap;

        #[tokio::test]
        async fn test_compile_and_run_basic() {
            let yaml = r#"
    version: "0.1.0"
    nodes:
      - id: start
        data:
          type: start
          title: Start
          variables:
            - variable: query
              label: Q
              type: string
              required: true
      - id: end
        data:
          type: end
          title: End
          outputs:
            - variable: result
              value_selector: ["start", "query"]
    edges:
      - source: start
        target: end
    "#;
            let compiled = WorkflowCompiler::compile(yaml, DslFormat::Yaml).unwrap();

            let mut inputs = HashMap::new();
            inputs.insert("query".to_string(), Value::String("hello".into()));

            let handle = compiled.runner().user_inputs(inputs).run().await.unwrap();
            let status = handle.wait().await;
            match status {
                ExecutionStatus::Completed(outputs) => {
                    assert_eq!(outputs.get("result"), Some(&Value::String("hello".into())));
                }
                other => panic!("Expected Completed, got {:?}", other),
            }
        }
}
