use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::*;

fn hex_bytes(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&value[at..at + 2], 16).unwrap())
        .collect()
}

fn counting_resolver(bytes: &[u8]) -> Arc<CountingResolver> {
    Arc::new(CountingResolver {
        bytes: bytes.to_vec(),
        reads: AtomicUsize::new(0),
        max_request: AtomicUsize::new(0),
    })
}

fn test_context(resolver: Arc<dyn BlobResolver>) -> Format3Context {
    test_context_with_quota(
        resolver,
        Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).unwrap(),
    )
}

fn test_context_with_quota(resolver: Arc<dyn BlobResolver>, quota: Format3Quota) -> Format3Context {
    let database = [1; 16];
    Format3Context::from_persisted(
        database,
        Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
        GitHash::Sha1,
        NativeOid::from_bytes(&[9; 20]).unwrap(),
        NativeOid::from_bytes(&[8; 20]).unwrap(),
        AvailabilityState::Available,
        OwnerLifetime::new([3; 16]),
        quota,
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
    let context = test_context(counting_resolver(b"abc"));
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
fn rov3_raw_descriptor_decode_preserves_metadata_without_read_authority() {
    let resolver = counting_resolver(b"payload");
    let identity = ContentIdentity::from_bytes(b"payload");
    let context = test_context(resolver.clone());
    let descriptor_oid = NativeOid::from_bytes(&[5; 20]).unwrap();
    let admitted = Blob::from_reference_with_annotation(
        context.reference(identity, descriptor_oid.clone()).unwrap(),
        "audio/mpeg",
        None,
    )
    .unwrap();
    let stored = encode_rov3(&admitted).unwrap();

    let decoded = decode_rov3_in_context(&stored, &context).unwrap();
    assert_eq!(decoded.content_identity(), identity);
    assert_eq!(decoded.media_type(), "audio/mpeg");
    assert_eq!(decoded.suffix(), None);
    assert_eq!(decoded.descriptor_oid(), Some(&descriptor_oid));
    assert_eq!(encode_rov3(&decoded).unwrap(), stored);
    assert_eq!(decoded.read(0, 1), Err(Error::ContentUnavailable));
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);

    assert_eq!(admitted.read(0, 1).unwrap(), b"p");
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn ovb2_blob_encoder_preflights_sink_and_streams_public_encoding() {
    let resolver = counting_resolver(b"abc");
    let context =
        test_context_with_quota(resolver.clone(), Format3Quota::new(2, 1_u64 << 62).unwrap());
    let blob = Blob::from_reference_with_annotation(
        context
            .reference(
                ContentIdentity::from_bytes(b"abc"),
                NativeOid::from_bytes(&[0x7f; 20]).unwrap(),
            )
            .unwrap(),
        "text/plain;charset=utf-8",
        None,
    )
    .unwrap();
    let expected =
        hex_bytes("d9eace83436162637818746578742f706c61696e3b636861727365743d7574662d38f6");

    assert_eq!(
        encode_ovb2_bounded(&blob, u64::try_from(expected.len() - 1).unwrap(),),
        Err(Error::QuotaExceeded)
    );
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);

    let mut undersized_output = Vec::new();
    assert_eq!(
        blob.encode_ovb2_to_writer(
            &mut undersized_output,
            u64::try_from(expected.len() - 1).unwrap(),
        ),
        Err(Error::QuotaExceeded)
    );
    assert!(undersized_output.is_empty());
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);

    assert_eq!(
        encode_ovb2_bounded(&blob, u64::try_from(expected.len()).unwrap()).unwrap(),
        expected
    );
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 2);
    assert_eq!(resolver.max_request.load(Ordering::SeqCst), 2);
}

