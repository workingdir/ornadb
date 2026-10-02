use orna_semantic_v1::{ModuleInput, analyze_with_catalogue};

use crate::{
    REFERENCE_STANDARD_COLLECTION_PATH_V1, REFERENCE_STANDARD_MATH_PATH_V1,
    REFERENCE_STANDARD_BITS_PATH_V1, REFERENCE_STANDARD_QUERY_PATH_V1,
    REFERENCE_STANDARD_TEXT_PATH_V1, REFERENCE_STANDARD_STATS_PATH_V1,
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
    REFERENCE_STANDARD_CONCURRENT_PATH_V1, REFERENCE_STANDARD_ERROR_PATH_V1,
    REFERENCE_STANDARD_TEST_PATH_V1,
    reference_standard_catalogue_v1,
    reference_standard_profile_v1, reference_standard_sources_v1,
};

#[test]
fn pinned_std_entrypoint_imports_optional_content_modules() {
    let entrypoint = include_str!("../../../../stdlib/std/main.orna");
    let parsed = orna_syntax_v1::parse_module_with_file(entrypoint, "std/main.orna");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    assert!(entrypoint.lines().any(|line| line.trim() == "use hash;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use random;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use encoding;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use url;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use net;"));
    assert!(entrypoint.lines().any(|line| line.trim() == "use test;"));
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
        "pub fn decode_with_options<T>(input: Str, ignore_unknown_fields: Bool = false): T",
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
