use super::*;
#[test]
fn constructed_collection_values_stay_closed_to_the_legacy_orv_encoders() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    for value in constructed_collection_values(&active) {
        assert_eq!(encode_value(&value), Err(ValueCodecError::UnsupportedValue));
        assert_eq!(
            encode_catalogue_value(active.catalogue(), &value),
            Err(ValueCodecError::UnsupportedValue)
        );
        assert_eq!(
            encode_active_value(&active, &value),
            Err(ValueCodecError::UnsupportedValue)
        );
        assert_eq!(
            encode_registered_value(&active, &registry, &value),
            Err(ValueCodecError::UnsupportedValue)
        );
    }
}

#[test]
fn orv5_round_trips_a_checked_option_with_independent_exact_bytes() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let descriptor = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let value = RuntimeValue::option(
        &active,
        descriptor.clone(),
        Some(RuntimeValue::Boolean(true)),
    )
    .unwrap();

    let mut expected = b"ORV5".to_vec();
    expected.push(0x0d);
    expected.extend_from_slice(&[0; 16]);
    expected.extend_from_slice(&51_u32.to_be_bytes());
    expected.extend_from_slice(&18_u16.to_be_bytes());
    expected.push(0x04);
    expected.push(0x00);
    expected.extend_from_slice(&[0; 15]);
    expected.push(0x01);
    expected.push(0x01);
    expected.extend_from_slice(&26_u32.to_be_bytes());
    expected.extend_from_slice(b"ORV5");
    expected.push(0x02);
    expected.extend_from_slice(&[0; 15]);
    expected.push(0x01);
    expected.extend_from_slice(&1_u32.to_be_bytes());
    expected.push(0x01);

    let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
    assert_eq!(encoded, expected);

    let decoded = decode_constructed_value(&active, &registry, &encoded).unwrap();
    assert_eq!(decoded, value);
}

#[test]
fn orv5_round_trips_all_admitted_constructors_and_rejects_hostile_option_bytes() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();

    for value in constructed_collection_values(&active) {
        let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
        assert_eq!(&encoded[..4], b"ORV5");
        assert_eq!(
            decode_constructed_value(&active, &registry, &encoded),
            Ok(value)
        );
    }

    let descriptor = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let option =
        RuntimeValue::option(&active, descriptor, Some(RuntimeValue::Boolean(true))).unwrap();
    let encoded = encode_constructed_value(&active, &registry, &option).unwrap();

    let mut identity = encoded.clone();
    identity[5] = 1;
    assert_eq!(
        decode_constructed_value(&active, &registry, &identity),
        Err(ValueCodecError::ConstructedTypeIdentityNotZero {
            identity: TypeId::from_bytes([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        })
    );

    let mut descriptor_tag = encoded.clone();
    descriptor_tag[27] = 0xff;
    assert_eq!(
        decode_constructed_value(&active, &registry, &descriptor_tag),
        Err(ValueCodecError::UnknownConstructedDescriptorTag { tag: 0xff })
    );

    let mut presence = encoded.clone();
    presence[45] = 2;
    assert_eq!(
        decode_constructed_value(&active, &registry, &presence),
        Err(ValueCodecError::InvalidOptionPresence { value: 2 })
    );

    let mut child_marker = encoded;
    child_marker[50..54].copy_from_slice(b"ORV4");
    assert_eq!(
        decode_constructed_value(&active, &registry, &child_marker),
        Err(ValueCodecError::ConstructedChild {
            path: vec![CollectionValuePathSegment::OptionChild],
            source: Box::new(ValueCodecError::InvalidMarker),
        })
    );
}

#[test]
fn orv5_admits_the_descriptor_before_the_body_and_wraps_nested_option_body_errors() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();

    let mut inactive_payload = Vec::new();
    inactive_payload.extend_from_slice(&18_u16.to_be_bytes());
    inactive_payload.push(0x04);
    inactive_payload.push(0x00);
    inactive_payload.extend_from_slice(&[0xfe; 16]);
    inactive_payload.push(0x02);
    let inactive = orv5_constructed(inactive_payload);
    let error = decode_constructed_value(&active, &registry, &inactive).unwrap_err();
    assert!(matches!(
        error,
        ValueCodecError::CollectionValue {
            source: CollectionValueError::UnsupportedDescriptor { .. },
        }
    ));

    let mut inner_payload = Vec::new();
    inner_payload.extend_from_slice(&18_u16.to_be_bytes());
    inner_payload.push(0x04);
    inner_payload.push(0x00);
    inner_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    inner_payload.push(0x02);
    let inner = orv5_constructed(inner_payload);

    let mut outer_payload = Vec::new();
    outer_payload.extend_from_slice(&19_u16.to_be_bytes());
    outer_payload.extend_from_slice(&[0x04, 0x04, 0x00]);
    outer_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    outer_payload.push(0x01);
    outer_payload.extend_from_slice(&(inner.len() as u32).to_be_bytes());
    outer_payload.extend_from_slice(&inner);
    let outer = orv5_constructed(outer_payload);
    assert_eq!(
        decode_constructed_value(&active, &registry, &outer),
        Err(ValueCodecError::ConstructedChild {
            path: vec![CollectionValuePathSegment::OptionChild],
            source: Box::new(ValueCodecError::InvalidOptionPresence { value: 2 }),
        })
    );

    let mut valid_inner_payload = Vec::new();
    valid_inner_payload.extend_from_slice(&18_u16.to_be_bytes());
    valid_inner_payload.push(0x04);
    valid_inner_payload.push(0x00);
    valid_inner_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    valid_inner_payload.push(0x00);
    let valid_inner = orv5_constructed(valid_inner_payload);
    let mut trailing_outer_payload = Vec::new();
    trailing_outer_payload.extend_from_slice(&19_u16.to_be_bytes());
    trailing_outer_payload.extend_from_slice(&[0x04, 0x04, 0x00]);
    trailing_outer_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    trailing_outer_payload.push(0x01);
    trailing_outer_payload.extend_from_slice(&(valid_inner.len() as u32).to_be_bytes());
    trailing_outer_payload.extend_from_slice(&valid_inner);
    trailing_outer_payload.push(0xff);
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(trailing_outer_payload),
        ),
        Err(ValueCodecError::TrailingBytes {
            declared: 51,
            actual: 52,
        })
    );

    let boolean = orv5_boolean(true);
    let mut trailing_inner_payload = Vec::new();
    trailing_inner_payload.extend_from_slice(&18_u16.to_be_bytes());
    trailing_inner_payload.push(0x04);
    trailing_inner_payload.push(0x00);
    trailing_inner_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    trailing_inner_payload.push(0x01);
    trailing_inner_payload.extend_from_slice(&(boolean.len() as u32).to_be_bytes());
    trailing_inner_payload.extend_from_slice(&boolean);
    trailing_inner_payload.push(0xff);
    let trailing_inner = orv5_constructed(trailing_inner_payload);
    let mut contained_outer_payload = Vec::new();
    contained_outer_payload.extend_from_slice(&19_u16.to_be_bytes());
    contained_outer_payload.extend_from_slice(&[0x04, 0x04, 0x00]);
    contained_outer_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    contained_outer_payload.push(0x01);
    contained_outer_payload.extend_from_slice(&(trailing_inner.len() as u32).to_be_bytes());
    contained_outer_payload.extend_from_slice(&trailing_inner);
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(contained_outer_payload),
        ),
        Err(ValueCodecError::ConstructedChild {
            path: vec![CollectionValuePathSegment::OptionChild],
            source: Box::new(ValueCodecError::TrailingBytes {
                declared: 31,
                actual: 32,
            }),
        })
    );
}

