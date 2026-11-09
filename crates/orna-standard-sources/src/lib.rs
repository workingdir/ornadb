//! Pinned source bundle for the Orna 1.0.0 reference standard library.

/// Declares every reference standard module exactly once.
///
/// One literal is both the public logical path (`std/math.orna`) and the path of
/// the embedded file below `stdlib/`, so a module's name and its text cannot be
/// paired wrongly. A `+ "file"` suffix appends that file from this crate's
/// `src/fixtures/` directory to the module text. Table order is the order of
/// [`reference_standard_sources_v1`].
macro_rules! reference_standard_modules {
    ($($name:ident = $path:literal $(+ $addendum:literal)?;)+) => {
        $(
            #[doc = concat!("Logical source path of the pinned Orna 1.0.0 reference module `", $path, "`.")]
            pub const $name: &str = $path;
        )+

        const SOURCE_COUNT: usize = [$($path),+].len();

        const SOURCES: [(&str, &str); SOURCE_COUNT] = [
            $((
                $name,
                concat!(
                    include_str!(concat!("../../../stdlib/", $path))
                    $(, include_str!(concat!("fixtures/", $addendum)))?
                ),
            )),+
        ];
    };
}

reference_standard_modules! {
    REFERENCE_STANDARD_MATH_PATH_V1 = "std/math.orna";
    REFERENCE_STANDARD_COLLECTION_PATH_V1 = "std/collection.orna";
    REFERENCE_STANDARD_QUERY_PATH_V1 = "std/query.orna" + "query_recursive_cte_n0o0e.orna";
    REFERENCE_STANDARD_TEXT_PATH_V1 = "std/text.orna";
    REFERENCE_STANDARD_BITS_PATH_V1 = "std/bits.orna";
    REFERENCE_STANDARD_STATS_PATH_V1 = "std/stats.orna";
    REFERENCE_STANDARD_TIME_PATH_V1 = "std/time.orna";
    REFERENCE_STANDARD_TIME_COMPACT_PATH_V1 = "std/time/duration/compact.orna";
    REFERENCE_STANDARD_TIME_CLOCK_PATH_V1 = "std/time/duration/clock.orna";
    REFERENCE_STANDARD_TIME_WORDS_PATH_V1 = "std/time/duration/words.orna";
    REFERENCE_STANDARD_TIME_ISO_PATH_V1 = "std/time/duration/iso.orna";
    REFERENCE_STANDARD_OPTION_PATH_V1 = "std/option.orna";
    REFERENCE_STANDARD_RESULT_PATH_V1 = "std/result.orna";
    REFERENCE_STANDARD_LIST_PATH_V1 = "std/list.orna";
    REFERENCE_STANDARD_MAP_PATH_V1 = "std/map.orna";
    REFERENCE_STANDARD_SET_PATH_V1 = "std/set.orna";
    REFERENCE_STANDARD_IO_PATH_V1 = "std/io/main.orna";
    REFERENCE_STANDARD_FS_PATH_V1 = "std/io/fs.orna";
    REFERENCE_STANDARD_CONCURRENT_PATH_V1 = "std/concurrent/main.orna";
    REFERENCE_STANDARD_ERROR_PATH_V1 = "std/error.orna";
    REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1 = "std/time/calendar.orna";
    REFERENCE_STANDARD_STREAM_PATH_V1 = "std/stream.orna";
    REFERENCE_STANDARD_RANDOM_PATH_V1 = "std/random.orna";
    REFERENCE_STANDARD_HASH_PATH_V1 = "std/hash.orna";
    REFERENCE_STANDARD_ENCODING_PATH_V1 = "std/encoding/main.orna";
    REFERENCE_STANDARD_ENCODING_ORNA_PATH_V1 = "std/encoding/orna.orna";
    REFERENCE_STANDARD_ENCODING_OVB_PATH_V1 = "std/encoding/ovb.orna";
    REFERENCE_STANDARD_ENCODING_JSON_PATH_V1 = "std/encoding/json.orna";
    REFERENCE_STANDARD_ENCODING_BASE64_PATH_V1 = "std/encoding/base64.orna";
    REFERENCE_STANDARD_NET_PATH_V1 = "std/net/main.orna";
    REFERENCE_STANDARD_URL_PATH_V1 = "std/url.orna";
    REFERENCE_STANDARD_NET_HTTP_PATH_V1 = "std/net/http.orna";
    REFERENCE_STANDARD_NET_WEBSOCKET_PATH_V1 = "std/net/websocket.orna";
    REFERENCE_STANDARD_TEST_PATH_V1 = "std/test.orna";
    REFERENCE_STANDARD_GENERICS_PATH_V1 = "std/generics.orna";
    REFERENCE_STANDARD_TYPE_UTILS_PATH_V1 = "std/type_utils.orna";
    REFERENCE_STANDARD_PATTERN_PATH_V1 = "std/pattern.orna";
    REFERENCE_STANDARD_ITERATOR_PATH_V1 = "std/iterator.orna";
    REFERENCE_STANDARD_LAZY_PATH_V1 = "std/lazy.orna";
    REFERENCE_STANDARD_VIEWS_PATH_V1 = "std/views.orna";
    REFERENCE_STANDARD_INTROSPECTION_PATH_V1 = "std/introspection.orna";
    REFERENCE_STANDARD_REFLECTION_PATH_V1 = "std/reflection.orna";
    REFERENCE_STANDARD_IO_PATH_MODULE_PATH_V1 = "std/io/path.orna";
    REFERENCE_STANDARD_IO_METADATA_PATH_V1 = "std/io/metadata.orna";
    REFERENCE_STANDARD_IO_PROCESS_PATH_V1 = "std/io/process.orna";
    REFERENCE_STANDARD_IO_ENVIRONMENT_PATH_V1 = "std/io/environment.orna";
    REFERENCE_STANDARD_ALGORITHM_PATH_V1 = "std/algorithm.orna";
    REFERENCE_STANDARD_REGEX_PATH_V1 = "std/regex.orna";
    REFERENCE_STANDARD_MONEY_PATH_V1 = "std/money.orna";
    REFERENCE_STANDARD_UI_PATH_V1 = "std/ui.orna";
    REFERENCE_STANDARD_IO_READER_PATH_V1 = "std/io/reader.orna";
    REFERENCE_STANDARD_IO_WRITER_PATH_V1 = "std/io/writer.orna";
    REFERENCE_STANDARD_FORMAT_PATH_V1 = "std/format.orna";
    REFERENCE_STANDARD_PARSE_PATH_V1 = "std/parse.orna";
    REFERENCE_STANDARD_PRELUDE_PATH_V1 = "std/prelude.orna";
    REFERENCE_STANDARD_IO_BUFFER_PATH_V1 = "std/io/buffer.orna";
    REFERENCE_STANDARD_CONCURRENT_RESULT_PATH_V1 = "std/concurrent/result.orna";
    REFERENCE_STANDARD_ITERATOR_ADAPTERS_PATH_V1 = "std/iterator/adapters.orna";
    REFERENCE_STANDARD_ERROR_COMBINATORS_PATH_V1 = "std/error/combinators.orna";
    REFERENCE_STANDARD_NUMERIC_PATH_V1 = "std/numeric.orna";
    REFERENCE_STANDARD_TEXT_BUILDER_PATH_V1 = "std/text/builder.orna";
    REFERENCE_STANDARD_ITERATOR_CONSUMERS_PATH_V1 = "std/iterator/consumers.orna";
    REFERENCE_STANDARD_COLLECTION_ADAPTERS_PATH_V1 = "std/collection/adapters.orna";
    REFERENCE_STANDARD_LAZY_ADAPTERS_PATH_V1 = "std/lazy/adapters.orna";
    REFERENCE_STANDARD_STREAM_ADAPTERS_PATH_V1 = "std/stream/adapters.orna";
    REFERENCE_STANDARD_MEMO_PATH_V1 = "std/memo.orna";
    REFERENCE_STANDARD_REGEX_UTILITIES_PATH_V1 = "std/regex/utilities.orna";
    REFERENCE_STANDARD_TIME_UTILITIES_PATH_V1 = "std/time/utilities.orna";
    REFERENCE_STANDARD_SORTING_PATH_V1 = "std/sorting.orna";
    REFERENCE_STANDARD_FORMAT_STRINGS_PATH_V1 = "std/format/strings.orna";
    REFERENCE_STANDARD_PARSE_UTILITIES_PATH_V1 = "std/parse/utilities.orna";
    REFERENCE_STANDARD_ENCODING_UTILITIES_PATH_V1 = "std/encoding/utilities.orna";
    REFERENCE_STANDARD_BYTES_PATH_V1 = "std/bytes.orna";
    REFERENCE_STANDARD_COLLECTION_ADVANCED_PATH_V1 = "std/collection/advanced.orna";
    REFERENCE_STANDARD_ITERATION_PATH_V1 = "std/iteration.orna";
    REFERENCE_STANDARD_TEXT_LINES_PATH_V1 = "std/text/lines.orna";
    REFERENCE_STANDARD_TEXT_SLICING_PATH_V1 = "std/text/slicing.orna";
}

