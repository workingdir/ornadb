use orna_conformance_v1::{
    BoundedEvaluator, Corpus, FixtureStageBinding, Harness, RuntimeAdapter, Stage,
};
use orna_traceability_v1::{Status, generate, generate_with_engine_witnesses};
use std::path::PathBuf;

const REQUIREMENT_ID: &str = "ORNA-SOURCE-001";
const FIXTURE_ID: &str = "valid/minimal-root.orna";
const FIXTURE_PATH: &str = "examples/valid/minimal-root.orna";
const IMPLEMENTATION_REF: &str = "crates/orna-conformance-v1/src/lib.rs::Harness::engine_witnesses";
const TEST_REF: &str = "crates/orna-traceability-v1/tests/authoritative_reference_boundary.rs::untouched_reference_does_not_authorize_a_fixture_witness";

fn reference_root() -> PathBuf {
    std::env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference/Orna-1.0.0")
        })
}

#[test]
fn untouched_reference_does_not_authorize_a_fixture_witness() {
    let reference = reference_root();
    let corpus = Corpus::load(&reference).expect("authoritative reference corpus loads");
    let harness = Harness::new(corpus);
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let conformance = harness.run(&mut adapter);
    let binding = FixtureStageBinding {
        requirement_id: REQUIREMENT_ID.into(),
        fixture_id: FIXTURE_ID.into(),
        fixture_path: FIXTURE_PATH.into(),
        stage: Stage::Parse,
        implementation_ref: IMPLEMENTATION_REF.into(),
        test_ref: TEST_REF.into(),
    };
    let witnesses = harness
        .engine_witnesses(&conformance, std::slice::from_ref(&binding))
        .expect("the passing fixture stage can be measured independently");
    assert_eq!(witnesses.witnesses().len(), 1);

    let error = generate_with_engine_witnesses(&reference, &witnesses)
        .expect_err("the frozen evidence register has no fixture/path declaration");
    assert!(
        error
            .to_string()
            .contains("engine witness requirement evidence does not declare fixture")
    );

    let report = generate(&reference).expect("unmodified reference report generates");
    let requirement = report
        .requirements
        .iter()
        .find(|requirement| requirement.requirement_id == REQUIREMENT_ID)
        .expect("source requirement exists");
    assert_eq!(requirement.status, Status::JustifiedGap);
    assert!(
        requirement
            .boundaries
            .iter()
            .all(|boundary| boundary.kind != "engine-witness")
    );
}
