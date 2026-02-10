use std::ops::ControlFlow;

use boa_engine::ast::expression::access::{PropertyAccess, PropertyAccessField};
use boa_engine::ast::expression::literal::Literal;
use boa_engine::ast::expression::{Call, Expression, Identifier, ImportCall, New};
use boa_engine::ast::statement::iteration::{DoWhileLoop, ForLoop, WhileLoop};
use boa_engine::ast::visitor::{VisitWith, Visitor};
use boa_engine::ast::Script;
use boa_engine::interner::Interner;
use boa_engine::parser::{Parser, Source};
use boa_engine::ast::scope::Scope;

use crate::sandbox::error::SandboxError;

#[derive(Debug, Clone)]
pub struct JsSandboxSecurityConfig {
    pub max_code_length: usize,
    pub default_timeout: std::time::Duration,
    pub max_memory_estimate: usize,
    pub max_output_bytes: usize,
    pub freeze_globals: bool,
    pub allowed_globals: Vec<String>,
}

impl Default for JsSandboxSecurityConfig {
    fn default() -> Self {
        Self {
            max_code_length: 1_000_000,
            default_timeout: std::time::Duration::from_secs(30),
            max_memory_estimate: 32 * 1024 * 1024,
            max_output_bytes: 1 * 1024 * 1024,
            freeze_globals: true,
            allowed_globals: vec![
                "JSON".into(),
                "Math".into(),
                "parseInt".into(),
                "parseFloat".into(),
                "isNaN".into(),
                "isFinite".into(),
                "Number".into(),
                "String".into(),
                "Boolean".into(),
                "Array".into(),
                "Object".into(),
                "encodeURIComponent".into(),
                "decodeURIComponent".into(),
                "encodeURI".into(),
                "decodeURI".into(),
                "btoa".into(),
                "atob".into(),
                "RegExp".into(),
                "Date".into(),
                "datetime".into(),
                "crypto".into(),
                "uuidv4".into(),
                "randomInt".into(),
                "randomFloat".into(),
                "randomBytes".into(),
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodeAnalysisResult {
    pub is_safe: bool,
    pub violations: Vec<CodeViolation>,
}

#[derive(Debug, Clone)]
pub struct CodeViolation {
    pub kind: ViolationKind,
    pub location: (usize, usize),
    pub description: String,
}

#[derive(Debug, Clone)]
pub enum ViolationKind {
    DynamicExecution,
    PrototypeTampering,
    ForbiddenGlobal,
    InfiniteLoopRisk,
}

pub trait CodeAnalyzer: Send + Sync {
    fn analyze(&self, code: &str) -> Result<CodeAnalysisResult, SandboxError>;
}

#[derive(Default)]
pub struct AstCodeAnalyzer;

impl AstCodeAnalyzer {
    fn analyze_script(
        &self,
        script: &Script,
        interner: &Interner,
    ) -> CodeAnalysisResult {
        let mut visitor = AstSecurityVisitor::new(interner);
        let _ = visitor.visit_script(script);
        let is_safe = visitor.violations.is_empty();
        CodeAnalysisResult {
            is_safe,
            violations: visitor.violations,
        }
    }
}

impl CodeAnalyzer for AstCodeAnalyzer {
    fn analyze(&self, code: &str) -> Result<CodeAnalysisResult, SandboxError> {
        let mut interner = Interner::default();
        let mut parser = Parser::new(Source::from_bytes(code));
        let script = parser
            .parse_script(&Scope::new_global(), &mut interner)
            .map_err(|e| SandboxError::CompilationError(e.to_string()))?;
        Ok(self.analyze_script(&script, &interner))
    }
}

struct AstSecurityVisitor<'a> {
    interner: &'a Interner,
    violations: Vec<CodeViolation>,
}

impl<'a> AstSecurityVisitor<'a> {
    fn new(interner: &'a Interner) -> Self {
        Self {
            interner,
            violations: Vec::new(),
        }
    }

    fn push_violation(&mut self, kind: ViolationKind, description: impl Into<String>) {
        self.violations.push(CodeViolation {
            kind,
            location: (0, 0),
            description: description.into(),
        });
    }

    fn ident_name(&self, ident: Identifier) -> Option<String> {
        self.interner
            .resolve(ident.sym())
            .map(|s| s.to_string())
    }

    fn is_literal_true(expr: &Expression) -> bool {
        matches!(expr, Expression::Literal(Literal::Bool(true)))
    }

    fn matches_identifier(expr: &Expression, name: &str, interner: &Interner) -> bool {
        if let Expression::Identifier(id) = expr {
            return interner
                .resolve(id.sym())
                .and_then(|s| s.utf8().map(|value| value == name))
                .unwrap_or(false);
        }
        false
    }
}

impl<'ast> Visitor<'ast> for AstSecurityVisitor<'_> {
    type BreakTy = ();

    fn visit_identifier(&mut self, node: &'ast Identifier) -> ControlFlow<Self::BreakTy> {
        if let Some(name) = self.ident_name(*node) {
            match name.as_str() {
                "process" | "require" | "globalThis" | "import" => {
                    self.push_violation(ViolationKind::ForbiddenGlobal, name);
                }
                _ => {}
            }
        }
        ControlFlow::Continue(())
    }

    fn visit_call(&mut self, node: &'ast Call) -> ControlFlow<Self::BreakTy> {
        if Self::matches_identifier(node.function(), "eval", self.interner)
            || Self::matches_identifier(node.function(), "Function", self.interner)
        {
            self.push_violation(ViolationKind::DynamicExecution, "dynamic call");
        }
        node.visit_with(self)
    }

    fn visit_new(&mut self, node: &'ast New) -> ControlFlow<Self::BreakTy> {
        if Self::matches_identifier(node.constructor(), "Function", self.interner) {
            self.push_violation(ViolationKind::DynamicExecution, "Function constructor");
        }
        node.visit_with(self)
    }

    fn visit_property_access(
        &mut self,
        node: &'ast PropertyAccess,
    ) -> ControlFlow<Self::BreakTy> {
        if let PropertyAccess::Simple(simple) = node {
            if let PropertyAccessField::Const(sym) = simple.field() {
                if let Some(field) = self.interner.resolve(*sym) {
                    if let Some(name) = field.utf8() {
                        if name == "__proto__" || name == "constructor" || name == "prototype" {
                            self.push_violation(ViolationKind::PrototypeTampering, name);
                        }
                    }
                }
            }
        }
        node.visit_with(self)
    }

    fn visit_while_loop(&mut self, node: &'ast WhileLoop) -> ControlFlow<Self::BreakTy> {
        if Self::is_literal_true(node.condition()) {
            self.push_violation(ViolationKind::InfiniteLoopRisk, "while(true)");
        }
        node.visit_with(self)
    }

    fn visit_do_while_loop(&mut self, node: &'ast DoWhileLoop) -> ControlFlow<Self::BreakTy> {
        if Self::is_literal_true(node.cond()) {
            self.push_violation(ViolationKind::InfiniteLoopRisk, "do { } while(true)");
        }
        node.visit_with(self)
    }

    fn visit_for_loop(&mut self, node: &'ast ForLoop) -> ControlFlow<Self::BreakTy> {
        if node.condition().is_none()
            || node
                .condition()
                .map(Self::is_literal_true)
                .unwrap_or(false)
        {
            self.push_violation(ViolationKind::InfiniteLoopRisk, "for(;;)");
        }
        node.visit_with(self)
    }

    fn visit_import_call(&mut self, _node: &'ast ImportCall) -> ControlFlow<Self::BreakTy> {
        self.push_violation(ViolationKind::ForbiddenGlobal, "import()");
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ast_analyzer_detects_eval() {
        let analyzer = AstCodeAnalyzer::default();
        let result = analyzer
            .analyze("function main(inputs){ return eval('1+1'); }")
            .unwrap();
        assert!(!result.is_safe);
        assert!(result
            .violations
            .iter()
            .any(|v| matches!(v.kind, ViolationKind::DynamicExecution)));
    }

    #[test]
    fn test_ast_analyzer_detects_proto() {
        let analyzer = AstCodeAnalyzer::default();
        let result = analyzer
            .analyze("function main(){ return ({}).__proto__; }")
            .unwrap();
        assert!(!result.is_safe);
        assert!(result
            .violations
            .iter()
            .any(|v| matches!(v.kind, ViolationKind::PrototypeTampering)));
    }
}