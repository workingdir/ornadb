use orna_semantic_v1::{ModuleInput, analyze_with_catalogue};

use crate::{
    REFERENCE_STANDARD_COLLECTION_PATH_V1, REFERENCE_STANDARD_MATH_PATH_V1,
    REFERENCE_STANDARD_BITS_PATH_V1, REFERENCE_STANDARD_QUERY_PATH_V1,
    REFERENCE_STANDARD_TEXT_PATH_V1, REFERENCE_STANDARD_STATS_PATH_V1,
    REFERENCE_STANDARD_TIME_PATH_V1,
    REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1,
    REFERENCE_STANDARD_STREAM_PATH_V1,
    REFERENCE_STANDARD_TIME_COMPACT_PATH_V1, REFERENCE_STANDARD_TIME_CLOCK_PATH_V1,
    REFERENCE_STANDARD_TIME_WORDS_PATH_V1, REFERENCE_STANDARD_TIME_ISO_PATH_V1,
    REFERENCE_STANDARD_OPTION_PATH_V1, REFERENCE_STANDARD_RESULT_PATH_V1,
    REFERENCE_STANDARD_LIST_PATH_V1, REFERENCE_STANDARD_MAP_PATH_V1,
    REFERENCE_STANDARD_SET_PATH_V1,
    REFERENCE_STANDARD_IO_PATH_V1, REFERENCE_STANDARD_FS_PATH_V1,
    REFERENCE_STANDARD_CONCURRENT_PATH_V1, REFERENCE_STANDARD_ERROR_PATH_V1,
    reference_standard_catalogue_v1,
    reference_standard_profile_v1, reference_standard_sources_v1,
};

