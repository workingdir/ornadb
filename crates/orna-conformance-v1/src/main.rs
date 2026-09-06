use orna_conformance_v1::{
    BoundedEvaluator, Corpus, Harness, ImplementationClaim, RuntimeAdapter, RuntimeEvaluator,
    Scenario, SourceUnit, StageOutcome, TransactionalEvaluator,
};

/// Routes each conformance surface to the evaluator that actually owns it.
/// Fixture and project stages stay on the bounded evaluator; only the two
/// exact transactional scenario contracts are delegated to the real table
/// evaluator. Unsupported scenarios remain explicit skips.
#[derive(Default)]
struct CompositeEvaluator {
    bounded: BoundedEvaluator,
    transactional: TransactionalEvaluator,
}

impl RuntimeEvaluator for CompositeEvaluator {
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.evaluate(unit)
    }

    fn evaluate_project(
        &mut self,
        project: &orna_conformance_v1::ProjectUnit,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.evaluate_project(project)
    }

    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.validate_row(unit)
    }

    fn validate_rows(
        &mut self,
        project: &orna_conformance_v1::ProjectUnit,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.validate_rows(project)
    }

    fn preflight_row_validation(
        &mut self,
        project: &orna_conformance_v1::ProjectUnit,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.preflight_row_validation(project)
    }

    fn validate_resolved_rows(
        &mut self,
        project: &orna_conformance_v1::ProjectUnit,
        analysis: &orna_semantic_v1::Analysis,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.validate_resolved_rows(project, analysis)
    }

    fn run_scenario(
        &mut self,
        scenario: &Scenario,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        if matches!(scenario.id.as_str(), "TXN-001" | "TXN-002") {
            self.transactional.run_scenario(scenario)
        } else {
            self.bounded.run_scenario(scenario)
        }
    }
}

fn main() {
    let corpus = Corpus::load_default().unwrap_or_else(|error| {
        eprintln!("cannot load authoritative Orna corpus: {error}");
        std::process::exit(2)
    });
    let mut adapter = RuntimeAdapter::new(CompositeEvaluator::default());
    let report = Harness::new(corpus)
        .with_claim(ImplementationClaim {
            implementation_id: "orna-conformance-v1".into(),
            profile: "bounded-expression-runtime".into(),
            command: "orna-conformance --profile bounded-expression-runtime".into(),
            environment: [
                (
                    "adapter".into(),
                    "RuntimeAdapter (syntax, semantic analysis, and bounded expression evaluator)"
                        .into(),
                ),
                (
                    "semantic-stages".into(),
                    "semantic stages execute through the read-only v1 analyzer".into(),
                ),
                (
                    "runtime-stages".into(),
                    "pure row/expression units and the two bounded transactional scenarios execute; module, effectful, and remaining scenario stages remain explicit skips".into(),
                ),
            ]
            .into_iter()
            .collect(),
        })
        .run(&mut adapter);
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report serializes")
    );
}
