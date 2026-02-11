//! Extension traits for plugins: template functions and DSL validators.

use crate::dsl::schema::WorkflowSchema;
use crate::dsl::validation::Diagnostic;

pub use xworkflow_types::template::TemplateFunction;

/// A custom DSL validation rule contributed by a plugin.
pub trait DslValidator: Send + Sync {
    /// Human-readable name for this validator.
    fn name(&self) -> &str;
    /// Run the validation against a workflow schema.
    fn validate(&self, schema: &WorkflowSchema) -> Vec<Diagnostic>;
}