#[test]
fn mime1_annotations_reject_noncanonical_and_unsafe_inputs() {
    assert_eq!(
        decode_typed::<Vec<u8>>(&hex_bytes("436a7067")).unwrap(),
        b"jpg"
    );

    assert_eq!(
        MediaAnnotation::new("image/*", None),
        Err(Error::InvalidMediaType)
    );
    assert_eq!(
        MediaAnnotation::new("text/plain", Some("../txt")),
        Err(Error::InvalidSuffix)
    );
    assert_eq!(
        MediaAnnotation::new("text/plain", Some("txt")),
        Err(Error::NonCanonical)
    );

    assert_eq!(
        Blob::from_bytes_with_annotation(
            b"module".to_vec(),
            "Application/JavaScript;CHARSET=\"UTF-8\"",
            Some("MJS"),
        ),
        Err(Error::NonCanonical)
    );
    let canonical = Blob::from_bytes_with_annotation(
        b"module".to_vec(),
        "text/javascript;charset=utf-8",
        Some("mjs"),
    )
    .unwrap();
    assert_eq!(canonical.media_type(), "text/javascript;charset=utf-8");
    assert_eq!(canonical.suffix(), Some("mjs"));
    assert_eq!(canonical.annotation().selected_suffix(), "mjs");
    assert_eq!(
        decode_ovb2(&encode_ovb2(&canonical).unwrap()).unwrap(),
        canonical
    );

    assert_eq!(
        Blob::from_bytes_with_annotation(b"image".to_vec(), "image/jpeg", Some("png")),
        Err(Error::IncompatibleSuffix)
    );
    let plain = Blob::from_bytes_with_annotation(b"text".to_vec(), "text/plain", None).unwrap();
    assert_eq!(
        plain.annotate("text/plain", Some("png")),
        Err(Error::IncompatibleSuffix)
    );
    assert_eq!(
        Blob::from_semantic_commitment(
            ContentIdentity::from_bytes(b"abc"),
            "image/jpeg",
            Some("png")
        ),
        Err(Error::IncompatibleSuffix)
    );
    assert_eq!(
        Blob::from_bytes_with_annotation(b"text".to_vec(), "text/plain", Some("TXT")),
        Err(Error::NonCanonical)
    );
    assert_eq!(
        Blob::from_bytes_with_annotation(b"opaque".to_vec(), "application/x-private", Some("data"))
            .unwrap()
            .annotation()
            .selected_suffix(),
        "data"
    );

    let identity = ContentIdentity::from_bytes(b"abc");
    let resolver = counting_resolver(b"abc");
    let context = test_context(resolver.clone());
    let reference = context
        .reference(identity, NativeOid::from_bytes(&[7; 20]).unwrap())
        .unwrap();
    assert_eq!(
        Blob::from_reference_with_annotation(reference.clone(), "image/jpeg", Some("png")),
        Err(Error::IncompatibleSuffix)
    );
    assert_eq!(
        Blob::from_reference_with_annotation(reference.clone(), "Audio/MPEG", None),
        Err(Error::NonCanonical)
    );
    assert_eq!(
        Blob::from_reference_with_annotation(reference.clone(), "audio/mpeg", Some("MP3")),
        Err(Error::NonCanonical)
    );
    let blob = Blob::from_reference_with_annotation(reference, "audio/mpeg", None).unwrap();
    let annotated = blob.annotate("audio/mpeg", Some("mp2")).unwrap();
    assert_eq!(blob.content_identity(), annotated.content_identity());
    assert!(blob.same_content(&annotated).unwrap());
    assert!(!blob.value_eq(&annotated).unwrap());
    assert_ne!(encode_rov3(&blob).unwrap(), encode_rov3(&annotated).unwrap());
    assert_ne!(encode_sov3(&blob).unwrap(), encode_sov3(&annotated).unwrap());
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        Blob::from_semantic_commitment_in_context(identity, &context, "image/jpeg", Some("png")),
        Err(Error::IncompatibleSuffix)
    );
    let ovb2 = tag(
        OVB2_BLOB_TAG,
        Raw::Array(vec![
            Raw::Bytes(b"abc".to_vec()),
            Raw::Text("image/jpeg".to_owned()),
            Raw::Text("png".to_owned()),
        ]),
    );
    assert_eq!(
        ContextValue::new(ValueFormat::Ovb2, ovb2.clone()),
        Err(Error::IncompatibleSuffix)
    );
    assert_eq!(
        encode_context_raw(&ovb2, ValueFormat::Ovb2, None, MimeRegistry::mime1()),
        Err(Error::IncompatibleSuffix)
    );
    let mut encoded = Vec::new();
    write_raw(&ovb2, &mut encoded).unwrap();
    assert_eq!(decode_ovb2(&encoded), Err(Error::IncompatibleSuffix));

    let noncanonical_ovb2 = tag(
        OVB2_BLOB_TAG,
        Raw::Array(vec![
            Raw::Bytes(b"module".to_vec()),
            Raw::Text("Application/JavaScript;CHARSET=\"UTF-8\"".to_owned()),
            Raw::Text("MJS".to_owned()),
        ]),
    );
    assert_eq!(
        ContextValue::new(ValueFormat::Ovb2, noncanonical_ovb2),
        Err(Error::NonCanonical)
    );

    let rov3 = tag(
        ROV3_BLOB_TAG,
        Raw::Array(vec![
            Raw::Int(3.into()),
            Raw::Bytes(identity.sha256().to_vec()),
            Raw::Text("image/jpeg".to_owned()),
            Raw::Text("png".to_owned()),
            Raw::Bytes(vec![7; 20]),
        ]),
    );
    assert_eq!(
        ContextValue::new_in_context(&context, ValueFormat::Rov3, rov3.clone()),
        Err(Error::IncompatibleSuffix)
    );
    assert_eq!(
        encode_context_raw(
            &rov3,
            ValueFormat::Rov3,
            Some(context.git_oid_algorithm()),
            context.mime_registry()
        ),
        Err(Error::IncompatibleSuffix)
    );
    let mut encoded = Vec::new();
    write_raw(&rov3, &mut encoded).unwrap();
    assert_eq!(
        decode_rov3_in_context(&encoded, &context),
        Err(Error::IncompatibleSuffix)
    );

    let sov3 = tag(
        SOV3_BLOB_TAG,
        Raw::Array(vec![
            Raw::Int(3.into()),
            Raw::Bytes(identity.sha256().to_vec()),
            Raw::Text("image/jpeg".to_owned()),
            Raw::Text("png".to_owned()),
        ]),
    );
    assert_eq!(
        ContextValue::new_in_context(&context, ValueFormat::Sov3, sov3.clone()),
        Err(Error::IncompatibleSuffix)
    );
    assert_eq!(
        encode_context_raw(
            &sov3,
            ValueFormat::Sov3,
            Some(context.git_oid_algorithm()),
            context.mime_registry()
        ),
        Err(Error::IncompatibleSuffix)
    );
    let mut encoded = Vec::new();
    write_raw(&sov3, &mut encoded).unwrap();
    assert_eq!(
        decode_sov3_in_context(&encoded, &context),
        Err(Error::IncompatibleSuffix)
    );
}