#[test]
fn orv5_public_tracers_retain_empty_nested_and_registered_values() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let option = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let list = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let map = TypeDescriptor::map(
        TypeDescriptor::named(INTEGER_TYPE_ID),
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
    )
    .unwrap();
    let nested = TypeDescriptor::list(option.clone()).unwrap();
    let opaque = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
    );
    let values = vec![
        RuntimeValue::option(&active, option.clone(), None).unwrap(),
        RuntimeValue::list(&active, list, Vec::new()).unwrap(),
        RuntimeValue::map(&active, map, Vec::new()).unwrap(),
        RuntimeValue::list(
            &active,
            nested,
            vec![
                RuntimeValue::option(&active, option.clone(), None).unwrap(),
                RuntimeValue::option(&active, option, Some(RuntimeValue::Boolean(true))).unwrap(),
            ],
        )
        .unwrap(),
        RuntimeValue::Integer(-7),
        opaque,
    ];
    for value in values {
        let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
        assert_eq!(
            decode_constructed_value(&active, &registry, &encoded),
            Ok(value)
        );
    }
}

#[test]
fn orv5_has_independent_list_and_map_goldens_and_rejects_noncanonical_map_order() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();

    let list_descriptor = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let list = RuntimeValue::list(
        &active,
        list_descriptor,
        vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
    )
    .unwrap();
    let mut expected_list = b"ORV5".to_vec();
    expected_list.push(0x0d);
    expected_list.extend_from_slice(&[0; 16]);
    expected_list.extend_from_slice(&84_u32.to_be_bytes());
    expected_list.extend_from_slice(&18_u16.to_be_bytes());
    expected_list.extend_from_slice(&[0x02, 0x00]);
    expected_list.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    expected_list.extend_from_slice(&2_u32.to_be_bytes());
    for value in [true, false] {
        let child = orv5_boolean(value);
        expected_list.extend_from_slice(&(child.len() as u32).to_be_bytes());
        expected_list.extend_from_slice(&child);
    }
    assert_eq!(
        encode_constructed_value(&active, &registry, &list),
        Ok(expected_list.clone())
    );
    assert_eq!(
        decode_constructed_value(&active, &registry, &expected_list),
        Ok(list)
    );

    let map_descriptor = TypeDescriptor::map(
        TypeDescriptor::named(INTEGER_TYPE_ID),
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
    )
    .unwrap();
    let map = RuntimeValue::map(
        &active,
        map_descriptor,
        vec![
            (RuntimeValue::Integer(2), RuntimeValue::Boolean(false)),
            (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap();
    let first = orv5_map_entry(orv5_integer(1), orv5_boolean(true));
    let second = orv5_map_entry(orv5_integer(2), orv5_boolean(false));
    let mut expected_map = b"ORV5".to_vec();
    expected_map.push(0x0d);
    expected_map.extend_from_slice(&[0; 16]);
    expected_map.extend_from_slice(&167_u32.to_be_bytes());
    expected_map.extend_from_slice(&35_u16.to_be_bytes());
    expected_map.push(0x03);
    expected_map.push(0x00);
    expected_map.extend_from_slice(&INTEGER_TYPE_ID.to_bytes());
    expected_map.push(0x00);
    expected_map.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    expected_map.extend_from_slice(&2_u32.to_be_bytes());
    expected_map.extend_from_slice(&first);
    expected_map.extend_from_slice(&second);
    assert_eq!(
        encode_constructed_value(&active, &registry, &map),
        Ok(expected_map.clone())
    );
    assert_eq!(
        decode_constructed_value(&active, &registry, &expected_map),
        Ok(map)
    );

    let mut noncanonical = expected_map[..expected_map.len() - first.len() - second.len()].to_vec();
    noncanonical.extend_from_slice(&second);
    noncanonical.extend_from_slice(&first);
    assert_eq!(
        decode_constructed_value(&active, &registry, &noncanonical),
        Err(ValueCodecError::NonCanonicalMapOrder { index: 0 })
    );
}

#[test]
fn orv6_round_trips_canonical_sets_and_rejects_noncanonical_or_unsupported_wire() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let descriptor = TypeDescriptor::set(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let value = RuntimeValue::set(
        &active,
        descriptor,
        vec![RuntimeValue::Boolean(true), RuntimeValue::Boolean(false)],
    )
    .unwrap();
    let encoded = encode_constructed_value(&active, &registry, &value).unwrap();
    assert_eq!(&encoded[..4], b"ORV6");
    assert_eq!(encoded[25], 0);
    assert_eq!(encoded[26], 18);
    assert_eq!(encoded[27], 0x05);
    assert_eq!(
        decode_constructed_value(&active, &registry, &encoded),
        Ok(value.clone())
    );
    assert_eq!(
        encode_constructed_value(&active, &registry, &value),
        Ok(encoded.clone())
    );

    let first_len = u32::from_be_bytes(encoded[49..53].try_into().unwrap()) as usize;
    let first_end = 53 + first_len;
    let second_len =
        u32::from_be_bytes(encoded[first_end..first_end + 4].try_into().unwrap()) as usize;
    let second_end = first_end + 4 + second_len;
    let mut noncanonical = encoded[..49].to_vec();
    noncanonical.extend_from_slice(&encoded[first_end..second_end]);
    noncanonical.extend_from_slice(&encoded[49..first_end]);
    assert_eq!(
        decode_constructed_value(&active, &registry, &noncanonical),
        Err(ValueCodecError::NonCanonicalSetOrder { index: 0 })
    );

    let mut duplicate = encoded[..49].to_vec();
    duplicate.extend_from_slice(&encoded[49..first_end]);
    duplicate.extend_from_slice(&encoded[49..first_end]);
    assert_eq!(
        decode_constructed_value(&active, &registry, &duplicate),
        Err(ValueCodecError::CollectionValue {
            source: CollectionValueError::DuplicateSetElement {
                first: 0,
                duplicate: 1,
            },
        })
    );

    let mut nested_descriptor = vec![0x05, 0x02];
    nested_descriptor.extend_from_slice(&orv5_named_descriptor(BOOLEAN_TYPE_ID));
    let nested = orv6_constructed(orv5_descriptor_payload(
        &nested_descriptor,
        &0_u32.to_be_bytes(),
    ));
    assert!(matches!(
        decode_constructed_value(&active, &registry, &nested),
        Err(ValueCodecError::CollectionValue {
            source: CollectionValueError::UnsupportedDescriptor { .. }
        })
    ));

    let mut orv5_set = encoded.clone();
    orv5_set[..4].copy_from_slice(b"ORV5");
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_set),
        Err(ValueCodecError::UnknownConstructedDescriptorTag { tag: 0x05 })
    );
}

