//! Source-independent facts for the Orna standard library.

use std::{error::Error, fmt};

use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, SchemaId, SourceBundleId,
    SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeBindingId, TypeId,
    catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, PreludeTypeName, PreludeTypeNameError,
        QualifiedSemanticName, SchemaDefinition, SemanticNameError, TypeBinding, TypeBindingError,
        TypeLookupName, ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{Sha256Digest, VerifiedStandardLibrarySnapshot},
    value::{
        INSPECT_CARRIER_CODEC_REGISTRATIONS, InspectCarrierCodecRegistration,
        OpaqueCodecRegistration, OpaqueCodecRegistry, OpaqueCodecRegistryError,
    },
};
use orna_semantic_v1::{
    Catalogue as StandardCatalogueV1, StandardCatalogueError, StandardDependencyProfile,
};

mod codecs;
mod ids;

#[cfg(test)]
mod tests;

pub use codecs::{
    RegisteredOpaqueCodecsError, is_registered_inspect_carrier_type,
    registered_inspect_carrier_codecs, registered_opaque_codecs,
};

pub use ids::*;
pub use orna_core::inspect::INSPECT_RENDER_CONTRACT;

/// The standard-library version represented by this manifest.
pub const STANDARD_LIBRARY_VERSION_IDENTITY: &str = "orna.std/1";

/// Logical source path of the pinned Orna 1.0.0 reference math module.
pub const REFERENCE_STANDARD_MATH_PATH_V1: &str = "std/math.orna";
pub const REFERENCE_STANDARD_COLLECTION_PATH_V1: &str = "std/collection.orna";
pub const REFERENCE_STANDARD_ALGORITHM_PATH_V1: &str = "std/algorithm.orna";
pub const REFERENCE_STANDARD_QUERY_PATH_V1: &str = "std/query.orna";
pub const REFERENCE_STANDARD_TEXT_PATH_V1: &str = "std/text.orna";
pub const REFERENCE_STANDARD_TEXT_BUILDER_PATH_V1: &str = "std/text/builder.orna";
pub const REFERENCE_STANDARD_BITS_PATH_V1: &str = "std/bits.orna";
pub const REFERENCE_STANDARD_STATS_PATH_V1: &str = "std/stats.orna";
pub const REFERENCE_STANDARD_MONEY_PATH_V1: &str = "std/money.orna";
pub const REFERENCE_STANDARD_TIME_PATH_V1: &str = "std/time.orna";
pub const REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1: &str = "std/time/calendar.orna";
pub const REFERENCE_STANDARD_STREAM_PATH_V1: &str = "std/stream.orna";
pub const REFERENCE_STANDARD_RANDOM_PATH_V1: &str = "std/random.orna";
pub const REFERENCE_STANDARD_HASH_PATH_V1: &str = "std/hash.orna";
pub const REFERENCE_STANDARD_ENCODING_PATH_V1: &str = "std/encoding/main.orna";
pub const REFERENCE_STANDARD_ENCODING_ORNA_PATH_V1: &str = "std/encoding/orna.orna";
pub const REFERENCE_STANDARD_ENCODING_OVB_PATH_V1: &str = "std/encoding/ovb.orna";
pub const REFERENCE_STANDARD_ENCODING_JSON_PATH_V1: &str = "std/encoding/json.orna";
pub const REFERENCE_STANDARD_ENCODING_BASE64_PATH_V1: &str = "std/encoding/base64.orna";
pub const REFERENCE_STANDARD_NET_PATH_V1: &str = "std/net/main.orna";
pub const REFERENCE_STANDARD_URL_PATH_V1: &str = "std/url.orna";
pub const REFERENCE_STANDARD_NET_HTTP_PATH_V1: &str = "std/net/http.orna";
pub const REFERENCE_STANDARD_NET_WEBSOCKET_PATH_V1: &str = "std/net/websocket.orna";
pub const REFERENCE_STANDARD_TIME_COMPACT_PATH_V1: &str = "std/time/duration/compact.orna";
pub const REFERENCE_STANDARD_TIME_CLOCK_PATH_V1: &str = "std/time/duration/clock.orna";
pub const REFERENCE_STANDARD_TIME_WORDS_PATH_V1: &str = "std/time/duration/words.orna";
pub const REFERENCE_STANDARD_TIME_ISO_PATH_V1: &str = "std/time/duration/iso.orna";
pub const REFERENCE_STANDARD_OPTION_PATH_V1: &str = "std/option.orna";
pub const REFERENCE_STANDARD_RESULT_PATH_V1: &str = "std/result.orna";
pub const REFERENCE_STANDARD_ERROR_PATH_V1: &str = "std/error.orna";
pub const REFERENCE_STANDARD_ERROR_COMBINATORS_PATH_V1: &str = "std/error/combinators.orna";
pub const REFERENCE_STANDARD_NUMERIC_PATH_V1: &str = "std/numeric.orna";
pub const REFERENCE_STANDARD_LIST_PATH_V1: &str = "std/list.orna";
pub const REFERENCE_STANDARD_MAP_PATH_V1: &str = "std/map.orna";
pub const REFERENCE_STANDARD_SET_PATH_V1: &str = "std/set.orna";
pub const REFERENCE_STANDARD_IO_PATH_V1: &str = "std/io/main.orna";
pub const REFERENCE_STANDARD_FS_PATH_V1: &str = "std/io/fs.orna";
pub const REFERENCE_STANDARD_IO_PATH_MODULE_PATH_V1: &str = "std/io/path.orna";
pub const REFERENCE_STANDARD_IO_METADATA_PATH_V1: &str = "std/io/metadata.orna";
pub const REFERENCE_STANDARD_IO_PROCESS_PATH_V1: &str = "std/io/process.orna";
pub const REFERENCE_STANDARD_IO_ENVIRONMENT_PATH_V1: &str = "std/io/environment.orna";
pub const REFERENCE_STANDARD_IO_READER_PATH_V1: &str = "std/io/reader.orna";
pub const REFERENCE_STANDARD_IO_WRITER_PATH_V1: &str = "std/io/writer.orna";
pub const REFERENCE_STANDARD_IO_BUFFER_PATH_V1: &str = "std/io/buffer.orna";
pub const REFERENCE_STANDARD_CONCURRENT_PATH_V1: &str = "std/concurrent/main.orna";
pub const REFERENCE_STANDARD_CONCURRENT_RESULT_PATH_V1: &str = "std/concurrent/result.orna";
pub const REFERENCE_STANDARD_ITERATOR_ADAPTERS_PATH_V1: &str = "std/iterator/adapters.orna";
pub const REFERENCE_STANDARD_ITERATOR_CONSUMERS_PATH_V1: &str = "std/iterator/consumers.orna";
pub const REFERENCE_STANDARD_COLLECTION_ADAPTERS_PATH_V1: &str = "std/collection/adapters.orna";
pub const REFERENCE_STANDARD_LAZY_ADAPTERS_PATH_V1: &str = "std/lazy/adapters.orna";
pub const REFERENCE_STANDARD_STREAM_ADAPTERS_PATH_V1: &str = "std/stream/adapters.orna";
pub const REFERENCE_STANDARD_MEMO_PATH_V1: &str = "std/memo.orna";
pub const REFERENCE_STANDARD_TEST_PATH_V1: &str = "std/test.orna";
pub const REFERENCE_STANDARD_GENERICS_PATH_V1: &str = "std/generics.orna";
pub const REFERENCE_STANDARD_TYPE_UTILS_PATH_V1: &str = "std/type_utils.orna";
pub const REFERENCE_STANDARD_PATTERN_PATH_V1: &str = "std/pattern.orna";
pub const REFERENCE_STANDARD_REGEX_PATH_V1: &str = "std/regex.orna";
pub const REFERENCE_STANDARD_REGEX_UTILITIES_PATH_V1: &str = "std/regex/utilities.orna";
pub const REFERENCE_STANDARD_ITERATOR_PATH_V1: &str = "std/iterator.orna";
pub const REFERENCE_STANDARD_LAZY_PATH_V1: &str = "std/lazy.orna";
pub const REFERENCE_STANDARD_VIEWS_PATH_V1: &str = "std/views.orna";
pub const REFERENCE_STANDARD_INTROSPECTION_PATH_V1: &str = "std/introspection.orna";
pub const REFERENCE_STANDARD_REFLECTION_PATH_V1: &str = "std/reflection.orna";
pub const REFERENCE_STANDARD_UI_PATH_V1: &str = "std/ui.orna";
pub const REFERENCE_STANDARD_FORMAT_PATH_V1: &str = "std/format.orna";
pub const REFERENCE_STANDARD_PARSE_PATH_V1: &str = "std/parse.orna";
pub const REFERENCE_STANDARD_PRELUDE_PATH_V1: &str = "std/prelude.orna";
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

