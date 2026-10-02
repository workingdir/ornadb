use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

#[test]
fn pinned_option_module_executes_its_orna_implementations() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/option.orna")
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources("orna.std/jx1e3-option", sources.clone())
        .unwrap();
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .unwrap();
    let mut session = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("{}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-option-result-jx1e3.orna")),
        Ok(None)
    );
    let evaluated = session
        .submit(include_str!("fixtures/stdlib-option-result-jx1e3.orna"))
        .unwrap_or_else(|error| panic!("{}", error.code()));
    assert_eq!(evaluated, Some(canonical(Raw::Bool(true))));
}

#[test]
fn core_operations_work_without_std_and_missing_option_stays_missing() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-without-std-jx1e3.orna")),
        Ok(Some(canonical(Raw::Bool(true))))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-option-without-snapshot-jx1e3.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