#[test]
fn orv5_enforces_descriptor_and_value_node_limits_before_later_body_failures() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();

    let mut deep_payload = Vec::new();
    deep_payload.extend_from_slice(&50_u16.to_be_bytes());
    deep_payload.extend(std::iter::repeat_n(0x04, 33));
    deep_payload.push(0x00);
    deep_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    deep_payload.push(0x00);
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_constructed(deep_payload)),
        Err(ValueCodecError::InvalidConstructedDescriptor {
            source: TypeDescriptorError::TooDeep {
                maximum: MAX_TYPE_DESCRIPTOR_DEPTH,
                actual: MAX_TYPE_DESCRIPTOR_DEPTH + 1,
            },
        })
    );

    let leaf = orv5_boolean(false);
    let mut at_limit_payload = orv5_boolean_list_prefix(65_535);
    for _ in 0..65_535 {
        at_limit_payload.extend_from_slice(&(leaf.len() as u32).to_be_bytes());
        at_limit_payload.extend_from_slice(&leaf);
    }
    assert!(
        decode_constructed_value(&active, &registry, &orv5_constructed(at_limit_payload),).is_ok()
    );

    let mut over_limit_payload = orv5_boolean_list_prefix(65_537);
    for _ in 0..65_536 {
        over_limit_payload.extend_from_slice(&(leaf.len() as u32).to_be_bytes());
        over_limit_payload.extend_from_slice(&leaf);
    }
    over_limit_payload.extend_from_slice(&25_u32.to_be_bytes());
    over_limit_payload.extend_from_slice(b"ORV5");
    over_limit_payload.push(0x02);
    over_limit_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    over_limit_payload.extend_from_slice(&1_u32.to_be_bytes());
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_constructed(over_limit_payload),),
        Err(ValueCodecError::CollectionValue {
            source: CollectionValueError::TooManyNodes {
                maximum: MAX_RUNTIME_VALUE_NODES,
            },
        })
    );
}

#[test]
fn orv5_reports_each_constructed_structure_failure_exactly() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_constructed(Vec::new())),
        Err(ValueCodecError::TruncatedConstructedHeader { actual: 0 })
    );
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_constructed(vec![0])),
        Err(ValueCodecError::TruncatedConstructedHeader { actual: 1 })
    );
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_constructed(vec![0, 0])),
        Err(ValueCodecError::EmptyConstructedDescriptor)
    );
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_constructed(vec![0, 3, 0x04])),
        Err(ValueCodecError::TruncatedConstructedDescriptor {
            declared: 3,
            available: 1,
        })
    );
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_constructed(vec![0, 1, 0x00])),
        Err(ValueCodecError::TruncatedConstructedDescriptorNode {
            offset: 0,
            required: 17,
            available: 1,
        })
    );
    assert_eq!(
        decode_constructed_value(&active, &registry, &orv5_constructed(vec![0, 1, 0x04])),
        Err(ValueCodecError::TruncatedConstructedDescriptorNode {
            offset: 1,
            required: 1,
            available: 0,
        })
    );
    let mut trailing_descriptor = orv5_named_descriptor(BOOLEAN_TYPE_ID);
    trailing_descriptor.push(0xff);
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&trailing_descriptor, &[])),
        ),
        Err(ValueCodecError::TrailingConstructedDescriptor { remaining: 1 })
    );

    let mut list_descriptor = vec![0x02];
    list_descriptor.extend_from_slice(&orv5_named_descriptor(BOOLEAN_TYPE_ID));
    let mut map_descriptor = vec![0x03];
    map_descriptor.extend_from_slice(&orv5_named_descriptor(INTEGER_TYPE_ID));
    map_descriptor.extend_from_slice(&orv5_named_descriptor(BOOLEAN_TYPE_ID));
    for (descriptor, child_path) in [
        (
            list_descriptor.as_slice(),
            vec![CollectionValuePathSegment::ListElement(0)],
        ),
        (
            map_descriptor.as_slice(),
            vec![CollectionValuePathSegment::MapKey(0)],
        ),
    ] {
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(descriptor, &[])),
            ),
            Err(ValueCodecError::TruncatedCollectionEntry { path: Vec::new() })
        );
        let mut truncated_header = 1_u32.to_be_bytes().to_vec();
        truncated_header.extend_from_slice(&[0; 3]);
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(descriptor, &truncated_header)),
            ),
            Err(ValueCodecError::TruncatedCollectionEntry {
                path: child_path.clone(),
            })
        );
        let mut truncated_region = 1_u32.to_be_bytes().to_vec();
        truncated_region.extend_from_slice(&26_u32.to_be_bytes());
        truncated_region.extend_from_slice(&[0; 25]);
        assert_eq!(
            decode_constructed_value(
                &active,
                &registry,
                &orv5_constructed(orv5_descriptor_payload(descriptor, &truncated_region)),
            ),
            Err(ValueCodecError::TruncatedCollectionEntry { path: child_path })
        );
    }

    let mut map_value_header = 1_u32.to_be_bytes().to_vec();
    map_value_header.extend_from_slice(&orv5_map_entry(orv5_integer(1), Vec::new()));
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&map_descriptor, &map_value_header)),
        ),
        Err(ValueCodecError::TruncatedCollectionEntry {
            path: vec![CollectionValuePathSegment::MapValue(0)],
        })
    );
    let mut map_value_region = 1_u32.to_be_bytes().to_vec();
    map_value_region.extend_from_slice(&(orv5_integer(1).len() as u32).to_be_bytes());
    map_value_region.extend_from_slice(&orv5_integer(1));
    map_value_region.extend_from_slice(&26_u32.to_be_bytes());
    map_value_region.extend_from_slice(&[0; 25]);
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&map_descriptor, &map_value_region)),
        ),
        Err(ValueCodecError::TruncatedCollectionEntry {
            path: vec![CollectionValuePathSegment::MapValue(0)],
        })
    );

    let mut short_child = 1_u32.to_be_bytes().to_vec();
    short_child.extend_from_slice(&24_u32.to_be_bytes());
    short_child.extend_from_slice(&[0; 24]);
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&list_descriptor, &short_child)),
        ),
        Err(ValueCodecError::TruncatedCollectionEntry {
            path: vec![CollectionValuePathSegment::ListElement(0)],
        })
    );
    let mut maximum_child = 1_u32.to_be_bytes().to_vec();
    maximum_child.extend_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&list_descriptor, &maximum_child)),
        ),
        Err(ValueCodecError::TruncatedCollectionEntry {
            path: vec![CollectionValuePathSegment::ListElement(0)],
        })
    );

    let maximum_count = u32::MAX.to_be_bytes().to_vec();
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&list_descriptor, &maximum_count)),
        ),
        Err(ValueCodecError::TruncatedCollectionEntry {
            path: vec![CollectionValuePathSegment::ListElement(0)],
        })
    );
    let mut oversized_header = b"ORV5".to_vec();
    oversized_header.push(0x0d);
    oversized_header.extend_from_slice(&[0; 16]);
    oversized_header.extend_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode_constructed_value(&active, &registry, &oversized_header),
        Err(ValueCodecError::PayloadTooLarge {
            actual: u32::MAX as usize,
            maximum: PAYLOAD_LIMIT,
        })
    );
}

