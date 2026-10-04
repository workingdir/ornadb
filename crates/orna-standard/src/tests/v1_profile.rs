use orna_semantic_v1::{
    Catalogue, ModuleInput, StandardDependencyProfile, Type, analyze_with_catalogue,
};

use crate::{
    REFERENCE_STANDARD_COLLECTION_PATH_V1, REFERENCE_STANDARD_MATH_PATH_V1,
    REFERENCE_STANDARD_MONEY_PATH_V1,
    REFERENCE_STANDARD_BITS_PATH_V1, REFERENCE_STANDARD_QUERY_PATH_V1,
    REFERENCE_STANDARD_TEXT_PATH_V1, REFERENCE_STANDARD_TEXT_BUILDER_PATH_V1,
    REFERENCE_STANDARD_STATS_PATH_V1,
    REFERENCE_STANDARD_TIME_PATH_V1,
    REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1,
    REFERENCE_STANDARD_STREAM_PATH_V1,
    REFERENCE_STANDARD_RANDOM_PATH_V1, REFERENCE_STANDARD_HASH_PATH_V1,
    REFERENCE_STANDARD_ENCODING_PATH_V1, REFERENCE_STANDARD_ENCODING_ORNA_PATH_V1,
    REFERENCE_STANDARD_ENCODING_OVB_PATH_V1, REFERENCE_STANDARD_ENCODING_JSON_PATH_V1,
    REFERENCE_STANDARD_ENCODING_BASE64_PATH_V1,
    REFERENCE_STANDARD_NET_PATH_V1, REFERENCE_STANDARD_URL_PATH_V1,
    REFERENCE_STANDARD_NET_HTTP_PATH_V1, REFERENCE_STANDARD_NET_WEBSOCKET_PATH_V1,
    REFERENCE_STANDARD_TIME_COMPACT_PATH_V1, REFERENCE_STANDARD_TIME_CLOCK_PATH_V1,
    REFERENCE_STANDARD_TIME_WORDS_PATH_V1, REFERENCE_STANDARD_TIME_ISO_PATH_V1,
    REFERENCE_STANDARD_OPTION_PATH_V1, REFERENCE_STANDARD_RESULT_PATH_V1,
    REFERENCE_STANDARD_LIST_PATH_V1, REFERENCE_STANDARD_MAP_PATH_V1,
    REFERENCE_STANDARD_SET_PATH_V1,
    REFERENCE_STANDARD_IO_PATH_V1, REFERENCE_STANDARD_FS_PATH_V1,
    REFERENCE_STANDARD_IO_PATH_MODULE_PATH_V1, REFERENCE_STANDARD_IO_METADATA_PATH_V1,
    REFERENCE_STANDARD_IO_BUFFER_PATH_V1,
    REFERENCE_STANDARD_IO_PROCESS_PATH_V1, REFERENCE_STANDARD_IO_ENVIRONMENT_PATH_V1,
    REFERENCE_STANDARD_CONCURRENT_PATH_V1, REFERENCE_STANDARD_ERROR_PATH_V1,
    REFERENCE_STANDARD_ERROR_COMBINATORS_PATH_V1,
    REFERENCE_STANDARD_NUMERIC_PATH_V1,
    REFERENCE_STANDARD_TEST_PATH_V1,
    REFERENCE_STANDARD_GENERICS_PATH_V1, REFERENCE_STANDARD_TYPE_UTILS_PATH_V1,
    REFERENCE_STANDARD_PATTERN_PATH_V1, REFERENCE_STANDARD_REGEX_PATH_V1,
    REFERENCE_STANDARD_REGEX_UTILITIES_PATH_V1,
    REFERENCE_STANDARD_TIME_UTILITIES_PATH_V1,
    REFERENCE_STANDARD_SORTING_PATH_V1,
    REFERENCE_STANDARD_FORMAT_STRINGS_PATH_V1,
    REFERENCE_STANDARD_PARSE_UTILITIES_PATH_V1,
    REFERENCE_STANDARD_ENCODING_UTILITIES_PATH_V1,
    REFERENCE_STANDARD_ITERATOR_PATH_V1, REFERENCE_STANDARD_ITERATOR_ADAPTERS_PATH_V1,
    REFERENCE_STANDARD_ITERATOR_CONSUMERS_PATH_V1,
    REFERENCE_STANDARD_COLLECTION_ADAPTERS_PATH_V1,
    REFERENCE_STANDARD_LAZY_ADAPTERS_PATH_V1,
    REFERENCE_STANDARD_STREAM_ADAPTERS_PATH_V1,
    REFERENCE_STANDARD_MEMO_PATH_V1,
    REFERENCE_STANDARD_LAZY_PATH_V1,
    REFERENCE_STANDARD_VIEWS_PATH_V1,
    REFERENCE_STANDARD_INTROSPECTION_PATH_V1, REFERENCE_STANDARD_REFLECTION_PATH_V1,
    REFERENCE_STANDARD_ALGORITHM_PATH_V1, REFERENCE_STANDARD_UI_PATH_V1,
    REFERENCE_STANDARD_FORMAT_PATH_V1, REFERENCE_STANDARD_PARSE_PATH_V1,
    REFERENCE_STANDARD_PRELUDE_PATH_V1, REFERENCE_STANDARD_PRELUDE_EXPORTS_V1,
    reference_standard_catalogue_v1,
    reference_standard_profile_v1, reference_standard_sources_v1,
};

#[test]
fn pinned_ui_presentation_helpers_are_included_as_source() {
    let sources = reference_standard_sources_v1();
    assert_eq!(sources.len(), 72);
    assert_eq!(sources[49].0, REFERENCE_STANDARD_UI_PATH_V1);
    let parsed = orna_syntax_v1::parse_module_with_file(
        &sources[49].1,
        REFERENCE_STANDARD_UI_PATH_V1,
    );
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
}

#[test]
fn pinned_algorithm_module_is_part_of_the_captured_std_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_ALGORITHM_PATH_V1)
        .expect("the pinned source bundle includes std.algorithm");
    assert_eq!(
        index, 46,
        "the new std source appends to preserve old indexes"
    );
    assert_eq!(path, REFERENCE_STANDARD_ALGORITHM_PATH_V1);
    for declaration in [
        "pub fn lower_bound<T, K>",
        "pub fn upper_bound<T, K>",
        "pub fn binary_search<T, K>",
        "pub fn is_sorted_by<T, K>",
        "pub fn stable_sort_by<T, K>",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("algorithm source bytes are recorded by the captured std profile");
    reference_standard_catalogue_v1()
        .expect("algorithm imports resolve in the captured standard catalogue");
}

#[test]
fn pinned_concurrent_result_helpers_are_included_in_the_captured_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == crate::REFERENCE_STANDARD_CONCURRENT_RESULT_PATH_V1)
        .expect("the pinned source bundle includes std.concurrent.result");
    assert_eq!(index, 56, "the async result helper source appends to the bundle");
    assert_eq!(path, crate::REFERENCE_STANDARD_CONCURRENT_RESULT_PATH_V1);
    for declaration in [
        "pub fn values<T, E>(",
        "pub fn errors<T, E>(",
        "pub fn partition<T, E>(",
        "pub fn parallel_partition<T, E>(",
        "pub fn parallel_map_partition<T, U, E>(",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    crate::reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("async result helper source bytes are recorded by the pinned std profile");
    crate::reference_standard_catalogue_v1()
        .expect("the concurrent result helper resolves against its pinned dependencies");
}