#[test]
fn annotate_canonicalizes_mime_and_suffix_without_hydrating_content() {
    let bytes = b"payload";
    let identity = ContentIdentity::from_bytes(bytes);
    let resolver = counting_resolver(bytes);
    let context = test_context(resolver.clone());
    let descriptor_oid = NativeOid::from_bytes(&[6; 20]).unwrap();
    let blob = Blob::from_reference_with_annotation(
        context.reference(identity, descriptor_oid.clone()).unwrap(),
        "audio/mpeg",
        None,
    )
    .unwrap();

    let mpeg = blob.annotate("Audio/MPEG", Some("MP2")).unwrap();
    assert_eq!(mpeg.media_type(), "audio/mpeg");
    assert_eq!(mpeg.suffix(), Some("mp2"));
    assert_eq!(mpeg.content_identity(), identity);
    assert_eq!(mpeg.descriptor_oid(), Some(&descriptor_oid));
    assert!(!mpeg.is_hydrated());

    let javascript = blob
        .annotate("Application/JavaScript;CHARSET=\"UTF-8\"", Some("JS"))
        .unwrap();
    assert_eq!(javascript.media_type(), "text/javascript;charset=utf-8");
    assert_eq!(javascript.suffix(), None, "preferred suffix is stored as no hint");
    assert_eq!(javascript.content_identity(), identity);

    let gzip = blob.annotate("application/gzip", Some("TAR.GZ")).unwrap();
    assert_eq!(gzip.suffix(), Some("tar.gz"));
    assert_eq!(
        blob.annotate("image/jpeg", Some("png")),
        Err(Error::IncompatibleSuffix)
    );
    assert_eq!(
        blob.annotate("image/jpeg", Some(".jpg")),
        Err(Error::InvalidSuffix)
    );
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);
}