#[test]
fn orv5_marker_substitution_covers_every_accepted_orv4_value_family() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let reference_target = TypeId::from_bytes([0x41; 16]);
    let values = vec![
        RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
        RuntimeValue::Boolean(true),
        RuntimeValue::Integer(-7),
        RuntimeValue::BigInt(-9),
        RuntimeValue::Float(RuntimeFloat::new(1.5).unwrap()),
        RuntimeValue::Text(String::from("literal ORV4 text payload")),
        RuntimeValue::Bytes(b"literal ORV4 byte payload".to_vec()),
        RuntimeValue::null(ResolvedType::reference(reference_target)).unwrap(),
        RuntimeValue::Reference {
            target: reference_target,
            object: ObjectId::from_bytes([0x42; 16]),
        },
        RuntimeValue::null(ResolvedType::named(ENUM_TYPE)).unwrap(),
        RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap()),
        RuntimeValue::Opaque(
            OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
        ),
    ];
    for value in values {
        assert_orv4_to_orv5_flat_marker_substitution(&active, &registry, value);
    }

    let nested_active = active_nested_record_revision();
    let nested_registry =
        registered_opaque_codecs(nested_active.catalogue_hash_context().standard().unwrap())
            .unwrap();
    let nested_value = nested_record_value(&nested_active);
    let inner_type = TypeId::from_bytes([0x31; 16]);
    let inner_field = FieldId::from_bytes([0x3a; 16]);
    let version_four = assemble_nested_envelope(b"ORV4", 0x0b, inner_type, inner_field, 1, 26, &[]);
    let expected = assemble_nested_envelope(b"ORV5", 0x0b, inner_type, inner_field, 1, 26, &[]);
    assert_eq!(
        encode_registered_value(&nested_active, &nested_registry, &nested_value),
        Ok(version_four)
    );
    assert_eq!(
        encode_constructed_value(&nested_active, &nested_registry, &nested_value),
        Ok(expected.clone())
    );
    assert_eq!(
        decode_constructed_value(&nested_active, &nested_registry, &expected),
        Ok(nested_value)
    );
}

#[test]
fn orv5_rechecks_stale_enum_reference_standard_and_opaque_authorities() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();

    let enum_value =
        RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap());
    let mut stale_enum = encode_constructed_value(&active, &registry, &enum_value).unwrap();
    stale_enum[25..34].copy_from_slice(b"obsolete!");
    assert_eq!(
        decode_constructed_value(&active, &registry, &stale_enum),
        Err(ValueCodecError::UndeclaredEnumLabel {
            enum_type: ENUM_TYPE,
            label: String::from("obsolete!"),
        })
    );

    let stale_reference_target = TypeId::from_bytes([0x74; 16]);
    let mut reference_descriptor = vec![0x04, 0x01];
    reference_descriptor.extend_from_slice(&stale_reference_target.to_bytes());
    let reference_error = decode_constructed_value(
        &active,
        &registry,
        &orv5_constructed(orv5_descriptor_payload(&reference_descriptor, &[0])),
    )
    .unwrap_err();
    let ValueCodecError::CollectionValue {
        source: CollectionValueError::UnsupportedDescriptor { path, descriptor },
    } = reference_error
    else {
        panic!("a stale reference target must fail collection admission");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::OptionChild]);
    assert_eq!(
        descriptor,
        TypeDescriptor::reference(stale_reference_target)
    );

    let opaque = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
    );
    let encoded_opaque = encode_constructed_value(&active, &registry, &opaque).unwrap();
    assert_eq!(
        decode_constructed_value(
            &active_revision_without_standard(),
            &registry,
            &encoded_opaque
        ),
        Err(ValueCodecError::OpaqueValue {
            source: OpaqueValueError::ActiveStandardRequired,
        })
    );
    let alternate_active = active_record_revision_with_types_and_standard(
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
        TypeDescriptor::named(ENUM_TYPE),
        alternate_verified_standard(),
    );
    assert_eq!(
        decode_constructed_value(&alternate_active, &registry, &encoded_opaque),
        Err(ValueCodecError::OpaqueValue {
            source: OpaqueValueError::ActiveStandardMismatch,
        })
    );
    let invalid_registration = orna_core::value::OpaqueCodecRegistration::fixed_length_identity(
        OPAQUE_TOKEN_TYPE_ID,
        QualifiedSemanticName::new(["std", "types", "opaque_token"]).unwrap(),
        "orna.std.value.opaque-token@2",
        16,
    )
    .unwrap();
    assert!(matches!(
        OpaqueCodecRegistry::new(
            active.catalogue_hash_context().standard().unwrap(),
            [invalid_registration],
        ),
        Err(
            orna_core::value::OpaqueCodecRegistryError::ContractMismatch {
                opaque_type: OPAQUE_TOKEN_TYPE_ID,
            }
        )
    ));
    let mut wrong_contract = encoded_opaque;
    wrong_contract[21..25].copy_from_slice(&15_u32.to_be_bytes());
    wrong_contract.pop();
    assert_eq!(
        decode_constructed_value(&active, &registry, &wrong_contract),
        Err(ValueCodecError::OpaqueValue {
            source: OpaqueValueError::WrongPayloadLength {
                opaque_type: OPAQUE_TOKEN_TYPE_ID,
                expected: 16,
                actual: 15,
            },
        })
    );
}

