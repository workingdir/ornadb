use orna_semantic_v1::{DIAG_TYPE, ModuleInput, analyze};

const ENUMS: &str = include_str!("fixtures/imported-enum-primary-key-types.orna");

fn analyze_consumer(consumer: &str) -> orna_semantic_v1::Analysis {
    analyze(&[
        ModuleInput::new("types.orna", ENUMS),
        ModuleInput::new("main.orna", consumer),
    ])
}

#[test]
fn imported_payload_free_enum_is_a_primary_key_type() {
    let consumer = include_str!("fixtures/imported-enum-primary-key-unit-consumer.orna");
    let analysis = analyze_consumer(consumer);

    assert!(analysis.is_ok(), "{consumer}: {:?}", analysis.diagnostics);
}

#[test]
fn imported_payload_enum_is_rejected_as_a_primary_key_type() {
    let consumer = include_str!("fixtures/imported-enum-primary-key-payload-consumer.orna");
    let analysis = analyze_consumer(consumer);

    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "payload-bearing enums cannot be primary-key types"
        }),
        "{consumer}: {:?}",
        analysis.diagnostics
    );
}
