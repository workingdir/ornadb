use orna_semantic_v1::{DIAG_TYPE, ModuleInput, analyze};

#[test]
fn payload_free_enum_is_a_primary_key_type() {
    let source = include_str!("fixtures/enum-primary-key-unit-variant.orna");
    let analysis = analyze(&[ModuleInput::new("enum-primary-key.orna", source)]);

    assert!(analysis.is_ok(), "{source}: {:?}", analysis.diagnostics);
}

#[test]
fn payload_enum_is_rejected_as_a_primary_key_type() {
    let source = include_str!("fixtures/enum-primary-key-payload-variant.orna");
    let analysis = analyze(&[ModuleInput::new("enum-primary-key.orna", source)]);

    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "payload-bearing enums cannot be primary-key types"
        }),
        "{source}: {:?}",
        analysis.diagnostics
    );
}