#[test]
fn orv5_cross_catalogue_collision_precedes_opaque_category_rejection() {
    let active = active_revision_with_standard_named_collision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let mut descriptor = vec![0x02];
    descriptor.extend_from_slice(&orv5_named_descriptor(OPAQUE_TOKEN_TYPE_ID));
    let error = decode_constructed_value(
        &active,
        &registry,
        &orv5_constructed(orv5_descriptor_payload(&descriptor, &0_u32.to_be_bytes())),
    )
    .unwrap_err();
    let ValueCodecError::CollectionValue {
        source: CollectionValueError::AmbiguousNamedType { path, type_id },
    } = error
    else {
        panic!("cross-catalogue identity collision must precede opaque rejection");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
    assert_eq!(type_id, OPAQUE_TOKEN_TYPE_ID);
}

#[test]
fn orv5_map_permutations_encode_to_the_same_canonical_bytes() {
    let active = active_record_revision();
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    let descriptor = TypeDescriptor::map(
        TypeDescriptor::named(INTEGER_TYPE_ID),
        TypeDescriptor::named(BOOLEAN_TYPE_ID),
    )
    .unwrap();
    let canonical = RuntimeValue::map(
        &active,
        descriptor.clone(),
        vec![
            (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
            (RuntimeValue::Integer(2), RuntimeValue::Boolean(false)),
        ],
    )
    .unwrap();
    let permuted = RuntimeValue::map(
        &active,
        descriptor,
        vec![
            (RuntimeValue::Integer(2), RuntimeValue::Boolean(false)),
            (RuntimeValue::Integer(1), RuntimeValue::Boolean(true)),
        ],
    )
    .unwrap();
    assert_eq!(
        encode_constructed_value(&active, &registry, &canonical),
        encode_constructed_value(&active, &registry, &permuted)
    );
}

#[test]
fn orv5_retains_legacy_bytes_and_keeps_markers_closed() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();

    let legacy = RuntimeValue::Boolean(true);
    let version_four = encode_registered_value(&active, &registry, &legacy).unwrap();
    let version_five = encode_constructed_value(&active, &registry, &legacy).unwrap();
    assert_eq!(&version_four[..4], b"ORV4");
    assert_eq!(&version_five[..4], b"ORV5");
    assert_eq!(&version_five[4..], &version_four[4..]);
    assert_eq!(
        decode_constructed_value(&active, &registry, &version_five),
        Ok(legacy)
    );

    assert_eq!(
        decode_constructed_value(&active, &registry, &version_four),
        Err(ValueCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_registered_value(&active, &registry, &version_five),
        Err(ValueCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_value(&version_five),
        Err(ValueCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_catalogue_value(active.catalogue(), &version_five),
        Err(ValueCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_active_value(&active, &version_five),
        Err(ValueCodecError::InvalidMarker)
    );
    for marker in [b"ORV1", b"ORV2", b"ORV3", b"ORV4"] {
        let mut crossed = version_five.clone();
        crossed[..4].copy_from_slice(marker);
        assert_eq!(
            decode_constructed_value(&active, &registry, &crossed),
            Err(ValueCodecError::InvalidMarker)
        );
    }

    let opaque = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
    );
    let opaque_version_four = encode_registered_value(&active, &registry, &opaque).unwrap();
    let opaque_version_five = encode_constructed_value(&active, &registry, &opaque).unwrap();
    assert_eq!(&opaque_version_five[4..], &opaque_version_four[4..]);
    assert_eq!(
        decode_constructed_value(&active, &registry, &opaque_version_five),
        Ok(opaque)
    );
}

#[test]
fn orv5_accepts_exact_depth_and_parses_the_256_node_descriptor_before_rejection() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();

    let mut depth_bytes = vec![0x04; MAX_TYPE_DESCRIPTOR_DEPTH];
    let mut depth_descriptor = TypeDescriptor::named(BOOLEAN_TYPE_ID);
    for _ in 0..MAX_TYPE_DESCRIPTOR_DEPTH {
        depth_descriptor = TypeDescriptor::option(depth_descriptor).unwrap();
    }
    depth_bytes.push(0x00);
    depth_bytes.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&depth_bytes, &[0])),
        ),
        Ok(RuntimeValue::option(&active, depth_descriptor, None).unwrap())
    );

    let mut tree_bytes = orv5_named_descriptor(BOOLEAN_TYPE_ID);
    let mut tree_descriptor = TypeDescriptor::named(BOOLEAN_TYPE_ID);
    for _ in 0..7 {
        let child_bytes = tree_bytes.clone();
        tree_bytes = vec![0x03];
        tree_bytes.extend_from_slice(&child_bytes);
        tree_bytes.extend_from_slice(&child_bytes);
        tree_descriptor = TypeDescriptor::map(tree_descriptor.clone(), tree_descriptor).unwrap();
    }
    let mut maximum_bytes = vec![0x04];
    maximum_bytes.extend_from_slice(&tree_bytes);
    let _maximum_descriptor = TypeDescriptor::option(tree_descriptor).unwrap();
    assert_eq!(maximum_bytes.len(), 2_304);
    let maximum_error = decode_constructed_value(
        &active,
        &registry,
        &orv5_constructed(orv5_descriptor_payload(&maximum_bytes, &[0])),
    )
    .unwrap_err();
    let ValueCodecError::CollectionValue {
        source: CollectionValueError::UnsupportedDescriptor { path, .. },
    } = maximum_error
    else {
        panic!("the 256-node descriptor must parse before collection admission rejects it");
    };
    assert_eq!(
        path.segments(),
        &[
            CollectionValuePathSegment::OptionChild,
            CollectionValuePathSegment::MapKeyChild,
        ]
    );

    let mut too_large_bytes = vec![0x04];
    too_large_bytes.extend_from_slice(&maximum_bytes);
    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&too_large_bytes, &[0])),
        ),
        Err(ValueCodecError::InvalidConstructedDescriptor {
            source: TypeDescriptorError::TooLarge {
                maximum: 256,
                actual: 257,
            },
        })
    );
}

#[test]
fn orv5_map_duplicate_keys_keep_original_wire_indexes() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let mut descriptor = vec![0x03];
    descriptor.extend_from_slice(&orv5_named_descriptor(INTEGER_TYPE_ID));
    descriptor.extend_from_slice(&orv5_named_descriptor(BOOLEAN_TYPE_ID));
    let mut body = 3_u32.to_be_bytes().to_vec();
    body.extend_from_slice(&orv5_map_entry(orv5_integer(2), orv5_boolean(false)));
    body.extend_from_slice(&orv5_map_entry(orv5_integer(1), orv5_boolean(true)));
    body.extend_from_slice(&orv5_map_entry(orv5_integer(2), orv5_boolean(true)));

    assert_eq!(
        decode_constructed_value(
            &active,
            &registry,
            &orv5_constructed(orv5_descriptor_payload(&descriptor, &body)),
        ),
        Err(ValueCodecError::CollectionValue {
            source: CollectionValueError::DuplicateMapKey {
                first: 0,
                duplicate: 2,
            },
        })
    );
}

