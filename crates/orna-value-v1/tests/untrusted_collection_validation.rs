//! Untrusted-input regression tests for collection validation.
//!
//! Duplicate-key and definition-reference checks used to scan a `Vec`
//! linearly per element, so a hostile collection that fits in one frame cost
//! quadratic CPU. These tests pin the accepted/rejected behaviour and bound the
//! time for very large (but in-limit) inputs.

use std::time::{Duration, Instant};

use orna_value_v1::{Error, Raw, SchemaDescriptor, Value};

const LARGE_COUNT: u64 = 300_000;
const LARGE_SCHEMA_COUNT: u64 = 100_000;
const BOUND: Duration = Duration::from_secs(10);

fn text_key(index: u64) -> Raw {
    Raw::Array(vec![Raw::Int(0.into()), Raw::Text(format!("k{index}"))])
}

fn present_child(key: Raw) -> Raw {
    Raw::Tag(
        60012,
        Box::new(Raw::Array(vec![
            Raw::Text("child".to_owned()),
            key,
            Raw::Map(Vec::new()),
            Raw::Array(Vec::new()),
        ])),
    )
}

fn present_root(children: Vec<Raw>) -> Raw {
    Raw::Tag(
        60012,
        Box::new(Raw::Array(vec![
            Raw::Text("root".to_owned()),
            Raw::Null,
            Raw::Map(Vec::new()),
            Raw::Array(children),
        ])),
    )
}

fn uuid(index: u64) -> Raw {
    Raw::Tag(
        37,
        Box::new(Raw::Bytes(u128::from(index).to_be_bytes().to_vec())),
    )
}

fn schema(keys: Vec<Raw>, fields: Vec<Raw>, definitions: Vec<Raw>) -> Raw {
    Raw::Map(vec![
        (Raw::Int(0.into()), Raw::Int(1.into())),
        (Raw::Int(1.into()), uuid(1)),
        (Raw::Int(2.into()), Raw::Array(keys)),
        (Raw::Int(3.into()), Raw::Array(fields)),
        (Raw::Int(4.into()), Raw::Array(definitions)),
    ])
}

#[test]
fn present_map_with_many_distinct_keys_validates_in_bounded_time() {
    let children = (0..LARGE_COUNT)
        .map(|index| present_child(text_key(index)))
        .collect();
    let raw = present_root(children);

    let started = Instant::now();
    let value = Value::new(raw).expect("300k distinct keys are a valid present value");
    let _encoded = value.encode().expect("validated value encodes");
    let elapsed = started.elapsed();

    assert!(
        elapsed < BOUND,
        "validating {LARGE_COUNT} distinct keys took {elapsed:?}"
    );
}

#[test]
fn present_duplicate_key_is_rejected_at_first_duplicate() {
    let duplicate_first_and_last = present_root(vec![
        present_child(text_key(0)),
        present_child(text_key(1)),
        present_child(text_key(0)),
    ]);
    assert_eq!(
        Value::new(duplicate_first_and_last).unwrap_err(),
        Error::DuplicateOrUnorderedMapKey
    );

    // The duplicate at index 1 is reported before the malformed child at
    // index 2 is ever inspected.
    let duplicate_before_malformed_child = present_root(vec![
        present_child(text_key(7)),
        present_child(text_key(7)),
        Raw::Null,
    ]);
    assert_eq!(
        Value::new(duplicate_before_malformed_child).unwrap_err(),
        Error::DuplicateOrUnorderedMapKey
    );

    // A malformed child before the duplicate is still reported first.
    let malformed_before_duplicate = present_root(vec![
        present_child(text_key(7)),
        Raw::Null,
        present_child(text_key(7)),
    ]);
    assert_eq!(
        Value::new(malformed_before_duplicate).unwrap_err(),
        Error::InvalidTag
    );
}

#[test]
fn schema_with_many_fields_and_definitions_validates_in_bounded_time() {
    let definitions: Vec<Raw> = (1..=LARGE_SCHEMA_COUNT)
        .map(|index| Raw::Array(vec![uuid(index), Raw::Int(0.into()), Raw::Array(Vec::new())]))
        .collect();
    let fields: Vec<Raw> = (1..=LARGE_SCHEMA_COUNT)
        .map(|index| {
            Raw::Array(vec![
                uuid(index),
                Raw::Text(format!("f{index}")),
                Raw::Array(vec![
                    Raw::Int(5.into()),
                    uuid(index % LARGE_SCHEMA_COUNT + 1),
                ]),
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        })
        .collect();

    let started = Instant::now();
    SchemaDescriptor::new(schema(Vec::new(), fields, definitions))
        .expect("100k fields and definitions form a valid schema");
    let elapsed = started.elapsed();

    assert!(
        elapsed < BOUND,
        "validating 100k-field schema took {elapsed:?}"
    );
}

#[test]
fn schema_duplicate_key_and_unsorted_ids_are_rejected() {
    let field = |index: u64| {
        Raw::Array(vec![
            uuid(index),
            Raw::Text(format!("f{index}")),
            Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Bool".to_owned())]),
            Raw::Int(1.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ])
    };

    let duplicate_key = schema(vec![uuid(1), uuid(1)], vec![field(1)], Vec::new());
    assert_eq!(
        SchemaDescriptor::new(duplicate_key).unwrap_err(),
        Error::InvalidSchema
    );

    let unknown_key = schema(vec![uuid(2)], vec![field(1)], Vec::new());
    assert_eq!(
        SchemaDescriptor::new(unknown_key).unwrap_err(),
        Error::InvalidSchema
    );

    let unsorted_fields = schema(Vec::new(), vec![field(2), field(1)], Vec::new());
    assert_eq!(
        SchemaDescriptor::new(unsorted_fields).unwrap_err(),
        Error::InvalidSchema
    );
}
