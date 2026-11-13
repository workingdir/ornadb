//! Runtime values of the executor.
//!
//! Only the shapes this stage runs are here. Each one encodes to the same
//! canonical bytes the old evaluator produces (`orna_value::Raw`), so results
//! compare byte-for-byte in the differential harness. Composite values share
//! their elements behind an `Rc`, so copying a value out of a slot never copies
//! its contents.

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::{BodyId, DefId, Name, Ty};
use orna_value::{
    CanonicalValue, DIGEST_TAG, DURATION_TAG, INSTANT_TAG, MONEY_TAG, OPTION_TAG, Raw, Snapshot,
    TUPLE_TAG, UNIT_TAG, UUID_TAG,
};

use crate::calendar;
use crate::decimal::Decimal;
use crate::host::SysSlot;

/// One runtime value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// The unit value, the result of a statement with no tail.
    Unit,
    /// A boolean.
    Bool(bool),
    /// An integer of arbitrary width.
    Int(BigInt),
    /// An exact decimal.
    Decimal(Decimal),
    /// A binary64 float, held as its bit pattern. Every NaN is the canonical NaN.
    Float(u64),
    /// A point on the time axis: whole seconds since the Unix epoch and a nanosecond within the second.
    Instant {
        /// Seconds since the Unix epoch.
        unix_seconds: i64,
        /// Nanoseconds within the second, below one billion.
        nanosecond: u32,
    },
    /// An elapsed span: floor-normalised whole seconds and a nonnegative nanosecond tail below one billion.
    Duration {
        /// Whole seconds, floor-normalised so a negative span keeps a nonnegative `nanosecond`.
        seconds: BigInt,
        /// Nanoseconds within the second, below one billion.
        nanosecond: u32,
    },
    /// A string, already unescaped.
    Text(String),
    /// A byte string: the blob a digest or a hash input is.
    Blob(Rc<[u8]>),
    /// A calendar date, as days since 1970-01-01.
    Date(i64),
    /// A calendar period of whole days, kept distinct from an elapsed duration.
    Period(BigInt),
    /// A UUID: its 16 bytes.
    Uuid([u8; 16]),
    /// A nominal `Digest`: the 32 bytes of a SHA-256 commitment.
    Digest([u8; 32]),
    /// An exact monetary amount, its currency named by the 16-byte object id of the currency type.
    Money {
        /// The exact amount.
        amount: Decimal,
        /// The currency's object id.
        currency: [u8; 16],
    },
    /// An optional value: `None`, or `Some` of the value it holds.
    Option(Option<Rc<Value>>),
    /// The absent value of a flat option (D3): `null`, whose canonical raw form is `Raw::Null`. The old evaluator
    /// returns it for an absent `first` and for an absent `asof_join` row.
    Null,
    /// A tuple. The empty tuple is distinct from [`Value::Unit`] so that a
    /// tuple pattern matches only a tuple, as in the old pipeline.
    Tuple(Rc<[Value]>),
    /// A list.
    List(Rc<[Value]>),
    /// A structural record, its fields in name order.
    Record(Rc<[(Name, Value)]>),
    /// A lambda with the values it captured.
    Closure(Rc<Closure>),
    /// A relation not read yet: a handle a pipeline reads one row at a time. It has no canonical form.
    Rel(Rc<RelHandle>),
    /// A snapshot selector (`CWD`, `HEAD`, or the value a `sys` snapshot reference names). It has the canonical form of
    /// the snapshot it names.
    Snapshot(Rc<Snapshot>),
    /// A failure, delivered to a recovery handler. It carries the canonical
    /// code the failure reports.
    Error(&'static str),
    /// An enum variant: its enum's type id, the variant id, and the payload
    /// record of a variant with fields. A payload-free variant has no payload.
    Enum {
        /// The declaring enum's ObjectId.
        type_id: [u8; 16],
        /// The variant's ObjectId, derived from its enum's qualified name.
        variant_id: [u8; 16],
        /// The payload fields, in name order.
        payload: Option<Rc<[(Name, Value)]>>,
    },
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Int(BigInt::from(value))
    }
}

/// One variable: the slot a binding owns and every closure that captured it
/// shares (`docs/language/reference/03-bindings-and-expressions.md`).
pub type Cell = Rc<RefCell<Value>>;

/// A lambda's body and the variables it captured when it was created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closure {
    /// The lambda's body.
    pub body: BodyId,
    /// The captured variables, in the order of the body's captures.
    pub captured: Box<[Cell]>,
    /// The type environment the lambda was created in, which its body runs with.
    pub env: Option<Rc<[Ty]>>,
}

/// Where the rows of a relation value come from. A `sys` handle names a member the host registers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelHandle {
    /// The rows of the `sys` relation in `slot`, read from the host when a pipeline runs.
    Sys {
        /// The slot the host gives the relation.
        slot: SysSlot,
    },
    /// The rows of a table as of `at`, read from the host's store for that snapshot when a pipeline runs.
    Table {
        /// The table's definition.
        table: DefId,
        /// The snapshot the rows are read as of.
        at: Snapshot,
    },
}