#[test]
fn orv5_revalidates_stale_records_and_rejects_unregistered_opaque_values() {
    let original = active_record_revision();
    let record = &original.catalogue().record_value_types()[0];
    let stale = RuntimeValue::Record(
        RecordValue::new(
            &original,
            record.id(),
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("verified"),
                    RuntimeValue::Enum(
                        EnumValue::new(original.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap(),
    );
    let active = active_record_revision_with_second_type(TypeDescriptor::named(BIGINT_TYPE_ID));
    let registry =
        registered_opaque_codecs(active.catalogue_hash_context().standard().unwrap()).unwrap();
    assert_eq!(
        encode_constructed_value(&active, &registry, &stale),
        Err(ValueCodecError::RecordValueNotActive {
            record_type: record.id(),
        })
    );

    let opaque = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, [0x71; 16]).unwrap(),
    );
    let mut encoded = encode_constructed_value(&active, &registry, &opaque).unwrap();
    encoded[5..21].fill(0x72);
    assert_eq!(
        decode_constructed_value(&active, &registry, &encoded),
        Err(ValueCodecError::OpaqueValue {
            source: OpaqueValueError::UnregisteredType {
                opaque_type: TypeId::from_bytes([0x72; 16]),
            },
        })
    );

    let mut opaque_list_descriptor = vec![0x02];
    opaque_list_descriptor.extend_from_slice(&orv5_named_descriptor(OPAQUE_TOKEN_TYPE_ID));
    let opaque_child = encode_constructed_value(&active, &registry, &opaque).unwrap();
    let mut opaque_list_body = 1_u32.to_be_bytes().to_vec();
    opaque_list_body.extend_from_slice(&(opaque_child.len() as u32).to_be_bytes());
    opaque_list_body.extend_from_slice(&opaque_child);
    let error = decode_constructed_value(
        &active,
        &registry,
        &orv5_constructed(orv5_descriptor_payload(
            &opaque_list_descriptor,
            &opaque_list_body,
        )),
    )
    .unwrap_err();
    let ValueCodecError::CollectionValue {
        source: CollectionValueError::UnsupportedDescriptor { path, descriptor },
    } = error
    else {
        panic!("opaque collection leaves must stay closed");
    };
    assert_eq!(path.segments(), &[CollectionValuePathSegment::ListChild]);
    assert_eq!(descriptor, TypeDescriptor::named(OPAQUE_TOKEN_TYPE_ID));
}

#[test]
fn constructed_collection_values_stay_closed_to_both_orf_value_paths() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let parameter = ParameterId::from_bytes([0x5f; 16]);
    for value in constructed_collection_values(&active) {
        let argument = ClientFrame::CallArgument {
            stream: 7,
            parameter,
            value: value.clone(),
        };
        assert_eq!(
            encode_client_frame(&argument),
            Err(FrameCodecError::Value {
                source: ValueCodecError::UnsupportedValue,
            })
        );
        assert_eq!(
            encode_catalogue_client_frame(active.catalogue(), &argument),
            Err(FrameCodecError::Value {
                source: ValueCodecError::UnsupportedValue,
            })
        );
        assert_eq!(
            encode_active_client_frame(&active, &argument),
            Err(FrameCodecError::Value {
                source: ValueCodecError::UnsupportedValue,
            })
        );
        assert_eq!(
            encode_registered_client_frame(&active, &registry, &argument),
            Err(FrameCodecError::Value {
                source: ValueCodecError::UnsupportedValue,
            })
        );

        let batch = ServerFrame::EventBatch {
            stream: 7,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value),
            }],
        };
        assert_eq!(
            encode_server_frame(&batch),
            Err(FrameCodecError::Value {
                source: ValueCodecError::UnsupportedValue,
            })
        );
        assert_eq!(
            encode_catalogue_server_frame(active.catalogue(), &batch),
            Err(FrameCodecError::Value {
                source: ValueCodecError::UnsupportedValue,
            })
        );
        assert_eq!(
            encode_active_server_frame(&active, &batch),
            Err(FrameCodecError::Value {
                source: ValueCodecError::UnsupportedValue,
            })
        );
        assert_eq!(
            encode_registered_server_frame(&active, &registry, &batch),
            Err(FrameCodecError::Value {
                source: ValueCodecError::UnsupportedValue,
            })
        );
    }
}

#[test]
fn supported_flat_values_prove_the_orf_value_rejection_is_causal() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let parameter = ParameterId::from_bytes([0x5f; 16]);
    let argument = ClientFrame::CallArgument {
        stream: 7,
        parameter,
        value: RuntimeValue::Boolean(true),
    };
    assert!(encode_client_frame(&argument).is_ok());
    assert!(encode_catalogue_client_frame(active.catalogue(), &argument).is_ok());
    assert!(encode_active_client_frame(&active, &argument).is_ok());
    assert!(encode_registered_client_frame(&active, &registry, &argument).is_ok());

    let batch = ServerFrame::EventBatch {
        stream: 7,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(RuntimeValue::Boolean(true)),
        }],
    };
    assert!(encode_server_frame(&batch).is_ok());
    assert!(encode_catalogue_server_frame(active.catalogue(), &batch).is_ok());
    assert!(encode_active_server_frame(&active, &batch).is_ok());
    assert!(encode_registered_server_frame(&active, &registry, &batch).is_ok());
}

