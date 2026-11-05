//! The physical and natural-key contract for the catalogue `sys.Object` view.
//!
//! `sys.Object` is the identity anchor for catalogue references.  The API
//! fixes its natural key (`id + snapshot`) but does not assign a physical
//! relation UUID; this implementation assigns one stable compatibility
//! coordinate to the read-only catalogue projection.  Semantic object IDs
//! still come only from the catalogue producer.

use crate::{
    CanonicalSnapshot, CwdCapture, ObjectRef, OvbRaw, RowRef, SystemReferenceError, TypedRowRef,
};

/// Stable physical identity of the read-only `sys.Object` catalogue
/// projection.  It is distinct from every `sys.Type` and `sys.Function`
/// relation identity and is never a semantic object ID.
pub const SYS_OBJECT_TABLE_ID: [u8; 16] = [
    0x12, 0x7e, 0x8b, 0x3a, 0x4d, 0x66, 0x47, 0x9f, 0xa1, 0x2c, 0x53, 0xd8, 0x70, 0x91, 0x00, 0x01,
];

/// Constructs the exact `sys.ObjectRef` natural key for one catalogue
/// object.  The caller must pass an identity already obtained from the
/// authoritative catalogue projection; this function does not mint or
/// validate semantic identities.
pub fn object_reference(
    database_id: [u8; 16],
    object_id: [u8; 16],
    snapshot: CanonicalSnapshot,
) -> Result<ObjectRef, SystemReferenceError> {
    ensure_snapshot_database(database_id, &snapshot)?;
    let key = object_key(object_id, &snapshot);
    let row = RowRef::new(database_id, SYS_OBJECT_TABLE_ID, key, snapshot)
        .map_err(|_| SystemReferenceError::InvalidObjectKey)?;
    Ok(ObjectRef::from_row_ref(row))
}

/// Validates the physical relation, CWD pin, and exact `sys.Object` natural
/// key shape of a persisted object reference.
pub fn validate_object_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<ObjectRef, SystemReferenceError> {
    if reference.database_id != capture.database_id() {
        return Err(SystemReferenceError::DatabaseMismatch);
    }
    if reference.snapshot != *capture.snapshot() {
        return Err(SystemReferenceError::SnapshotMismatch);
    }
    if reference.table_id != SYS_OBJECT_TABLE_ID {
        return Err(SystemReferenceError::RelationMismatch);
    }
    validate_object_key(&reference, None)?;
    Ok(TypedRowRef::from_row_ref(reference))
}

/// Validates an object reference and proves that its natural-key identity is
/// the catalogue identity selected by the trusted producer.
pub fn validate_object_reference_for_id(
    reference: RowRef,
    capture: &CwdCapture,
    expected_object_id: [u8; 16],
) -> Result<ObjectRef, SystemReferenceError> {
    let reference = validate_object_reference(reference, capture)?;
    validate_object_key(reference.as_row_ref(), Some(expected_object_id))?;
    Ok(reference)
}

pub(crate) fn validate_object_reference_shape(
    reference: &RowRef,
) -> Result<(), SystemReferenceError> {
    if reference.table_id != SYS_OBJECT_TABLE_ID {
        return Err(SystemReferenceError::RelationMismatch);
    }
    validate_object_key(reference, None)
}

fn validate_object_key(
    reference: &RowRef,
    expected_object_id: Option<[u8; 16]>,
) -> Result<(), SystemReferenceError> {
    let OvbRaw::Array(parts) = &reference.key else {
        return Err(SystemReferenceError::InvalidObjectKey);
    };
    let [object_id, snapshot] = parts.as_slice() else {
        return Err(SystemReferenceError::InvalidObjectKey);
    };
    let OvbRaw::Tag(tag, bytes) = object_id else {
        return Err(SystemReferenceError::InvalidObjectKey);
    };
    if *tag != 37 || !matches!(bytes.as_ref(), OvbRaw::Bytes(value) if value.len() == 16) {
        return Err(SystemReferenceError::InvalidObjectKey);
    }
    if let Some(expected) = expected_object_id
        && !matches!(bytes.as_ref(), OvbRaw::Bytes(value) if value.as_slice() == expected)
    {
        return Err(SystemReferenceError::InvalidObjectKey);
    }
    if *snapshot != reference.snapshot.raw() {
        return Err(SystemReferenceError::InvalidObjectKey);
    }
    Ok(())
}

fn object_key(object_id: [u8; 16], snapshot: &CanonicalSnapshot) -> OvbRaw {
    OvbRaw::Array(vec![
        OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(object_id.to_vec()))),
        snapshot.raw(),
    ])
}

fn ensure_snapshot_database(
    database_id: [u8; 16],
    snapshot: &CanonicalSnapshot,
) -> Result<(), SystemReferenceError> {
    let snapshot_database = match snapshot {
        CanonicalSnapshot::Cwd { database, .. } | CanonicalSnapshot::Commit { database, .. } => {
            *database
        }
    };
    if snapshot_database != database_id {
        return Err(SystemReferenceError::DatabaseMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanonicalSnapshot, Value};

    fn capture() -> CwdCapture {
        let snapshot = CanonicalSnapshot::cwd([1; 16], [2; 16], 3.into()).unwrap();
        CwdCapture::new(snapshot, [4; 32]).unwrap()
    }

    #[test]
    fn object_reference_uses_object_relation_and_id_snapshot_key() {
        let capture = capture();
        let reference =
            object_reference(capture.database_id(), [5; 16], capture.snapshot().clone()).unwrap();
        assert_eq!(reference.as_row_ref().table_id, SYS_OBJECT_TABLE_ID);
        assert_eq!(
            validate_object_reference_for_id(reference.as_row_ref().clone(), &capture, [5; 16])
                .unwrap(),
            reference
        );
        let decoded = Value::decode(&reference.as_row_ref().encode().unwrap()).unwrap();
        assert_eq!(decoded.raw(), &reference.as_row_ref().raw().unwrap());
    }

    #[test]
    fn object_reference_rejects_wrong_relation_and_identity() {
        let capture = capture();
        let mut row = object_reference(capture.database_id(), [5; 16], capture.snapshot().clone())
            .unwrap()
            .into_row_ref();
        row.table_id = [9; 16];
        assert_eq!(
            validate_object_reference(row, &capture),
            Err(SystemReferenceError::RelationMismatch)
        );

        let row = object_reference(capture.database_id(), [5; 16], capture.snapshot().clone())
            .unwrap()
            .into_row_ref();
        assert_eq!(
            validate_object_reference_for_id(row, &capture, [6; 16]),
            Err(SystemReferenceError::InvalidObjectKey)
        );
    }
}
