use orna_semantic_v1::{ModuleInput, analyze_with_catalogue};

use crate::{
    REFERENCE_STANDARD_COLLECTION_PATH_V1, REFERENCE_STANDARD_MATH_PATH_V1,
    reference_standard_catalogue_v1,
    reference_standard_profile_v1, reference_standard_sources_v1,
};

#[test]
fn reference_standard_uses_pinned_orna_1_source_and_resolves_its_imports() {
    let sources = reference_standard_sources_v1();
    assert_eq!(sources[0].0, REFERENCE_STANDARD_MATH_PATH_V1);
    assert!(sources[0].1.starts_with("pub fn increment(value: Int): Int"));
    assert_eq!(sources[1].0, REFERENCE_STANDARD_COLLECTION_PATH_V1);
    assert!(sources[1].1.contains("pub fn asof_join<T, Time, Key>"));

    let profile = reference_standard_profile_v1();
    assert_eq!(profile.snapshot(), "orna.std/v1-reference-library");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("the exact 1.0 source is pinned");
    }

    let catalogue = reference_standard_catalogue_v1().expect("the standard module checks");
    let consumer = include_str!("fixtures/v1_standard_consumer.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("consumer.orna", consumer)],
        &catalogue,
    );
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
}
