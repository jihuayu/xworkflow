use serde_json::Value;

use crate::dsl::schema::WorkflowSchema;
use crate::dsl::validation::Diagnostic;

pub trait TemplateFunction: Send + Sync {
    fn name(&self) -> &str;
    fn call(&self, args: &[Value]) -> Result<Value, String>;
}

pub trait DslValidator: Send + Sync {
    fn name(&self) -> &str;
    fn validate(&self, schema: &WorkflowSchema) -> Vec<Diagnostic>;
}
