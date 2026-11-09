//! The intrinsic identity enum.
//!
//! Declaration order is the catalogue order: `IntrinsicId::ALL[i]` is the id of
//! `catalogue()[i]`, and `as_u16()` equals `i`. `tests/catalogue_is_total.rs`
//! proves both properties, so a new id without a descriptor (or the reverse)
//! cannot compile into a passing build.

macro_rules! intrinsic_ids {
    ($($variant:ident),+ $(,)?) => {
        /// One identity per native operation in the catalogue.
        ///
        /// The enum is `#[repr(u16)]` so an admitted call can carry the id in a
        /// compact field and dispatch on it without comparing strings.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(u16)]
        pub enum IntrinsicId {
            $($variant),+
        }

        impl IntrinsicId {
            /// Every id, in declaration order.
            pub const ALL: &'static [IntrinsicId] = &[$(IntrinsicId::$variant),+];

            /// The stable numeric identity.
            pub const fn as_u16(self) -> u16 {
                self as u16
            }

            /// The symbolic name, for diagnostics and the JSON export.
            pub const fn name(self) -> &'static str {
                match self {
                    $(IntrinsicId::$variant => stringify!($variant)),+
                }
            }
        }
    };
}

intrinsic_ids! {
    // The root intrinsic environment (spec 9:5-7): no import, no std snapshot.
    CoreSome,
    CoreNow,
    CoreUuid7,
    CoreError,
    CoreFail,
    // The core relation operations. `std.collection` and `std.query` re-export
    // the same identity (spec 9:9), so one descriptor carries both spellings.
    CoreFilter,
    CoreMap,
    CoreFlatMap,
    CoreSortBy,
    CoreTake,
    CoreDrop,
    CoreDistinct,
    CoreUnion,
    CoreCount,
    CoreFirst,
    CoreLast,
    CoreOne,
    CoreSum,
    CoreMin,
    CoreMax,
    CoreEvery,
    CoreExists,
    // std.collection and std.query list helpers.
    CollectionChunk,
    CollectionFlatten,
    CollectionPartition,
    CollectionZip,
    CollectionZipExact,
    CollectionUnique,
    CollectionPairs,
    CollectionWindow,
    CollectionSplitWhen,
    CollectionRank,
    CollectionGroupBy,
    CollectionBucketBy,
    CollectionAsofJoin,
    CollectionRecursiveCte,
    CollectionStrictlyOrderedTimeSeries,
    CollectionWindowRate,
    CollectionWindowRateIntegrate,
    CollectionWindowDerivative,
    CollectionWindowIntegrate,
    // Historical-pin-only leaves.
    CollectionListLength,
    CollectionListConcat,
    CollectionNumericSum,
    CollectionStableSort,
    CollectionMinimum,
    CollectionMaximum,
    // std.math.
    MathIncrement,
    MathDecrement,
    MathIsZero,
    MathMin,
    MathMax,
    MathClamp,
    // std.text.
    TextTrim,
    TextSplit,
    TextJoin,
    TextStartsWith,
    TextEndsWith,
    TextContains,
    TextReplace,
    TextNormalise,
    TextLower,
    TextUpper,
    // std.bits.
    BitsBitOr,
    BitsBitAnd,
    BitsBitXor,
    BitsBitNot,
    BitsShiftLeft,
    BitsShiftRight,
    // std.stream.
    StreamFromList,
    StreamFromProvider,
    StreamForEach,
    StreamBatch,
    StreamBuffer,
    StreamMerge,
    StreamThrottle,
    StreamDebounce,
    StreamRetry,
    StreamRecover,
    // std.concurrent.
    ConcurrentParallel,
    ConcurrentRace,
    ConcurrentTimeout,
    // std.ui.
    UiNode,
    // std.stats.
    StatsMean,
    StatsMedian,
    StatsPercentile,
    StatsSum,
    StatsMin,
    StatsMax,
    StatsRange,
    StatsMode,
    StatsVariance,
    StatsStandardDeviation,
    StatsHistogram,
    StatsRate,
    StatsDerivative,
    StatsIntegrate,
    // std.time.
    TimeOffsetAt,
    TimeResolveLocal,
    TimeDurationCompactFormat,
    TimeDurationClockFormat,
    TimeDurationWordsFormat,
    TimeDurationIsoFormat,
    // std.hash.
    HashSha256,
    HashSha256Text,
    HashDomainSha256,
    HashToHex,
    HashFromHex,
    // std.random.
    RandomBytes,
    RandomInteger,
    RandomChoose,
    RandomShuffle,
    // Codecs.
    Base64Encode,
    Base64Decode,
    JsonEncode,
    JsonDecode,
    JsonDecodeWithOptions,
    OrnaCodecEncode,
    OrnaCodecDecode,
    OvbCodecEncode,
    OvbCodecDecode,
    // std.money.
    MoneyQuantize,
    MoneyAllocate,
    MoneyFormat,
}
