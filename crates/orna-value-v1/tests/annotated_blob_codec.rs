#![cfg(feature = "test-support")]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use orna_value_v1::{
    AvailabilityState, Blob, BlobResolver, ContentIdentity, ContentReference, ContextValue, Error,
    Format3Context, Format3Quota, GitHash, NativeOid, OwnerLifetime, Raw, Snapshot, Value,
    ValueFormat, decode_ovb2, decode_rov3, decode_rov3_in_context, decode_sov3_in_context,
    encode_ovb2, encode_rov3, encode_sov3,
};

fn hex_bytes(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&value[at..at + 2], 16).unwrap())
        .collect()
}

fn test_context(resolver: Arc<dyn BlobResolver>) -> Format3Context {
    let database = [1; 16];
    Format3Context::for_tests(
        database,
        Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
        GitHash::Sha1,
        NativeOid::from_bytes(&[9; 20]).unwrap(),
        NativeOid::from_bytes(&[8; 20]).unwrap(),
        AvailabilityState::Available,
        OwnerLifetime::new([3; 16]),
        Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).unwrap(),
        resolver,
    )
    .unwrap()
}

#[test]
fn published_blob_vectors_use_explicit_profiles() {
    let ovb2 = Blob::from_bytes_with_annotation(b"abc".to_vec(), "text/plain;charset=utf-8", None)
        .unwrap();
    assert_eq!(
        encode_ovb2(&ovb2).unwrap(),
        hex_bytes("d9eace83436162637818746578742f706c61696e3b636861727365743d7574662d38f6")
    );

    let identity = ContentIdentity::from_bytes(b"abc");
    let context = test_context(Arc::new(CountingResolver {
        bytes: b"abc".to_vec(),
        reads: AtomicUsize::new(0),
    }));
    let reference = context
        .reference(identity, NativeOid::from_bytes(&[0x7f; 20]).unwrap())
        .unwrap();
    let rov3 =
        Blob::from_reference_with_annotation(reference, "text/plain;charset=utf-8", None).unwrap();
    assert!(!rov3.is_hydrated());
    assert_eq!(
        encode_rov3(&rov3).unwrap(),
        hex_bytes(
            "d9eacf85035820ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad7818746578742f706c61696e3b636861727365743d7574662d38f6547f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f"
        )
    );
    let decoded_rov3 = decode_rov3_in_context(&encode_rov3(&rov3).unwrap(), &context).unwrap();
    assert_eq!(decoded_rov3.content_identity(), rov3.content_identity());
    assert_eq!(decoded_rov3.media_type(), rov3.media_type());
    assert_eq!(decoded_rov3.descriptor_oid(), rov3.descriptor_oid());

    let sov3 = Blob::from_semantic_commitment_in_context(
        identity,
        &context,
        "text/plain;charset=utf-8",
        None,
    )
    .unwrap();
    assert_eq!(
        encode_sov3(&sov3).unwrap(),
        hex_bytes(
            "d9ead084035820ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad7818746578742f706c61696e3b636861727365743d7574662d38f6"
        )
    );
    let decoded_sov3 = decode_sov3_in_context(&encode_sov3(&sov3).unwrap(), &context).unwrap();
    assert_eq!(decoded_sov3.content_identity(), sov3.content_identity());
    assert_eq!(decoded_sov3.media_type(), sov3.media_type());
    assert_eq!(
        decoded_sov3.same_content(&sov3),
        Err(Error::ContentUnavailable)
    );
    assert_eq!(decode_ovb2(&encode_ovb2(&ovb2).unwrap()).unwrap(), ovb2);
}

#[test]
fn legacy_codec_rejects_new_tags_and_profiles_do_not_mix() {
    let raw = Raw::Tag(
        60110,
        Box::new(Raw::Array(vec![
            Raw::Bytes(b"abc".to_vec()),
            Raw::Text("text/plain".to_owned()),
            Raw::Null,
        ])),
    );
    assert_eq!(Value::new(raw.clone()), Err(Error::InvalidTag));
    assert_eq!(
        ContextValue::new_in_context(
            &test_context(Arc::new(CountingResolver {
                bytes: b"abc".to_vec(),
                reads: AtomicUsize::new(0),
            })),
            ValueFormat::Rov3,
            raw,
        ),
        Err(Error::InvalidProfile)
    );
}

