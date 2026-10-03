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

fn tuple(parts: Vec<Raw>) -> Raw {
    Raw::Tag(60015, Box::new(Raw::Array(parts)))
}

fn value(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected test value to be canonical")
}

fn encoded(raw: Raw) -> CanonicalValue {
    value(Raw::Bytes(
        value(raw).encode().expect("expected OVB encoding"),
    ))
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