const REFERENCE_STANDARD_MATH_SOURCE_V1: &str = include_str!("../../../stdlib/std/math.orna");
const REFERENCE_STANDARD_COLLECTION_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/collection.orna");
const REFERENCE_STANDARD_ALGORITHM_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/algorithm.orna");
const REFERENCE_STANDARD_QUERY_SOURCE_V1: &str = include_str!("../../../stdlib/std/query.orna");
const REFERENCE_STANDARD_TEXT_SOURCE_V1: &str = include_str!("../../../stdlib/std/text.orna");
const REFERENCE_STANDARD_TEXT_BUILDER_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/text/builder.orna");
const REFERENCE_STANDARD_BITS_SOURCE_V1: &str = include_str!("../../../stdlib/std/bits.orna");
const REFERENCE_STANDARD_STATS_SOURCE_V1: &str = include_str!("../../../stdlib/std/stats.orna");
const REFERENCE_STANDARD_MONEY_SOURCE_V1: &str = include_str!("../../../stdlib/std/money.orna");
const REFERENCE_STANDARD_TIME_SOURCE_V1: &str = include_str!("../../../stdlib/std/time.orna");
const REFERENCE_STANDARD_TIME_CALENDAR_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/calendar.orna");
const REFERENCE_STANDARD_STREAM_SOURCE_V1: &str = include_str!("../../../stdlib/std/stream.orna");
const REFERENCE_STANDARD_RANDOM_SOURCE_V1: &str = include_str!("../../../stdlib/std/random.orna");
const REFERENCE_STANDARD_HASH_SOURCE_V1: &str = include_str!("../../../stdlib/std/hash.orna");
const REFERENCE_STANDARD_ENCODING_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/encoding/main.orna");
const REFERENCE_STANDARD_ENCODING_ORNA_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/encoding/orna.orna");
const REFERENCE_STANDARD_ENCODING_OVB_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/encoding/ovb.orna");
const REFERENCE_STANDARD_ENCODING_JSON_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/encoding/json.orna");
const REFERENCE_STANDARD_ENCODING_BASE64_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/encoding/base64.orna");
const REFERENCE_STANDARD_NET_SOURCE_V1: &str = include_str!("../../../stdlib/std/net/main.orna");
const REFERENCE_STANDARD_URL_SOURCE_V1: &str = include_str!("../../../stdlib/std/url.orna");
const REFERENCE_STANDARD_NET_HTTP_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/net/http.orna");
const REFERENCE_STANDARD_NET_WEBSOCKET_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/net/websocket.orna");
const REFERENCE_STANDARD_TIME_COMPACT_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/duration/compact.orna");
const REFERENCE_STANDARD_TIME_CLOCK_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/duration/clock.orna");
const REFERENCE_STANDARD_TIME_WORDS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/duration/words.orna");
const REFERENCE_STANDARD_TIME_ISO_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/time/duration/iso.orna");
const REFERENCE_STANDARD_OPTION_SOURCE_V1: &str = include_str!("../../../stdlib/std/option.orna");
const REFERENCE_STANDARD_RESULT_SOURCE_V1: &str = include_str!("../../../stdlib/std/result.orna");
const REFERENCE_STANDARD_ERROR_SOURCE_V1: &str = include_str!("../../../stdlib/std/error.orna");
const REFERENCE_STANDARD_ERROR_COMBINATORS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/error/combinators.orna");
const REFERENCE_STANDARD_NUMERIC_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/numeric.orna");
const REFERENCE_STANDARD_LIST_SOURCE_V1: &str = include_str!("../../../stdlib/std/list.orna");
const REFERENCE_STANDARD_MAP_SOURCE_V1: &str = include_str!("../../../stdlib/std/map.orna");
const REFERENCE_STANDARD_SET_SOURCE_V1: &str = include_str!("../../../stdlib/std/set.orna");
const REFERENCE_STANDARD_IO_SOURCE_V1: &str = include_str!("../../../stdlib/std/io/main.orna");
const REFERENCE_STANDARD_FS_SOURCE_V1: &str = include_str!("../../../stdlib/std/io/fs.orna");
const REFERENCE_STANDARD_IO_PATH_MODULE_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/io/path.orna");
const REFERENCE_STANDARD_IO_METADATA_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/io/metadata.orna");
const REFERENCE_STANDARD_IO_PROCESS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/io/process.orna");
const REFERENCE_STANDARD_IO_ENVIRONMENT_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/io/environment.orna");
const REFERENCE_STANDARD_IO_READER_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/io/reader.orna");
const REFERENCE_STANDARD_IO_WRITER_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/io/writer.orna");
const REFERENCE_STANDARD_IO_BUFFER_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/io/buffer.orna");
const REFERENCE_STANDARD_CONCURRENT_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/concurrent/main.orna");
const REFERENCE_STANDARD_CONCURRENT_RESULT_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/concurrent/result.orna");
const REFERENCE_STANDARD_ITERATOR_ADAPTERS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/iterator/adapters.orna");
const REFERENCE_STANDARD_ITERATOR_CONSUMERS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/iterator/consumers.orna");
const REFERENCE_STANDARD_COLLECTION_ADAPTERS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/collection/adapters.orna");
const REFERENCE_STANDARD_LAZY_ADAPTERS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/lazy/adapters.orna");
const REFERENCE_STANDARD_STREAM_ADAPTERS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/stream/adapters.orna");
const REFERENCE_STANDARD_MEMO_SOURCE_V1: &str = include_str!("../../../stdlib/std/memo.orna");
const REFERENCE_STANDARD_REGEX_UTILITIES_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/regex/utilities.orna");
const REFERENCE_STANDARD_TEST_SOURCE_V1: &str = include_str!("../../../stdlib/std/test.orna");
const REFERENCE_STANDARD_GENERICS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/generics.orna");
const REFERENCE_STANDARD_TYPE_UTILS_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/type_utils.orna");
const REFERENCE_STANDARD_PATTERN_SOURCE_V1: &str = include_str!("../../../stdlib/std/pattern.orna");
const REFERENCE_STANDARD_REGEX_SOURCE_V1: &str = include_str!("../../../stdlib/std/regex.orna");
const REFERENCE_STANDARD_ITERATOR_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/iterator.orna");
const REFERENCE_STANDARD_LAZY_SOURCE_V1: &str = include_str!("../../../stdlib/std/lazy.orna");
const REFERENCE_STANDARD_VIEWS_SOURCE_V1: &str = include_str!("../../../stdlib/std/views.orna");
const REFERENCE_STANDARD_INTROSPECTION_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/introspection.orna");
const REFERENCE_STANDARD_REFLECTION_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/reflection.orna");
const REFERENCE_STANDARD_UI_SOURCE_V1: &str = include_str!("../../../stdlib/std/ui.orna");
const REFERENCE_STANDARD_FORMAT_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/format.orna");
const REFERENCE_STANDARD_PARSE_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/parse.orna");
const REFERENCE_STANDARD_PRELUDE_SOURCE_V1: &str =
    include_str!("../../../stdlib/std/prelude.orna");

