use std::collections::BTreeMap;

use orna_evaluator_v1::{
    Environment, Functions, Limits, PureFunction, evaluate_expression_with_functions,
};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn pinned_functions() -> Functions {
    let mut functions = BTreeMap::new();
    for (path, source) in orna_evaluator_v1::reference_standard_sources() {
        let parsed = orna_syntax_v1::parse_module(&source);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
        let module = path
            .strip_prefix("std/")
            .and_then(|path| path.strip_suffix(".orna"))
            .expect("pinned std paths have std/ prefix and .orna suffix")
            .replace('/', ".");
        for item in parsed.value.items {
            if let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration {
                let name = format!("std.{module}.{}", signature.name);
                assert!(
                    functions
                        .insert(
                            name.clone(),
                            PureFunction {
                                parameters: signature.parameters,
                                body,
                                environment: Environment::new(),
                            },
                        )
                        .is_none(),
                    "duplicate pinned standard function {name}"
                );
            }
        }
    }
    functions
}

fn evaluate(source: &str) -> Result<CanonicalValue, orna_evaluator_v1::EvaluationError> {
    evaluate_expression_with_functions(
        source,
        &Environment::new(),
        &pinned_functions(),
        Limits::default(),
    )
}

fn integer(value: i64) -> Raw {
    Raw::Int(value.into())
}

fn big_integer(value: &str) -> Raw {
    Raw::Int(value.parse().expect("test integer literal is valid"))
}

fn tuple(parts: Vec<Raw>) -> Raw {
    Raw::Tag(60015, Box::new(Raw::Array(parts)))
}

fn nested_tuple_key(leaf: &str, levels: usize) -> Raw {
    let mut key = tuple(vec![Raw::Text(leaf.into()), integer(0)]);
    for suffix in 1..=levels {
        key = tuple(vec![key, integer(suffix as i64)]);
    }
    key
}

fn value(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected test value to be canonical")
}

fn encoded(raw: Raw) -> CanonicalValue {
    value(Raw::Bytes(
        value(raw).encode().expect("expected OVB encoding"),
    ))
}

fn encoded_bytes(raw: Raw) -> Raw {
    Raw::Bytes(value(raw).encode().expect("expected OVB encoding"))
}

#[test]
fn map_and_set_order_survives_ovb_without_normalising_string_values() {
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-map-order-value-o3yue.orna")),
        Ok(value(Raw::Array(vec![
            tuple(vec![Raw::Text("b".into()), integer(3)]),
            tuple(vec![Raw::Text("a".into()), integer(2)]),
        ])))
    );

    let distinct_strings = vec![
        Raw::Text("é".into()),
        Raw::Text("e\u{301}".into()),
        Raw::Text("z".into()),
    ];
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-set-order-value-o3yue.orna")),
        Ok(value(Raw::Array(distinct_strings.clone())))
    );

    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-map-order-ovb-o3yue.orna")),
        Ok(encoded(Raw::Array(vec![
            tuple(vec![Raw::Text("é".into()), integer(4)]),
            tuple(vec![Raw::Text("e\u{301}".into()), integer(2)]),
            tuple(vec![Raw::Text("z".into()), integer(3)]),
        ])))
    );
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-set-order-ovb-o3yue.orna")),
        Ok(encoded(Raw::Array(distinct_strings)))
    );
}

#[test]
fn map_order_keeps_four_unique_insertions() {
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-map-four-unique-o3yue.orna")),
        Ok(value(Raw::Array(vec![
            tuple(vec![Raw::Text("a".into()), integer(1)]),
            tuple(vec![Raw::Text("b".into()), integer(2)]),
            tuple(vec![Raw::Text("c".into()), integer(3)]),
            tuple(vec![Raw::Text("d".into()), integer(4)]),
        ])))
    );
}

