use super::*;

pub struct RuntimeAdapter<R> {
    pub(super) semantic: SemanticAdapter,
    pub(super) runtime: R,
}
impl<R> RuntimeAdapter<R> {
    pub fn new(runtime: R) -> Self {
        Self {
            semantic: SemanticAdapter::default(),
            runtime,
        }
    }
    pub fn into_runtime(self) -> R {
        self.runtime
    }
}

/// A source-digest-bound pure-function admission.  It contains the bounded
/// runtime's retained parsed declarations, rather than a source string for a
/// later evaluator call.  This is deliberately *not* a compiler-produced
/// executable artifact: the current compiler exposes analysis only, and the
/// bounded evaluator performed the declaration loading at admission time.
///
/// Keeping this seam explicit prevents a caller from treating a successful
/// bounded invocation as authoritative scenario execution, while defining the
/// shape an eventual compiler artifact handoff must replace.
pub struct PureFunctionAdmission {
    pub(super) source_id: String,
    pub(super) source_digest: [u8; 32],
    pub(super) function: String,
    pub(super) runtime: BoundedEvaluator,
}

impl PureFunctionAdmission {
    /// Corpus-relative logical source identifier bound at admission.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// SHA-256 of the exact admitted source bytes.
    pub fn source_digest(&self) -> [u8; 32] {
        self.source_digest
    }

    /// Name resolved in the admitted pure-function namespace.
    pub fn function(&self) -> &str {
        &self.function
    }
}

impl RuntimeAdapter<BoundedEvaluator> {
    /// Runs the v1 parse/resolve/typecheck stages and retains one bounded pure
    /// function for a later invocation.  No source is supplied at invocation
    /// time; the admission keeps the exact source digest and parsed namespace.
    /// This remains a bounded-adapter artifact, not a compiler executable.
    pub fn admit_pure_function(
        &mut self,
        unit: &SourceUnit,
        function: &str,
    ) -> Result<PureFunctionAdmission, Box<StageOutcome<Diagnostic>>> {
        for outcome in [
            self.semantic.parse(unit),
            self.semantic.resolve(unit),
            self.semantic.typecheck(unit),
            self.runtime.evaluate(unit),
        ] {
            if !matches!(outcome, StageOutcome::Passed) {
                return Err(Box::new(outcome));
            }
        }
        if !self.runtime.functions.contains_key(function) {
            return Err(Box::new(StageOutcome::Skipped {
                reason: "requested function is not retained by the admitted bounded module".into(),
            }));
        }
        let source_digest = Sha256::digest(unit.source.as_bytes()).into();
        Ok(PureFunctionAdmission {
            source_id: unit.source_id.clone(),
            source_digest,
            function: function.into(),
            runtime: self.runtime.clone(),
        })
    }

    /// Invokes a previously admitted bounded pure function.  The only
    /// executable state crossing this boundary is the retained parsed
    /// declaration namespace; this method never reparses source text.
    pub fn invoke_admitted_pure_function(
        &self,
        admission: PureFunctionAdmission,
        arguments: &Environment,
    ) -> Result<Value, Box<StageOutcome<Diagnostic>>> {
        admission
            .runtime
            .invoke_value_with(&admission.function, arguments)
            .map_err(|error| Box::new(StageOutcome::Failed(*error)))
    }

    /// Compatibility helper for existing bounded pure-function evidence.
    /// It preserves the same non-authoritative boundary as separately
    /// admitting then invoking the function.
    pub fn compile_and_invoke_pure_function(
        &mut self,
        unit: &SourceUnit,
        function: &str,
        arguments: &Environment,
    ) -> Result<Value, Box<StageOutcome<Diagnostic>>> {
        let admission = self.admit_pure_function(unit, function)?;
        self.invoke_admitted_pure_function(admission, arguments)
    }
}

impl<R: RuntimeEvaluator> ConformanceAdapter for RuntimeAdapter<R> {
    type Diagnostic = Diagnostic;
    fn diagnostic_code(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.code().into()
    }
    fn diagnostic_message(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.message().into()
    }
    fn parse(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.semantic.parse(unit)
    }
    fn resolve(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.semantic.resolve(unit)
    }
    fn typecheck(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.semantic.typecheck(unit)
    }
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.runtime.evaluate(unit)
    }
    fn evaluate_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.runtime.evaluate_project(project)
    }
    fn parse_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.semantic.parse_project(project)
    }
    fn resolve_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.semantic.resolve_project(project)
    }
    fn typecheck_project(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.semantic.typecheck_project(project)
    }
    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.runtime.validate_row(unit)
    }
    fn validate_rows(&mut self, project: &ProjectUnit) -> StageOutcome<Diagnostic> {
        match self.runtime.preflight_row_validation(project) {
            StageOutcome::Passed => {}
            outcome => return outcome,
        }
        let analysis = self.semantic.project_analysis(project);
        if let Some(diagnostic) = analysis.diagnostics.first() {
            return StageOutcome::Failed(diagnostic.clone().redacted());
        }
        self.runtime.validate_resolved_rows(project, &analysis)
    }
    fn run_scenario(&mut self, scenario: &Scenario) -> StageOutcome<Diagnostic> {
        self.runtime.run_scenario(scenario)
    }
}
