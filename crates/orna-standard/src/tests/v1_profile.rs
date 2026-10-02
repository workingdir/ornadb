use orna_semantic_v1::{ModuleInput, analyze_with_catalogue};

use crate::{
    REFERENCE_STANDARD_COLLECTION_PATH_V1, REFERENCE_STANDARD_MATH_PATH_V1,
    REFERENCE_STANDARD_BITS_PATH_V1, REFERENCE_STANDARD_QUERY_PATH_V1,
    REFERENCE_STANDARD_TEXT_PATH_V1, REFERENCE_STANDARD_STATS_PATH_V1,
    REFERENCE_STANDARD_TIME_PATH_V1,
    REFERENCE_STANDARD_TIME_COMPACT_PATH_V1, REFERENCE_STANDARD_TIME_CLOCK_PATH_V1,
    REFERENCE_STANDARD_TIME_WORDS_PATH_V1, REFERENCE_STANDARD_TIME_ISO_PATH_V1,
    REFERENCE_STANDARD_OPTION_PATH_V1, REFERENCE_STANDARD_RESULT_PATH_V1,
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
    assert_eq!(sources[2].0, REFERENCE_STANDARD_QUERY_PATH_V1);
    assert!(sources[2].1.contains("pub fn filter<T>"));
    assert_eq!(sources[3].0, REFERENCE_STANDARD_TEXT_PATH_V1);
    assert!(sources[3].1.contains("pub fn normalise(value: Str, form: Str)"));
    assert_eq!(sources[4].0, REFERENCE_STANDARD_BITS_PATH_V1);
    assert!(sources[4].1.contains("pub fn shift_right(value: Int, count: Int)"));
    assert_eq!(sources[5].0, REFERENCE_STANDARD_STATS_PATH_V1);
    assert!(sources[5].1.contains("pub fn mean<T>"));
    assert!(sources[5].1.contains("pub fn percentile<T, P>"));
    assert_eq!(sources[6].0, REFERENCE_STANDARD_TIME_PATH_V1);
    assert!(sources[6].1.contains("pub fn timezone_data_version()"));
    assert!(sources[6].1.contains("pub fn resolve_local("));
    for (index, path, operation) in [
        (7, REFERENCE_STANDARD_TIME_COMPACT_PATH_V1, "compact"),
        (8, REFERENCE_STANDARD_TIME_CLOCK_PATH_V1, "clock"),
        (9, REFERENCE_STANDARD_TIME_WORDS_PATH_V1, "words"),
        (10, REFERENCE_STANDARD_TIME_ISO_PATH_V1, "iso"),
    ] {
        assert_eq!(sources[index].0, path);
        assert!(sources[index].1.contains(&format!("std.time.duration.{operation}.format")));
    }
    assert_eq!(sources[11].0, REFERENCE_STANDARD_OPTION_PATH_V1);
    assert!(sources[11].1.contains("pub fn and_then<T, U>"));
    assert_eq!(sources[12].0, REFERENCE_STANDARD_RESULT_PATH_V1);
    assert!(sources[12].1.contains("pub enum Result<T, E>"));
    assert!(sources[12].1.contains("pub fn map_error<T, E, F>"));
    let option_module_analysis = analyze_with_catalogue(
        &[ModuleInput::new("option.orna", &sources[11].1)],
        &orna_semantic_v1::Catalogue::authoritative_core(),
    );
    assert!(
        option_module_analysis.is_ok(),
        "{}",
        option_module_analysis
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
            .join("; ")
    );

    let profile = reference_standard_profile_v1();
    assert_eq!(profile.snapshot(), "orna.std/v1-reference-library");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("the exact 1.0 source is pinned");
    }

    let catalogue = reference_standard_catalogue_v1().expect("the standard module checks");
    let consumer = include_str!("fixtures/v1_collection_operations_consumer.orna");
    let asof_consumer = include_str!("fixtures/v1_standard_consumer.orna");
    let option_result_consumer = include_str!("fixtures/v1_option_result_consumer.orna");
    let analysis = analyze_with_catalogue(
        &[
            ModuleInput::new("asof_consumer.orna", asof_consumer),
            ModuleInput::new("collection_ops.orna", consumer),
            ModuleInput::new("option_result_consumer.orna", option_result_consumer),
        ],
        &catalogue,
    );
    assert!(
        analysis.is_ok(),
        "{}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
            .join("; ")
    );
}
