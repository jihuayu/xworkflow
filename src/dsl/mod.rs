//! DSL parsing, schema types, and validation.
//!
//! This module handles the complete lifecycle of a workflow definition:
//! 1. **Parsing** — [`parse_dsl`] converts YAML/JSON text into a [`WorkflowSchema`].
//! 2. **Schema types** — [`schema`] defines all DSL data structures (nodes, edges,
//!    error strategies, LLM config, etc.).
//! 3. **Validation** — [`validate_dsl`] / [`validate_schema`] perform three-layer
//!    validation (structure, topology, semantics) producing a [`ValidationReport`].

pub mod parser;
pub mod schema;
pub mod validation;

pub use parser::{parse_dsl, DslFormat};
pub use schema::*;
pub use validation::{validate_dsl, validate_schema, Diagnostic, DiagnosticLevel, ValidationReport};