#[test]
fn map_size_counts_a_duplicate_key_once() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-map-size-fourth-duplicate-o3yue.orna"
        )),
        Ok(value(integer(2)))
    );
}

#[test]
fn fourth_map_entry_replaces_value_without_moving_original_key() {
    let actual = evaluate(include_str!(
        "fixtures/stdlib-map-fourth-duplicate-o3yue.orna"
    ))
    .expect("the fourth duplicate map entry evaluates")
    .encode()
    .expect("the updated entries have canonical bytes");
    let expected = value(Raw::Array(vec![
        tuple(vec![Raw::Text("a".into()), integer(4)]),
        tuple(vec![Raw::Text("b".into()), integer(2)]),
        tuple(vec![Raw::Text("c".into()), integer(3)]),
    ]))
    .encode()
    .expect("the expected entries have canonical bytes");
    assert_eq!(actual, expected);
}

#[test]
fn map_insert_replaces_an_existing_key_in_place() {
    let actual = evaluate(include_str!(
        "fixtures/stdlib-map-insert-existing-o3yue.orna"
    ));
    assert_eq!(
        actual,
        Ok(value(Raw::Array(vec![
            tuple(vec![Raw::Text("a".into()), integer(4)]),
            tuple(vec![Raw::Text("b".into()), integer(2)]),
            tuple(vec![Raw::Text("c".into()), integer(3)]),
        ])))
    );
}

#[test]
fn map_merge_keeps_left_key_positions_and_right_values() {
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-map-merge-order-o3yue.orna")),
        Ok(value(Raw::Array(vec![
            tuple(vec![Raw::Text("b".into()), integer(1)]),
            tuple(vec![Raw::Text("a".into()), integer(4)]),
            tuple(vec![Raw::Text("c".into()), integer(3)]),
        ])))
    );
}

#[test]
fn ovb_sorts_structural_keys_but_collections_keep_signed_zero_representatives() {
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-record-order-ovb-o3yue.orna")),
        Ok(value(Raw::Bytes(vec![
            0xa4, 0x61, b'b', 0x01, 0x61, b'z', 0x00, 0x62, b'a', b'a', 0x02, 0x62, 0xc3, 0xa9,
            0x03,
        ])))
    );

    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-map-signed-zero-ovb-o3yue.orna"
        )),
        Ok(encoded(Raw::Array(vec![tuple(vec![
            Raw::Float((-0.0f64).to_bits()),
            Raw::Text("last".into()),
        ])])))
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-set-signed-zero-ovb-o3yue.orna"
        )),
        Ok(encoded(Raw::Array(vec![Raw::Float((-0.0f64).to_bits())])))
    );
}

#[test]
fn stable_sort_orders_binary_ovb_keys_lexicographically() {
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-ovb-key-sort-w02pm.orna")),
        Ok(value(Raw::Array(vec![
            Raw::Text("z".into()),
            Raw::Text("aa".into()),
            Raw::Text("é".into()),
        ])))
    );
}

#[test]
fn map_projection_uses_complete_ovb_key_bytes_at_text_length_boundaries() {
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-map-ovb-key-order-w02pm.orna")),
        Ok(value(Raw::Array(vec![
            tuple(vec![Raw::Text("é".into()), integer(2)]),
            tuple(vec![
                Raw::Text("12345678901234567890123".into()),
                integer(23),
            ]),
            tuple(vec![
                Raw::Text("abcdefghijklmnopqrstuvwx".into()),
                integer(24),
            ]),
        ])))
    );
}

#[test]
fn map_projection_sorts_nested_tuple_key_encodings() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-map-nested-ovb-key-order-w02pm.orna"
        )),
        Ok(value(Raw::Array(vec![
            tuple(vec![
                tuple(vec![Raw::Text("z".into()), integer(0)]),
                integer(1),
            ]),
            tuple(vec![
                tuple(vec![Raw::Text("aa".into()), integer(0)]),
                integer(2),
            ]),
        ])))
    );
}

