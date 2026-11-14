use super::*;

pub struct SemanticAdapter {
    pub(super) syntax: SyntaxAdapter,
    pub(super) catalogue: Catalogue,
}

impl Default for SemanticAdapter {
    fn default() -> Self {
        Self {
            syntax: SyntaxAdapter,
            catalogue: Catalogue::authoritative_fixture(),
        }
    }
}

impl SemanticAdapter {
    pub(super) fn project_analysis(&self, project: &ProjectUnit) -> orna_check::Analysis {
        let prefix = format!("{}/", project.project_id.trim_end_matches('/'));
        let inputs = project
            .modules
            .iter()
            .map(|unit| {
                let logical_path = unit
                    .source_id
                    .strip_prefix(&prefix)
                    .unwrap_or(&unit.source_id)
                    .to_owned();
                ModuleInput::new(logical_path, unit.source.clone())
            })
            .collect::<Vec<_>>();
        analyze_with_providers(&inputs, &self.catalogue, SystemProviders::Production)
    }

    pub(super) fn analyze_units(
        &self,
        units: impl IntoIterator<Item = SourceUnit>,
        phase: SemanticPhase,
    ) -> StageOutcome<Diagnostic> {
        let inputs = units
            .into_iter()
            .map(|unit| ModuleInput::new(unit.source_id, unit.source))
            .collect::<Vec<_>>();
        let analysis =
            analyze_with_providers(&inputs, &self.catalogue, SystemProviders::DeclaredSurface);
        select_diagnostic(analysis.diagnostics, phase)
    }

    pub(super) fn analyze_project(
        &self,
        project: &ProjectUnit,
        phase: SemanticPhase,
    ) -> StageOutcome<Diagnostic> {
        let analysis = self.project_analysis(project);
        select_diagnostic(analysis.diagnostics, phase)
    }
    pub(super) fn unsupported_runtime() -> StageOutcome<Diagnostic> {
        StageOutcome::Skipped {
            reason: "orna-runtime supplies durable runtime state only; it has no source evaluator or scenario invocation contract".into(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum SemanticPhase {
    Resolve,
    Typecheck,
}

impl SemanticPhase {
    pub(super) fn accepts(self, diagnostic: &Diagnostic) -> bool {
        // Annotation-boundary type names (for example unimported nominal
        // annotations) fail before inference; the conformance harness treats
        // them as typecheck-phase failures rather than resolution failures.
        let is_typecheck = typecheck_phase(diagnostic);
        match self {
            Self::Resolve => {
                !is_typecheck && {
                    matches!(
                        diagnostic.code(),
                        "ORNA100-E-SYS-RUNTIME"
                            | "ORNA091-E-TRYFROM"
                            | "ORNA-S001-PATH"
                            | "ORNA-S002-NAMESPACE"
                            | "ORNA-S003-RESERVED"
                            | "ORNA-S010-IMPORT"
                            | "ORNA-S011-AMBIGUOUS"
                            | "ORNA-S012-UNRESOLVED"
                            | "ORNA-S013-DUPLICATE"
                    )
                }
            }
            Self::Typecheck => is_typecheck,
        }
    }
}

pub(super) fn typecheck_phase(diagnostic: &Diagnostic) -> bool {
    matches!(
        diagnostic.code(),
        "ORNA091-E-RESULT"
            | "E2001"
            | "E2002"
            | "E2003"
            | "ORNA-S020-ANNOTATION"
            | "ORNA-S021-TYPE"
            | "ORNA-S022-UNSUPPORTED"
            | "ORNA-A091-004"
            | "ORNA-A091-003"
            | "ORNA-A091-007"
            | "ORNA-A091-012"
            | "ORNA-A091-002"
            | "ORNA091-E-CURRENCY-SYMBOL"
            | "ORNA091-E-CONVERSION-CHAIN"
            | "ORNA100-E-SYS-ADMIN-METHOD"
            | "ORNA100-E-SYS-STORAGE-CALL"
            | "E1008"
            | "E1009"
            | "E3002"
            | "E3003"
            | "E3010"
            | "E3011"
            | "E3012"
            | "E3013"
            | "E4107"
            | "E5001"
            | "E5002"
            | "E5003"
            | "E5103"
            | "E5004"
            | "E6001"
            | "E7001"
            | "E7002"
            | "E7201"
            | "E9001"
            | "E9101"
            | "E9102"
    ) || diagnostic.message() == "type name cannot be resolved"
}

/// Spec-published codes win over the generic internal `ORNA-S*`/`ORNA-A091-*`
/// codes, which keep emission order.
pub(super) fn is_published_code(code: &str) -> bool {
    !code.starts_with("ORNA-") || code == "ORNA-A091-002"
}

/// Picks one diagnostic for the stage: a published code or the spec-mandated
/// Result diagnostic wins over generic internal codes; otherwise the first
/// accepted diagnostic does.
pub(super) fn select_diagnostic(
    diagnostics: Vec<Diagnostic>,
    phase: SemanticPhase,
) -> StageOutcome<Diagnostic> {
    let mut first_accepted = None;
    for diagnostic in diagnostics {
        if is_published_code(diagnostic.code()) {
            if phase.accepts(&diagnostic) {
                return StageOutcome::Failed(diagnostic);
            }
        } else if first_accepted.is_none() && phase.accepts(&diagnostic) {
            first_accepted = Some(diagnostic);
        }
    }
    match first_accepted {
        Some(diagnostic) => StageOutcome::Failed(diagnostic),
        // Conformance compares compiler codes and message fragments. Runtime
        // execution boundaries redact their failures separately.
        None => StageOutcome::Passed,
    }
}

impl ConformanceAdapter for SemanticAdapter {
    type Diagnostic = Diagnostic;
    fn diagnostic_code(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.code().into()
    }
    fn diagnostic_message(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.message().into()
    }
    fn parse(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.syntax.parse(unit)
    }
    fn resolve(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.analyze_units([unit.clone()], SemanticPhase::Resolve)
    }
    fn typecheck(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.analyze_units([unit.clone()], SemanticPhase::Typecheck)
    }
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        TransactionalEvaluator::default()
            .with_fixture_catalogue()
            .execute_duplicate_key_fixture(unit)
            .unwrap_or_else(Self::unsupported_runtime)
    }
    fn parse_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        let mut syntax = SyntaxAdapter;
        for unit in &project.modules {
            if let outcome @ StageOutcome::Failed(_) = syntax.parse(unit) {
                return outcome;
            }
        }
        StageOutcome::Passed
    }
    fn resolve_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.analyze_project(project, SemanticPhase::Resolve)
    }
    fn typecheck_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.analyze_project(project, SemanticPhase::Typecheck)
    }
    fn evaluate_project(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        Self::unsupported_runtime()
    }
    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        validate_unsafe_row_key_repeat(unit).unwrap_or_else(Self::unsupported_runtime)
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        Self::unsupported_runtime()
    }
    fn run_scenario(&mut self, _: &Scenario) -> StageOutcome<Diagnostic> {
        Self::unsupported_runtime()
    }
}