#[test]
fn pinned_iterator_adapters_are_included_in_the_captured_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_ITERATOR_ADAPTERS_PATH_V1)
        .expect("the pinned source bundle includes std.iterator.adapters");
    assert_eq!(index, 57, "the adapter module appends without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_ITERATOR_ADAPTERS_PATH_V1);
    for declaration in [
        "pub fn filter_map<T, U>(",
        "pub fn flat_map<T, U>(",
        "pub fn flatten<T>(",
        "pub fn skip_while<T>(",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    for contract in [
        "Some(null) remains an emitted value",
        "Drains each mapped cursor before requesting the next source item.",
        "after the first rejected item",
    ] {
        assert!(source.contains(contract), "missing iterator adapter contract `{contract}`");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("adapter source bytes are recorded by the pinned std profile");
    reference_standard_catalogue_v1()
        .expect("the iterator adapter module resolves against the pinned dependencies");
}

#[test]
fn pinned_iterator_consumers_are_included_in_the_captured_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_ITERATOR_CONSUMERS_PATH_V1)
        .expect("the pinned source bundle includes std.iterator.consumers");
    assert_eq!(index, 61, "iterator consumers append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_ITERATOR_CONSUMERS_PATH_V1);
    for declaration in [
        "pub fn any<T>(",
        "pub fn all<T>(",
        "pub fn find<T>(",
        "pub fn find_map<T, U>(",
        "pub fn position<T>(",
        "pub fn count<T>(",
        "pub fn count_by<T>(",
        "pub fn partition<T>(",
        "pub fn reduce<T>(",
    ] {
        assert!(source.contains(declaration), "missing iterator consumer `{declaration}`");
    }
    for contract in [
        "Stops at the first item accepted by `predicate`",
        "stops at the first rejected item",
        "Returns the first matching item",
        "Skips null transform results",
        "zero-based position",
        "Counts a finite cursor",
        "stable accepted and rejected lists",
        "Reduces a nonempty finite cursor from left to right",
    ] {
        assert!(source.contains(contract), "missing iterator consumer contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("iterator consumer bytes match the captured standard profile");
    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after profile capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("iterator consumers resolve in the captured standard catalogue");
}

#[test]
fn pinned_collection_adapters_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_COLLECTION_ADAPTERS_PATH_V1)
        .expect("the pinned source bundle includes std.collection.adapters");
    assert_eq!(index, 62, "collection adapters append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_COLLECTION_ADAPTERS_PATH_V1);
    for declaration in [
        "pub fn enumerate<T>(values: [T]): [(Int, T)]",
        "pub fn map_indexed<T, U>(values: [T], transform: fn(Int, T): U): [U]",
        "pub fn zip_with<T, U, V>(left: [T], right: [U], combine: fn(T, U): V): [V]",
        "pub fn unzip<T, U>(pairs: [(T, U)]): ([T], [U])",
    ] {
        assert!(source.contains(declaration), "missing collection adapter `{declaration}`");
    }
    for contract in [
        "zero-based source position",
        "from left to right",
        "stops at the shorter input",
        "Splits pairs into parallel lists",
    ] {
        assert!(source.contains(contract), "missing collection adapter contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("collection adapter bytes match the captured standard profile");
    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after profile capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("collection adapters resolve in the captured standard catalogue");
}

#[test]
fn pinned_lazy_sequence_adapters_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_LAZY_ADAPTERS_PATH_V1)
        .expect("the pinned source bundle includes std.lazy.adapters");
    assert_eq!(index, 63, "lazy adapters append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_LAZY_ADAPTERS_PATH_V1);
    for declaration in [
        "pub fn from_iterator<T>(source: std.iterator.Iterator<T>): fn(): std.iterator.Iterator<T>",
        "pub fn from_list<T>(values: [T]): fn(): std.iterator.Iterator<T>",
        "pub fn map<T, U>(",
        "pub fn filter<T>(",
        "pub fn flat_map<T, U>(",
        "pub fn chain<T>(",
        "pub fn zip_with<T, U, V>(",
        "pub fn take<T>(",
        "pub fn drop<T>(",
        "pub fn collect<T>(source: fn(): std.iterator.Iterator<T>): fn(): [T]",
    ] {
        assert!(source.contains(declaration), "missing lazy sequence adapter `{declaration}`");
    }
    for contract in [
        "Building a pipeline does not call its source thunk",
        "cursor adapters run only when pulled",
        "requires a finite cursor",
        "infinite sources, bound with `take`",
    ] {
        assert!(source.contains(contract), "missing lazy sequence adapter contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("lazy adapter bytes match the captured standard profile");
    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after profile capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("lazy sequence adapters resolve in the captured standard catalogue");
}

#[test]
fn pinned_stream_adapters_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_STREAM_ADAPTERS_PATH_V1)
        .expect("the pinned source bundle includes std.stream.adapters");
    assert_eq!(index, 64, "stream adapters append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_STREAM_ADAPTERS_PATH_V1);
    for declaration in [
        "pub fn from_iterator<T>(",
        "pub fn from_sequence<T>(",
        "pub fn from_iterator_batches<T>(",
        "pub fn from_sequence_batches<T>(",
        "pub fn buffered_batches<T>(",
    ] {
        assert!(source.contains(declaration), "missing stream adapter `{declaration}`");
    }
    for contract in [
        "These helpers require finite",
        "resulting list as a replayable stream source",
        "Invoke a sequence factory once",
        "final short batch is retained",
        "Item order and the final short batch are preserved",
    ] {
        assert!(source.contains(contract), "missing stream adapter contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("stream adapter bytes match the captured standard profile");
    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after snapshot capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("stream adapters resolve in the captured standard catalogue");
}

#[test]
fn pinned_memo_helpers_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_MEMO_PATH_V1)
        .expect("the pinned source bundle includes std.memo");
    assert_eq!(index, 65, "memo helpers append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_MEMO_PATH_V1);
    for declaration in [
        "pub fn memoize_thunk<T>(",
        "pub fn memoize<K, V>(",
        "pub fn get<K, V>(",
        "pub fn remove<K, V>(",
        "pub fn clear<K, V>(",
    ] {
        assert!(source.contains(declaration), "missing memo helper `{declaration}`");
    }
    for contract in [
        "immutable",
        "updated cache beside its value",
        "optional wrapper distinguishes an empty cache",
        "cached null",
        "lawful equality relation",
    ] {
        assert!(source.contains(contract), "missing memoization contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("memo helper bytes match the captured standard profile");
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("memo helpers resolve in the captured standard catalogue");
}

#[test]
fn pinned_error_combinators_are_included_in_the_captured_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_ERROR_COMBINATORS_PATH_V1)
        .expect("the pinned source bundle includes std.error.combinators");
    assert_eq!(index, 58, "the error combinators append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_ERROR_COMBINATORS_PATH_V1);
    for declaration in [
        "pub fn with_context(value: Error, code: Str, message: Str): Error",
        "pub fn with_contexts(value: Error, contexts: [(Str, Str)]): Error",
        "pub fn contains_code(value: Error, expected: Str): Bool",
        "pub fn contains_any_code(value: Error, expected: [Str]): Bool",
        "pub fn contains_all_codes(value: Error, expected: [Str]): Bool",
        "pub fn matches_code_chain(value: Error, expected: [Str]): Bool",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    for contract in [
        "never catch, replace, or convert a language failure",
        "Contexts are listed outermost first",
        "matches vacuously",
        "breadth-first order",
    ] {
        assert!(source.contains(contract), "missing error combinator contract `{contract}`");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("error combinator source bytes are recorded by the captured std profile");
    reference_standard_catalogue_v1()
        .expect("the error combinator module resolves against the captured std snapshot");
}

#[test]
fn pinned_numeric_conversions_are_included_in_the_captured_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_NUMERIC_PATH_V1)
        .expect("the pinned source bundle includes std.numeric");
    assert_eq!(index, 59, "the numeric module appends without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_NUMERIC_PATH_V1);
    for declaration in [
        "pub fn integer_from_text(input: Str): Int?",
        "pub fn integer_to_text(value: Int): Str",
        "pub fn decimal_from_integer(value: Int): Decimal",
        "pub fn decimal_from_text(input: Str): Decimal?",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    for contract in [
        "never pass through binary Float",
        "minimal signed base-ten text",
        "redundant leading zeroes",
        "fractional part must contain digits",
        "No rounding is performed",
    ] {
        assert!(source.contains(contract), "missing numeric conversion contract `{contract}`");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("numeric source bytes are recorded by the captured std profile");
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("the numeric conversion module resolves against the captured std snapshot");
}

#[test]
fn pinned_text_builder_is_included_in_the_captured_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_TEXT_BUILDER_PATH_V1)
        .expect("the pinned source bundle includes std.text.builder");
    assert_eq!(index, 60, "the text builder appends without moving old source entries");
    assert_eq!(path, REFERENCE_STANDARD_TEXT_BUILDER_PATH_V1);
    for declaration in [
        "pub fn new(): [Str]",
        "pub fn from_text(value: Str): [Str]",
        "pub fn append(builder: [Str], value: Str): [Str]",
        "pub fn append_all(builder: [Str], values: [Str]): [Str]",
        "pub fn append_line(builder: [Str], value: Str): [Str]",
        "pub fn build(builder: [Str]): Str",
        "pub fn is_empty(builder: [Str]): Bool",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    for contract in [
        "ordered [Str] chunks until build joins them",
        "leaves the original reusable",
        "Adds one line followed by LF",
        "Emptiness describes the built text",
    ] {
        assert!(source.contains(contract), "missing string builder contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("text builder source bytes are recorded by the captured std profile");
    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after profile capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("the text builder resolves against the captured standard snapshot");
}

#[test]
fn pinned_std_entrypoint_imports_optional_content_modules() {
    let entrypoint = include_str!("../../../../stdlib/std/main.orna");
    let parsed = orna_syntax_v1::parse_module_with_file(entrypoint, "std/main.orna");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    assert!(entrypoint.lines().any(|line| line.trim() == "use hash;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use random;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use money;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use encoding;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use url;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use net;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use test;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use generics;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use type_utils;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use pattern;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use regex;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use iterator;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use lazy;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use views;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use introspection;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use reflection;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use format;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use parse;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use prelude;"));
}

#[test]
fn pinned_std_prelude_publishes_its_versioned_curated_export_set() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_PRELUDE_PATH_V1)
        .expect("the pinned source bundle includes std.prelude");
    assert_eq!(index, 54, "the facade appends without renumbering old source entries");
    for declaration in [
        "pub fn api_version(): Str",
        "pub fn version(): Str",
        "pub fn count<T>(values: [T]): Int",
        "pub fn first<T>(values: [T]): T?",
        "pub fn unique<T>(values: [T]): [T]",
        "pub fn trim(value: Str): Str",
        "pub fn split(value: Str, separator: Str): [Str]",
        "pub fn is_some<T>(value: T?): Bool",
        "pub fn is_none<T>(value: T?): Bool",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);

    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("facade bytes are bound to the captured std snapshot");
    assert_eq!(
        profile
            .module_prelude_exports()
            .get(REFERENCE_STANDARD_PRELUDE_PATH_V1)
            .expect("the prelude export set is recorded by the snapshot")
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        REFERENCE_STANDARD_PRELUDE_EXPORTS_V1
    );
    reference_standard_catalogue_v1()
        .expect("curated names resolve through the pinned std catalogue");
}

#[test]
fn pinned_test_should_contracts_typecheck_from_the_captured_module() {
    let test_source = reference_standard_sources_v1()
        .into_iter()
        .find(|(path, _)| path == REFERENCE_STANDARD_TEST_PATH_V1)
        .expect("the pinned source bundle includes std.test");
    reference_standard_profile_v1()
        .verify_source(&test_source.0, &test_source.1)
        .expect("the test contracts use bytes from the captured std snapshot");
    let sources = vec![test_source];
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/test-assert-contracts",
        sources.clone(),
    )
    .expect("the selected pinned test source forms a captured module snapshot");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources)
        .expect("the pinned test module resolves without host test-runner imports");
    let consumer = include_str!("fixtures/v1_test_consumer.orna");
    let analysis =
        analyze_with_catalogue(&[ModuleInput::new("test_consumer.orna", consumer)], &catalogue);
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
}

#[test]
fn pinned_format_and_parse_modules_are_captured_and_resolve() {
    let sources = reference_standard_sources_v1();
    for (index, path, export) in [
        (52, REFERENCE_STANDARD_FORMAT_PATH_V1, "pub fn integer(value: Int): Str"),
        (53, REFERENCE_STANDARD_PARSE_PATH_V1, "pub fn integer(input: Str): Int?"),
    ] {
        assert_eq!(sources[index].0, path);
        assert!(sources[index].1.contains(export));
        let parsed = orna_syntax_v1::parse_module_with_file(&sources[index].1, path);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        reference_standard_profile_v1()
            .verify_source(path, &sources[index].1)
            .expect("published module bytes belong to the pinned standard profile");
    }
    reference_standard_catalogue_v1()
        .expect("format and parse imports resolve through the pinned standard catalogue");
}

#[test]
fn pinned_pattern_surface_typechecks_exhaustive_matches_and_destructuring() {
    let source = reference_standard_sources_v1()
        .into_iter()
        .find(|(path, _)| path == REFERENCE_STANDARD_PATTERN_PATH_V1)
        .expect("the pinned source bundle includes std.pattern")
        .1;
    let parsed = orna_syntax_v1::parse_module_with_file(&source, REFERENCE_STANDARD_PATTERN_PATH_V1);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1().expect("the pinned pattern module checks with std sources");
}

#[test]
fn pinned_regex_and_pattern_surfaces_are_versioned_and_snapshot_bound() {
    let sources = reference_standard_sources_v1();
    let (regex_index, (regex_path, regex_source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_REGEX_PATH_V1)
        .expect("the regex package is included in the captured source bundle");
    assert_eq!(regex_index, 47, "new source units append to preserve existing indexes");
    assert_eq!(regex_path, REFERENCE_STANDARD_REGEX_PATH_V1);
    for declaration in [
        "pub enum Regex",
        "pub enum Match",
        "pub fn dialect_version(): Str",
        "pub fn compile(pattern: Str): Regex",
        "pub fn is_match(regex: Regex, text: Str): Bool",
        "pub fn find(regex: Regex, text: Str): Match?",
        "pub fn find_all(regex: Regex, text: Str): [Match]",
        "pub fn count_matches(regex: Regex, text: Str): Int",
        "pub fn matched_text(value: Match): Str",
        "pub fn start(value: Match): Int",
        "pub fn end(value: Match): Int",
        "pub fn captures(value: Match): [Str?]",
        "pub fn replace_all(regex: Regex, text: Str, replacement: Str): Str",
        "pub fn split(regex: Regex, text: Str): [Str]",
        "pub fn escape_literal(text: Str): Str",
    ] {
        assert!(regex_source.contains(declaration), "missing {declaration}");
    }
    for contract in [
        "orna.regex/1",
        "Unicode 16.0.0",
        "Look-around, backreferences",
        "earliest-starting match",
        "Zero-width matches",
        "preserve empty fields",
        "silently truncated",
    ] {
        assert!(regex_source.contains(contract), "missing regex contract `{contract}`");
    }

    let profile = reference_standard_profile_v1();
    profile
        .verify_source(regex_path, regex_source)
        .expect("the regex declarations are recorded by the standard snapshot");
    let mut modified_regex = regex_source.clone();
    modified_regex.push_str("\n// changed after capture\n");
    assert!(profile.verify_source(regex_path, &modified_regex).is_err());

    let (pattern_path, pattern_source) = sources
        .iter()
        .find(|(path, _)| path == REFERENCE_STANDARD_PATTERN_PATH_V1)
        .expect("the pattern module is included in the captured source bundle");
    for declaration in [
        "pub fn fold<L, R, U>(",
        "pub fn map_left<L, R, U>(",
        "pub fn map_right<L, R, U>(",
        "pub fn bimap<L, R, A, B>(",
    ] {
        assert!(pattern_source.contains(declaration), "missing {declaration}");
    }
    profile
        .verify_source(pattern_path, pattern_source)
        .expect("pattern combinators are captured in the same standard snapshot");

    let parsed = orna_syntax_v1::parse_module_with_file(regex_source, regex_path);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let catalogue = reference_standard_catalogue_v1()
        .expect("regex and pattern declarations resolve within the pinned std profile");
    let consumer = include_str!("fixtures/v1_regex_pattern_consumer_g8rd3.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("regex_pattern_consumer.orna", consumer)],
        &catalogue,
    );
    assert!(
        analysis.is_ok(),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
fn pinned_regex_utilities_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_REGEX_UTILITIES_PATH_V1)
        .expect("the pinned source bundle includes std.regex.utilities");
    assert_eq!(index, 66, "regex utilities append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_REGEX_UTILITIES_PATH_V1);
    for declaration in [
        "pub fn capture_at(value: std.regex.Match, index: Int): Str?",
        "pub fn span(value: std.regex.Match): (Int, Int)",
        "pub fn span_length(value: std.regex.Match): Int",
        "pub fn is_zero_width(value: std.regex.Match): Bool",
        "pub fn matched_texts(values: [std.regex.Match]): [Str]",
        "pub fn spans(values: [std.regex.Match]): [(Int, Int)]",
        "pub fn capture_column(values: [std.regex.Match], index: Int): [Str?]",
        "pub fn zero_width_count(values: [std.regex.Match]): Int",
    ] {
        assert!(source.contains(declaration), "missing regex utility `{declaration}`");
    }
    for contract in [
        "Negative,",
        "nonparticipating groups all return null",
        "half-open scalar spans",
        "Preserve the source order, duplicate text, and empty matches",
        "result stays aligned with the input match list",
    ] {
        assert!(source.contains(contract), "missing regex utility contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("regex utility bytes match the captured standard profile");
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("regex utilities resolve in the captured standard catalogue");
}

#[test]
fn pinned_time_utilities_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_TIME_UTILITIES_PATH_V1)
        .expect("the pinned source bundle includes std.time.utilities");
    assert_eq!(index, 67, "time utilities append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_TIME_UTILITIES_PATH_V1);
    for declaration in [
        "pub fn is_weekend(year: Int, month: Int, day: Int): Bool?",
        "pub fn is_business_day(year: Int, month: Int, day: Int): Bool?",
        "pub fn week_start(year: Int, month: Int, day: Int): (Int, Int, Int)?",
        "pub fn week_end(year: Int, month: Int, day: Int): (Int, Int, Int)?",
        "pub fn quarter_of(year: Int, month: Int): Int?",
        "pub fn quarter_start(year: Int, month: Int): (Int, Int, Int)?",
        "pub fn quarter_end(year: Int, month: Int): (Int, Int, Int)?",
        "pub fn instant_in_closed_range(value: Instant, left: Instant, right: Instant): Bool",
        "pub fn instant_distance(left: Instant, right: Instant): Duration",
    ] {
        assert!(source.contains(declaration), "missing time utility `{declaration}`");
    }
    for contract in [
        "Weekdays use ISO numbering",
        "Return the Monday and Sunday",
        "Quarters are numbered 1 through 4",
        "Test membership in a closed instant interval",
        "nonnegative, exact elapsed distance",
    ] {
        assert!(source.contains(contract), "missing time utility contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("time utility bytes match the captured standard profile");
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("time utilities resolve in the captured standard catalogue");
}

#[test]
fn pinned_sorting_utilities_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_SORTING_PATH_V1)
        .expect("the pinned source bundle includes std.sorting");
    assert_eq!(index, 68, "sorting utilities append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_SORTING_PATH_V1);
    for declaration in [
        "pub fn compare<K>",
        "pub fn sort_by<T, K>",
        "pub fn sort_by_descending<T, K>",
        "pub fn sort<T>",
        "pub fn sort_descending<T>",
        "pub fn is_sorted_by<T, K>",
        "pub fn is_sorted_by_descending<T, K>",
        "pub fn min_by<T, K>",
        "pub fn max_by<T, K>",
    ] {
        assert!(source.contains(declaration), "missing sorting utility `{declaration}`");
    }
    for contract in [
        "Sort operations evaluate the key once for each input value.",
        "preserving the source order of equal keys",
        "equal adjacent keys are allowed",
        "Return the first value with the minimum key",
        "Return the first value with the maximum key",
    ] {
        assert!(source.contains(contract), "missing sorting utility contract `{contract}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("sorting utility bytes match the captured standard profile");
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("sorting utilities resolve in the captured standard catalogue");
}

#[test]
fn pinned_string_formatting_utilities_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_FORMAT_STRINGS_PATH_V1)
        .expect("the pinned source bundle includes std.format.strings");
    assert_eq!(index, 69, "string formatting utilities append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_FORMAT_STRINGS_PATH_V1);
    for declaration in [
        "pub fn repeat(value: Str, count: Int): Str",
        "pub fn pad_left(value: Str, width: Int, fill: Str = \" \"): Str",
        "pub fn pad_right(value: Str, width: Int, fill: Str = \" \"): Str",
        "pub fn center(value: Str, width: Int, fill: Str = \" \"): Str",
        "pub fn truncate(value: Str, width: Int, suffix: Str = \"…\"): Str",
        "pub fn indent(value: Str, prefix: Str): Str",
    ] {
        assert!(source.contains(declaration), "missing string formatting utility `{declaration}`");
    }
    for contract in [
        "Unicode scalar values",
        "multi-scalar fill string repeats as a scalar pattern",
        "padding scalar goes on the right",
        "preserving empty lines and a final trailing newline",
    ] {
        assert!(source.contains(contract), "missing string formatting contract `{contract}`");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("string formatting utility bytes match the captured standard profile");
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("string formatting utilities resolve in the captured standard catalogue");
}

#[test]
fn pinned_parsing_utilities_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_PARSE_UTILITIES_PATH_V1)
        .expect("the pinned source bundle includes std.parse.utilities");
    assert_eq!(index, 70, "parsing utilities append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_PARSE_UTILITIES_PATH_V1);
    for declaration in [
        "pub fn integer_or(input: Str, fallback: Int): Int",
        "pub fn boolean_or(input: Str, fallback: Bool): Bool",
        "pub fn integer_list(input: Str, separator: Str): [Int]?",
        "pub fn boolean_list(input: Str, separator: Str): [Bool]?",
        "pub fn split_once(input: Str, separator: Str): (Str, Str)?",
    ] {
        assert!(source.contains(declaration), "missing parsing utility `{declaration}`");
    }
    for contract in [
        "all-or-nothing",
        "Empty input is `Some([])`",
        "empty/invalid field",
        "returns null",
        "complete remainder",
        "after it",
    ] {
        assert!(source.contains(contract), "missing parsing utility contract `{contract}`");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("parsing utility bytes match the captured standard profile");
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("parsing utilities resolve in the captured standard catalogue");
}

#[test]
fn pinned_unicode_and_encoding_utilities_are_included_and_typecheck() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_ENCODING_UTILITIES_PATH_V1)
        .expect("the pinned source bundle includes std.encoding.utilities");
    assert_eq!(index, 71, "Unicode and encoding utilities append without moving old sources");
    assert_eq!(path, REFERENCE_STANDARD_ENCODING_UTILITIES_PATH_V1);
    for declaration in [
        "pub fn unicode_scalars(value: Str): [Str]",
        "pub fn unicode_scalar_length(value: Str): Int",
        "pub fn unicode_reverse(value: Str): Str",
        "pub fn unicode_slice(value: Str, start: Int, end: Int): Str",
        "pub fn base64url_encode(input: Blob): Str",
        "pub fn base64url_decode(input: Str): Blob",
    ] {
        assert!(source.contains(declaration), "missing Unicode/encoding utility `{declaration}`");
    }
    for contract in [
        "Unicode scalar values",
        "combining marks and joiners stay separate",
        "bounds past the string clamp",
        "unpadded URL-safe Base64",
        "nonzero unused trailing bits",
    ] {
        assert!(source.contains(contract), "missing Unicode/encoding contract `{contract}`");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("Unicode and encoding utility bytes match the captured standard profile");
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("Unicode and encoding utilities resolve in the captured standard catalogue");
}

#[test]
fn pinned_iterator_and_lazy_surfaces_typecheck_as_ordinary_std_modules() {
    let sources = reference_standard_sources_v1();
    for (index, path) in [
        (37, REFERENCE_STANDARD_ITERATOR_PATH_V1),
        (38, REFERENCE_STANDARD_LAZY_PATH_V1),
    ] {
        assert_eq!(sources[index].0, path);
        let parsed = orna_syntax_v1::parse_module_with_file(&sources[index].1, path);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    }
    reference_standard_catalogue_v1().expect("iterator and lazy module sources check in the pinned profile");
}

#[test]
fn pinned_collection_views_and_slices_typecheck_as_an_ordinary_std_module() {
    let sources = reference_standard_sources_v1();
    assert_eq!(sources[39].0, REFERENCE_STANDARD_VIEWS_PATH_V1);
    let parsed = orna_syntax_v1::parse_module_with_file(
        &sources[39].1,
        REFERENCE_STANDARD_VIEWS_PATH_V1,
    );
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let catalogue = reference_standard_catalogue_v1().expect("views source checks in the pinned std profile");
    let consumer = include_str!("fixtures/v1_views_consumer_wc6kr.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("views_consumer.orna", consumer)],
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
fn pinned_option_combinators_are_in_the_captured_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_OPTION_PATH_V1)
        .expect("the pinned source bundle includes std.option");
    assert_eq!(index, 11, "the option module retains its source index");
    for declaration in [
        "pub fn and<T, U>",
        "pub fn or<T>",
        "pub fn xor<T>",
        "pub fn flatten<T>(value: T? ?): T?",
        "pub fn map_or<T, U>",
        "pub fn map_or_else<T, U>(",
        "pub fn unwrap_or_else<T>",
        "pub fn contains<T>",
        "pub fn zip_with<T, U, V>(",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    for contract in [
        "never catch failures raised by callbacks",
        "use `and_then` when",
        "use `or_else` for a",
        "exactly one input is present",
        "calls `fallback` only for null",
        "callback runs only when both options contain values",
    ] {
        assert!(source.contains(contract), "missing option contract `{contract}`");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("option combinator source bytes are captured by the std profile");
    reference_standard_catalogue_v1()
        .expect("option combinators resolve in the captured standard catalogue");
}

#[test]
fn pinned_result_combinators_are_in_the_captured_snapshot() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_RESULT_PATH_V1)
        .expect("the pinned source bundle includes std.result");
    assert_eq!(index, 12, "the result module retains its source index");
    for declaration in [
        "pub fn and<T, U, E>(",
        "pub fn or<T, E, F>(",
        "pub fn contains<T, E>(",
        "pub fn contains_error<T, E>(",
        "pub fn zip<T, U, E>(",
        "pub fn transpose<T, E>(result: Result<T?, E>): Result<T, E>?",
    ] {
        assert!(source.contains(declaration), "missing `{declaration}`");
    }
    for contract in [
        "language failure raised by a callback",
        "use `and_then` to construct `next` conditionally",
        "computed only for the Err branch",
        "first Err from left to right",
        "empty option, or the error payload",
    ] {
        assert!(
            source.contains(contract),
            "missing result combinator contract `{contract}`"
        );
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("result combinator bytes match the captured standard profile");
    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after profile capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    reference_standard_catalogue_v1()
        .expect("result combinators resolve in the captured standard catalogue");
}

#[test]
fn pinned_bits_source_includes_bit_and_unsigned_byte_contracts() {
    let sources = reference_standard_sources_v1();
    let (index, (path, source)) = sources
        .iter()
        .enumerate()
        .find(|(_, (path, _))| path == REFERENCE_STANDARD_BITS_PATH_V1)
        .expect("the pinned source bundle includes std.bits");
    assert_eq!(index, 4, "the existing bits module keeps its source index");
    for declaration in [
        "pub fn test_bit(value: Int, index: Int): Bool?",
        "pub fn set_bit(value: Int, index: Int): Int?",
        "pub fn clear_bit(value: Int, index: Int): Int?",
        "pub fn toggle_bit(value: Int, index: Int): Int?",
        "pub fn is_byte(value: Int): Bool",
        "pub fn unsigned_to_bytes(value: Int, width: Int, order: Str): [Int]?",
        "pub fn unsigned_from_bytes(bytes: [Int], order: Str): Int?",
    ] {
        assert!(source.contains(declaration), "missing {declaration}");
    }
    for contract in [
        "infinite sign extension",
        "exactly `width` bytes",
        "Little endian places the least-significant byte first",
        "most-significant byte first",
        "Leading zero bytes are accepted",
    ] {
        assert!(source.contains(contract), "missing bit/byte contract `{contract}`");
    }
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("bit and byte helper source bytes are captured by the std profile");
    reference_standard_catalogue_v1()
        .expect("bit and byte helper imports resolve in the captured catalogue");
}

#[test]
fn pinned_collection_overloads_preserve_relation_result_kinds() {
    let catalogue = reference_standard_catalogue_v1().expect("the pinned std profile checks");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "relation_collection_consumer.orna",
            include_str!("fixtures/v1_relation_collection_overloads_0re2w.orna"),
        )],
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
    let module = analysis
        .modules
        .values()
        .find(|module| module.namespace.display() == "relation_collection_consumer")
        .expect("fixture module");
    let result_type = |name: &str| match &module.symbols[name].ty {
        Type::Function { result, .. } => result.as_ref(),
        other => panic!("{name} should be a function, got {other:?}"),
    };
    let sample = Type::Named("Sample".into());
    let tag = Type::Named("Tag".into());
    let relation = |element| Type::Relation(Box::new(element));

    for name in ["chunked", "windowed", "split", "piped_chunks"] {
        assert_eq!(
            result_type(name),
            &relation(Type::List(Box::new(sample.clone()))),
            "{name} should preserve a Relation result"
        );
    }
    assert_eq!(
        result_type("bucketed"),
        &relation(Type::List(Box::new(Type::Instant)))
    );
    assert_eq!(result_type("flattened"), &relation(sample.clone()));
    assert_eq!(
        result_type("partitioned"),
        &Type::Tuple(vec![relation(sample.clone()), relation(sample.clone())])
    );
    assert_eq!(
        result_type("zipped"),
        &relation(Type::Tuple(vec![sample.clone(), tag]))
    );
    assert_eq!(result_type("unique_rows"), &relation(sample.clone()));
    assert_eq!(
        result_type("grouped"),
        &relation(Type::Tuple(vec![
            Type::Text,
            Type::List(Box::new(sample.clone())),
        ]))
    );
    assert_eq!(
        result_type("pairs"),
        &relation(Type::Tuple(vec![sample.clone(), sample.clone()]))
    );
    assert_eq!(
        result_type("ranked"),
        &relation(Type::Tuple(vec![sample.clone(), Type::Int]))
    );
    assert_eq!(
        result_type("joined"),
        &relation(Type::Tuple(vec![
            sample.clone(),
            Type::Optional(Box::new(sample)),
        ]))
    );
}

#[test]
fn pinned_map_ovb_order_projection_is_snapshot_bound_and_typechecks() {
    let sources = reference_standard_sources_v1();
    let (path, source) = sources
        .iter()
        .find(|(path, _)| path == "std/map.orna")
        .expect("the pinned standard snapshot contains std.map");
    assert!(source.contains("pub fn entries_by_ovb_key"));
    assert!(source.contains("pub fn encoded_entries_by_ovb_key"));
    reference_standard_profile_v1()
        .verify_source(path, source)
        .expect("map ordering behavior is captured by the pinned std snapshot");

    let catalogue = reference_standard_catalogue_v1().expect("the pinned std sources typecheck");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "map_ovb_order_consumer.orna",
            include_str!("fixtures/v1_map_ovb_order_consumer_w02pm.orna"),
        )],
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
fn pinned_query_and_statistics_aggregates_accept_relation_inputs() {
    let catalogue = reference_standard_catalogue_v1()
        .expect("the pinned std profile checks");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "relation_statistics_consumer.orna",
            include_str!("fixtures/v1_relation_statistics_overloads_6c10u.orna"),
        )],
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
    let module = analysis
        .modules
        .values()
        .find(|module| module.namespace.display() == "relation_statistics_consumer")
        .expect("fixture module");
    let result_type = |name: &str| match &module.symbols[name].ty {
        Type::Function { result, .. } => result.as_ref(),
        other => panic!("{name} should be a function, got {other:?}"),
    };
    let optional_int = Type::Optional(Box::new(Type::Int));
    assert_eq!(result_type("query_count"), &Type::Int);
    assert_eq!(result_type("query_sum"), &Type::Int);
    assert_eq!(result_type("query_min"), &optional_int);
    assert_eq!(result_type("query_max"), &optional_int);
    assert_eq!(result_type("average"), &optional_int);
    assert_eq!(result_type("median"), &optional_int);
    assert_eq!(result_type("percentile"), &optional_int);
    assert_eq!(result_type("histogram"), &Type::List(Box::new(Type::Int)));
    assert_eq!(
        result_type("derivative"),
        &Type::List(Box::new(Type::Tuple(vec![Type::Instant, Type::Int])))
    );
    assert_eq!(result_type("piped_average"), &optional_int);
}
#[test]
fn pinned_query_plan_hint_exports_structured_plan() {
    let catalogue = reference_standard_catalogue_v1()
        .expect("the pinned std profile checks");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "query_plan_hint_consumer.orna",
            include_str!("fixtures/v1_query_plan_hint_statistics_azb4m.orna"),
        )],
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
    let module = analysis
        .modules
        .values()
        .find(|module| module.namespace.display() == "query_plan_hint_consumer")
        .expect("fixture module");
    let result_type = |name: &str| match &module.symbols[name].ty {
        Type::Function { result, .. } => result.as_ref(),
        other => panic!("{name} should be a function, got {other:?}"),
    };
    assert!(matches!(result_type("plan_hint"), Type::Named(name) if name == "sys.Plan"));
}
#[test]
fn pinned_calendar_arithmetic_source_typechecks_against_core() {
    let source = reference_standard_sources_v1()
        .into_iter()
        .find(|(path, _)| path == REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1)
        .expect("the pinned source bundle includes std.time.calendar")
        .1;
    let parsed = orna_syntax_v1::parse_module_with_file(
        &source,
        REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1,
    );
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("calendar.orna", source)],
        &orna_semantic_v1::Catalogue::authoritative_core(),
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
fn pinned_timezone_and_calendar_surfaces_typecheck_and_publish_in_snapshot() {
    let sources = reference_standard_sources_v1();
    assert_eq!(sources[6].0, REFERENCE_STANDARD_TIME_PATH_V1);
    let time = &sources[6].1;
    for declaration in [
        "pub fn timezone_data_version(): Str",
        "pub fn offset_at(instant: Instant, zone: Str): Int",
        "pub fn resolve_local(local: Str, zone: Str, ambiguous: Str): Instant",
    ] {
        assert!(time.contains(declaration), "missing std.time declaration `{declaration}`");
    }
    assert!(time.contains("orna-iana-2024a"));
    let parsed = orna_syntax_v1::parse_module_with_file(time, REFERENCE_STANDARD_TIME_PATH_V1);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let catalogue = reference_standard_catalogue_v1()
        .expect("time-zone APIs and calendar helpers resolve in the captured std profile");
    let consumer = include_str!("fixtures/v1_timezone_calendar_consumer_b8tgd.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("timezone_calendar_consumer.orna", consumer)],
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
    let mut changed_time = time.clone();
    changed_time.push_str("\n// changed after the captured snapshot\n");
    assert!(reference_standard_profile_v1()
        .verify_source(REFERENCE_STANDARD_TIME_PATH_V1, &changed_time)
        .is_err());
}

#[test]
fn pinned_reflection_and_introspection_modules_typecheck_as_ordinary_std_modules() {
    let sources = reference_standard_sources_v1();
    for (index, path) in [
        (40, REFERENCE_STANDARD_INTROSPECTION_PATH_V1),
        (41, REFERENCE_STANDARD_REFLECTION_PATH_V1),
    ] {
        assert_eq!(sources[index].0, path);
        let parsed = orna_syntax_v1::parse_module_with_file(&sources[index].1, path);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    }
    let catalogue = reference_standard_catalogue_v1()
        .expect("reflection and introspection resolve against the mandatory sys surface");
    let consumer = include_str!("fixtures/v1_reflection_consumer_y3xo7.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("reflection_consumer.orna", consumer)],
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
fn pinned_encoding_entrypoint_exports_its_named_codec_modules() {
    let entrypoint = include_str!("../../../../stdlib/std/encoding/main.orna");
    for module in ["base64", "json", "orna", "ovb"] {
        assert!(
            entrypoint.lines().any(|line| line.trim() == format!("use {module};")),
            "std.encoding must import {module}"
        );
    }
}

#[test]
fn pinned_network_entrypoint_exports_http_and_websocket_modules() {
    let entrypoint = include_str!("../../../../stdlib/std/net/main.orna");
    for module in ["http", "websocket"] {
        assert!(
            entrypoint.lines().any(|line| line.trim() == format!("use {module};")),
            "std.net must import {module}"
        );
    }
}

#[test]
fn pinned_test_module_resolves_against_core_without_importing_optional_std() {
    let source = include_str!("../../../../stdlib/std/test.orna");
    let parsed = orna_syntax_v1::parse_module_with_file(source, "test.orna");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("test.orna", source)],
        &orna_semantic_v1::Catalogue::authoritative_core(),
    );
    assert!(
        analysis.is_ok(),
        "{:#?}\n{}",
        analysis.diagnostics,
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
            .join("; ")
    );
}

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
    for declaration in [
        "pub fn sum<T>(rows: [T]): T",
        "pub fn min<T>(rows: [T]): T?",
        "pub fn max<T>(rows: [T]): T?",
        "pub fn range<T>(rows: [T]): T?",
        "pub fn mode<T>(rows: [T]): [T]",
        "pub fn variance<T>(",
        "pub fn standard_deviation<T>(",
        "pub fn histogram<T>(",
        "pub fn rate<T>(points: [(Instant, T)]): T?",
        "pub fn derivative<T>(points: [(Instant, T)]): [(Instant, T)]",
        "pub fn integrate<T>(points: [(Instant, T)]): T?",
    ] {
        assert!(sources[5].1.contains(declaration), "missing stats declaration `{declaration}`");
    }
    for contract in [
        "Empty sum is the additive zero",
        "Float aggregates preserve input order",
        "nonoverlapping [lower, upper)",
        "strictly increasing timestamps",
        "trapezoidal integration",
    ] {
        assert!(sources[5].1.contains(contract), "missing stats contract `{contract}`");
    }
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
        assert!(sources[index].1.contains("pub fn format("), "{operation} formatter export");
        assert!(sources[index].1.contains("fn __format("), "{operation} formatter body");
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
        "pub fn from_list_batches<T>(",
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
        "final short batch is retained",
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
    assert_eq!(sources[22].0, REFERENCE_STANDARD_RANDOM_PATH_V1);
    for declaration in [
        "pub fn bytes(count: Int): Blob",
        "pub fn integer(lower_inclusive: Int, upper_exclusive: Int): Int",
        "pub fn choose<T>(values: [T]): T?",
        "pub fn shuffle<T>(values: [T]): [T]",
    ] {
        assert!(sources[22].1.contains(declaration), "missing random declaration `{declaration}`");
    }
    for contract in [
        "host cryptographically secure random source",
        "nondeterministic host effects",
        "predictable pseudorandom source",
        "rejection sampling",
        "Return null for an empty list",
        "unbiased Fisher-Yates permutation",
    ] {
        assert!(sources[22].1.contains(contract), "missing random contract `{contract}`");
    }
    assert_eq!(sources[23].0, REFERENCE_STANDARD_HASH_PATH_V1);
    for declaration in [
        "pub fn sha256(input: Blob): Digest",
        "pub fn sha256_text(input: Str): Digest",
        "pub fn domain_sha256(domain: Str, payload: Blob): Digest",
        "pub fn to_hex(digest: Digest): Str",
        "pub fn from_hex(value: Str): Digest?",
    ] {
        assert!(sources[23].1.contains(declaration), "missing hash declaration `{declaration}`");
    }
    for contract in [
        "complete Blob",
        "exact UTF-8 bytes",
        "SHA256(ASCII(domain) || 00 || payload)",
        "64 lowercase hexadecimal characters",
        "Hashes of low-entropy secret values are sensitive",
        "Digests are fingerprints, not equality proofs",
    ] {
        assert!(sources[23].1.contains(contract), "missing hash contract `{contract}`");
    }
    assert_eq!(sources[24].0, REFERENCE_STANDARD_ENCODING_PATH_V1);
    for module in ["base64", "json", "orna", "ovb"] {
        assert!(sources[24].1.contains(&format!("use {module};")));
    }
    assert_eq!(sources[25].0, REFERENCE_STANDARD_ENCODING_ORNA_PATH_V1);
    for declaration in [
        "pub fn encode<T>(value: T): Str",
        "pub fn decode<T>(input: Str): T",
    ] {
        assert!(sources[25].1.contains(declaration), "missing Orna codec declaration `{declaration}`");
    }
    for contract in [
        "schema directed",
        "never executes declarations, imports",
        "exactly one final LF",
        "primary-key order followed by stable field-ID order",
        "NFC field",
        "finite Float uses 17 significant",
        "padded standard Base64",
        "decode(input, as: T)",
    ] {
        assert!(sources[25].1.contains(contract), "missing canonical text contract `{contract}`");
    }
    assert_eq!(sources[26].0, REFERENCE_STANDARD_ENCODING_OVB_PATH_V1);
    for declaration in [
        "pub fn encode<T>(value: T): Blob",
        "pub fn decode<T>(input: Blob): T",
    ] {
        assert!(sources[26].1.contains(declaration), "missing OVB declaration `{declaration}`");
    }
    for contract in [
        "OVB-1 is the pinned binary value profile",
        "shortest permitted",
        "duplicate keys",
        "noncanonical",
        "Decimal uses tag 60000",
        "Nominal records, enums, references and system values",
        "explicit type witness",
    ] {
        assert!(sources[26].1.contains(contract), "missing OVB contract `{contract}`");
    }
    assert_eq!(sources[27].0, REFERENCE_STANDARD_ENCODING_JSON_PATH_V1);
    for declaration in [
        "pub fn encode<T>(value: T): Str",
        "pub fn decode<T>(input: Str): T",
        "pub fn decode_with_options<T>(",
        "input: Str,",
        "ignore_unknown_fields: Bool = false",
    ] {
        assert!(sources[27].1.contains(declaration), "missing JSON declaration `{declaration}`");
    }
    for contract in [
        "schema directed",
        "Duplicate object keys are always rejected",
        "Missing fields remain distinct",
        "numeric tokens are parsed without first rounding",
        "Unknown fields fail by default",
        "nonstandard numeric tokens",
    ] {
        assert!(sources[27].1.contains(contract), "missing JSON contract `{contract}`");
    }
    assert_eq!(sources[28].0, REFERENCE_STANDARD_ENCODING_BASE64_PATH_V1);
    for declaration in [
        "pub fn encode(input: Blob): Str",
        "pub fn decode(input: Str): Blob",
    ] {
        assert!(sources[28].1.contains(declaration), "missing Base64 declaration `{declaration}`");
    }
    for contract in [
        "RFC 4648 alphabet",
        "required `=` padding",
        "noncanonical",
        "nonzero unused trailing bits",
        "URL-safe Base64",
    ] {
        assert!(sources[28].1.contains(contract), "missing Base64 contract `{contract}`");
    }
    assert_eq!(sources[29].0, REFERENCE_STANDARD_NET_PATH_V1);
    for module in ["http", "websocket"] {
        assert!(sources[29].1.contains(&format!("use {module};")));
    }
    assert_eq!(sources[30].0, REFERENCE_STANDARD_URL_PATH_V1);
    for declaration in [
        "pub fn parse(input: Str): Str",
        "pub fn resolve(base_url: Str, reference: Str): Str",
        "pub fn format(value: Str): Str",
        "pub fn encode_component(value: Str): Str",
        "pub fn decode_component(value: Str): Str",
        "pub fn query_parameters(value: Str): [(Str, Str?)]",
        "pub fn with_query_parameter(value: Str, name: Str, parameter: Str?): Str",
    ] {
        assert!(sources[30].1.contains(declaration), "missing URL declaration `{declaration}`");
    }
    for contract in [
        "http, https",
        "non-ASCII host labels",
        "malformed percent escapes",
        "RFC 3986 reference resolution",
        "Non-ASCII path, query, and fragment scalars are UTF-8",
        "Query parameters preserve source order and duplicates",
        "distinct from an explicit empty value",
        "'+' is a literal plus",
        "never transmit a fragment",
    ] {
        assert!(sources[30].1.contains(contract), "missing URL contract `{contract}`");
    }
    assert_eq!(sources[31].0, REFERENCE_STANDARD_NET_HTTP_PATH_V1);
    for declaration in [
        "pub fn start(",
        "max_header_bytes: Int",
        "max_body_bytes: Int",
        "pub fn wait(handle: Uuid, timeout: Duration?): (Int, [(Str, Str)], Blob)",
        "pub fn cancel(handle: Uuid): Bool",
        "pub fn send(",
    ] {
        assert!(sources[31].1.contains(declaration), "missing HTTP declaration `{declaration}`");
    }
    for contract in [
        "ordered list of name/value pairs",
        "http/https URLs are accepted",
        "HTTPS always validates",
        "No redirect is followed and no retry is implicit",
        "max_header_bytes must be positive",
        "max_body_bytes is nonnegative",
        "max_header_bytes",
        "max_body_bytes",
        "caller may cancel explicitly",
        "cannot retract bytes already sent",
    ] {
        assert!(sources[31].1.contains(contract), "missing HTTP contract `{contract}`");
    }
    assert_eq!(sources[32].0, REFERENCE_STANDARD_NET_WEBSOCKET_PATH_V1);
    for declaration in [
        "pub fn connect(",
        "pub fn send(connection: Uuid, kind: Str, payload: Blob): Unit",
        "pub fn receive(connection: Uuid): (Str, Blob)?",
        "pub fn selected_subprotocol(connection: Uuid): Str?",
        "pub fn close(connection: Uuid, code: Int, reason: Str): Unit",
        "pub fn cancel(connection: Uuid): Bool",
    ] {
        assert!(sources[32].1.contains(declaration), "missing WebSocket declaration `{declaration}`");
    }
    for contract in [
        "Only ws/wss URLs",
        "max_handshake_bytes",
        "max_message_bytes",
        "status is 101 and repeated headers are preserved",
        "must be positive",
        "owner ending cancels pending",
        "text payload must be valid UTF-8",
        "Return null only after an orderly peer close",
        "Immediate cancellation aborts pending reads/writes",
    ] {
        assert!(sources[32].1.contains(contract), "missing WebSocket contract `{contract}`");
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
    assert!(sources[16].1.contains("use buffer;"));
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
    assert_eq!(sources[33].0, REFERENCE_STANDARD_TEST_PATH_V1);
    for declaration in [
        "pub fn expect(condition: Bool",
        "pub fn expect_true(value: Bool)",
        "pub fn expect_false(value: Bool)",
        "pub fn expect_equal<T>(actual: T, expected: T)",
        "pub fn expect_not_equal<T>(actual: T, unexpected: T)",
        "pub fn expect_some<T>(value: T?)",
        "pub fn expect_none<T>(value: T?)",
        "pub fn should(condition: Bool)",
        "pub fn should_be_true(value: Bool)",
        "pub fn should_be_false(value: Bool)",
        "pub fn should_equal<T>(actual: T, expected: T)",
        "pub fn should_not_equal<T>(actual: T, unexpected: T)",
        "pub fn should_be_some<T>(value: T?)",
        "pub fn should_be_none<T>(value: T?)",
        "pub fn should_satisfy<T>(value: T, predicate: fn(T): Bool)",
        "pub fn expect_failure<T>(action: fn(): T, code: Str?)",
        "pub fn for_all<T>(",
        "pub fn integer_range(",
        "pub fn elements<T>(values: [T])",
        "pub fn list_of<T>(element: fn(Int, Int): T, maximum_length: Int)",
        "pub fn run_fixture(",
        "pub fn with_isolated_database<T>(",
    ] {
        assert!(sources[33].1.contains(declaration), "missing std.test declaration `{declaration}`");
    }
    for contract in [
        "Core `assert` remains",
        "Should-style forms use the same pure Boolean contract",
        "zero-based case index",
        "pair always produces the same value",
        "worktree-local CWD",
        "developer credentials, local",
        "live services are never implicit",
        "lower-inclusive",
        "upper-exclusive",
    ] {
        assert!(sources[33].1.contains(contract), "missing std.test contract `{contract}`");
    }
    assert_eq!(sources.len(), 72);
    assert_eq!(sources[49].0, REFERENCE_STANDARD_UI_PATH_V1);
    for declaration in [
        "pub fn Field<T>(label: Str, value: T): UI",
        "pub fn Text(value: Str): UI",
        "pub fn Rows(children: [UI]): UI",
        "pub fn Details(fields: [UI]): UI",
        "pub fn Button<A>(label: Str, action: A): UI",
        "pub fn Form<A>(fields: [UI], submit: A? = null): UI",
        "pub fn Input<T, A>(",
    ] {
        assert!(sources[49].1.contains(declaration), "missing std.ui declaration `{declaration}`");
    }
    for contract in [
        "original value and its runtime type",
        "Inspect fallback",
        "never execute action descriptors",
        "expected input type checked by the server-created event handle",
    ] {
        assert!(sources[49].1.contains(contract), "missing std.ui contract `{contract}`");
    }
    assert_eq!(sources[34].0, REFERENCE_STANDARD_GENERICS_PATH_V1);
    for declaration in [
        "pub fn identity<T>(value: T): T",
        "pub fn constant<T, U>(value: T, ignored: U): T",
        "pub fn apply<T, U>(value: T, transform: fn(T): U): U",
        "pub fn compose<T, U, V>(first: fn(T): U, then: fn(U): V): fn(T): V",
        "pub fn both<T, U, V>(",
        "pub fn pipe<T, U, V>(",
    ] {
        assert!(sources[34].1.contains(declaration), "missing std.generics declaration `{declaration}`");
    }
    for contract in [
        "do not box values or add runtime reflection",
        "second value is ignored after evaluation",
        "Composition applies `first` before `then`",
        "transform_left runs before transform_right",
    ] {
        assert!(sources[34].1.contains(contract), "missing std.generics contract `{contract}`");
    }
    assert_eq!(sources[35].0, REFERENCE_STANDARD_TYPE_UTILS_PATH_V1);
    for declaration in [
        "pub fn pair<A, B>(first: A, second: B): (A, B)",
        "pub fn first_of<A, B>(value: (A, B)): A",
        "pub fn second_of<A, B>(value: (A, B)): B",
        "pub fn swap_pair<A, B>(value: (A, B)): (B, A)",
        "pub fn map_first<A, B, C>(",
        "pub fn map_second<A, B, C>(",
        "pub fn map_pair<A, B, C, D>(",
        "pub fn some<T>(value: T): T?",
        "pub fn map_option<T, U>(value: T?, transform: fn(T): U): U?",
    ] {
        assert!(sources[35].1.contains(declaration), "missing std.type_utils declaration `{declaration}`");
    }
    for contract in [
        "preserve each component's inferred type",
        "dynamic Any are not introduced",
        "Transform the first component before the second",
    ] {
        assert!(sources[35].1.contains(contract), "missing std.type_utils contract `{contract}`");
    }
    assert_eq!(sources[36].0, REFERENCE_STANDARD_PATTERN_PATH_V1);
    for declaration in [
        "pub enum Either<LeftValue, RightValue>",
        "pub fn is_left<L, R>(value: Either<L, R>): Bool",
        "pub fn is_right<L, R>(value: Either<L, R>): Bool",
        "pub fn left_value<L, R>(value: Either<L, R>): L?",
        "pub fn right_value<L, R>(value: Either<L, R>): R?",
        "pub fn split<L, R>(value: Either<L, R>): (L?, R?)",
        "pub fn left_or<L, R>(value: Either<L, R>, fallback: L): L",
        "pub fn right_or<L, R>(value: Either<L, R>, fallback: R): R",
        "pub fn swap<L, R>(value: Either<L, R>): Either<R, L>",
    ] {
        assert!(sources[36].1.contains(declaration), "missing std.pattern declaration `{declaration}`");
    }
    for contract in [
        "Orna's `case` expression remains the matching syntax",
        "Return null when the requested branch is inactive",
        "Split into two optional projections",
    ] {
        assert!(sources[36].1.contains(contract), "missing std.pattern contract `{contract}`");
    }
    assert_eq!(sources[37].0, REFERENCE_STANDARD_ITERATOR_PATH_V1);
    for declaration in [
        "pub enum Iterator<T>",
        "pub fn empty<T>(): Iterator<T>",
        "pub fn next<T>(iterator: Iterator<T>): (T?, Iterator<T>)",
        "pub fn from_list<T>(values: [T]): Iterator<T>",
        "pub fn iterate<T>(seed: T, successor: fn(T): T): Iterator<T>",
        "pub fn repeat<T>(value: T): Iterator<T>",
        "pub fn map<T, U>(source: Iterator<T>, transform: fn(T): U): Iterator<U>",
        "pub fn filter<T>(source: Iterator<T>, predicate: fn(T): Bool): Iterator<T>",
        "pub fn take<T>(source: Iterator<T>, count: Int): Iterator<T>",
        "pub fn drop<T>(source: Iterator<T>, count: Int): Iterator<T>",
        "pub fn collect<T>(source: Iterator<T>): [T]",
        "pub fn nth<T>(source: Iterator<T>, index: Int): T?",
    ] {
        assert!(sources[37].1.contains(declaration), "missing std.iterator declaration `{declaration}`");
    }
    for contract in [
        "Creating or composing one does",
        "replays its computation",
        "pure callbacks when repeatable observations are required",
        "Use `take` before `collect` on an infinite source",
        "Terminal operation. Finite iterators are required",
        "iterator count must be nonnegative",
        "iterator index must be nonnegative",
    ] {
        assert!(sources[37].1.contains(contract), "missing std.iterator contract `{contract}`");
    }
    assert_eq!(sources[38].0, REFERENCE_STANDARD_LAZY_PATH_V1);
    for declaration in [
        "pub fn defer<T>(computation: fn(): T): fn(): T",
        "pub fn force<T>(computation: fn(): T): T",
        "pub fn map<T, U>(computation: fn(): T, transform: fn(T): U): fn(): U",
        "pub fn and_then<T, U>(",
        "pub fn zip<T, U>(left: fn(): T, right: fn(): U): fn(): (T, U)",
        "pub fn constant<T>(value: T): fn(): T",
    ] {
        assert!(sources[38].1.contains(declaration), "missing std.lazy declaration `{declaration}`");
    }
    for contract in [
        "return thunks without evaluating their computations",
        "Every `force` call invokes its thunk again",
        "call-by-name with no memoization",
    ] {
        assert!(sources[38].1.contains(contract), "missing std.lazy contract `{contract}`");
    }
    assert_eq!(sources[39].0, REFERENCE_STANDARD_VIEWS_PATH_V1);
    for declaration in [
        "pub enum View<T>",
        "pub fn from_list<T>(source: [T]): View<T>",
        "pub fn length<T>(view: View<T>): Int",
        "pub fn is_empty<T>(view: View<T>): Bool",
        "pub fn get<T>(view: View<T>, index: Int): T?",
        "pub fn slice<T>(view: View<T>, start: Int, end: Int): View<T>",
        "pub fn take<T>(view: View<T>, count: Int): View<T>",
        "pub fn drop<T>(view: View<T>, count: Int): View<T>",
        "pub fn to_list<T>(view: View<T>): [T]",
        "pub fn windows<T>(view: View<T>, width: Int, step: Int = 1): [View<T>]",
    ] {
        assert!(sources[39].1.contains(declaration), "missing std.views declaration `{declaration}`");
    }
    for contract in [
        "zero-based half-open range",
        "materializes the selected values in source order",
        "operations validate ranges constructed",
        "get` returns null for any negative or out-of-range index",
        "Invalid bounds fail instead of being clamped",
        "view count must be nonnegative",
        "An incomplete final window is omitted",
    ] {
        assert!(sources[39].1.contains(contract), "missing std.views contract `{contract}`");
    }
    assert_eq!(sources[40].0, REFERENCE_STANDARD_INTROSPECTION_PATH_V1);
    for declaration in [
        "pub fn metadata<T>(value: T): sys.ValueMetadata<T>",
        "pub fn resolve_object(",
        "pub fn resolve_function(",
        "pub fn describe(object: sys.ObjectRef): sys.ObjectDescription",
        "pub fn source(object: sys.ObjectRef): sys.SourceDocument",
        "pub fn file_source(file: sys.FileRef): sys.SourceDocument",
        "pub fn history(object: sys.ObjectRef): Relation<sys.Revision>",
        "pub fn file_history(file: sys.FileRef): Relation<sys.FileVersion>",
        "pub fn dependencies(",
        "pub fn dependents(",
        "pub fn explain_function(function: sys.FunctionRef): sys.Plan",
        "pub fn explain_query<T>(query: Query<T>): sys.Plan",
        "pub fn explain_diagnostic(diagnostic: sys.Diagnostic): sys.Explanation",
    ] {
        assert!(sources[40].1.contains(declaration), "missing std.introspection declaration `{declaration}`");
    }
    assert_eq!(sources[41].0, REFERENCE_STANDARD_REFLECTION_PATH_V1);
    for declaration in [
        "pub fn invoke_as<T>(",
        "pub fn invoke_value(",
        "pub fn start_as<T>(",
        "pub fn start_value(",
        "pub fn await_result<T>(",
        "pub fn cancel<T>(",
    ] {
        assert!(sources[41].1.contains(declaration), "missing std.reflection declaration `{declaration}`");
    }
    let reflection_contract_text = sources[41]
        .1
        .lines()
        .map(|line| line.trim().trim_start_matches("//").trim())
        .collect::<Vec<_>>()
        .join(" ");
    for contract in [
        "wrappers do not evaluate source text",
        "The result witness is explicit and checked before target effects",
        "Asynchronous starts use a separate transaction by default",
        "A timeout does not cancel the child",
    ] {
        assert!(reflection_contract_text.contains(contract), "missing std.reflection contract `{contract}`");
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
    let semantic_catalogue = orna_semantic_v1::Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone());
    assert!(
        semantic_catalogue.is_ok(),
        "source-backed standard catalogue rejected the pinned modules: {:?}",
        semantic_catalogue.err()
    );
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
    for (path, source_index) in [
        (REFERENCE_STANDARD_RANDOM_PATH_V1, 22),
        (REFERENCE_STANDARD_HASH_PATH_V1, 23),
        (REFERENCE_STANDARD_ENCODING_PATH_V1, 24),
        (REFERENCE_STANDARD_ENCODING_ORNA_PATH_V1, 25),
        (REFERENCE_STANDARD_ENCODING_OVB_PATH_V1, 26),
        (REFERENCE_STANDARD_ENCODING_JSON_PATH_V1, 27),
        (REFERENCE_STANDARD_ENCODING_BASE64_PATH_V1, 28),
        (REFERENCE_STANDARD_NET_PATH_V1, 29),
        (REFERENCE_STANDARD_URL_PATH_V1, 30),
        (REFERENCE_STANDARD_NET_HTTP_PATH_V1, 31),
        (REFERENCE_STANDARD_NET_WEBSOCKET_PATH_V1, 32),
        (REFERENCE_STANDARD_TEST_PATH_V1, 33),
        (REFERENCE_STANDARD_GENERICS_PATH_V1, 34),
        (REFERENCE_STANDARD_TYPE_UTILS_PATH_V1, 35),
        (REFERENCE_STANDARD_PATTERN_PATH_V1, 36),
        (REFERENCE_STANDARD_ITERATOR_PATH_V1, 37),
        (REFERENCE_STANDARD_LAZY_PATH_V1, 38),
        (REFERENCE_STANDARD_VIEWS_PATH_V1, 39),
        (REFERENCE_STANDARD_INTROSPECTION_PATH_V1, 40),
        (REFERENCE_STANDARD_REFLECTION_PATH_V1, 41),
    ] {
        let mut changed_source = sources[source_index].1.clone();
        changed_source.push_str("\n// changed after the captured snapshot\n");
        assert!(profile.verify_source(path, &changed_source).is_err());
    }

    for (path, source) in &sources {
        let parsed = orna_syntax_v1::parse_module_with_file(source, path);
        assert!(parsed.is_ok(), "{path}: {:?}", parsed.diagnostics);
    }
    let serialization_calls =
        include_str!("fixtures/v1_serialization_calls.orna");
    let parsed_serialization_calls = orna_syntax_v1::parse_module_with_file(
        serialization_calls,
        "serialization_calls.orna",
    );
    assert!(
        parsed_serialization_calls.is_ok(),
        "{:?}",
        parsed_serialization_calls.diagnostics
    );
    let network_calls = include_str!("fixtures/v1_network_calls.orna");
    let parsed_network_calls =
        orna_syntax_v1::parse_module_with_file(network_calls, "network_calls.orna");
    assert!(parsed_network_calls.is_ok(), "{:?}", parsed_network_calls.diagnostics);
    let catalogue = reference_standard_catalogue_v1().expect("the standard module checks");
    let consumer = include_str!("fixtures/v1_collection_operations_consumer.orna");
    let asof_consumer = include_str!("fixtures/v1_standard_consumer.orna");
    let option_result_consumer = include_str!("fixtures/v1_option_result_consumer.orna");
    let text_numeric_consumer = include_str!("fixtures/v1_text_numeric_consumer.orna");
    let collections_consumer = include_str!("fixtures/v1_collections_consumer.orna");
    let concurrent_consumer = include_str!("fixtures/v1_concurrent_consumer.orna");
    let error_result_consumer = include_str!("fixtures/v1_error_result_consumer.orna");
    let time_calendar_consumer = include_str!("fixtures/v1_time_calendar_consumer.orna");
    let iteration_consumer = include_str!("fixtures/v1_iteration_consumer.orna");
    let random_hash_consumer = include_str!("fixtures/v1_random_hash_consumer.orna");
    let serialization_consumer = include_str!("fixtures/v1_serialization_consumer.orna");
    let network_consumer = include_str!("fixtures/v1_network_consumer.orna");
    let test_consumer = include_str!("fixtures/v1_test_consumer.orna");
    let generics_type_utils_consumer =
        include_str!("fixtures/v1_generics_type_utils_consumer.orna");
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
            ModuleInput::new("random_hash_consumer.orna", random_hash_consumer),
            ModuleInput::new("serialization_consumer.orna", serialization_consumer),
            ModuleInput::new("network_consumer.orna", network_consumer),
            ModuleInput::new("test_consumer.orna", test_consumer),
            ModuleInput::new("generics_type_utils_consumer.orna", generics_type_utils_consumer),
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
fn pinned_stats_aggregate_surface_typechecks_for_consumers() {
    let source = include_str!("fixtures/stats_consumer_fdqo9.orna");
    let parsed = orna_syntax_v1::parse_module_with_file(source, "stats-consumer.orna");
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let catalogue = reference_standard_catalogue_v1().unwrap();
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("stats-consumer.orna", source)],
        &catalogue,
    );
    assert!(
        analysis.is_ok(),
        "{:#?}\n{}",
        analysis.diagnostics,
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
            .join("; ")
    );
}

#[test]
fn pinned_money_operations_are_bound_to_the_captured_source_snapshot() {
    let sources = reference_standard_sources_v1();
    let (path, source) = sources
        .iter()
        .find(|(path, _)| path == REFERENCE_STANDARD_MONEY_PATH_V1)
        .expect("the pinned source bundle includes std.money");
    for declaration in [
        "pub fn quantize<T>(amount: T, minor_digits: Int, rounding: Str): T",
        "pub fn allocate<T>(",
        "pub fn format<T>(",
        "fn __quantize<T>(",
        "fn __allocate<T>(",
        "fn __format<T>(",
    ] {
        assert!(source.contains(declaration), "missing std.money declaration `{declaration}`");
    }
    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("money source bytes belong to the captured std profile");
    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after snapshot capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
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

#[test]
fn pinned_filesystem_path_and_metadata_modules_are_captured_and_typecheck() {
    let sources = reference_standard_sources_v1();
    assert_eq!(sources.len(), 72);
    for (index, path) in [
        (42, REFERENCE_STANDARD_IO_PATH_MODULE_PATH_V1),
        (43, REFERENCE_STANDARD_IO_METADATA_PATH_V1),
    ] {
        assert_eq!(sources[index].0, path);
        let parsed = orna_syntax_v1::parse_module_with_file(&sources[index].1, path);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    }

    let profile = reference_standard_profile_v1();
    for path in [
        REFERENCE_STANDARD_IO_PATH_MODULE_PATH_V1,
        REFERENCE_STANDARD_IO_METADATA_PATH_V1,
    ] {
        let source = sources
            .iter()
            .find(|(source_path, _)| source_path == path)
            .expect("filesystem path and metadata source is pinned");
        profile
            .verify_source(path, &source.1)
            .expect("source bytes match the captured std snapshot");
        let mut changed_source = source.1.clone();
        changed_source.push_str("\n// changed after snapshot capture\n");
        assert!(profile.verify_source(path, &changed_source).is_err());
    }

    let catalogue = reference_standard_catalogue_v1()
        .expect("the path and metadata modules resolve in the captured std profile");
    let consumer_source =
        include_str!("fixtures/v1_filesystem_paths_metadata_consumer_l80o5.orna");
    let parsed = orna_syntax_v1::parse_module_with_file(
        consumer_source,
        "filesystem_paths_metadata_consumer.orna",
    );
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "filesystem_paths_metadata_consumer.orna",
            consumer_source,
        )],
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
    let consumer = analysis
        .modules
        .values()
        .find(|module| module.namespace.display() == "filesystem_paths_metadata_consumer")
        .expect("filesystem consumer module");
    assert!(consumer.symbols.contains_key("child_path"));
}

#[test]
fn pinned_io_buffer_module_is_captured_and_resolves_stream_adapters() {
    let sources = reference_standard_sources_v1();
    assert_eq!(sources.len(), 72);
    let (path, source) = sources
        .iter()
        .find(|(path, _)| path == REFERENCE_STANDARD_IO_BUFFER_PATH_V1)
        .expect("the pinned std snapshot includes std.io.buffer");
    for declaration in [
        "pub fn read_line_batches(",
        "source_identity: Str",
        "batch_size: Int",
        "pub fn write_line_batches(",
        "std.stream.from_list",
        "std.stream.batch",
        "std.collection.flatten",
        "std.io.writer.write_all_text",
    ] {
        assert!(source.contains(declaration), "missing buffer contract `{declaration}`");
    }
    let parsed = orna_syntax_v1::parse_module_with_file(source, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);

    let profile = reference_standard_profile_v1();
    profile
        .verify_source(path, source)
        .expect("buffer module bytes belong to the captured std snapshot");
    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after snapshot capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
    Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the semantic catalogue accepts the full IO buffer snapshot");
    reference_standard_catalogue_v1()
        .expect("the IO buffer module and its stream dependencies typecheck");
}

#[test]
fn pinned_process_and_environment_modules_are_captured_and_typecheck() {
    let sources = reference_standard_sources_v1();
    assert_eq!(sources.len(), 72);
    for (index, path) in [
        (44, REFERENCE_STANDARD_IO_PROCESS_PATH_V1),
        (45, REFERENCE_STANDARD_IO_ENVIRONMENT_PATH_V1),
    ] {
        assert_eq!(sources[index].0, path);
        let parsed = orna_syntax_v1::parse_module_with_file(&sources[index].1, path);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    }

    let profile = reference_standard_profile_v1();
    for path in [
        REFERENCE_STANDARD_IO_PROCESS_PATH_V1,
        REFERENCE_STANDARD_IO_ENVIRONMENT_PATH_V1,
    ] {
        let source = sources
            .iter()
            .find(|(source_path, _)| source_path == path)
            .expect("process and environment modules are pinned");
        profile
            .verify_source(path, &source.1)
            .expect("source bytes match the captured std snapshot");
        let mut changed_source = source.1.clone();
        changed_source.push_str("\n// changed after snapshot capture\n");
        assert!(profile.verify_source(path, &changed_source).is_err());
    }

    let io_entrypoint = include_str!("../../../../stdlib/std/io/main.orna");
    assert!(io_entrypoint.lines().any(|line| line.trim() == "use process;"));
    assert!(io_entrypoint
        .lines()
        .any(|line| line.trim() == "use environment;"));
    let catalogue = reference_standard_catalogue_v1()
        .expect("process and environment modules resolve in the captured std profile");
    let consumer = include_str!("fixtures/v1_process_environment_consumer_xbf3n.orna");
    let parsed = orna_syntax_v1::parse_module_with_file(
        consumer,
        "process_environment_consumer.orna",
    );
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("process_environment_consumer.orna", consumer)],
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
