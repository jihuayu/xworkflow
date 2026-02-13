//! Validation diagnostic types.

use serde::{Deserialize, Serialize};

/// Severity level of a validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticLevel {
    Error,
    Warning,
}

/// A single validation finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub message: String,
    pub node_id: Option<String>,
    pub edge_id: Option<String>,
    pub field_path: Option<String>,
}

/// Aggregated result of DSL validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub is_valid: bool,
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    /// Return only the error-level diagnostics.
    pub fn errors(&self) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.level == DiagnosticLevel::Error)
            .collect()
    }

    /// Return only the warning-level diagnostics.
    pub fn warnings(&self) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.level == DiagnosticLevel::Warning)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_diagnostic(level: DiagnosticLevel, code: &str) -> Diagnostic {
        Diagnostic {
            level,
            code: code.to_string(),
            message: format!("test {}", code),
            node_id: None,
            edge_id: None,
            field_path: None,
        }
    }

    #[test]
    fn test_diagnostic_level_eq() {
        assert_eq!(DiagnosticLevel::Error, DiagnosticLevel::Error);
        assert_eq!(DiagnosticLevel::Warning, DiagnosticLevel::Warning);
        assert_ne!(DiagnosticLevel::Error, DiagnosticLevel::Warning);
    }

    #[test]
    fn test_validation_report_errors_empty() {
        let report = ValidationReport {
            is_valid: true,
            diagnostics: vec![],
        };
        assert!(report.errors().is_empty());
        assert!(report.warnings().is_empty());
    }

    #[test]
    fn test_validation_report_errors_only() {
        let report = ValidationReport {
            is_valid: false,
            diagnostics: vec![
                make_diagnostic(DiagnosticLevel::Error, "E001"),
                make_diagnostic(DiagnosticLevel::Error, "E002"),
            ],
        };
        assert_eq!(report.errors().len(), 2);
        assert_eq!(report.warnings().len(), 0);
    }

    #[test]
    fn test_validation_report_warnings_only() {
        let report = ValidationReport {
            is_valid: true,
            diagnostics: vec![make_diagnostic(DiagnosticLevel::Warning, "W001")],
        };
        assert_eq!(report.errors().len(), 0);
        assert_eq!(report.warnings().len(), 1);
    }

    #[test]
    fn test_validation_report_mixed() {
        let report = ValidationReport {
            is_valid: false,
            diagnostics: vec![
                make_diagnostic(DiagnosticLevel::Error, "E001"),
                make_diagnostic(DiagnosticLevel::Warning, "W001"),
                make_diagnostic(DiagnosticLevel::Error, "E002"),
                make_diagnostic(DiagnosticLevel::Warning, "W002"),
            ],
        };
        assert_eq!(report.errors().len(), 2);
        assert_eq!(report.warnings().len(), 2);
    }

    #[test]
    fn test_diagnostic_with_all_fields() {
        let d = Diagnostic {
            level: DiagnosticLevel::Error,
            code: "E001".into(),
            message: "err".into(),
            node_id: Some("n1".into()),
            edge_id: Some("e1".into()),
            field_path: Some("config.x".into()),
        };
        assert_eq!(d.node_id.as_deref(), Some("n1"));
        assert_eq!(d.edge_id.as_deref(), Some("e1"));
        assert_eq!(d.field_path.as_deref(), Some("config.x"));
    }

    #[test]
    fn test_diagnostic_serde_roundtrip() {
        let d = make_diagnostic(DiagnosticLevel::Error, "E001");
        let json = serde_json::to_string(&d).unwrap();
        let deserialized: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.code, "E001");
        assert_eq!(deserialized.level, DiagnosticLevel::Error);
    }

    #[test]
    fn test_validation_report_serde_roundtrip() {
        let report = ValidationReport {
            is_valid: false,
            diagnostics: vec![make_diagnostic(DiagnosticLevel::Error, "E001")],
        };
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: ValidationReport = serde_json::from_str(&json).unwrap();
        assert!(!deserialized.is_valid);
        assert_eq!(deserialized.diagnostics.len(), 1);
    }
}