#[test]
fn orf5_retains_orf4_frames_and_embeds_orv5_values() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let parameter = ParameterId::from_bytes([0x71; 16]);
    let argument = ClientFrame::CallArgument {
        stream: 7,
        parameter,
        value: RuntimeValue::Boolean(true),
    };
    let value = orv5_boolean(true);
    let mut expected_argument = b"ORF5\x02\0".to_vec();
    expected_argument.extend_from_slice(&7_u64.to_be_bytes());
    expected_argument.extend_from_slice(&42_u32.to_be_bytes());
    expected_argument.extend_from_slice(&parameter.to_bytes());
    expected_argument.extend_from_slice(&value);
    assert_eq!(
        encode_constructed_client_frame(&active, &registry, &argument),
        Ok(expected_argument.clone())
    );
    assert_eq!(
        decode_constructed_client_frame(&active, &registry, &expected_argument),
        Ok(argument)
    );
    assert_eq!(
        decode_registered_client_frame(&active, &registry, &expected_argument),
        Err(FrameCodecError::InvalidMarker)
    );

    let event_frame = ServerFrame::EventBatch {
        stream: 7,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(RuntimeValue::Boolean(true)),
        }],
    };
    let mut expected_events = b"ORF5\x82\0".to_vec();
    expected_events.extend_from_slice(&7_u64.to_be_bytes());
    expected_events.extend_from_slice(&42_u32.to_be_bytes());
    expected_events.push(0x01);
    expected_events.extend_from_slice(&1_u16.to_be_bytes());
    expected_events.extend_from_slice(&1_u64.to_be_bytes());
    expected_events.push(0x01);
    expected_events.extend_from_slice(&26_u32.to_be_bytes());
    expected_events.extend_from_slice(&value);
    assert_eq!(
        encode_constructed_server_frame(&active, &registry, &event_frame),
        Ok(expected_events.clone())
    );
    assert_eq!(
        decode_constructed_server_frame(&active, &registry, &expected_events),
        Ok(event_frame)
    );
    assert_eq!(
        decode_registered_server_frame(&active, &registry, &expected_events),
        Err(FrameCodecError::InvalidMarker)
    );

    let client_non_value = ClientFrame::Ping {
        token: [1, 2, 3, 4, 5, 6, 7, 8],
    };
    let client_expected = orf5_frame(0x06, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(
        encode_constructed_client_frame(&active, &registry, &client_non_value),
        Ok(client_expected.clone())
    );
    assert_eq!(
        decode_constructed_client_frame(&active, &registry, &client_expected),
        Ok(client_non_value)
    );

    let server_non_value = ServerFrame::CallAccepted {
        stream: 1,
        invocation: InvocationId::from_bytes([0x72; 16]),
    };
    let server_expected = orf5_frame(0x81, 1, &[0x72; 16]);
    assert_eq!(
        encode_constructed_server_frame(&active, &registry, &server_non_value),
        Ok(server_expected.clone())
    );
    assert_eq!(
        decode_constructed_server_frame(&active, &registry, &server_expected),
        Ok(server_non_value)
    );

    let enum_frame = ServerFrame::EventBatch {
        stream: 9,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(RuntimeValue::Enum(
                EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
            )),
        }],
    };
    let mut enum_value = b"ORV5".to_vec();
    enum_value.push(0x0a);
    enum_value.extend_from_slice(&ENUM_TYPE.to_bytes());
    enum_value.extend_from_slice(&4_u32.to_be_bytes());
    enum_value.extend_from_slice(b"lead");
    let mut enum_payload = vec![0x01];
    enum_payload.extend_from_slice(&1_u16.to_be_bytes());
    enum_payload.extend_from_slice(&1_u64.to_be_bytes());
    enum_payload.push(0x01);
    enum_payload.extend_from_slice(&(enum_value.len() as u32).to_be_bytes());
    enum_payload.extend_from_slice(&enum_value);
    let enum_expected = orf5_frame(0x82, 9, &enum_payload);
    assert_eq!(
        encode_constructed_server_frame(&active, &registry, &enum_frame),
        Ok(enum_expected.clone())
    );
    assert_eq!(
        decode_constructed_server_frame(&active, &registry, &enum_expected),
        Ok(enum_frame)
    );

    for marker in [b"ORF1", b"ORF2", b"ORF3", b"ORF4"] {
        let mut crossed = expected_argument.clone();
        crossed[..4].copy_from_slice(marker);
        assert_eq!(
            decode_constructed_client_frame(&active, &registry, &crossed),
            Err(FrameCodecError::InvalidMarker)
        );
    }
    for marker in [b"ORF1", b"ORF2", b"ORF3", b"ORF4"] {
        let mut crossed = expected_events.clone();
        crossed[..4].copy_from_slice(marker);
        assert_eq!(
            decode_constructed_server_frame(&active, &registry, &crossed),
            Err(FrameCodecError::InvalidMarker)
        );
    }

    assert_eq!(
        decode_client_frame(&expected_argument),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_catalogue_client_frame(active.catalogue(), &expected_argument),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_active_client_frame(&active, &expected_argument),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_registered_client_frame(&active, &registry, &expected_argument),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_server_frame(&expected_events),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_catalogue_server_frame(active.catalogue(), &expected_events),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_active_server_frame(&active, &expected_events),
        Err(FrameCodecError::InvalidMarker)
    );
    assert_eq!(
        decode_registered_server_frame(&active, &registry, &expected_events),
        Err(FrameCodecError::InvalidMarker)
    );
}

#[test]
fn orf5_rejects_constructed_arguments_and_events_after_value_validation() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let descriptor = TypeDescriptor::option(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let value = RuntimeValue::option(
        &active,
        descriptor.clone(),
        Some(RuntimeValue::Boolean(true)),
    )
    .unwrap();
    let parameter = ParameterId::from_bytes([0x74; 16]);
    let argument = ClientFrame::CallArgument {
        stream: 7,
        parameter,
        value: value.clone(),
    };
    let expected_error = FrameCodecError::ConstructedValueNotAccepted {
        descriptor: descriptor.clone(),
    };
    assert_eq!(
        encode_constructed_client_frame(&active, &registry, &argument),
        Err(expected_error.clone())
    );
    assert_eq!(
        expected_error.to_string(),
        "constructed runtime values are not accepted by protocol 5 frames"
    );
    assert!(std::error::Error::source(&expected_error).is_none());

    let mut option_payload = 18_u16.to_be_bytes().to_vec();
    option_payload.extend_from_slice(&[0x04, 0x00]);
    option_payload.extend_from_slice(&BOOLEAN_TYPE_ID.to_bytes());
    option_payload.push(1);
    option_payload.extend_from_slice(&26_u32.to_be_bytes());
    option_payload.extend_from_slice(&orv5_boolean(true));
    let encoded_value = orv5_constructed(option_payload);
    let mut encoded_argument = b"ORF5\x02\0".to_vec();
    encoded_argument.extend_from_slice(&7_u64.to_be_bytes());
    encoded_argument.extend_from_slice(
        &(u32::try_from(parameter.to_bytes().len() + encoded_value.len()).unwrap()).to_be_bytes(),
    );
    encoded_argument.extend_from_slice(&parameter.to_bytes());
    encoded_argument.extend_from_slice(&encoded_value);
    assert_eq!(
        decode_constructed_client_frame(&active, &registry, &encoded_argument),
        Err(expected_error.clone())
    );

    let mut malformed_argument = encoded_argument.clone();
    malformed_argument[79] = 2;
    assert_eq!(
        decode_constructed_client_frame(&active, &registry, &malformed_argument),
        Err(FrameCodecError::Value {
            source: ValueCodecError::InvalidOptionPresence { value: 2 },
        })
    );

    let event = ServerFrame::EventBatch {
        stream: 7,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(value),
        }],
    };
    assert_eq!(
        encode_constructed_server_frame(&active, &registry, &event),
        Err(expected_error.clone())
    );

    let mut encoded_event = b"ORF5\x82\0".to_vec();
    encoded_event.extend_from_slice(&7_u64.to_be_bytes());
    encoded_event.extend_from_slice(
        &(u32::try_from(1 + 2 + 8 + 1 + 4 + encoded_value.len()).unwrap()).to_be_bytes(),
    );
    encoded_event.push(0x01);
    encoded_event.extend_from_slice(&1_u16.to_be_bytes());
    encoded_event.extend_from_slice(&1_u64.to_be_bytes());
    encoded_event.push(0x01);
    encoded_event.extend_from_slice(&(encoded_value.len() as u32).to_be_bytes());
    encoded_event.extend_from_slice(&encoded_value);
    assert_eq!(
        decode_constructed_server_frame(&active, &registry, &encoded_event),
        Err(expected_error.clone())
    );

    let mut malformed_event = encoded_event;
    malformed_event[79] = 2;
    assert_eq!(
        decode_constructed_server_frame(&active, &registry, &malformed_event),
        Err(FrameCodecError::Value {
            source: ValueCodecError::InvalidOptionPresence { value: 2 },
        })
    );
}