#[test]
fn annotation_rewrite_preserves_ovb2_and_rov3_sov3_identity_fields() {
    let bytes = b"annotated payload";
    let identity = ContentIdentity::from_bytes(bytes);
    let context = test_context(counting_resolver(bytes));
    let descriptor_oid = NativeOid::from_bytes(&[0x55; 20]).unwrap();
    let reference = context.reference(identity, descriptor_oid.clone()).unwrap();
    let rov3_blob = Blob::from_reference_with_annotation(reference, "audio/mpeg", None).unwrap();
    let sov3_blob =
        Blob::from_semantic_commitment_in_context(identity, &context, "audio/mpeg", None).unwrap();
    let rov3 = ContextValue::from_blob(&rov3_blob, ValueFormat::Rov3).unwrap();
    let sov3 = ContextValue::from_blob(&sov3_blob, ValueFormat::Sov3).unwrap();

    let annotated_rov3 = rov3
        .with_blob_annotation("Audio/MPEG", Some("MP2"))
        .unwrap();
    let annotated_sov3 = sov3
        .with_blob_annotation("Audio/MPEG", Some("MP2"))
        .unwrap();
    let Raw::Tag(ROV3_BLOB_TAG, before_rov3) = rov3.raw() else {
        panic!("expected ROV-3 Blob");
    };
    let Raw::Tag(ROV3_BLOB_TAG, after_rov3) = annotated_rov3.raw() else {
        panic!("expected annotated ROV-3 Blob");
    };
    let (Raw::Array(before_rov3), Raw::Array(after_rov3)) =
        (before_rov3.as_ref(), after_rov3.as_ref())
    else {
        panic!("expected ROV-3 field arrays");
    };
    assert_eq!(before_rov3[0], after_rov3[0]);
    assert_eq!(before_rov3[1], after_rov3[1]);
    assert_eq!(before_rov3[4], after_rov3[4]);

    let Raw::Tag(SOV3_BLOB_TAG, before_sov3) = sov3.raw() else {
        panic!("expected SOV-3 Blob");
    };
    let Raw::Tag(SOV3_BLOB_TAG, after_sov3) = annotated_sov3.raw() else {
        panic!("expected annotated SOV-3 Blob");
    };
    let (Raw::Array(before_sov3), Raw::Array(after_sov3)) =
        (before_sov3.as_ref(), after_sov3.as_ref())
    else {
        panic!("expected SOV-3 field arrays");
    };
    assert_eq!(before_sov3[0], after_sov3[0]);
    assert_eq!(before_sov3[1], after_sov3[1]);

    let rov3_round_trip =
        decode_rov3_in_context(&annotated_rov3.encode().unwrap(), &context).unwrap();
    let sov3_round_trip =
        decode_sov3_in_context(&annotated_sov3.encode().unwrap(), &context).unwrap();
    assert_eq!(rov3_round_trip.content_identity(), identity);
    assert_eq!(rov3_round_trip.descriptor_oid(), Some(&descriptor_oid));
    assert_eq!(rov3_round_trip.suffix(), Some("mp2"));
    assert_eq!(sov3_round_trip.content_identity(), identity);
    assert_eq!(sov3_round_trip.suffix(), Some("mp2"));

    let ovb2_blob = Blob::from_bytes_with_annotation(bytes.to_vec(), "audio/mpeg", None).unwrap();
    let ovb2 = ContextValue::from_blob(&ovb2_blob, ValueFormat::Ovb2).unwrap();
    let annotated_ovb2 = ovb2
        .with_blob_annotation("Audio/MPEG", Some("MP2"))
        .unwrap();
    let Raw::Tag(OVB2_BLOB_TAG, before_ovb2) = ovb2.raw() else {
        panic!("expected OVB-2 Blob");
    };
    let Raw::Tag(OVB2_BLOB_TAG, after_ovb2) = annotated_ovb2.raw() else {
        panic!("expected annotated OVB-2 Blob");
    };
    let (Raw::Array(before_ovb2), Raw::Array(after_ovb2)) =
        (before_ovb2.as_ref(), after_ovb2.as_ref())
    else {
        panic!("expected OVB-2 field arrays");
    };
    assert_eq!(before_ovb2[0], after_ovb2[0]);
    assert_eq!(annotated_ovb2.blob().unwrap().content_identity(), identity);
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
            &test_context(counting_resolver(b"abc")),
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
    let authorized_context = test_context(counting_resolver(b"abc"));
    let authorized_blob = Blob::from_reference(
        authorized_context
            .reference(identity, NativeOid::from_bytes(&[7; 20]).unwrap())
            .unwrap(),
    )
    .unwrap();
    let encoded = encode_rov3(&authorized_blob).unwrap();
    assert_eq!(decode_rov3(&encoded), Err(Error::InvalidContext));

    let resolver = counting_resolver(b"abc");
    let database = [4; 16];
    let owner = OwnerLifetime::new([5; 16]);
    let quota = Format3Quota::new(8 * 1024 * 1024, 1024).unwrap();
    let valid_pin = Snapshot::cwd(database, [6; 16], 0.into()).unwrap();
    let mut forged_pin = valid_pin.clone();
    let Snapshot::Cwd { id, .. } = &mut forged_pin else {
        unreachable!();
    };
    *id = [0xa5; 32];
    assert!(matches!(
        Format3Context::from_persisted(
            database,
            forged_pin,
            GitHash::Sha1,
            NativeOid::from_bytes(&[9; 20]).unwrap(),
            NativeOid::from_bytes(&[8; 20]).unwrap(),
            AvailabilityState::Available,
            owner.clone(),
            quota,
            resolver.clone(),
        ),
        Err(Error::InvalidContext)
    ));
    assert!(matches!(
        Format3Context::from_persisted(
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
    assert!(matches!(
        Format3Context::from_persisted(
            database,
            valid_pin,
            GitHash::Sha1,
            NativeOid::from_bytes(&[9; 32]).unwrap(),
            NativeOid::from_bytes(&[8; 20]).unwrap(),
            AvailabilityState::Available,
            OwnerLifetime::new([5; 16]),
            quota,
            resolver.clone(),
        ),
        Err(Error::InvalidContext)
    ));

    let context = test_context(resolver.clone());
    let reference = context
        .reference(identity, NativeOid::from_bytes(&[1; 20]).unwrap())
        .unwrap();
    let blob = Blob::from_reference(reference).unwrap();
    context.cancel();
    assert_eq!(blob.read(0, 3), Err(Error::Cancelled));
    assert_eq!(resolver.reads.load(Ordering::SeqCst), 0);
}

#[test]
fn rov3_oid_width_must_match_the_bound_context_algorithm() {
    let context = test_context(counting_resolver(b"abc"));
    let raw = Raw::Tag(
        ROV3_BLOB_TAG,
        Box::new(Raw::Array(vec![
            Raw::Int(3.into()),
            Raw::Bytes(ContentIdentity::from_bytes(b"abc").sha256().to_vec()),
            Raw::Text("text/plain".to_owned()),
            Raw::Null,
            Raw::Bytes(vec![7; 32]),
        ])),
    );
    assert_eq!(
        ContextValue::new_in_context(&context, ValueFormat::Rov3, raw),
        Err(Error::InvalidOid)
    );
}

#[test]
fn expiration_is_monotonic_and_cannot_restore_availability() {
    let context = test_context(counting_resolver(b"abc"));
    let reference = context
        .reference(
            ContentIdentity::from_bytes(b"abc"),
            NativeOid::from_bytes(&[1; 20]).unwrap(),
        )
        .unwrap();
    let blob = Blob::from_reference(reference).unwrap();
    context.expire();
    assert_eq!(context.availability(), AvailabilityState::Expired);
    assert_eq!(blob.read(0, 1), Err(Error::OwnerExpired));
}

#[test]
fn full_reads_and_content_comparisons_honor_small_read_quotas() {
    let quota = Format3Quota::new(2, 1_u64 << 62).unwrap();
    let left_resolver = counting_resolver(b"payload");
    let right_resolver = counting_resolver(b"payload");
    let identity = ContentIdentity::from_bytes(b"payload");
    let left_context = test_context_with_quota(left_resolver.clone(), quota);
    let right_context = test_context_with_quota(right_resolver.clone(), quota);
    let left = Blob::from_reference(
        left_context
            .reference(identity, NativeOid::from_bytes(&[1; 20]).unwrap())
            .unwrap(),
    )
    .unwrap();
    let right = Blob::from_reference(
        right_context
            .reference(identity, NativeOid::from_bytes(&[2; 20]).unwrap())
            .unwrap(),
    )
    .unwrap();

    assert_eq!(left.read_to_end().unwrap(), b"payload");
    assert_eq!(left.same_content(&right).unwrap(), true);
    assert!(left_resolver.max_request.load(Ordering::SeqCst) <= 2);
    assert!(right_resolver.max_request.load(Ordering::SeqCst) <= 2);
}

#[test]
fn lazy_reference_metadata_and_semantic_encoding_do_not_read_payload() {
    let resolver = counting_resolver(b"payload");
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

struct CountingResolver {
    bytes: Vec<u8>,
    reads: AtomicUsize,
    max_request: AtomicUsize,
}

impl BlobResolver for CountingResolver {
    fn read_range(
        &self,
        _reference: &ContentReference,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.max_request
            .fetch_max(length as usize, Ordering::SeqCst);
        let start = offset as usize;
        let end = start + length as usize;
        Ok(self.bytes[start..end].to_vec())
    }
}