/// Source units for the Orna 1.0.0 reference standard dependency.
///
/// This is the current source-backed standard boundary. The retained `orna.std/1`–
/// `orna.std/11` APIs below model older, explicitly versioned snapshots.
#[must_use]
pub fn reference_standard_sources_v1() -> [(String, String); 67] {
    let mut sources: [(String, String); 67] = [
        (
            REFERENCE_STANDARD_MATH_PATH_V1.into(),
            REFERENCE_STANDARD_MATH_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_COLLECTION_PATH_V1.into(),
            REFERENCE_STANDARD_COLLECTION_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_QUERY_PATH_V1.into(),
            REFERENCE_STANDARD_QUERY_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TEXT_PATH_V1.into(),
            REFERENCE_STANDARD_TEXT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_BITS_PATH_V1.into(),
            REFERENCE_STANDARD_BITS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_STATS_PATH_V1.into(),
            REFERENCE_STANDARD_STATS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_COMPACT_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_COMPACT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_CLOCK_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_CLOCK_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_WORDS_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_WORDS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_ISO_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_ISO_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_OPTION_PATH_V1.into(),
            REFERENCE_STANDARD_OPTION_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_RESULT_PATH_V1.into(),
            REFERENCE_STANDARD_RESULT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_LIST_PATH_V1.into(),
            REFERENCE_STANDARD_LIST_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_MAP_PATH_V1.into(),
            REFERENCE_STANDARD_MAP_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_SET_PATH_V1.into(),
            REFERENCE_STANDARD_SET_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_IO_PATH_V1.into(),
            REFERENCE_STANDARD_IO_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_FS_PATH_V1.into(),
            REFERENCE_STANDARD_FS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_CONCURRENT_PATH_V1.into(),
            REFERENCE_STANDARD_CONCURRENT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ERROR_PATH_V1.into(),
            REFERENCE_STANDARD_ERROR_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TIME_CALENDAR_PATH_V1.into(),
            REFERENCE_STANDARD_TIME_CALENDAR_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_STREAM_PATH_V1.into(),
            REFERENCE_STANDARD_STREAM_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_RANDOM_PATH_V1.into(),
            REFERENCE_STANDARD_RANDOM_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_HASH_PATH_V1.into(),
            REFERENCE_STANDARD_HASH_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ENCODING_PATH_V1.into(),
            REFERENCE_STANDARD_ENCODING_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ENCODING_ORNA_PATH_V1.into(),
            REFERENCE_STANDARD_ENCODING_ORNA_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ENCODING_OVB_PATH_V1.into(),
            REFERENCE_STANDARD_ENCODING_OVB_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ENCODING_JSON_PATH_V1.into(),
            REFERENCE_STANDARD_ENCODING_JSON_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ENCODING_BASE64_PATH_V1.into(),
            REFERENCE_STANDARD_ENCODING_BASE64_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_NET_PATH_V1.into(),
            REFERENCE_STANDARD_NET_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_URL_PATH_V1.into(),
            REFERENCE_STANDARD_URL_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_NET_HTTP_PATH_V1.into(),
            REFERENCE_STANDARD_NET_HTTP_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_NET_WEBSOCKET_PATH_V1.into(),
            REFERENCE_STANDARD_NET_WEBSOCKET_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TEST_PATH_V1.into(),
            REFERENCE_STANDARD_TEST_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_GENERICS_PATH_V1.into(),
            REFERENCE_STANDARD_GENERICS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TYPE_UTILS_PATH_V1.into(),
            REFERENCE_STANDARD_TYPE_UTILS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_PATTERN_PATH_V1.into(),
            REFERENCE_STANDARD_PATTERN_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ITERATOR_PATH_V1.into(),
            REFERENCE_STANDARD_ITERATOR_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_LAZY_PATH_V1.into(),
            REFERENCE_STANDARD_LAZY_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_VIEWS_PATH_V1.into(),
            REFERENCE_STANDARD_VIEWS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_INTROSPECTION_PATH_V1.into(),
            REFERENCE_STANDARD_INTROSPECTION_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_REFLECTION_PATH_V1.into(),
            REFERENCE_STANDARD_REFLECTION_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_IO_PATH_MODULE_PATH_V1.into(),
            REFERENCE_STANDARD_IO_PATH_MODULE_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_IO_METADATA_PATH_V1.into(),
            REFERENCE_STANDARD_IO_METADATA_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_IO_PROCESS_PATH_V1.into(),
            REFERENCE_STANDARD_IO_PROCESS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_IO_ENVIRONMENT_PATH_V1.into(),
            REFERENCE_STANDARD_IO_ENVIRONMENT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ALGORITHM_PATH_V1.into(),
            REFERENCE_STANDARD_ALGORITHM_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_REGEX_PATH_V1.into(),
            REFERENCE_STANDARD_REGEX_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_MONEY_PATH_V1.into(),
            REFERENCE_STANDARD_MONEY_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_UI_PATH_V1.into(),
            REFERENCE_STANDARD_UI_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_IO_READER_PATH_V1.into(),
            REFERENCE_STANDARD_IO_READER_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_IO_WRITER_PATH_V1.into(),
            REFERENCE_STANDARD_IO_WRITER_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_FORMAT_PATH_V1.into(),
            REFERENCE_STANDARD_FORMAT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_PARSE_PATH_V1.into(),
            REFERENCE_STANDARD_PARSE_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_PRELUDE_PATH_V1.into(),
            REFERENCE_STANDARD_PRELUDE_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_IO_BUFFER_PATH_V1.into(),
            REFERENCE_STANDARD_IO_BUFFER_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_CONCURRENT_RESULT_PATH_V1.into(),
            REFERENCE_STANDARD_CONCURRENT_RESULT_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ITERATOR_ADAPTERS_PATH_V1.into(),
            REFERENCE_STANDARD_ITERATOR_ADAPTERS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ERROR_COMBINATORS_PATH_V1.into(),
            REFERENCE_STANDARD_ERROR_COMBINATORS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_NUMERIC_PATH_V1.into(),
            REFERENCE_STANDARD_NUMERIC_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_TEXT_BUILDER_PATH_V1.into(),
            REFERENCE_STANDARD_TEXT_BUILDER_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_ITERATOR_CONSUMERS_PATH_V1.into(),
            REFERENCE_STANDARD_ITERATOR_CONSUMERS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_COLLECTION_ADAPTERS_PATH_V1.into(),
            REFERENCE_STANDARD_COLLECTION_ADAPTERS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_LAZY_ADAPTERS_PATH_V1.into(),
            REFERENCE_STANDARD_LAZY_ADAPTERS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_STREAM_ADAPTERS_PATH_V1.into(),
            REFERENCE_STANDARD_STREAM_ADAPTERS_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_MEMO_PATH_V1.into(),
            REFERENCE_STANDARD_MEMO_SOURCE_V1.into(),
        ),
        (
            REFERENCE_STANDARD_REGEX_UTILITIES_PATH_V1.into(),
            REFERENCE_STANDARD_REGEX_UTILITIES_SOURCE_V1.into(),
        ),
    ];
    sources[2]
        .1
        .push_str(include_str!("fixtures/query_recursive_cte_n0o0e.orna"));
    sources
}

