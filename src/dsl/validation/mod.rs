mod layer1_structure;
mod layer2_topology;
mod layer3_semantic;
mod known_types;
mod types;

use crate::dsl::parser::{parse_dsl, DslFormat};
use crate::dsl::schema::WorkflowSchema;

pub use types::{Diagnostic, DiagnosticLevel, ValidationReport};

pub fn validate_dsl(content: &str, format: DslFormat) -> ValidationReport {
    match parse_dsl(content, format) {
        Ok(schema) => validate_schema(&schema),
        Err(err) => ValidationReport {
            is_valid: false,
            diagnostics: vec![types::Diagnostic {
                level: types::DiagnosticLevel::Error,
                code: "E001".to_string(),
                message: format!("DSL parse error: {}", err),
                node_id: None,
                edge_id: None,
                field_path: None,
            }],
        },
    }
}

pub fn validate_schema(schema: &WorkflowSchema) -> ValidationReport {
    let mut diagnostics = Vec::new();

    let layer1 = layer1_structure::validate(schema);
    diagnostics.extend(layer1);

    let has_fatal_structure = diagnostics.iter().any(|d| {
        d.level == types::DiagnosticLevel::Error
            && matches!(d.code.as_str(), "E003" | "E004" | "E005")
    });

    let topo_info = if has_fatal_structure {
        layer2_topology::TopologyInfo {
            reachable: std::collections::HashSet::new(),
            topo_level: std::collections::HashMap::new(),
        }
    } else {
        let (layer2, info) = layer2_topology::validate(schema);
        diagnostics.extend(layer2);
        info
    };

    diagnostics.extend(layer3_semantic::validate(schema, &topo_info));

    let is_valid = diagnostics
        .iter()
        .all(|d| d.level != types::DiagnosticLevel::Error);

    ValidationReport {
        is_valid,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::dsl::parser::DslFormat;
        use crate::dsl::schema::WorkflowSchema;

        #[test]
        fn test_validate_dsl_parse_error() {
                let report = validate_dsl("{{invalid", DslFormat::Json);
                assert!(!report.is_valid);
                assert!(report.diagnostics.iter().any(|d| d.code == "E001"));
        }

        #[test]
        fn test_cycle_detected() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"Start"}},{"id":"a","data":{"type":"code","title":"A","code":"x","language":"javascript"}},{"id":"end","data":{"type":"end","title":"End","outputs":[]}}],"edges":[{"source":"start","target":"a"},{"source":"a","target":"start"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E101"));
        }

        #[test]
        fn test_invalid_start_var_type() {
                let json = r#"{"version":"0.1.0","nodes":[{"id":"start","data":{"type":"start","title":"Start","variables":[{"variable":"query","type":"invalid_type"}]}},{"id":"end","data":{"type":"end","title":"End","outputs":[{"variable":"out","value_selector":["start","query"]}]}}],"edges":[{"source":"start","target":"end"}]}"#;
                let schema: WorkflowSchema = serde_json::from_str(json).unwrap();
                let report = validate_schema(&schema);
                assert!(report.diagnostics.iter().any(|d| d.code == "E201"));
        }
}
