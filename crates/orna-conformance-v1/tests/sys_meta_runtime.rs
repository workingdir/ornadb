use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_sys_v1::{
    SystemEffect, TypeId, TypedValue, ValueMetadataFacts, ValueMetadataResolver,
    system_function_descriptor, system_value_metadata,
};

const SYS_META_SOURCE: &str = include_str!("fixtures/sys-meta-protected.orna");

struct FixtureMetadataResolver;

impl ValueMetadataResolver for FixtureMetadataResolver {
    fn value_metadata_facts(&self, static_type: &TypeId) -> Option<ValueMetadataFacts> {
        (static_type.as_str() == "Str")
            .then(|| ValueMetadataFacts::new(None, [], ["OVB-1".to_owned()]).ok())
            .flatten()
    }
}

#[test]
fn sys_meta_executes_safe_metadata_for_a_protected_source_value() {
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", SYS_META_SOURCE)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);

    let descriptor = system_function_descriptor("sys.meta").expect("registered sys.meta");
    assert_eq!(descriptor.effect, SystemEffect::Read);
    assert_eq!(
        descriptor.signature,
        "fn sys.meta<T>(value: T): sys.ValueMetadata<T>"
    );

    let value = TypedValue::protected(TypeId::new("Str"), b"fixture-secret".to_vec());
    let metadata = system_value_metadata(&value, &FixtureMetadataResolver)
        .expect("the catalogue resolves the input type");
    assert_eq!(metadata.static_type().as_str(), "Str");
    assert_eq!(metadata.codecs(), ["OVB-1"]);
    assert!(metadata.is_redacted());
    assert_eq!(value.canonical(), None);

    let encoded = serde_json::to_vec(&metadata).expect("metadata serialization");
    assert!(!String::from_utf8_lossy(&encoded).contains("fixture-secret"));
}