/// Profile that pins the exact 1.0.0 reference-standard source bytes.
#[must_use]
pub fn reference_standard_profile_v1() -> StandardDependencyProfile {
    StandardDependencyProfile::from_sources(
        "orna.std/v1-reference-library",
        reference_standard_sources_v1(),
    )
    .expect("the bundled Orna 1.0.0 standard module path is valid")
    .with_module_prelude_exports(
        REFERENCE_STANDARD_PRELUDE_PATH_V1,
        REFERENCE_STANDARD_PRELUDE_EXPORTS_V1.iter().copied(),
    )
}

/// Builds the semantic catalogue from the pinned Orna 1.0.0 source, without
/// routing through the pre-1.0 SQL parser/compiler used by retained snapshots.
pub fn reference_standard_catalogue_v1() -> Result<StandardCatalogueV1, StandardCatalogueError> {
    StandardCatalogueV1::from_standard_sources(
        &reference_standard_profile_v1(),
        reference_standard_sources_v1(),
    )
}

/// The language version associated with this standard-library version.
pub const LANGUAGE_VERSION_IDENTITY: &str = "orna.language/1";

/// The logical path reserved for the retained standard-library source.
pub const SOURCE_LOGICAL_PATH: &str = "std/types.orna";

/// The stable identity of `orna.std/1`.
pub const STANDARD_LIBRARY_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(1));

/// The stable identity of the standard catalogue revision.
pub const STANDARD_CATALOGUE_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(1));

/// The stable identity reserved for the standard source bundle.
pub const STANDARD_SOURCE_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(1));

/// The stable identity reserved for the standard source revision.
pub const STANDARD_SOURCE_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(1));

/// The stable identity reserved for `std/types.orna`.
pub const STANDARD_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(1));

/// The stable identity of the `std` schema.
pub const STD_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(1));

/// The stable identity of the `std.types` schema.
pub const STD_TYPES_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(2));

/// The stable identity of `std.types.boolean`.
pub const BOOLEAN_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(1));

/// The stable identity of `std.types.integer`.
pub const INTEGER_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(2));

/// The stable identity of `std.types.bigint`.
pub const BIGINT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(3));

/// The stable identity of `std.types.float`.
pub const FLOAT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(4));

/// The stable identity of `std.types.decimal`.
pub const DECIMAL_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(5));

/// The stable identity of `std.types.character_large_object`.
pub const CHARACTER_LARGE_OBJECT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(6));

/// The stable identity of `std.types.binary_large_object`.
pub const BINARY_LARGE_OBJECT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(7));

/// The stable identity of `std.types.uuid`.
pub const UUID_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(8));

/// The stable identity of `std.types.date`.
pub const DATE_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(9));

/// The stable identity of `std.types.time`.
pub const TIME_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(10));

/// The stable identity of `std.types.timestamp`.
pub const TIMESTAMP_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(11));

/// The stable identity of `std.types.duration`.
pub const DURATION_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(12));

/// The stable identity of `std.types.void`.
pub const VOID_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(13));

/// The stable identity of `std.types.opaque_token`.
pub const OPAQUE_TOKEN_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(14));

/// All initial standard type identities in manifest order.
pub const STANDARD_TYPE_IDS: [TypeId; 14] = [
    BOOLEAN_TYPE_ID,
    INTEGER_TYPE_ID,
    BIGINT_TYPE_ID,
    FLOAT_TYPE_ID,
    DECIMAL_TYPE_ID,
    CHARACTER_LARGE_OBJECT_TYPE_ID,
    BINARY_LARGE_OBJECT_TYPE_ID,
    UUID_TYPE_ID,
    DATE_TYPE_ID,
    TIME_TYPE_ID,
    TIMESTAMP_TYPE_ID,
    DURATION_TYPE_ID,
    VOID_TYPE_ID,
    OPAQUE_TOKEN_TYPE_ID,
];

/// The standard-library version represented by the V2 manifest.
pub const STANDARD_LIBRARY_V2_VERSION_IDENTITY: &str = "orna.std/2";

/// The stable identity of `orna.std/2`.
pub const STANDARD_LIBRARY_V2_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(2));

/// The stable identity of the V2 standard catalogue revision.
pub const STANDARD_CATALOGUE_V2_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(2));

/// The stable identity reserved for the V2 standard source bundle.
pub const STANDARD_SOURCE_V2_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(2));

/// The stable identity reserved for the V2 standard source revision.
pub const STANDARD_SOURCE_V2_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(2));

/// The logical path reserved for the retained V2 invoke source unit.
pub const STD_INVOKE_SOURCE_LOGICAL_PATH: &str = "std/invoke.orna";

const fn reserved_id(final_byte: u8) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[15] = final_byte;
    bytes
}

/// The standard-library version represented by the V3 manifest (work ADR 0058).
pub const STANDARD_LIBRARY_V3_VERSION_IDENTITY: &str = "orna.std/3";

/// The stable identity of `orna.std/3`.
pub const STANDARD_LIBRARY_V3_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(3));

/// The stable identity of the V3 standard catalogue revision.
pub const STANDARD_CATALOGUE_V3_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(3));

/// The stable identity reserved for the V3 standard source bundle.
pub const STANDARD_SOURCE_V3_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(3));

/// The stable identity reserved for the V3 standard source revision.
pub const STANDARD_SOURCE_V3_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(3));

/// The logical path reserved for the retained V3 output source unit.
pub const STD_OUTPUT_SOURCE_LOGICAL_PATH: &str = "std/output.orna";

/// The stable identity of the retained `std/output.orna` unit in the V3 bundle.
pub const STD_OUTPUT_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(4));

/// The stable identity of the `std.terminal` schema.
pub const STD_TERMINAL_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(4));

/// The stable identity of the `std.io` schema.
pub const STD_IO_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(5));

/// The stable identity of `std.terminal.Document`.
pub const STD_TERMINAL_DOCUMENT_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(15));

/// The stable identity of `std.io.ByteStream`.
pub const STD_IO_BYTE_STREAM_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(16));

/// The kernel representation contract of `std.terminal.Document`.
pub const STD_TERMINAL_DOCUMENT_CONTRACT: &str = "orna.std.value.terminal-document@1";