impl Value {
    /// The canonical raw form of this value, or the canonical code of the
    /// reason it has none. A closure has no canonical form, and an error
    /// reports the failure it carries.
    ///
    /// # Errors
    ///
    /// The canonical error code of the failure that prevents the encoding.
    pub fn to_raw(&self) -> Result<Raw, &'static str> {
        match self {
            Self::Snapshot(snapshot) => Ok(snapshot.raw()),
            Self::Unit => Ok(unit_raw()),
            Self::Bool(value) => Ok(Raw::Bool(*value)),
            Self::Int(value) => Ok(Raw::Int(value.clone())),
            Self::Decimal(value) => Ok(value.to_raw()),
            Self::Float(bits) => Ok(Raw::Float(*bits)),
            Self::Instant {
                unix_seconds,
                nanosecond,
            } => Ok(Raw::Tag(
                INSTANT_TAG,
                Box::new(Raw::Array(vec![
                    Raw::Int((*unix_seconds).into()),
                    Raw::Int((*nanosecond).into()),
                ])),
            )),
            Self::Duration {
                seconds,
                nanosecond,
            } => Ok(Raw::Tag(
                DURATION_TAG,
                Box::new(Raw::Array(vec![
                    Raw::Int(seconds.clone()),
                    Raw::Int((*nanosecond).into()),
                ])),
            )),
            Self::Text(value) => Ok(Raw::Text(value.clone())),
            Self::Blob(bytes) => Ok(Raw::Bytes(bytes.to_vec())),
            Self::Date(days) => calendar::date_raw(*days).map_err(|_| "ORNA-EVAL-VALUE"),
            Self::Period(_) => Err("ORNA-EVAL-UNSUPPORTED"),
            Self::Uuid(bytes) => Ok(Raw::Tag(UUID_TAG, Box::new(Raw::Bytes(bytes.to_vec())))),
            Self::Digest(bytes) => Ok(Raw::Tag(
                DIGEST_TAG,
                Box::new(Raw::Array(vec![
                    Raw::Text("sha256".to_owned()),
                    Raw::Bytes(bytes.to_vec()),
                ])),
            )),
            Self::Money { amount, currency } => Ok(Raw::Tag(
                MONEY_TAG,
                Box::new(Raw::Array(vec![
                    amount.to_raw(),
                    Raw::Tag(UUID_TAG, Box::new(Raw::Bytes(currency.to_vec()))),
                ])),
            )),
            Self::Option(value) => option_raw(value.as_deref()),
            Self::Null => Ok(Raw::Null),
            Self::Tuple(values) if values.is_empty() => Ok(unit_raw()),
            Self::Tuple(values) => Ok(Raw::Tag(TUPLE_TAG, Box::new(array(values)?))),
            Self::List(values) => array(values),
            Self::Record(fields) => record_raw(fields),
            Self::Closure(_) | Self::Rel(_) | Self::Enum { .. } => Err("ORNA-EVAL-UNSUPPORTED"),
            Self::Error(code) => Err(code),
        }
    }
}

/// The unit value's raw form.
fn unit_raw() -> Raw {
    Raw::Tag(UNIT_TAG, Box::new(Raw::Array(Vec::new())))
}

/// The canonical raw form of an optional value: a tag over `[1, value]` for `Some`, and over `[0]` for `None`.
fn option_raw(value: Option<&Value>) -> Result<Raw, &'static str> {
    let parts = match value {
        Some(value) => vec![Raw::Int(BigInt::from(1)), value.to_raw()?],
        None => vec![Raw::Int(BigInt::from(0))],
    };
    Ok(Raw::Tag(OPTION_TAG, Box::new(Raw::Array(parts))))
}

/// An array of values in order.
fn array(values: &[Value]) -> Result<Raw, &'static str> {
    values
        .iter()
        .map(Value::to_raw)
        .collect::<Result<Vec<_>, _>>()
        .map(Raw::Array)
}

/// A record as a map from text keys in canonical key order. The old pipeline
/// orders map entries by the canonical encoding of each key, so the sort uses
/// those bytes rather than the names.
fn record_raw(fields: &[(Name, Value)]) -> Result<Raw, &'static str> {
    let mut keyed = Vec::with_capacity(fields.len());
    for (name, value) in fields {
        let key = Raw::Text(name.as_str().to_owned());
        let encoded = CanonicalValue::new(key.clone())
            .map_err(|_| "ORNA-EVAL-VALUE")?
            .encode()
            .map_err(|_| "ORNA-EVAL-VALUE")?;
        keyed.push((encoded, key, value));
    }
    keyed.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
    let entries = keyed
        .into_iter()
        .map(|(_, key, value)| value.to_raw().map(|value| (key, value)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Raw::Map(entries))
}

/// The payload record of an enum variant: its fields in name order, or none
/// for a payload-free variant.
pub(crate) type Payload = Option<Rc<[(Name, Value)]>>;