#[test]
fn reference_standard_uses_pinned_orna_1_source_and_resolves_its_imports() {
    let sources = reference_standard_sources_v1();
    assert_eq!(sources[0].0, REFERENCE_STANDARD_MATH_PATH_V1);
    assert!(sources[0].1.contains("pub fn increment(value: Int): Int"));
    for name in [
        "abs", "signum", "is_even", "is_odd", "square", "cube", "gcd", "lcm",
        "pow_nonnegative",
    ] {
        assert!(sources[0].1.contains(&format!("pub fn {name}(")));
    }
    assert_eq!(sources[1].0, REFERENCE_STANDARD_COLLECTION_PATH_V1);
    assert!(sources[1].1.contains("pub fn asof_join<T, Time, Key>"));
    assert_eq!(sources[2].0, REFERENCE_STANDARD_QUERY_PATH_V1);
    assert!(sources[2].1.contains("pub fn filter<T>"));
    assert_eq!(sources[3].0, REFERENCE_STANDARD_TEXT_PATH_V1);
    assert!(sources[3].1.contains("pub fn normalise(value: Str, form: Str)"));
    for name in [
        "trim", "split", "join", "starts_with", "ends_with", "contains", "replace",
        "normalise", "lower", "upper",
    ] {
        assert!(sources[3].1.contains(&format!("pub fn {name}(")));
    }
    assert_eq!(sources[4].0, REFERENCE_STANDARD_BITS_PATH_V1);
    assert!(sources[4].1.contains("pub fn shift_right(value: Int, count: Int)"));
    for name in ["bit_or", "bit_and", "bit_xor", "bit_not", "shift_left", "shift_right"] {
        assert!(sources[4].1.contains(&format!("pub fn {name}(")));
    }
    assert_eq!(sources[5].0, REFERENCE_STANDARD_STATS_PATH_V1);
    assert!(sources[5].1.contains("pub fn mean<T>"));
    assert!(sources[5].1.contains("pub fn percentile<T, P>"));
    assert!(sources[5].1.contains("pub fn median<T>"));
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
    assert_eq!(sources[20].0, REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1);
    for declaration in [
        "pub fn is_leap_year(year: Int): Bool",
        "pub fn days_in_month(year: Int, month: Int): Int?",
        "pub fn is_valid_date(year: Int, month: Int, day: Int): Bool",
        "pub fn day_of_year(year: Int, month: Int, day: Int): Int?",
    ] {
        assert!(sources[20].1.contains(declaration), "missing `{declaration}`");
    }
    for name in [
        "chunk", "flatten", "partition", "zip", "zip_exact", "unique", "group_by", "pairs",
        "window", "split_when", "rank", "bucket_by",
    ] {
        assert!(sources[1].1.contains(&format!("pub fn {name}<")), "std.collection.{name}");
        assert!(sources[2].1.contains(&format!("pub fn {name}<")), "std.query.{name}");
    }
    assert_eq!(sources[21].0, REFERENCE_STANDARD_STREAM_PATH_V1);
    for declaration in [
        "pub fn from_list<T>(values: [T], source_identity: Str): Stream<T>",
        "pub fn for_each<T>(stream: Stream<T>, action: fn(T): Unit): Unit",
        "pub fn batch<T>(stream: Stream<T>, size: Int): Stream<[T]>",
        "pub fn buffer<T>(stream: Stream<T>, capacity: Int): Stream<T>",
        "pub fn merge<T>(streams: [Stream<T>]): Stream<T>",
        "pub fn throttle<T>(",
        "pub fn debounce<T>(",
        "pub fn retry<T>(",
        "pub fn recover<T>(stream: Stream<T>, handler: fn(Error): T): Stream<T>",
    ] {
        assert!(sources[21].1.contains(declaration), "missing stream declaration `{declaration}`");
    }
    for contract in [
        "canonical typed digest",
        "Process one item at a time",
        "default backpressure",
        "observed arrival order",
        "explicit policy",
        "without advancing the checkpoint",
        "cannot acknowledge an ordered delivery by silently skipping it",
    ] {
        assert!(sources[21].1.contains(contract), "missing stream contract `{contract}`");
    }
    for contract in [
        "consecutive nonempty groups",
        "partition returns (matching, remaining)",
        "zip stops at the shorter input",
        "unique keeps each lawful equality class's first occurrence",
        "lawful keys while preserving input order inside each group",
        "adjacent and overlapping",
        "window emits complete windows only",
        "split_when starts a group before a matching item",
        "competition ranks (ties share a rank",
    ] {
        assert!(sources[1].1.contains(contract), "missing collection contract `{contract}`");
        assert!(sources[2].1.contains(contract), "missing query contract `{contract}`");
    }
    assert_eq!(sources[11].0, REFERENCE_STANDARD_OPTION_PATH_V1);
    assert!(sources[11].1.contains("pub fn and_then<T, U>"));
    assert_eq!(sources[12].0, REFERENCE_STANDARD_RESULT_PATH_V1);
    assert!(sources[12].1.contains("pub enum Result<T, E>"));
    assert!(sources[12].1.contains("pub fn map_error<T, E, F>"));
    assert_eq!(sources[13].0, REFERENCE_STANDARD_LIST_PATH_V1);
    for name in ["append", "last", "reverse", "unique"] {
        assert!(sources[13].1.contains(&format!("pub fn {name}<")));
    }
    assert_eq!(sources[14].0, REFERENCE_STANDARD_MAP_PATH_V1);
    for name in ["get", "insert", "remove", "merge"] {
        assert!(sources[14].1.contains(&format!("pub fn {name}<")));
    }
    assert_eq!(sources[15].0, REFERENCE_STANDARD_SET_PATH_V1);
    for name in ["from_list", "contains", "insert", "union", "intersection", "difference"] {
        assert!(sources[15].1.contains(&format!("pub fn {name}<")));
    }
    assert_eq!(sources[16].0, REFERENCE_STANDARD_IO_PATH_V1);
    assert!(sources[16].1.contains("use fs;"));
    assert_eq!(sources[17].0, REFERENCE_STANDARD_FS_PATH_V1);
    for name in [
        "read_text", "write_text", "append_text", "exists", "is_directory", "list",
        "create_dir", "remove_file", "copy_file", "move_file",
    ] {
        assert!(sources[17].1.contains(&format!("pub fn {name}(")));
    }
    assert!(sources[17]
        .1
        .contains("pub fn read_text(root: Str, path: Str): Str"));
    assert!(sources[17]
        .1
        .contains("pub fn write_text(root: Str, path: Str, contents: Str, overwrite: Bool)"));
    assert!(sources[17].1.contains("source_path: Str"));
    assert!(sources[17].1.contains("destination_path: Str"));
    assert!(sources[17].1.contains("overwrite: Bool"));
    assert_eq!(sources[18].0, REFERENCE_STANDARD_CONCURRENT_PATH_V1);
    for declaration in [
        "pub fn parallel<T>(callbacks: [fn(): T]): [T]",
        "pub fn race<T>(callbacks: [fn(): T]): T",
        "pub fn timeout<T>(callback: fn(): T, duration: Duration): T",
        "pub fn sleep(duration: Duration): Null",
    ] {
        assert!(sources[18].1.contains(declaration), "missing `{declaration}`");
    }
    for contract in [
        "input order",
        "cancel and join",
        "lowest input-index",
        "clock/waiting effect",
        "cancellation-aware",
    ] {
        assert!(sources[18].1.contains(contract), "missing contract: {contract}");
    }
    assert_eq!(sources[19].0, REFERENCE_STANDARD_ERROR_PATH_V1);
    assert!(sources[19].1.contains("pub fn make(code: Str, message: Str): Error"));
    assert!(sources[19]
        .1
        .contains("pub fn caused_by(code: Str, message: Str, cause: Error): Error"));
    for declaration in [
        "pub fn fold<T, E, U>(",
        "pub fn map_or<T, E, U>(",
        "pub fn map_or_else<T, E, U>(",
        "pub fn flatten<T, E>(result: Result<Result<T, E>, E>): Result<T, E>",
    ] {
        assert!(sources[12].1.contains(declaration), "missing `{declaration}`");
    }
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
    let concurrent_module_analysis = analyze_with_catalogue(
        &[ModuleInput::new("concurrent.orna", &sources[18].1)],
        &orna_semantic_v1::Catalogue::authoritative_core(),
    );
    assert!(
        concurrent_module_analysis.is_ok(),
        "{}",
        concurrent_module_analysis
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
    let mut changed_error_source = sources[19].1.clone();
    changed_error_source.push_str("\n// changed after the captured snapshot\n");
    assert!(profile
        .verify_source(REFERENCE_STANDARD_ERROR_PATH_V1, &changed_error_source)
        .is_err());
    let mut changed_calendar_source = sources[20].1.clone();
    changed_calendar_source.push_str("\n// changed after the captured snapshot\n");
    assert!(profile
        .verify_source(REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1, &changed_calendar_source)
        .is_err());
    let mut changed_stream_source = sources[21].1.clone();
    changed_stream_source.push_str("\n// changed after the captured snapshot\n");
    assert!(profile
        .verify_source(REFERENCE_STANDARD_STREAM_PATH_V1, &changed_stream_source)
        .is_err());

    let catalogue = reference_standard_catalogue_v1().expect("the standard module checks");
    for (path, source) in &sources[13..] {
        let module_analysis = analyze_with_catalogue(
            &[ModuleInput::new(path, source)],
            &orna_semantic_v1::Catalogue::authoritative_core(),
        );
        let parse_diagnostics = module_analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == "ORNA-S000-PARSE")
            .map(|diagnostic| diagnostic.message())
            .collect::<Vec<_>>();
        assert!(parse_diagnostics.is_empty(), "{path}: {parse_diagnostics:?}");
    }
    let consumer = include_str!("fixtures/v1_collection_operations_consumer.orna");
    let asof_consumer = include_str!("fixtures/v1_standard_consumer.orna");
    let option_result_consumer = include_str!("fixtures/v1_option_result_consumer.orna");
    let text_numeric_consumer = include_str!("fixtures/v1_text_numeric_consumer.orna");
    let collections_consumer = include_str!("fixtures/v1_collections_consumer.orna");
    let concurrent_consumer = include_str!("fixtures/v1_concurrent_consumer.orna");
    let error_result_consumer = include_str!("fixtures/v1_error_result_consumer.orna");
    let time_calendar_consumer = include_str!("fixtures/v1_time_calendar_consumer.orna");
    let iteration_consumer = include_str!("fixtures/v1_iteration_consumer.orna");
    for (path, source) in [
        ("list_consumer.orna", include_str!("fixtures/v1_list_consumer.orna")),
        ("map_consumer.orna", include_str!("fixtures/v1_map_consumer.orna")),
        ("set_consumer.orna", include_str!("fixtures/v1_set_consumer.orna")),
    ] {
        let module_analysis =
            analyze_with_catalogue(&[ModuleInput::new(path, source)], &catalogue);
        assert!(
            module_analysis.is_ok(),
            "{path}: {}",
            module_analysis
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let analysis = analyze_with_catalogue(
        &[
            ModuleInput::new("asof_consumer.orna", asof_consumer),
            ModuleInput::new("collection_ops.orna", consumer),
            ModuleInput::new("option_result_consumer.orna", option_result_consumer),
            ModuleInput::new("text_numeric_consumer.orna", text_numeric_consumer),
            ModuleInput::new("collections_consumer.orna", collections_consumer),
            ModuleInput::new("concurrent_consumer.orna", concurrent_consumer),
            ModuleInput::new("error_result_consumer.orna", error_result_consumer),
            ModuleInput::new("time_calendar_consumer.orna", time_calendar_consumer),
            ModuleInput::new("iteration_consumer.orna", iteration_consumer),
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

#[test]
fn pinned_filesystem_effect_is_visible_to_consumers_and_forbidden_in_assertions() {
    let catalogue = reference_standard_catalogue_v1().expect("the standard module checks");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "io_consumer.orna",
            include_str!("fixtures/v1_io_fs_effect_consumer.orna"),
        )],
        &catalogue,
    );
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.message() == "declaration assertion uses forbidden filesystem effect"
        }),
        "{}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
            .join("; ")
    );
    let consumer = analysis
        .modules
        .values()
        .find(|module| module.namespace.display() == "io_consumer")
        .expect("filesystem consumer module");
    let reads = consumer.symbols.get("reads").expect("filesystem wrapper");
    assert!(reads.effects.effects.contains("filesystem"));
    assert!(reads.effects.may_fail);
}