/// The kernel representation contract of `std.io.ByteStream`.
pub const STD_IO_BYTE_STREAM_CONTRACT: &str = "orna.std.value.byte-stream@1";

/// The canonical ASCII magic prefix of a `std.terminal.Document` payload.
///
/// The canonical payload is exactly `ORNA-TERMINAL-DOCUMENT/1 ` followed by a
/// big-endian `u32` body length and the body bytes.
pub const TERMINAL_DOCUMENT_MAGIC: &str = "ORNA-TERMINAL-DOCUMENT/1 ";

/// The canonical ASCII magic prefix of a `std.io.ByteStream` payload.
///
/// The canonical payload is exactly `ORNA-BYTE-STREAM/1 ` followed by a
/// big-endian `u32` media-type length, the media type, a big-endian `u32`
/// body length, and the body bytes.
pub const BYTE_STREAM_MAGIC: &str = "ORNA-BYTE-STREAM/1 ";

/// The standard-library version represented by the V4 manifest (work ADR 0062).
pub const STANDARD_LIBRARY_V4_VERSION_IDENTITY: &str = "orna.std/4";

/// The stable identity of `orna.std/4`.
pub const STANDARD_LIBRARY_V4_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(4));

/// The stable identity of the V4 standard catalogue revision.
pub const STANDARD_CATALOGUE_V4_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(4));

/// The stable identity reserved for the V4 standard source bundle.
pub const STANDARD_SOURCE_V4_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(4));

/// The stable identity reserved for the V4 standard source revision.
pub const STANDARD_SOURCE_V4_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(4));

/// The logical path reserved for the retained V4 ui source unit.
pub const STD_UI_SOURCE_LOGICAL_PATH: &str = "std/ui.orna";

/// The stable identity of the retained `std/ui.orna` unit in the V4 bundle.
pub const STD_UI_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(5));

/// The stable identity of the `std.ui` schema.
pub const STD_UI_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(8));

/// The stable identity of `std.ui.UI`.
pub const STD_UI_TYPE_ID: TypeId = TypeId::from_bytes(reserved_id(19));

/// The kernel representation contract of `std.ui.UI`.
pub const STD_UI_CONTRACT: &str = "orna.std.value.ui@1";

/// The canonical ASCII magic prefix of a `std.ui.UI` payload.
///
/// The canonical payload is exactly `ORNA-UI/1 ` followed by a big-endian
/// `u32` body length and the body bytes (work ADR 0062 provisional frame).
pub const UI_MAGIC: &str = "ORNA-UI/1 ";

/// The standard-library version represented by the V5 manifest (ADR 0075).
pub const STANDARD_LIBRARY_V5_VERSION_IDENTITY: &str = "orna.std/5";
pub const STANDARD_LIBRARY_V5_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(5));
pub const STANDARD_CATALOGUE_V5_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(5));
pub const STANDARD_SOURCE_V5_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(5));
pub const STANDARD_SOURCE_V5_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(5));
pub const STD_JSON_SOURCE_LOGICAL_PATH: &str = "std/json.orna";
pub const STD_JSON_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(6));
pub const STD_JSON_CONTRACT: &str = "orna.std.value.json@1";
pub const JSON_MAGIC: &str = "ORNA-JSON-VALUE/1 ";

/// The standard-library version represented by the V6 manifest (ADR 0079).
pub const STANDARD_LIBRARY_V6_VERSION_IDENTITY: &str = "orna.std/6";
pub const STANDARD_LIBRARY_V6_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(6));
pub const STANDARD_CATALOGUE_V6_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(6));
pub const STANDARD_SOURCE_V6_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(6));
pub const STANDARD_SOURCE_V6_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(6));
pub const ACTION_MAGIC: &str = "ORNA-ACTION/1 ";

/// The standard-library version represented by the V7 manifest (ADR 0019).
pub const STANDARD_LIBRARY_V7_VERSION_IDENTITY: &str = "orna.std/7";
pub const STANDARD_LIBRARY_V7_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(7));
pub const STANDARD_CATALOGUE_V7_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(7));

/// The standard-library version represented by the V8 manifest (Work ADR 0087).
pub const STANDARD_LIBRARY_V8_VERSION_IDENTITY: &str = "orna.std/8";
pub const STANDARD_LIBRARY_V8_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(8));
pub const STANDARD_CATALOGUE_V8_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(8));
pub const STANDARD_SOURCE_V8_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(8));
pub const STANDARD_SOURCE_V8_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(8));
pub const STD_DATA_SOURCE_LOGICAL_PATH: &str = "std/data.orna";
pub const STD_DATA_ROWS_CONTRACT: &str = "orna.std.value.rows@1";
pub const STD_DATA_ROWS_SEMANTIC_NAME: &str = "std.data.rows";
pub const STD_DATA_ROWS_EXPORT_NAME: &str = "std.Rows";

/// The standard-library version represented by the V9 manifest (Work ADR 0088).
pub const STANDARD_LIBRARY_V9_VERSION_IDENTITY: &str = "orna.std/9";
pub const STANDARD_LIBRARY_V9_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(9));
pub const STANDARD_CATALOGUE_V9_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(9));
pub const STANDARD_SOURCE_V9_BUNDLE_ID: SourceBundleId = SourceBundleId::from_bytes(reserved_id(9));
pub const STANDARD_SOURCE_V9_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(9));

/// The standard-library version represented by the V10 manifest.
pub const STANDARD_LIBRARY_V10_VERSION_IDENTITY: &str = "orna.std/10";
pub const STANDARD_LIBRARY_V10_REVISION_ID: StandardLibraryRevisionId =
    StandardLibraryRevisionId::from_bytes(reserved_id(10));
pub const STANDARD_CATALOGUE_V10_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes(reserved_id(10));
pub const STANDARD_SOURCE_V10_BUNDLE_ID: SourceBundleId =
    SourceBundleId::from_bytes(reserved_id(10));
pub const STANDARD_SOURCE_V10_REVISION_ID: SourceRevisionId =
    SourceRevisionId::from_bytes(reserved_id(10));
pub const STD_CLI_SOURCE_LOGICAL_PATH: &str = "std/cli.orna";
pub const STD_CLI_SOURCE_UNIT_ID: SourceUnitId = SourceUnitId::from_bytes(reserved_id(11));
pub const STD_CLI_SCHEMA_ID: SchemaId = SchemaId::from_bytes(reserved_id(10));
pub const STD_CLI_REPL_FUNCTION_ID: FunctionId = FunctionId::from_bytes(reserved_id(0x1C));
pub const STD_CLI_REPL_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes(reserved_id(0x1C));
pub const STD_CLI_REPL_REVISION_NUMBER: u64 = 1;