#[test]
fn map_projection_orders_a_four_entry_map_by_canonical_key() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-map-four-entry-ovb-order-w02pm.orna"
        )),
        Ok(value(Raw::Array(vec![
            tuple(vec![Raw::Text("a".into()), integer(1)]),
            tuple(vec![Raw::Text("b".into()), integer(2)]),
            tuple(vec![Raw::Text("c".into()), integer(3)]),
            tuple(vec![Raw::Text("d".into()), integer(4)]),
        ])))
    );
}

#[test]
fn encoded_map_projection_orders_integer_head_boundaries_and_normalizes_duplicates() {
    let ordered = vec![
        (integer(0), integer(0)),
        (integer(23), integer(23)),
        (integer(24), integer(240)),
        (integer(255), integer(255)),
        (integer(256), integer(256)),
        (integer(-1), integer(-1)),
        (integer(-24), integer(-24)),
        (integer(-25), integer(-25)),
        (
            big_integer("18446744073709551616"),
            big_integer("18446744073709551616"),
        ),
        (
            big_integer("-18446744073709551617"),
            big_integer("-18446744073709551617"),
        ),
    ];
    let expected = value(Raw::Array(
        ordered
            .into_iter()
            .map(|(key, item)| tuple(vec![encoded_bytes(key), encoded_bytes(item)]))
            .collect(),
    ));
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-map-ovb-integer-heads-ugnd3.orna"
        )),
        Ok(expected)
    );
}

#[test]
fn encoded_map_projection_orders_deep_tuple_keys_by_complete_ovb_bytes() {
    let aa_key = tuple(vec![
        tuple(vec![
            tuple(vec![Raw::Text("aa".into()), integer(0)]),
            tuple(vec![integer(1), integer(2)]),
        ]),
        integer(0),
    ]);
    let z_key = tuple(vec![
        tuple(vec![
            tuple(vec![Raw::Text("z".into()), integer(0)]),
            tuple(vec![integer(1), integer(3)]),
        ]),
        integer(1),
    ]);
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-map-ovb-nested-ugnd3.orna")),
        Ok(value(Raw::Array(vec![
            tuple(vec![encoded_bytes(z_key), encoded_bytes(integer(13))]),
            tuple(vec![encoded_bytes(aa_key), encoded_bytes(integer(12))]),
        ])))
    );
}

#[test]
fn encoded_map_projection_keeps_values_paired_through_deep_key_ordering() {
    let expected = value(Raw::Array(vec![
        tuple(vec![
            encoded_bytes(nested_tuple_key("a", 10)),
            encoded_bytes(integer(11)),
        ]),
        tuple(vec![
            encoded_bytes(nested_tuple_key("z", 10)),
            encoded_bytes(integer(13)),
        ]),
        tuple(vec![
            encoded_bytes(nested_tuple_key("aa", 10)),
            encoded_bytes(integer(12)),
        ]),
    ]));
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-map-ovb-deep-order-m6kj2.orna")),
        Ok(expected)
    );
}

#[test]
fn encoded_map_projection_of_empty_map_is_empty() {
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-map-ovb-empty-m6kj2.orna")),
        Ok(value(Raw::Array(Vec::new())))
    );
}

#[test]
fn structural_record_ovb_order_uses_encoded_nfc_field_keys() {
    let expected = encoded(Raw::Map(vec![
        (Raw::Text("aa".into()), integer(1)),
        (Raw::Text("é".into()), integer(2)),
        (Raw::Text("aaaaaaaaaaaaaaaaaaaaaaa".into()), integer(23)),
        (Raw::Text("bbbbbbbbbbbbbbbbbbbbbbbb".into()), integer(24)),
    ]));
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-record-key-boundaries-w02pm.orna"
        )),
        Ok(expected)
    );
}

