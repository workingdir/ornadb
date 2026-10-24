use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};

/// Production admission must not inherit the conformance fixture catalogue.
/// The user module `vehicle/corsa.orna` collides with the fixture namespace
/// `vehicle.corsa`; on the production path it must be admitted with no
/// diagnostic at all (a fixture-leaking admission reports ORNA-S002-NAMESPACE).
#[test]
fn production_transaction_admission_accepts_user_module_under_fixture_namespace() {
    let mut evaluator = TransactionalEvaluator::new("main", Default::default());
    let unit = SourceUnit {
        fixture_id: "production-fixture-leak".into(),
        source_id: "vehicle/corsa.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn main() = 1;".into(),
    };

    match evaluator.execute_source(&unit) {
        StageOutcome::Passed => {}
        StageOutcome::Failed(diagnostic) => panic!(
            "production admission reported {} for a user module under a fixture namespace",
            diagnostic.code()
        ),
        outcome => panic!("production admission must admit the module: {outcome:?}"),
    }
}