#[test]
fn orf5_accepts_opaque_results_and_rejects_opaque_arguments() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let payload = [0x73; 16];
    let opaque = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, OPAQUE_TOKEN_TYPE_ID, payload).unwrap(),
    );
    let mut encoded_value = b"ORV5".to_vec();
    encoded_value.push(0x0c);
    encoded_value.extend_from_slice(&OPAQUE_TOKEN_TYPE_ID.to_bytes());
    encoded_value.extend_from_slice(&16_u32.to_be_bytes());
    encoded_value.extend_from_slice(&payload);

    let result = ServerFrame::EventBatch {
        stream: 8,
        channel: Channel::ResultValues,
        events: vec![EventRecord {
            sequence: 1,
            event: Event::Value(opaque.clone()),
        }],
    };
    let mut result_payload = vec![0x01];
    result_payload.extend_from_slice(&1_u16.to_be_bytes());
    result_payload.extend_from_slice(&1_u64.to_be_bytes());
    result_payload.push(0x01);
    result_payload.extend_from_slice(&(encoded_value.len() as u32).to_be_bytes());
    result_payload.extend_from_slice(&encoded_value);
    let expected_result = orf5_frame(0x82, 8, &result_payload);
    assert_eq!(
        encode_constructed_server_frame(&active, &registry, &result),
        Ok(expected_result.clone())
    );
    assert_eq!(
        decode_constructed_server_frame(&active, &registry, &expected_result),
        Ok(result)
    );

    let parameter = ParameterId::from_bytes([0x78; 16]);
    let argument = ClientFrame::CallArgument {
        stream: 8,
        parameter,
        value: opaque.clone(),
    };
    let opaque_error = FrameCodecError::OpaqueArgumentNotAccepted {
        opaque_type: OPAQUE_TOKEN_TYPE_ID,
    };
    assert_eq!(
        encode_constructed_client_frame(&active, &registry, &argument),
        Err(opaque_error.clone())
    );
    let mut argument_payload = parameter.to_bytes().to_vec();
    argument_payload.extend_from_slice(&encoded_value);
    let expected_argument = orf5_frame(0x02, 8, &argument_payload);
    assert_eq!(
        decode_constructed_client_frame(&active, &registry, &expected_argument),
        Err(opaque_error.clone())
    );

    let function = FunctionId::from_bytes([0x79; 16]);
    let mut connection = ProtocolConnection::new();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 8,
                function,
            },
        )
        .unwrap();
    let before_argument = connection.clone();
    assert_eq!(
        connection.receive_constructed(
            &active,
            &registry,
            ClientFrame::CallArgument {
                stream: 8,
                parameter,
                value: opaque,
            },
        ),
        Err(ConnectionError::InvalidFrame {
            source: opaque_error,
        })
    );
    assert_eq!(connection, before_argument);
}

#[test]
fn orf5_constructed_rejection_preserves_connection_state_and_credit() {
    let active = active_record_revision();
    let standard = active.catalogue_hash_context().standard().unwrap();
    let registry = registered_opaque_codecs(standard).unwrap();
    let function = FunctionId::from_bytes([0x75; 16]);
    let parameter = ParameterId::from_bytes([0x76; 16]);
    let descriptor = TypeDescriptor::list(TypeDescriptor::named(BOOLEAN_TYPE_ID)).unwrap();
    let constructed = RuntimeValue::list(
        &active,
        descriptor.clone(),
        vec![RuntimeValue::Boolean(true)],
    )
    .unwrap();
    let rejection = FrameCodecError::ConstructedValueNotAccepted { descriptor };
    let mut connection = ProtocolConnection::new();
    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::CallRawStart {
                stream: 1,
                function,
            },
        )
        .unwrap();
    let before_argument = connection.clone();
    assert_eq!(
        connection.receive_constructed(
            &active,
            &registry,
            ClientFrame::CallArgument {
                stream: 1,
                parameter,
                value: constructed.clone(),
            },
        ),
        Err(ConnectionError::InvalidFrame {
            source: rejection.clone(),
        })
    );
    assert_eq!(connection, before_argument);

    connection
        .receive_constructed(
            &active,
            &registry,
            ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 4096,
            },
        )
        .unwrap();
    assert_eq!(
        connection
            .receive_constructed(
                &active,
                &registry,
                ClientFrame::CallArgumentsComplete { stream: 1 },
            )
            .unwrap(),
        Some(ClientAction::Dispatch {
            stream: 1,
            call: RawCall {
                function,
                arguments: vec![],
            },
        })
    );
    connection
        .apply_constructed(
            &active,
            &registry,
            ServerAction::Accepted {
                stream: 1,
                invocation: InvocationId::from_bytes([0x77; 16]),
            },
        )
        .unwrap();

    let before_event = connection.clone();
    assert_eq!(
        connection.apply_constructed(
            &active,
            &registry,
            ServerAction::Events {
                stream: 1,
                events: vec![Event::Value(constructed)],
            },
        ),
        Err(ConnectionError::InvalidFrame { source: rejection })
    );
    assert_eq!(connection, before_event);

    assert_eq!(
        connection
            .apply_constructed(
                &active,
                &registry,
                ServerAction::Events {
                    stream: 1,
                    events: vec![Event::Value(RuntimeValue::Boolean(true))],
                },
            )
            .unwrap(),
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::Boolean(true)),
            }],
        }
    );
}