const ACCEPTED_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x40, 0x0e, 0xb4, 0x35, 0x5d, 0xa2, 0x8f, 0x41, 0xf4, 0xd4, 0xae, 0x8c, 0x06, 0x21, 0x24, 0x89,
    0xbe, 0x60, 0xf6, 0xd8, 0x7c, 0x6d, 0x8e, 0xf3, 0x0c, 0x29, 0x1c, 0xc8, 0x3b, 0x2c, 0xfb, 0x6b,
]);
const ACCEPTED_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xbe, 0x61, 0x9c, 0xaa, 0xf6, 0xb2, 0x0b, 0xb7, 0xf8, 0xbc, 0x8d, 0xf9, 0x56, 0xd4, 0x89, 0xad,
    0xe4, 0x9b, 0xc8, 0xdf, 0xe0, 0x3c, 0xd6, 0xd9, 0x64, 0x70, 0x5b, 0x30, 0x23, 0x5b, 0x08, 0x1d,
]);
const ACCEPTED_V2_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x75, 0x5f, 0x9e, 0xfd, 0xb3, 0x39, 0xe7, 0x36, 0x9d, 0xa8, 0x75, 0x89, 0x42, 0x7e, 0x1c, 0x4a,
    0x0e, 0xae, 0x18, 0xbe, 0xe4, 0x53, 0x2b, 0x8e, 0x7d, 0x46, 0xbc, 0x9c, 0x79, 0x9e, 0x57, 0x89,
]);
const ACCEPTED_V2_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xb3, 0xb0, 0xf9, 0xb7, 0xed, 0x69, 0x1a, 0xaf, 0x03, 0x57, 0x9b, 0x20, 0x1c, 0xf3, 0xda, 0xc1,
    0xb7, 0x25, 0xba, 0xdf, 0x90, 0xb6, 0x91, 0x1a, 0x98, 0x23, 0xa3, 0x24, 0x91, 0x06, 0x73, 0xce,
]);
const ACCEPTED_V3_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x60, 0xb7, 0xba, 0xdc, 0x70, 0x1c, 0xf6, 0x2c, 0x2c, 0xd2, 0x83, 0xd3, 0xae, 0x5e, 0x5b, 0xc5,
    0x01, 0xc4, 0xff, 0x8f, 0x7b, 0x1d, 0x75, 0x7e, 0xa1, 0xdc, 0x0d, 0xf6, 0x48, 0xa2, 0x29, 0x44,
]);
const ACCEPTED_V3_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x9e, 0xf4, 0xcb, 0x13, 0xb7, 0x5e, 0xaf, 0x81, 0x40, 0x51, 0xd0, 0x37, 0x47, 0x9c, 0x34, 0x5c,
    0x0e, 0x3b, 0x1d, 0x4e, 0xe0, 0x70, 0x32, 0x3e, 0x36, 0x31, 0x59, 0xe2, 0x79, 0x2c, 0x7d, 0xcd,
]);
const ACCEPTED_V4_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xab, 0x6d, 0xba, 0x9d, 0xfc, 0x42, 0x35, 0x39, 0xc8, 0xea, 0x90, 0x55, 0xf5, 0xbf, 0x40, 0x6f,
    0x45, 0xb0, 0xd3, 0x36, 0x2c, 0x06, 0x35, 0x7e, 0x34, 0x13, 0x23, 0x88, 0xff, 0x51, 0x41, 0xdd,
]);
const ACCEPTED_V4_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xdc, 0xff, 0xa5, 0x23, 0x16, 0x43, 0xf4, 0x73, 0x29, 0xd3, 0x00, 0x34, 0x1f, 0xba, 0xa2, 0x4f,
    0x5a, 0xbf, 0xa6, 0xbc, 0xed, 0x77, 0x56, 0x5c, 0xd3, 0x82, 0x74, 0xce, 0xa4, 0x83, 0x8f, 0xb9,
]);
const ACCEPTED_V5_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x91, 0x2f, 0xb3, 0xb6, 0x6c, 0x28, 0x35, 0xb3, 0x68, 0x68, 0x68, 0x76, 0x75, 0x5d, 0x7c, 0x78,
    0x9c, 0xc3, 0xf2, 0x5c, 0x26, 0x87, 0x27, 0xb0, 0x83, 0xd9, 0x6e, 0x70, 0x7a, 0x99, 0xbc, 0x51,
]);
const ACCEPTED_V5_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x22, 0x60, 0x9b, 0xe8, 0xc6, 0x6a, 0xce, 0x4a, 0xbe, 0x37, 0x6b, 0x2d, 0xfa, 0x82, 0x07, 0xd1,
    0x9f, 0xe0, 0xae, 0xa9, 0x49, 0xde, 0x0b, 0xbc, 0xc8, 0xcf, 0x97, 0xbf, 0xec, 0xf8, 0xef, 0xed,
]);
const ACCEPTED_V10_CLI_CONTENT_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x1e, 0x99, 0xf3, 0x2f, 0xf7, 0xc2, 0xf0, 0x65, 0x4d, 0x67, 0x61, 0xa6, 0xa9, 0xce, 0xd8, 0x26,
    0x95, 0x6c, 0x09, 0x5d, 0xdf, 0xd1, 0x2e, 0xbb, 0xdc, 0xc1, 0x8e, 0xc4, 0xe9, 0xab, 0x19, 0xc4,
]);
const ACCEPTED_V10_SOURCE_REVISION_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xe9, 0x35, 0x47, 0x5a, 0x86, 0x4d, 0xaa, 0x61, 0x66, 0x01, 0x9a, 0xda, 0xf1, 0x59, 0x86, 0xc4,
    0xee, 0x43, 0x85, 0xcd, 0x8e, 0x1e, 0x64, 0x3e, 0x61, 0x3b, 0x55, 0x3a, 0xb0, 0x6e, 0x04, 0x5a,
]);
const ACCEPTED_V10_STANDARD_LIBRARY_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0x9b, 0x72, 0xf3, 0x38, 0x8b, 0x46, 0xe5, 0x28, 0x42, 0x5a, 0x84, 0x1c, 0x5b, 0x90, 0x61, 0x65,
    0xf7, 0x31, 0x61, 0xe9, 0x2c, 0x9f, 0x93, 0xe6, 0x01, 0x99, 0x81, 0x76, 0x10, 0x6d, 0xdb, 0xbd,
]);

