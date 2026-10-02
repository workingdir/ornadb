use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_list_map_and_set_modules_preserve_their_documented_order() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for import in [
        include_str!("fixtures/stdlib-use-list-i7bat.orna"),
        include_str!("fixtures/stdlib-use-map-i7bat.orna"),
        include_str!("fixtures/stdlib-use-set-i7bat.orna"),
    ] {
        let imported = session.submit(import);
        assert!(imported.is_ok(), "{}", imported.unwrap_err().code());
    }
    for declaration in [
        include_str!("fixtures/stdlib-list-contract-i7bat.orna"),
        include_str!("fixtures/stdlib-map-contract-i7bat.orna"),
        include_str!("fixtures/stdlib-set-contract-i7bat.orna"),
    ] {
        let parsed = orna_syntax_v1::parse_repl_with_file(declaration, "collection-contract.orna");
        assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
        let admitted = session.submit(declaration);
        assert!(
            admitted.is_ok(),
            "{declaration}: {:#?}",
            admitted.as_ref().err()
        );
    }
    for call in [
        include_str!("fixtures/stdlib-call-list-contract-i7bat.orna"),
        include_str!("fixtures/stdlib-call-map-contract-i7bat.orna"),
        include_str!("fixtures/stdlib-call-set-contract-i7bat.orna"),
    ] {
        let evaluated = session.submit(call);
        assert!(evaluated.is_ok(), "{call}: {}", evaluated.unwrap_err().code());
        assert_eq!(evaluated.unwrap(), Some(bool_value(true)));
    }
}

#[test]
fn core_intrinsics_work_without_the_optional_collection_modules() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-without-collections-i7bat.orna")),
        Ok(Some(bool_value(true)))
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-map-without-snapshot-i7bat.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-without-collections-i7bat.orna")),
        Ok(Some(bool_value(true)))
    );
}

#[test]
fn pinned_stream_iteration_surface_is_optional_and_does_not_replace_core() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-stream-n8phe.orna")),
        Ok(None)
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std.submit(include_str!("fixtures/stdlib-core-without-collections-i7bat.orna")),
        Ok(Some(bool_value(true)))
    );
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-stream-without-snapshot-n8phe.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        without_std.submit(include_str!("fixtures/stdlib-core-without-collections-i7bat.orna")),
        Ok(Some(bool_value(true)))
    );
}
