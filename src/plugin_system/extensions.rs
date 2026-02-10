use crate::dsl::schema::WorkflowSchema;
use crate::dsl::validation::Diagnostic;

pub use xworkflow_types::template::TemplateFunction;

pub trait DslValidator: Send + Sync {
    fn name(&self) -> &str;
    fn validate(&self, schema: &WorkflowSchema) -> Vec<Diagnostic>;
}