#[derive(Clone, Copy)]
struct ValueTypeFact {
    id: TypeId,
    local_name: &'static str,
    representation_contract: &'static str,
    persistence: ValueTypePersistence,
    prelude_names: &'static [&'static [&'static str]],
}

const BOOLEAN_PRELUDE_NAMES: &[&[&str]] = &[&["BOOLEAN"], &["BOOL"]];
const INTEGER_PRELUDE_NAMES: &[&[&str]] = &[&["INTEGER"], &["INT"]];
const BIGINT_PRELUDE_NAMES: &[&[&str]] = &[&["BIGINT"]];
const FLOAT_PRELUDE_NAMES: &[&[&str]] = &[&["FLOAT"]];
const DECIMAL_PRELUDE_NAMES: &[&[&str]] = &[&["DECIMAL"]];
const CHARACTER_LARGE_OBJECT_PRELUDE_NAMES: &[&[&str]] =
    &[&["CHARACTER", "LARGE", "OBJECT"], &["TEXT"]];
const BINARY_LARGE_OBJECT_PRELUDE_NAMES: &[&[&str]] = &[&["BINARY", "LARGE", "OBJECT"], &["BYTES"]];
const UUID_PRELUDE_NAMES: &[&[&str]] = &[&["UUID"]];
const DATE_PRELUDE_NAMES: &[&[&str]] = &[&["DATE"]];
const TIME_PRELUDE_NAMES: &[&[&str]] = &[&["TIME"]];
const TIMESTAMP_PRELUDE_NAMES: &[&[&str]] = &[&["TIMESTAMP"]];
const DURATION_PRELUDE_NAMES: &[&[&str]] = &[&["DURATION"]];
const VOID_PRELUDE_NAMES: &[&[&str]] = &[&["VOID"]];

const VALUE_TYPE_FACTS: [ValueTypeFact; 13] = [
    ValueTypeFact {
        id: BOOLEAN_TYPE_ID,
        local_name: "boolean",
        representation_contract: "orna.kernel.value.boolean@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: BOOLEAN_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: INTEGER_TYPE_ID,
        local_name: "integer",
        representation_contract: "orna.kernel.value.integer@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: INTEGER_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: BIGINT_TYPE_ID,
        local_name: "bigint",
        representation_contract: "orna.kernel.value.bigint@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: BIGINT_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: FLOAT_TYPE_ID,
        local_name: "float",
        representation_contract: "orna.kernel.value.float@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: FLOAT_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: DECIMAL_TYPE_ID,
        local_name: "decimal",
        representation_contract: "orna.kernel.value.decimal@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: DECIMAL_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: CHARACTER_LARGE_OBJECT_TYPE_ID,
        local_name: "character_large_object",
        representation_contract: "orna.kernel.value.character-large-object@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: CHARACTER_LARGE_OBJECT_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: BINARY_LARGE_OBJECT_TYPE_ID,
        local_name: "binary_large_object",
        representation_contract: "orna.kernel.value.binary-large-object@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: BINARY_LARGE_OBJECT_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: UUID_TYPE_ID,
        local_name: "uuid",
        representation_contract: "orna.kernel.value.uuid@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: UUID_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: DATE_TYPE_ID,
        local_name: "date",
        representation_contract: "orna.kernel.value.date@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: DATE_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: TIME_TYPE_ID,
        local_name: "time",
        representation_contract: "orna.kernel.value.time@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: TIME_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: TIMESTAMP_TYPE_ID,
        local_name: "timestamp",
        representation_contract: "orna.kernel.value.timestamp@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: TIMESTAMP_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: DURATION_TYPE_ID,
        local_name: "duration",
        representation_contract: "orna.kernel.value.duration@1",
        persistence: ValueTypePersistence::Persistable,
        prelude_names: DURATION_PRELUDE_NAMES,
    },
    ValueTypeFact {
        id: VOID_TYPE_ID,
        local_name: "void",
        representation_contract: "orna.kernel.value.void@1",
        persistence: ValueTypePersistence::Transient,
        prelude_names: VOID_PRELUDE_NAMES,
    },
];

const OPAQUE_TOKEN_LOCAL_NAME: &str = "opaque_token";
const OPAQUE_TOKEN_CONTRACT: &str = "orna.std.value.opaque-token@1";

// This order is part of the accepted manifest: each value type's qualified
// binding comes first, followed by that type's prelude bindings.
const EXPECTED_TYPE_BINDING_IDS: [[u8; 16]; 31] = [
    [
        0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd, 0x4d,
        0x31,
    ],
    [
        0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8, 0x15,
        0xaa,
    ],
    [
        0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61, 0x11, 0x0c,
        0xad,
    ],
    [
        0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2, 0xdf, 0x25,
        0x93,
    ],
    [
        0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf, 0x18, 0x57,
        0x78,
    ],
    [
        0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e, 0x7c, 0x58,
        0x89,
    ],
    [
        0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed, 0x5b, 0x0d,
        0x1e,
    ],
    [
        0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b, 0xaa, 0x5e,
        0x88,
    ],
    [
        0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71, 0x9e, 0xfc,
        0xb5,
    ],
    [
        0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87, 0x97, 0xb5,
        0xe6,
    ],
    [
        0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8, 0x6b, 0x45,
        0xf9,
    ],
    [
        0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3, 0x10, 0x6e,
        0xd5,
    ],
    [
        0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b, 0xc2, 0xd8,
        0x70,
    ],
    [
        0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6, 0x89,
        0x1a,
    ],
    [
        0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99, 0xf0, 0xdb,
        0xc2,
    ],
    [
        0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c, 0x7d, 0x81,
        0x34,
    ],
    [
        0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d, 0x33, 0xaf,
        0xbf,
    ],
    [
        0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51, 0xb6, 0x9d,
        0xb7,
    ],
    [
        0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f, 0x31, 0x66,
        0x00,
    ],
    [
        0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6, 0x7c, 0x79,
        0x7f,
    ],
    [
        0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1, 0x71, 0xf7,
        0xac,
    ],
    [
        0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e, 0x1f, 0x93,
        0x56,
    ],
    [
        0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8, 0xa0, 0x34,
        0x8d,
    ],
    [
        0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72, 0xd2, 0x33,
        0xd4,
    ],
    [
        0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30, 0xfc, 0xff,
        0x04,
    ],
    [
        0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97, 0x9c, 0x8a,
        0xa1,
    ],
    [
        0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb, 0x62, 0x14,
        0x9a,
    ],
    [
        0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80, 0x2d, 0xdc,
        0x37,
    ],
    [
        0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc, 0xbf, 0x94,
        0x0f,
    ],
    [
        0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a, 0x01, 0x73,
        0xfb,
    ],
    [
        0x4d, 0xab, 0x42, 0x83, 0x03, 0x1f, 0xcd, 0x81, 0xb5, 0x8d, 0x09, 0xd8, 0x87, 0x63, 0x46,
        0xae,
    ],
];

/// The source-independent facts required to recognise the initial standard library.
///
/// This value does not contain standard source, origins, hashes, a digest, or
/// authority to install or use a standard-library snapshot.
#[derive(Clone, Debug)]
pub struct StandardLibraryManifest {
    catalogue: CatalogueSnapshot,
}

impl StandardLibraryManifest {
    /// Returns the standard-library version label.
    pub const fn standard_library_version(&self) -> &'static str {
        STANDARD_LIBRARY_VERSION_IDENTITY
    }

    /// Returns the standard-library revision identity.
    pub const fn standard_library_revision(&self) -> StandardLibraryRevisionId {
        STANDARD_LIBRARY_REVISION_ID
    }

    /// Returns the associated language version label.
    pub const fn language_version(&self) -> &'static str {
        LANGUAGE_VERSION_IDENTITY
    }

    /// Returns the identity reserved for the later retained source bundle.
    pub const fn source_bundle(&self) -> SourceBundleId {
        STANDARD_SOURCE_BUNDLE_ID
    }

    /// Returns the identity reserved for the later retained source revision.
    pub const fn source_revision(&self) -> SourceRevisionId {
        STANDARD_SOURCE_REVISION_ID
    }

    /// Returns the identity reserved for the later retained source unit.
    pub const fn source_unit(&self) -> SourceUnitId {
        STANDARD_SOURCE_UNIT_ID
    }

    /// Returns the logical path reserved for the later retained source unit.
    pub const fn source_logical_path(&self) -> &'static str {
        SOURCE_LOGICAL_PATH
    }

    /// Returns the validated source-independent standard catalogue.
    pub const fn catalogue(&self) -> &CatalogueSnapshot {
        &self.catalogue
    }
}

