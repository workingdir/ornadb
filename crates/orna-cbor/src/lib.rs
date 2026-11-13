//! Canonical CBOR for OrnaDB: shortest-form definite heads, a bounded reader
//! that builds each caller's own data model through [`Build`], and a
//! deterministic encoder over [`Encode`] views.
//!
//! Each caller supplies its own model and its own limits; the byte rules live
//! here once.

mod head;
mod read;
mod write;

pub use head::head;
pub use read::Reader;
pub use write::{encode, encode_into};

/// The canonical quiet NaN. Every NaN is written as this bit pattern, and the
/// reader accepts no other NaN.
pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

/// Whether `bits` is an IEEE-754 binary64 NaN, whatever its sign or payload.
pub fn is_nan_bits(bits: u64) -> bool {
    bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0
}

/// Why a byte string is not canonical CBOR in this profile, or why a limit
/// stopped the reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The input ended inside an item.
    Truncated,
    /// Well formed, but not the canonical form: a non-shortest head, a NaN
    /// other than [`CANONICAL_NAN_BITS`], or a bignum payload with a leading
    /// zero or fewer than nine bytes.
    NonCanonical,
    /// A major type, simple value or indefinite length this profile does not
    /// model.
    Unsupported,
    /// A text string is not UTF-8.
    InvalidUtf8,
    /// Map keys are not strictly increasing in encoded order, or two keys
    /// encode alike.
    DuplicateOrUnorderedMapKey,
    /// Nesting depth or the item count exceeded the reader's [`Limits`].
    Limit,
    /// A bignum tag (2 or 3) carried a payload that is not a byte string. The
    /// reader has already consumed the whole payload when it reports this.
    BignumPayload,
}

/// The bounds one [`Reader`] enforces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// The deepest nesting level an item may reach.
    pub max_depth: usize,
    /// The most items one reader may consume.
    pub max_items: usize,
}

/// A data model the [`Reader`] builds directly from bytes.
pub trait Build {
    /// The model's value type.
    type Value;

    fn null() -> Self::Value;
    fn boolean(value: bool) -> Self::Value;
    /// The integer `value`.
    fn unsigned(value: u64) -> Self::Value;
    /// The integer `-1 - magnitude`.
    fn negative(magnitude: u64) -> Self::Value;
    /// The integer `magnitude`, negated and reduced by one when `negative`
    /// (tag 2 or 3). The magnitude is big-endian with no leading zero and at
    /// least nine bytes.
    fn bignum(negative: bool, magnitude: &[u8]) -> Self::Value;
    /// The IEEE-754 binary64 value with `bits`. Only [`CANONICAL_NAN_BITS`]
    /// reaches this for a NaN.
    fn float(bits: u64) -> Self::Value;
    fn bytes(value: &[u8]) -> Self::Value;
    fn text(value: &str) -> Self::Value;
    fn array(values: Vec<Self::Value>) -> Self::Value;
    fn map(entries: Vec<(Self::Value, Self::Value)>) -> Self::Value;
    /// A tag other than the bignum tags 2 and 3.
    fn tag(number: u64, value: Self::Value) -> Self::Value;
}

/// One data item's shape, as the encoder sees it. Children are borrowed from
/// the model, so a view allocates only for a bignum magnitude.
pub enum View<'a, T> {
    Null,
    Boolean(bool),
    Unsigned(u64),
    Negative(u64),
    Bignum { negative: bool, magnitude: Vec<u8> },
    Float(u64),
    Bytes(&'a [u8]),
    Text(&'a str),
    Array(&'a [T]),
    Map(&'a [(T, T)]),
    Tag(u64, &'a T),
}

/// A data model the encoder can read.
pub trait Encode: Sized {
    fn view(&self) -> View<'_, Self>;
}
