//! Runtime values of the executor.
//!
//! Only the shapes this stage runs are here. Each one encodes to the same
//! canonical bytes the old evaluator produces (`orna_value::Raw`), so results
//! compare byte-for-byte in the differential harness.

use num_bigint::BigInt;
use orna_value::Raw;
use orna_value::UNIT_TAG;

/// One runtime value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// The unit value, the result of a statement with no tail.
    Unit,
    /// A boolean.
    Bool(bool),
    /// An integer of arbitrary width.
    Int(BigInt),
    /// A string, already unescaped.
    Text(String),
}

impl Value {
    /// The canonical raw form of this value.
    #[must_use]
    pub fn to_raw(&self) -> Raw {
        match self {
            Self::Unit => Raw::Tag(UNIT_TAG, Box::new(Raw::Array(Vec::new()))),
            Self::Bool(value) => Raw::Bool(*value),
            Self::Int(value) => Raw::Int(value.clone()),
            Self::Text(value) => Raw::Text(value.clone()),
        }
    }
}