/// Builds and validates the accepted source-independent standard manifest.
///
/// This is a `Result` boundary because the core catalogue validates the
/// manifest facts. A failure means that the compiled manifest and the core
/// catalogue contract do not agree.
pub fn standard_library_manifest() -> Result<StandardLibraryManifest, StandardLibraryManifestError>
{
    let schemas = vec![
        SchemaDefinition::new(STD_SCHEMA_ID, semantic_name("std", ["std"])?),
        SchemaDefinition::new(
            STD_TYPES_SCHEMA_ID,
            semantic_name("std.types", ["std", "types"])?,
        ),
    ];
    let mut value_types = Vec::with_capacity(VALUE_TYPE_FACTS.len() + 1);
    for fact in VALUE_TYPE_FACTS {
        value_types.push(ValueTypeDefinition::primitive(
            fact.id,
            semantic_name(
                format!("std.types.{}", fact.local_name),
                ["std", "types", fact.local_name],
            )?,
            ValueTypeMutability::Immutable,
            fact.persistence,
            fact.representation_contract,
        ));
    }
    value_types.push(ValueTypeDefinition::opaque(
        OPAQUE_TOKEN_TYPE_ID,
        semantic_name(
            "std.types.opaque_token",
            ["std", "types", OPAQUE_TOKEN_LOCAL_NAME],
        )?,
        OPAQUE_TOKEN_CONTRACT,
    ));
    let type_bindings = build_type_bindings(&EXPECTED_TYPE_BINDING_IDS)?;
    let catalogue = CatalogueSnapshot::new_with_types(
        STANDARD_CATALOGUE_REVISION_ID,
        schemas,
        Vec::new(),
        value_types,
        type_bindings,
    )
    .map_err(|source| StandardLibraryManifestError::Catalogue { source })?;

    Ok(StandardLibraryManifest { catalogue })
}

fn build_type_bindings(
    expected_ids: &[[u8; 16]],
) -> Result<Vec<TypeBinding>, StandardLibraryManifestError> {
    let mut bindings = Vec::with_capacity(EXPECTED_TYPE_BINDING_IDS.len());
    for fact in VALUE_TYPE_FACTS {
        let qualified_name =
            semantic_name(format!("std.{}", fact.local_name), ["std", fact.local_name])?;
        let qualified_lookup = TypeLookupName::qualified(qualified_name.clone());
        let binding = TypeBinding::qualified(qualified_name, fact.id).map_err(|source| {
            StandardLibraryManifestError::TypeBinding {
                name: qualified_lookup,
                source,
            }
        })?;
        bindings.push(binding);

        for words in fact.prelude_names {
            let prelude_name = PreludeTypeName::new(words.iter().copied()).map_err(|source| {
                StandardLibraryManifestError::PreludeName {
                    name: words.join(" "),
                    source,
                }
            })?;
            let prelude_lookup = TypeLookupName::prelude(prelude_name.clone());
            let binding = TypeBinding::prelude(prelude_name, fact.id).map_err(|source| {
                StandardLibraryManifestError::TypeBinding {
                    name: prelude_lookup,
                    source,
                }
            })?;
            bindings.push(binding);
        }
    }

    let qualified_name = semantic_name("std.opaque_token", ["std", OPAQUE_TOKEN_LOCAL_NAME])?;
    let qualified_lookup = TypeLookupName::qualified(qualified_name.clone());
    let binding =
        TypeBinding::qualified(qualified_name, OPAQUE_TOKEN_TYPE_ID).map_err(|source| {
            StandardLibraryManifestError::TypeBinding {
                name: qualified_lookup,
                source,
            }
        })?;
    bindings.push(binding);

    validate_binding_identities(&bindings, expected_ids)?;
    Ok(bindings)
}

fn validate_binding_identities(
    bindings: &[TypeBinding],
    expected_ids: &[[u8; 16]],
) -> Result<(), StandardLibraryManifestError> {
    if bindings.len() != expected_ids.len() {
        return Err(StandardLibraryManifestError::TypeBindingCountMismatch {
            expected: expected_ids.len(),
            actual: bindings.len(),
        });
    }

    for (binding, expected_bytes) in bindings.iter().zip(expected_ids) {
        let expected = TypeBindingId::from_bytes(*expected_bytes);
        let actual = binding.id();
        if actual != expected {
            return Err(StandardLibraryManifestError::TypeBindingIdentityMismatch {
                name: binding.name().clone(),
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn semantic_name<const N: usize>(
    name: impl Into<String>,
    parts: [&'static str; N],
) -> Result<QualifiedSemanticName, StandardLibraryManifestError> {
    QualifiedSemanticName::new(parts).map_err(|source| StandardLibraryManifestError::SemanticName {
        name: name.into(),
        source,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StandardLibraryManifestError {
    /// One accepted qualified name is not valid under the core name contract.
    SemanticName {
        /// The accepted manifest spelling.
        name: String,
        /// The core validation error.
        source: SemanticNameError,
    },
    /// One accepted standard-prelude spelling is not a valid keyword name.
    PreludeName {
        /// The accepted keyword spelling.
        name: String,
        /// The core validation error.
        source: PreludeTypeNameError,
    },
    /// One accepted binding cannot be constructed under the core binding contract.
    TypeBinding {
        /// The accepted binding name.
        name: TypeLookupName,
        /// The core validation error.
        source: TypeBindingError,
    },
    /// A derived binding identity does not match the accepted manifest identity.
    TypeBindingIdentityMismatch {
        /// The accepted binding name.
        name: TypeLookupName,
        /// The hard-coded accepted identity.
        expected: TypeBindingId,
        /// The identity derived by the core binding contract.
        actual: TypeBindingId,
    },
    /// The compiled binding facts and identity table have different lengths.
    TypeBindingCountMismatch {
        /// The number of hard-coded accepted identities.
        expected: usize,
        /// The number of binding facts.
        actual: usize,
    },
    /// The accepted facts cannot form a coherent catalogue snapshot.
    Catalogue {
        /// The core catalogue validation error.
        source: CatalogueSnapshotError,
    },
}

impl fmt::Display for StandardLibraryManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SemanticName { name, source } => {
                write!(
                    formatter,
                    "the standard library manifest contains an invalid semantic name {name}: {source}"
                )
            }
            Self::PreludeName { name, source } => {
                write!(
                    formatter,
                    "the standard library manifest contains an invalid prelude name {name}: {source}"
                )
            }
            Self::TypeBinding { name, source } => {
                write!(
                    formatter,
                    "the standard library manifest contains an invalid type binding {name}: {source}"
                )
            }
            Self::TypeBindingIdentityMismatch {
                name,
                expected,
                actual,
            } => write!(
                formatter,
                "standard library type binding {name} has identity {actual}, expected {expected}"
            ),
            Self::TypeBindingCountMismatch { expected, actual } => write!(
                formatter,
                "the standard library manifest has {actual} type bindings, expected {expected}"
            ),
            Self::Catalogue { source } => write!(
                formatter,
                "the standard library manifest cannot form a catalogue: {source}"
            ),
        }
    }
}

impl Error for StandardLibraryManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SemanticName { source, .. } => Some(source),
            Self::PreludeName { source, .. } => Some(source),
            Self::TypeBinding { source, .. } => Some(source),
            Self::TypeBindingIdentityMismatch { .. } | Self::TypeBindingCountMismatch { .. } => {
                None
            }
            Self::Catalogue { source } => Some(source),
        }
    }
}
