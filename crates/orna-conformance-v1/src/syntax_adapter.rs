//! Parse-only adapter for the independently versioned Orna 1.0 syntax crate.
//!
//! Parser diagnostics are converted at the foundation boundary.  In
//! particular, this adapter does not call syntax's `*_with_file` entry points:
//! conformance fixture identifiers are logical names, not diagnostic source
//! observations, and the report must not disclose host paths or source text.

use crate::{ConformanceAdapter, ProjectUnit, SourceUnit, StageOutcome};
use orna_foundation_v1::{Diagnostic, DiagnosticSeverity, SafeText};
use orna_syntax_v1::{SyntaxDiagnostic, parse_module, parse_row};

/// Executes the syntax portion of conformance and explicitly leaves semantic
/// and runtime stages for their future owning implementations.
#[derive(Default)]
pub struct SyntaxAdapter;

impl ConformanceAdapter for SyntaxAdapter {
    type Diagnostic = Diagnostic;

    fn diagnostic_code(&self, diagnostic: &Self::Diagnostic) -> String {
        diagnostic.code().into()
    }

    fn diagnostic_message(&self, diagnostic: &Self::Diagnostic) -> String {
        diagnostic.message().into()
    }

    fn parse(&mut self, unit: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        let diagnostic = match unit.parse_as.as_str() {
            "module_unit" => parse_module(&unit.source).diagnostics.into_iter().next(),
            "row_unit" => parse_row(&unit.source).diagnostics.into_iter().next(),
            _ => return StageOutcome::Failed(unsupported_entry_point()),
        };
        match diagnostic {
            Some(diagnostic) => StageOutcome::Failed(foundation_diagnostic(&diagnostic)),
            None => StageOutcome::Passed,
        }
    }

    fn resolve(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        semantic_stage_skipped()
    }

    fn typecheck(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        semantic_stage_skipped()
    }

    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        semantic_stage_skipped()
    }

    fn parse_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        semantic_stage_skipped()
    }

    fn resolve_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        semantic_stage_skipped()
    }

    fn typecheck_project(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        semantic_stage_skipped()
    }

    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Self::Diagnostic> {
        semantic_stage_skipped()
    }

    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Self::Diagnostic> {
        semantic_stage_skipped()
    }
}

fn foundation_diagnostic(diagnostic: &SyntaxDiagnostic) -> Diagnostic {
    // Syntax diagnostics are parser-owned static messages.  We still admit
    // them through SafeText and intentionally omit native spans, labels and
    // payload: those may contain source observations but the corpus has no
    // pinned repository snapshot at this seam.
    Diagnostic::new(
        SafeText::new(diagnostic.code).expect("syntax diagnostic codes are safe"),
        DiagnosticSeverity::Error,
        SafeText::new(&diagnostic.message).unwrap_or_else(|_| SafeText::redacted()),
    )
    .expect("syntax diagnostics have non-empty codes")
    .redacted()
}

fn unsupported_entry_point() -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-CONFORMANCE-PARSE-ENTRY").expect("static code is safe"),
        DiagnosticSeverity::Error,
        SafeText::new("unsupported conformance parse entry point").expect("static message is safe"),
    )
    .expect("static diagnostic is valid")
    .redacted()
}

fn semantic_stage_skipped<D>() -> StageOutcome<D> {
    StageOutcome::Skipped {
        reason: "syntax adapter implements Parse only; semantic/runtime stage is not integrated"
            .into(),
    }
}