pub const REFERENCE_STANDARD_PRELUDE_EXPORTS_V1: &[&str] = &[
    "api_version",
    "count",
    "first",
    "is_none",
    "is_some",
    "split",
    "trim",
    "unique",
    "version",
];

/// Source units for the Orna 1.0.0 reference standard dependency.
///
/// This is the current source-backed standard boundary.
#[must_use]
pub fn reference_standard_sources_v1() -> [(String, String); SOURCE_COUNT] {
    SOURCES.map(|(path, source)| (path.to_owned(), source.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_paths_are_unique_and_every_source_is_embedded() {
        let mut seen = std::collections::BTreeSet::new();
        for (path, source) in reference_standard_sources_v1() {
            assert!(!source.is_empty(), "{path} embeds no source text");
            assert!(seen.insert(path.clone()), "{path} is listed twice");
        }
    }

    #[test]
    fn query_module_is_its_file_followed_by_the_recursive_cte_fixture() {
        let (_, query) = reference_standard_sources_v1()
            .into_iter()
            .find(|(path, _)| path == REFERENCE_STANDARD_QUERY_PATH_V1)
            .expect("the bundle contains std/query.orna");
        let file = include_str!("../../../stdlib/std/query.orna");
        let fixture = include_str!("fixtures/query_recursive_cte_n0o0e.orna");
        assert_eq!(query, format!("{file}{fixture}"));
    }
}
