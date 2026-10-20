use std::sync::Arc;

use orna_value_v1::{
    decode_rov3, decode_rov3_in_context, encode_rov3, AvailabilityState, Blob, BlobResolver,
    ContentIdentity, ContentReference, ContextValue, Error, Format3Context, Format3Quota, GitHash,
    NativeOid, OwnerLifetime, Snapshot, ValueFormat,
};

struct FixedResolver(Vec<u8>);

impl BlobResolver for FixedResolver {
    fn read_range(
        &self,
        _reference: &ContentReference,
        offset: u64,
        length: u64,
    ) -> orna_value_v1::Result<Vec<u8>> {
        let start = usize::try_from(offset).map_err(|_| Error::InvalidRange)?;
        let length = usize::try_from(length).map_err(|_| Error::InvalidRange)?;
        let end = start.checked_add(length).ok_or(Error::InvalidRange)?;
        self.0
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(Error::ContentUnavailable)
    }
}

fn context(
    database: [u8; 16],
    snapshot: Snapshot,
    resolver: Arc<FixedResolver>,
) -> Result<Format3Context, Error> {
    Format3Context::from_persisted(
        database,
        snapshot,
        GitHash::Sha1,
        NativeOid::from_bytes(&[9; 20]).unwrap(),
        NativeOid::from_bytes(&[8; 20]).unwrap(),
        AvailabilityState::Available,
        OwnerLifetime::new([3; 16]),
        Format3Quota::new(2, 1024).unwrap(),
        resolver,
    )
}

fn authorized_blob(database: [u8; 16], resolver: Arc<FixedResolver>) -> (Format3Context, Blob) {
    let context = context(
        database,
        Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
        resolver,
    )
    .unwrap();
    let blob = Blob::from_reference(
        context
            .reference(
                ContentIdentity::from_bytes(b"abc"),
                NativeOid::from_bytes(&[7; 20]).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    (context, blob)
}

/// ORNA-FORMAT-003 (partial): covers only database pinning and read authority of a foreign snapshot.
/// Type, table and key schema agreement is not exercised here; remainder tracked in gap bead ornadb-5il1.28.
#[test]
fn foreign_snapshot_neither_replaces_the_context_nor_authorizes_reads() {
    let resolver = Arc::new(FixedResolver(b"abc".to_vec()));
    let (current, blob) = authorized_blob([1; 16], resolver.clone());
    let encoded = encode_rov3(&blob).unwrap();

    // A snapshot pinned to a different database cannot be bound to this database's context.
    assert_eq!(
        context(
            [1; 16],
            Snapshot::cwd([4; 16], [2; 16], 0.into()).unwrap(),
            resolver.clone(),
        )
        .err(),
        Some(Error::InvalidContext)
    );

    // Decoding under a foreign database keeps that database's authority: the value is not readable through it.
    let foreign = context(
        [4; 16],
        Snapshot::cwd([4; 16], [5; 16], 0.into()).unwrap(),
        resolver.clone(),
    )
    .unwrap();
    let foreign_decoded = decode_rov3_in_context(&encoded, &foreign);
    assert_eq!(
        foreign_decoded.map(|value| value.read(0, 3)),
        Ok(Err(Error::ContentUnavailable))
    );

    // The current execution context is unchanged and still authorizes its own reads.
    assert_eq!(blob.read(1, 2).unwrap(), b"bc");
    let decoded = decode_rov3_in_context(&encoded, &current).unwrap();
    assert_eq!(decoded.metadata().length(), 3);
}

/// ORNA-FORMAT-006: Stored and semantic reference tags MUST be rejected outside their recorded ROV-3/SOV-3 contexts; a raw SHA or Git OID is never permission to read content.
#[test]
fn rov3_reference_tag_is_rejected_outside_its_recorded_context() {
    let resolver = Arc::new(FixedResolver(b"abc".to_vec()));
    let (authorized, blob) = authorized_blob([1; 16], resolver);
    let encoded = encode_rov3(&blob).unwrap();

    assert_eq!(decode_rov3(&encoded), Err(Error::InvalidContext));
    let decoded = decode_rov3_in_context(&encoded, &authorized).unwrap();
    assert_eq!(decoded.metadata().length(), 3);
    assert_eq!(blob.read(1, 2).unwrap(), b"bc");
}

/// ORNA-FORMAT-005 (partial): new annotated Blob values MUST NOT silently lose their annotation through the old OVB-1 interface.
/// Covers only the reject half (OVB-1 refuses the annotated value); the OVB-2 preserve half is tracked in gap bead ornadb-5il1.29.
#[test]
fn annotated_blob_is_rejected_by_ovb1_instead_of_losing_its_annotation() {
    let blob = Blob::from_bytes_with_annotation(b"image".to_vec(), "image/jpeg", Some("jpeg"))
        .expect("valid MIME type and suffix");
    assert_eq!(
        ContextValue::from_blob(&blob, ValueFormat::Ovb1).err(),
        Some(Error::InvalidProfile)
    );
}
