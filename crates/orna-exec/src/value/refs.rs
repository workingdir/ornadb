//! Typed catalogue references as executor values.
//!
//! A `sys.FunctionRef` and a `sys.ObjectRef` are checked [`RowRef`]s on the
//! physical `sys.Function` and `sys.Object` relations. Each one is its own
//! [`Value`] variant, so a provider reads the typed reference through
//! [`Value::as_function_ref`] and [`Value::as_object_ref`] instead of an opaque
//! `Raw`. Its canonical bytes are the ones [`Value::to_raw`] produces, so a
//! stored reference reads back as an equal value.

use std::rc::Rc;

use orna_value::{
    CanonicalValue, FunctionRef, ObjectRef, Raw, RowRef, SYS_FUNCTION_TABLE_ID, SYS_OBJECT_TABLE_ID,
};

use super::Value;

impl Value {
    /// The executor value of a checked `sys.FunctionRef`.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-VALUE` when the reference is not on the `sys.Function` relation or its
    /// canonical form does not encode.
    pub fn function_ref(reference: FunctionRef) -> Result<Self, &'static str> {
        checked_row(reference.as_row_ref(), SYS_FUNCTION_TABLE_ID)?;
        Ok(Self::FunctionRef(Rc::new(reference)))
    }

    /// The executor value of a checked `sys.ObjectRef`.
    ///
    /// # Errors
    ///
    /// `ORNA-EVAL-VALUE` when the reference is not on the `sys.Object` relation or its
    /// canonical form does not encode.
    pub fn object_ref(reference: ObjectRef) -> Result<Self, &'static str> {
        checked_row(reference.as_row_ref(), SYS_OBJECT_TABLE_ID)?;
        Ok(Self::ObjectRef(Rc::new(reference)))
    }

    /// The `sys.FunctionRef` this value holds, if it is one.
    pub fn as_function_ref(&self) -> Option<&FunctionRef> {
        match self {
            Self::FunctionRef(reference) => Some(reference.as_ref()),
            _ => None,
        }
    }

    /// The `sys.ObjectRef` this value holds, if it is one.
    pub fn as_object_ref(&self) -> Option<&ObjectRef> {
        match self {
            Self::ObjectRef(reference) => Some(reference.as_ref()),
            _ => None,
        }
    }
}

/// The canonical raw form of a reference's row, which [`Value::to_raw`] returns.
pub(crate) fn typed_row_raw(row: &RowRef) -> Result<Raw, &'static str> {
    row.to_canonical()
        .map(|value| value.raw().clone())
        .map_err(|_| "ORNA-EVAL-VALUE")
}

/// The typed reference that the canonical raw value `raw` holds, or `None` when `raw` is not
/// exactly the canonical form of a `sys.FunctionRef` or `sys.ObjectRef`. The caller keeps `None`
/// as an opaque [`Value::RowRef`].
///
/// The relation identity selects the variant. `RowRef::decode` checks only the reference's shape, not
/// its table-specific key; a provider that dereferences the value must validate the key first. The
/// reference must re-encode to `raw` byte for byte.
pub(crate) fn typed_reference_from_raw(raw: &Raw) -> Option<Value> {
    let bytes = CanonicalValue::new(raw.clone()).ok()?.encode().ok()?;
    let row = RowRef::decode(&bytes).ok()?;
    let table = row.table_id;
    let value = if table == SYS_FUNCTION_TABLE_ID {
        Value::FunctionRef(Rc::new(FunctionRef::from_row_ref(row)))
    } else if table == SYS_OBJECT_TABLE_ID {
        Value::ObjectRef(Rc::new(ObjectRef::from_row_ref(row)))
    } else {
        return None;
    };
    (value.to_raw().ok()? == *raw).then_some(value)
}

/// Refuses `row` unless it is on `table` and its canonical form encodes.
fn checked_row(row: &RowRef, table: [u8; 16]) -> Result<(), &'static str> {
    if row.table_id != table {
        return Err("ORNA-EVAL-VALUE");
    }
    typed_row_raw(row).map(|_| ())
}

/// Checked catalogue references shared by the tests of this crate.
#[cfg(test)]
pub(crate) mod samples {
    use orna_value::{
        CanonicalSnapshot, FunctionRef, ObjectRef, function_reference, object_reference,
    };

    /// The `sys.ObjectRef` of catalogue object `[5; 16]` on database `[1; 16]`, pinned to a CWD snapshot.
    pub(crate) fn object_ref() -> ObjectRef {
        let snapshot = CanonicalSnapshot::cwd([1; 16], [2; 16], 3.into()).expect("snapshot");
        object_reference([1; 16], [5; 16], snapshot).expect("object reference")
    }

    /// The `sys.FunctionRef` of [`object_ref`]: the same key and pin on the function relation.
    pub(crate) fn function_ref() -> FunctionRef {
        function_reference(object_ref()).expect("function reference")
    }
}

#[cfg(test)]
mod tests {
    use orna_value::{FunctionRef, ObjectRef, RowRef};

    use super::samples::{function_ref, object_ref};
    use super::typed_reference_from_raw;
    use crate::raw_value::from_raw;
    use crate::value::Value;

    #[test]
    fn function_reference_is_a_typed_value_that_round_trips() {
        let reference = function_ref();
        let value = Value::function_ref(reference.clone()).expect("typed function reference");
        assert_eq!(value.as_function_ref(), Some(&reference));
        assert_eq!(value.as_object_ref(), None);
        let raw = value.to_raw().expect("encodes");
        assert_eq!(from_raw(&raw).expect("reads back"), value);
    }

    #[test]
    fn object_reference_is_a_typed_value_that_round_trips() {
        let reference = object_ref();
        let value = Value::object_ref(reference.clone()).expect("typed object reference");
        assert_eq!(value.as_object_ref(), Some(&reference));
        assert_eq!(value.as_function_ref(), None);
        let raw = value.to_raw().expect("encodes");
        assert_eq!(from_raw(&raw).expect("reads back"), value);
    }

    #[test]
    fn constructors_refuse_a_reference_on_another_relation() {
        assert_eq!(
            Value::object_ref(ObjectRef::from_row_ref(function_ref().into_row_ref())),
            Err("ORNA-EVAL-VALUE")
        );
        assert_eq!(
            Value::function_ref(FunctionRef::from_row_ref(object_ref().into_row_ref())),
            Err("ORNA-EVAL-VALUE")
        );
    }

    #[test]
    fn a_reference_on_another_relation_reads_back_opaque() {
        let object = object_ref().into_row_ref();
        let other =
            RowRef::new([1; 16], [9; 16], object.key, object.snapshot).expect("row reference");
        let raw = other.to_canonical().expect("canonical").raw().clone();
        assert!(typed_reference_from_raw(&raw).is_none());
        assert!(matches!(from_raw(&raw), Ok(Value::RowRef(_))));
    }
}