#[test]
fn map_entry_tuples_roundtrip_as_list_of_tuples() {
    let expected_tuple = value(Raw::Array(vec![tuple(vec![
        Raw::Text("x".into()),
        integer(1),
    ])]))
    .encode()
    .expect("the expected typed tuple has canonical OVB bytes");
    let decoded_tuple = evaluate(include_str!("fixtures/stdlib-decode-tuple-ovb-o3yue.orna"))
        .expect("typed tuple decoding succeeds")
        .encode()
        .expect("the decoded typed tuple has canonical OVB bytes");
    assert_eq!(decoded_tuple, expected_tuple);

    let expected_entries = value(Raw::Array(vec![
        tuple(vec![Raw::Text("é".into()), integer(4)]),
        tuple(vec![Raw::Text("e\u{301}".into()), integer(2)]),
        tuple(vec![Raw::Text("z".into()), integer(3)]),
    ]))
    .encode()
    .expect("the expected map entries have canonical OVB bytes");
    let decoded_entries = evaluate(include_str!(
        "fixtures/stdlib-map-order-roundtrip-o3yue.orna"
    ))
    .expect("the std map entries roundtrip through OVB");
    let decoded_entries = decoded_entries
        .encode()
        .expect("the decoded map entries have canonical OVB bytes");
    assert_eq!(decoded_entries, expected_entries);
}

#[test]
fn zero_arity_tuple_uses_the_canonical_unit_tag() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-decode-empty-tuple-ovb-o3yue.orna"
        )),
        Ok(value(Raw::Tag(60014, Box::new(Raw::Array(Vec::new())))))
    );
}

#[test]
fn ovb_tuple_data_does_not_match_an_integer_witness() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-decode-tuple-wrong-witness-o3yue.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn set_values_roundtrip_as_lists_without_unicode_normalization() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-set-order-roundtrip-o3yue.orna"
        )),
        Ok(value(Raw::Array(vec![
            Raw::Text("é".into()),
            Raw::Text("e\u{301}".into()),
            Raw::Text("z".into()),
        ])))
    );
}

#[test]
fn structural_record_roundtrip_uses_the_declared_field_schema() {
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-record-order-roundtrip-o3yue.orna"
        )),
        Ok(value(Raw::Map(vec![
            (Raw::Text("b".into()), integer(1)),
            (Raw::Text("z".into()), integer(0)),
            (Raw::Text("aa".into()), integer(2)),
            (Raw::Text("é".into()), integer(3)),
        ])))
    );
}

#[test]
fn map_and_set_bodies_are_bound_to_the_published_std_snapshot() {
    let sources = orna_evaluator_v1::reference_standard_sources();
    let profile = orna_evaluator_v1::reference_standard_profile();
    for path in ["std/map.orna", "std/set.orna"] {
        let (_, source) = sources
            .iter()
            .find(|(source_path, _)| source_path == path)
            .expect("the pinned standard snapshot contains map and set modules");
        profile
            .verify_source(path, source)
            .expect("the map and set bodies are recorded by the published snapshot");
        let mut changed = source.clone();
        changed.push_str("\n// changed outside the pinned snapshot\n");
        assert!(profile.verify_source(path, &changed).is_err());
    }
    let (_, map_source) = sources
        .iter()
        .find(|(path, _)| path == "std/map.orna")
        .expect("the pinned standard snapshot contains std.map");
    assert!(map_source.contains("pub fn encoded_entries_by_ovb_key"));
}

#[test]
fn ovb_structural_record_keys_are_nfc_on_both_boundaries() {
    assert_eq!(
        evaluate(include_str!("fixtures/stdlib-record-nfc-decode-o3yue.orna")),
        Ok(value(Raw::Map(vec![(Raw::Text("Å".into()), integer(1),)])))
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-record-non-nfc-decode-o3yue.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        evaluate(include_str!(
            "fixtures/stdlib-record-non-nfc-encode-o3yue.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-EVAL-VALUE"
    );
}
