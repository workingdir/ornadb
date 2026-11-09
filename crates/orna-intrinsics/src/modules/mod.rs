//! The intrinsic catalogue and its per-module descriptor files.
//!
//! Each `std` module owns one plain `const` array in its own file. There is no
//! macro-generated table: a descriptor is an ordinary struct literal, so a
//! reviewer can compare it directly with the pinned declaration it describes.

use std::sync::LazyLock;

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::pin::PinRange;
use crate::signature::Signature;

pub(crate) mod base64;
pub(crate) mod bits;
pub(crate) mod collection;
pub(crate) mod concurrent;
pub(crate) mod core;
pub(crate) mod hash;
pub(crate) mod json;
pub(crate) mod math;
pub(crate) mod money;
pub(crate) mod orna_codec;
pub(crate) mod ovb_codec;
pub(crate) mod random;
pub(crate) mod retired;
pub(crate) mod stats;
pub(crate) mod stream;
pub(crate) mod text;
pub(crate) mod time;
pub(crate) mod ui;

/// The `std` module an intrinsic belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Module {
    Core,
    Collection,
    Query,
    Math,
    Text,
    Bits,
    Stream,
    Concurrent,
    Ui,
    Stats,
    Time,
    Hash,
    Random,
    Base64,
    Json,
    OrnaCodec,
    OvbCodec,
    Money,
}

impl Module {
    /// The pinned source module path, or the empty string for the root
    /// intrinsic environment.
    pub const fn path(self) -> &'static str {
        match self {
            Module::Core => "",
            Module::Collection => "std.collection",
            Module::Query => "std.query",
            Module::Math => "std.math",
            Module::Text => "std.text",
            Module::Bits => "std.bits",
            Module::Stream => "std.stream",
            Module::Concurrent => "std.concurrent",
            Module::Ui => "std.ui",
            Module::Stats => "std.stats",
            Module::Time => "std.time",
            Module::Hash => "std.hash",
            Module::Random => "std.random",
            Module::Base64 => "std.encoding.base64",
            Module::Json => "std.encoding.json",
            Module::OrnaCodec => "std.encoding.orna",
            Module::OvbCodec => "std.encoding.ovb",
            Module::Money => "std.money",
        }
    }

    /// The stable export name; `core` stands in for the empty root path.
    pub const fn name(self) -> &'static str {
        match self {
            Module::Core => "core",
            Module::Collection => "std.collection",
            Module::Query => "std.query",
            Module::Math => "std.math",
            Module::Text => "std.text",
            Module::Bits => "std.bits",
            Module::Stream => "std.stream",
            Module::Concurrent => "std.concurrent",
            Module::Ui => "std.ui",
            Module::Stats => "std.stats",
            Module::Time => "std.time",
            Module::Hash => "std.hash",
            Module::Random => "std.random",
            Module::Base64 => "std.encoding.base64",
            Module::Json => "std.encoding.json",
            Module::OrnaCodec => "std.encoding.orna",
            Module::OvbCodec => "std.encoding.ovb",
            Module::Money => "std.money",
        }
    }
}

/// One native operation's complete descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Intrinsic {
    pub id: IntrinsicId,
    pub module: Module,
    /// Name inside the module, as the pinned source spells the public export.
    pub name: &'static str,
    /// Private marker leaf the registry binds, when the current table uses one.
    pub leaf: Option<&'static str>,
    pub signature: Signature,
    pub effect: Effect,
    pub dispatch: Dispatch,
    pub justification: Justification,
    /// One sentence, emitted into `std_intrinsics.json` and the design docs.
    pub note: &'static str,
    /// Pins in which this binding is active (spec 9:11, ORNA-LIB-001).
    pub pins: PinRange,
}

/// One spelling exported by a second module under an existing identity.
///
/// `std.query` and `std.stream` may re-export intrinsic operators with identical
/// identities and behaviour (`spec 09:9`). The table currently spells the
/// `std.query` copies separately, so the catalogue records the mapping instead
/// of duplicating the descriptor.
pub type Reexport = (Module, &'static str, IntrinsicId);

/// The catalogue, in declaration order, built once from the per-module arrays.
pub fn catalogue() -> &'static [Intrinsic] {
    static CATALOGUE: LazyLock<Vec<Intrinsic>> = LazyLock::new(|| {
        let mut all = Vec::with_capacity(IntrinsicId::ALL.len());
        all.extend_from_slice(core::CORE);
        all.extend_from_slice(collection::COLLECTION);
        all.extend_from_slice(retired::RETIRED);
        all.extend_from_slice(math::MATH);
        all.extend_from_slice(text::TEXT);
        all.extend_from_slice(bits::BITS);
        all.extend_from_slice(stream::STREAM);
        all.extend_from_slice(concurrent::CONCURRENT);
        all.extend_from_slice(ui::UI);
        all.extend_from_slice(stats::STATS);
        all.extend_from_slice(time::TIME);
        all.extend_from_slice(hash::HASH);
        all.extend_from_slice(random::RANDOM);
        all.extend_from_slice(base64::BASE64);
        all.extend_from_slice(json::JSON);
        all.extend_from_slice(orna_codec::ORNA_CODEC);
        all.extend_from_slice(ovb_codec::OVB_CODEC);
        all.extend_from_slice(money::MONEY);
        all
    });
    CATALOGUE.as_slice()
}

