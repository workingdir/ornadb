//! Runtime values of the executor.
//!
//! Only the shapes this stage runs are here. Each one encodes to the same
//! canonical bytes the old evaluator produces (`orna_value::Raw`), so results
//! compare byte-for-byte in the differential harness. Composite values share
//! their elements behind an `Rc`, so copying a value out of a slot never copies
//! its contents.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::{BodyId, Name};
use orna_value::{CanonicalValue, Raw, TUPLE_TAG, UNIT_TAG};

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
    /// A tuple. The empty tuple is distinct from [`Value::Unit`] so that a
    /// tuple pattern matches only a tuple, as in the old pipeline.
    Tuple(Rc<[Value]>),
    /// A list.
    List(Rc<[Value]>),
    /// A structural record, its fields in name order.
    Record(Rc<[(Name, Value)]>),
    /// A lambda with the values it captured.
    Closure(Rc<Closure>),
    /// A failure, delivered to a recovery handler. It carries the canonical
    /// code the failure reports.
    Error(&'static str),
}

/// A lambda's body and the values captured when it was created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closure {
    /// The lambda's body.
    pub body: BodyId,
    /// The captured values, in the order of the body's captures.
    pub captured: Box<[Value]>,
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
            Self::Unit => Ok(unit_raw()),
            Self::Bool(value) => Ok(Raw::Bool(*value)),
            Self::Int(value) => Ok(Raw::Int(value.clone())),
            Self::Text(value) => Ok(Raw::Text(value.clone())),
            Self::Tuple(values) if values.is_empty() => Ok(unit_raw()),
            Self::Tuple(values) => Ok(Raw::Tag(TUPLE_TAG, Box::new(array(values)?))),
            Self::List(values) => array(values),
            Self::Record(fields) => record_raw(fields),
            Self::Closure(_) => Err("ORNA-EVAL-UNSUPPORTED"),
            Self::Error(code) => Err(code),
        }
    }
}

/// The unit value's raw form.
fn unit_raw() -> Raw {
    Raw::Tag(UNIT_TAG, Box::new(Raw::Array(Vec::new())))
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