#[test]
fn raw_reference_and_spoofed_context_cannot_authorize_reads() {
    let identity = ContentIdentity::from_bytes(b"abc");
    let raw = Raw::Tag(
        60111,
        Box::new(Raw::Array(vec![
            Raw::Int(3.into()),
            Raw::Bytes(identity.sha256().to_vec()),
            Raw::Text("text/plain".to_owned()),
            Raw::Null,
            Raw::Bytes(vec![7; 20]),
        ])),
    );
    assert_eq!(
        ContextValue::new(ValueFormat::Rov3, raw),
        Err(Error::InvalidContext)
    );
    let authorized_context = test_context(Arc::new(CountingResolver {
        bytes: b"abc".to_vec(),
        reads: AtomicUsize::new(0),
    }));
    let authorized_blob = Blob::from_reference(
        authorized_context
            .reference(identity, NativeOid::from_bytes(&[7; 20]).unwrap())
            .unwrap(),
    )
    .unwrap();
    let encoded = encode_rov3(&authorized_blob).unwrap();
    assert_eq!(decode_rov3(&encoded), Err(Error::InvalidContext));

    let resolver = Arc::new(CountingResolver {
        bytes: b"abc".to_vec(),
        reads: AtomicUsize::new(0),
    });
    let database = [4; 16];
    let owner = OwnerLifetime::new([5; 16]);
    let quota = Format3Quota::new(8 * 1024 * 1024, 1024).unwrap();
    assert!(matches!(
        Format3Context::for_tests(
            database,
            Snapshot::cwd(database, [6; 16], 0.into()).unwrap(),
            GitHash::Sha1,
            NativeOid::from_bytes(&[9; 32]).unwrap(),
            NativeOid::from_bytes(&[8; 20]).unwrap(),
            AvailabilityState::Available,
            owner.clone(),
            quota,
            resolver.clone(),
        ),
        Err(Error::InvalidContext)
    ));
    assert!(matches!(
        Format3Context::for_tests(
            database,
            Snapshot::cwd([8; 16], [6; 16], 0.into()).unwrap(),
            GitHash::Sha1,
            NativeOid::from_bytes(&[9; 20]).unwrap(),
            NativeOid::from_bytes(&[8; 20]).unwrap(),
            AvailabilityState::Available,
            owner,
            quota,
            resolver.clone(),
        ),
        Err(Error::InvalidContext)
    ));

    let context = test_context(resolver.clone());
    assert_eq!(
        context.reference(identity, NativeOid::from_bytes(&[1; 32]).unwrap()),
        Err(Error::InvalidOid)
    );
    let reference = context
        .reference(identity, NativeOid::from_bytes(&[1; 20]).unwrap())
        .unwrap();
    let blob = Blob::from_reference(reference).unwrap();
    context.cancel();
    assert_eq!(blob.read(0, 3), Err(Error::Cancelled));
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);
}

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
        let start = offset as usize;
        let end = start + length as usize;
        Ok(self.bytes[start..end].to_vec())
    }
}

#[test]
fn lazy_reference_metadata_and_semantic_encoding_do_not_read_payload() {
    let resolver = Arc::new(CountingResolver {
        bytes: b"payload".to_vec(),
        reads: AtomicUsize::new(0),
    });
    let identity = ContentIdentity::from_bytes(b"payload");
    let context = test_context(resolver.clone());
    let reference = context
        .reference(identity, NativeOid::from_bytes(&[3; 20]).unwrap())
        .unwrap();
    let blob = Blob::from_reference_with_annotation(reference, "audio/mpeg", None).unwrap();
    assert_eq!(blob.length(), 7);
    assert_eq!(blob.media_type(), "audio/mpeg");
    assert_eq!(blob.suffix(), None);
    assert_eq!(
        encode_rov3(&blob).unwrap(),
        encode_rov3(&blob.annotate("audio/mpeg", Some("mp3")).unwrap()).unwrap()
    );
    assert_eq!(
        encode_sov3(&blob).unwrap(),
        encode_sov3(&blob.annotate("audio/mpeg", Some("mp3")).unwrap()).unwrap()
    );
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);
    assert_eq!(blob.read(1, 3).unwrap(), b"ayl");
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn content_identity_is_separate_from_annotated_value_equality() {
    let left = Blob::from_bytes_with_annotation(b"same".to_vec(), "text/plain", None).unwrap();
    let right = left.annotate("text/plain", Some("text")).unwrap();
    assert!(left.same_content(&right).unwrap());
    assert!(!left.value_eq(&right).unwrap());

    let identity = ContentIdentity::from_bytes(b"same");
    let first = Blob::from_semantic_commitment(identity, "text/plain", None).unwrap();
    let second = Blob::from_semantic_commitment(identity, "text/plain", None).unwrap();
    assert_eq!(first.same_content(&second), Err(Error::ContentUnavailable));
    assert_eq!(first.value_eq(&second), Err(Error::ContentUnavailable));
}