/// The `std.query` spellings that share an identity with `std.collection`.
pub const REEXPORTS: &[Reexport] = &[
    (Module::Query, "chunk", IntrinsicId::CollectionChunk),
    (Module::Query, "flatten", IntrinsicId::CollectionFlatten),
    (Module::Query, "filter", IntrinsicId::CoreFilter),
    (Module::Query, "map", IntrinsicId::CoreMap),
    (Module::Query, "flat_map", IntrinsicId::CoreFlatMap),
    (Module::Query, "sort_by", IntrinsicId::CoreSortBy),
    (Module::Query, "rank", IntrinsicId::CollectionRank),
    (Module::Query, "take", IntrinsicId::CoreTake),
    (Module::Query, "drop", IntrinsicId::CoreDrop),
    (Module::Query, "distinct", IntrinsicId::CoreDistinct),
    (Module::Query, "unique", IntrinsicId::CollectionUnique),
    (Module::Query, "union", IntrinsicId::CoreUnion),
    (Module::Query, "count", IntrinsicId::CoreCount),
    (Module::Query, "first", IntrinsicId::CoreFirst),
    (Module::Query, "one", IntrinsicId::CoreOne),
    (Module::Query, "sum", IntrinsicId::CoreSum),
    (Module::Query, "min", IntrinsicId::CoreMin),
    (Module::Query, "max", IntrinsicId::CoreMax),
    (Module::Query, "every", IntrinsicId::CoreEvery),
    (Module::Query, "exists", IntrinsicId::CoreExists),
    (Module::Query, "partition", IntrinsicId::CollectionPartition),
    (Module::Query, "zip", IntrinsicId::CollectionZip),
    (Module::Query, "zip_exact", IntrinsicId::CollectionZipExact),
    (Module::Query, "group_by", IntrinsicId::CollectionGroupBy),
    (Module::Query, "pairs", IntrinsicId::CollectionPairs),
    (Module::Query, "window", IntrinsicId::CollectionWindow),
    (
        Module::Query,
        "split_when",
        IntrinsicId::CollectionSplitWhen,
    ),
    (Module::Query, "bucket_by", IntrinsicId::CollectionBucketBy),
    (Module::Query, "asof_join", IntrinsicId::CollectionAsofJoin),
    (
        Module::Query,
        "__list_length",
        IntrinsicId::CollectionListLength,
    ),
    (
        Module::Query,
        "__list_concat",
        IntrinsicId::CollectionListConcat,
    ),
    (
        Module::Query,
        "__numeric_sum",
        IntrinsicId::CollectionNumericSum,
    ),
    (
        Module::Query,
        "__stable_sort",
        IntrinsicId::CollectionStableSort,
    ),
    (Module::Query, "__minimum", IntrinsicId::CollectionMinimum),
    (Module::Query, "__maximum", IntrinsicId::CollectionMaximum),
    (Module::Query, "__group_by", IntrinsicId::CollectionGroupBy),
    (Module::Query, "__rank", IntrinsicId::CollectionRank),
    (
        Module::Query,
        "__asof_join",
        IntrinsicId::CollectionAsofJoin,
    ),
    (
        Module::Query,
        "__bucket_by",
        IntrinsicId::CollectionBucketBy,
    ),
];

/// The root names the language supplies without an import (`spec 09:5-7`).
///
/// Only `Core*` identities appear here. `std.collection`/`std.query` helpers
/// that also work at the root are reached through their admitted declarations,
/// not this environment.
pub const ROOT_NAMES: &[(&str, IntrinsicId)] = &[
    ("Some", IntrinsicId::CoreSome),
    ("now", IntrinsicId::CoreNow),
    ("uuid7", IntrinsicId::CoreUuid7),
    ("error", IntrinsicId::CoreError),
    ("fail", IntrinsicId::CoreFail),
    ("filter", IntrinsicId::CoreFilter),
    ("map", IntrinsicId::CoreMap),
    ("flat_map", IntrinsicId::CoreFlatMap),
    ("sort_by", IntrinsicId::CoreSortBy),
    ("take", IntrinsicId::CoreTake),
    ("drop", IntrinsicId::CoreDrop),
    ("distinct", IntrinsicId::CoreDistinct),
    ("union", IntrinsicId::CoreUnion),
    ("count", IntrinsicId::CoreCount),
    ("first", IntrinsicId::CoreFirst),
    ("last", IntrinsicId::CoreLast),
    ("one", IntrinsicId::CoreOne),
    ("sum", IntrinsicId::CoreSum),
    ("min", IntrinsicId::CoreMin),
    ("max", IntrinsicId::CoreMax),
    ("every", IntrinsicId::CoreEvery),
    ("exists", IntrinsicId::CoreExists),
];
