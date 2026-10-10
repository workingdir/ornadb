use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use orna_value_v1::{
    decode_rov3_in_context, encode_rov3, AvailabilityState, Blob, BlobResolver, ContentIdentity,
    ContentReference, Format3Context, Format3Quota, GitHash, NativeOid, OwnerLifetime, Snapshot,
};

struct CountingResolver {
    bytes: Vec<u8>,
    reads: AtomicUsize,
}

impl BlobResolver for CountingResolver {
    fn read_range(
        &self,
        _reference: &ContentReference,
        offset: u64,
        length: u64,
    ) -> orna_value_v1::Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let start = usize::try_from(offset).map_err(|_| orna_value_v1::Error::InvalidRange)?;
        let length = usize::try_from(length).map_err(|_| orna_value_v1::Error::InvalidRange)?;
        let end = start
            .checked_add(length)
            .ok_or(orna_value_v1::Error::InvalidRange)?;
        self.bytes
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(orna_value_v1::Error::ContentUnavailable)
    }
}

#[test]
fn verified_context_exposes_lazy_references_to_external_crates() {
    let bytes = b"abc";
    let database = [1; 16];
    let resolver = Arc::new(CountingResolver {
        bytes: bytes.to_vec(),
        reads: AtomicUsize::new(0),
    });
    let context = Format3Context::from_persisted(
        database,
        Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
        GitHash::Sha1,
        NativeOid::from_bytes(&[9; 20]).unwrap(),
        NativeOid::from_bytes(&[8; 20]).unwrap(),
        AvailabilityState::Available,
        OwnerLifetime::new([3; 16]),
        Format3Quota::new(2, 1024).unwrap(),
        resolver.clone(),
    )
    .unwrap();
    let blob = Blob::from_reference_with_annotation(
        context
            .reference(
                ContentIdentity::from_bytes(bytes),
                NativeOid::from_bytes(&[7; 20]).unwrap(),
            )
            .unwrap(),
        "text/plain",
        None,
    )
    .unwrap();
    let encoded = encode_rov3(&blob).unwrap();
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);

    let decoded = decode_rov3_in_context(&encoded, &context).unwrap();
    let metadata = decoded.metadata();
    assert_eq!(metadata.length(), 3);
    assert_eq!(metadata.media_type(), "text/plain");
    assert_eq!(
        metadata.descriptor_oid(),
        Some(blob.descriptor_oid().unwrap())
    );
    assert!(!metadata.is_hydrated());
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);

    assert_eq!(blob.read(1, 2).unwrap(), b"bc");
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 1);
}
