//! Orna's frozen OVB-1 compatibility values and format-3 value foundations.
//!
//! This crate deliberately has no storage or runtime dependencies.  Values are
//! validated before they become digest input, and errors describe malformed
//! structure without echoing decoded payloads.
//!
//! The implementation is split along value seams rather than by line count:
//! `error` (the payload-free taxonomy), `raw` (the CBOR data model and the
//! validated [`Value`]), `cbor` (canonical encoders and decoders), `validation`
//! (the tag registry checks), `codec` (typed OVB-1 codecs), `decimal`,
//! `snapshot`, `identity`, `primary_key`, `path`, `schema`, `blob`, `format3`
//! (profiles and their context) and `media` (MIME-1 annotations). This module
//! is the facade: it re-exports each seam at the crate root so the public
//! surface is unchanged.

mod blob_role;
pub use blob_role::{BlobRole, MediaFamily};

mod mime1;

mod tags;
pub use tags::*;

#[cfg(test)]
#[path = "format3_context_tests.rs"]
mod format3_context_tests;

pub const OVB_VERSION: &str = "OVB-1";
pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const MAX_DEPTH: usize = 128;
const MAX_ITEMS: usize = 1_000_000;

mod error;
pub use error::*;
mod raw;
pub use raw::*;
mod format3;
pub use format3::*;
mod blob;
pub use blob::*;
mod media;
pub use media::*;
mod primary_key;
pub use primary_key::*;
mod codec;
pub use codec::*;
mod decimal;
pub use decimal::*;
mod snapshot;
pub use snapshot::*;
mod identity;
pub use identity::*;
mod enum_identity;
pub use enum_identity::*;
mod schema;
pub use schema::*;
mod path;
pub use path::*;
mod cbor;
#[cfg(test)]
pub(crate) use cbor::*;
mod validation;

mod contract;
pub use contract::*;
pub mod object_reference;
pub use raw::Raw as OvbRaw;

// The inline test modules exercise the seams directly, so they need the std
// types the seam modules import for themselves.
#[cfg(test)]
use num_bigint::BigInt;
#[cfg(test)]
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering as AtomicOrdering},
};

#[cfg(test)]
mod tests {
    use super::*;
    fn h(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks(2)
            .map(|x| u8::from_str_radix(std::str::from_utf8(x).unwrap(), 16).unwrap())
            .collect()
    }

    fn schema_type(code: u64, body: Raw) -> Raw {
        Raw::Array(vec![Raw::Int(code.into()), body])
    }

    fn nominal_type(id: [u8; 16]) -> Raw {
        schema_type(5, uuid_raw(id))
    }

    fn schema_field(id: u8, name: &str, ty: Raw, role: u64) -> Raw {
        Raw::Array(vec![
            uuid_raw([id; 16]),
            Raw::Text(name.to_owned()),
            ty,
            Raw::Int(role.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ])
    }

    fn nominal_definition(id: u8, kind: u64, body: Raw) -> Raw {
        Raw::Array(vec![uuid_raw([id; 16]), Raw::Int(kind.into()), body])
    }

    fn record_definition(id: u8, fields: Vec<Raw>) -> Raw {
        nominal_definition(id, 0, Raw::Array(fields))
    }

    fn schema_for_type(value_type: Raw, definitions: Vec<Raw>) -> Raw {
        let type_int = schema_type(0, Raw::Text("Int".into()));
        Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), uuid_raw([9; 16])),
            (Raw::Int(2.into()), Raw::Array(vec![uuid_raw([1; 16])])),
            (
                Raw::Int(3.into()),
                Raw::Array(vec![
                    schema_field(1, "id", type_int, 0),
                    schema_field(2, "value", value_type, 1),
                ]),
            ),
            (Raw::Int(4.into()), Raw::Array(definitions)),
        ])
    }

    fn stored_blob_reference(media_type: &str, suffix: Raw) -> Raw {
        Raw::Tag(
            ROV3_BLOB_TAG,
            Box::new(Raw::Array(vec![
                Raw::Int(3.into()),
                Raw::Bytes(vec![4; 32]),
                Raw::Text(media_type.to_owned()),
                suffix,
                Raw::Bytes(vec![5; 20]),
            ])),
        )
    }

    #[test]
    fn schema_descriptor_validates_complete_stored_row_binding() {
        let schema = SchemaDescriptor::new(schema_for_type(
            schema_type(0, Raw::Text("Blob".into())),
            vec![],
        ))
        .unwrap();
        let key = encode_raw(&Raw::Int(7.into())).unwrap();
        let fields = encode_raw(&Raw::Array(vec![Raw::Bytes(vec![1, 2, 3])])).unwrap();

        assert!(schema.validate_stored_row([9; 16], &key, &fields).is_err());
        assert!(schema.validate_stored_row([8; 16], &key, &fields).is_err());
        assert!(
            schema
                .validate_stored_row(
                    [9; 16],
                    &encode_raw(&Raw::Text("7".into())).unwrap(),
                    &fields
                )
                .is_err()
        );
        assert!(
            schema
                .validate_stored_row([9; 16], &key, &encode_raw(&Raw::Array(vec![])).unwrap())
                .is_err()
        );
        assert!(
            schema
                .validate_stored_row(
                    [9; 16],
                    &key,
                    &encode_raw(&Raw::Array(vec![Raw::Text("bad".into())])).unwrap()
                )
                .is_err()
        );

        let blob_reference = stored_blob_reference("image/png", Raw::Null);
        let referenced_fields =
            encode_canonical_storage_value(&Raw::Array(vec![blob_reference])).unwrap();
        assert!(
            schema
                .validate_stored_row([9; 16], &key, &referenced_fields)
                .is_ok()
        );

        for malformed_reference in [
            stored_blob_reference("not a mime", Raw::Null),
            stored_blob_reference("image/png", Raw::Text("PNG".into())),
        ] {
            let mut fields = Vec::new();
            write_raw(&Raw::Array(vec![malformed_reference]), &mut fields).unwrap();
            assert!(schema.validate_stored_row([9; 16], &key, &fields).is_err());
        }
    }

    #[test]
    fn schema_quantity_uses_canonical_unit_identity_and_dimension_vector() {
        let unit_a = [1; 16];
        let unit_b = [2; 16];
        let unit_definition = |id, dimensions: &[i64]| {
            nominal_definition(
                id,
                3,
                Raw::Array(vec![
                    Raw::Array(
                        dimensions
                            .iter()
                            .map(|value| Raw::Int((*value).into()))
                            .collect(),
                    ),
                    Raw::Int(1.into()),
                    Raw::Int(1.into()),
                    Raw::Int(0.into()),
                    Raw::Int(1.into()),
                    Raw::Bool(false),
                ]),
            )
        };
        let unit_definitions = vec![
            unit_definition(1, &[1, 0, 0, 0, 0, 0, 0, 1]),
            unit_definition(2, &[0, 1, 0, 0, 0, 0, 0, 1]),
        ];
        let quantity_type = |unit_id| {
            Raw::Array(vec![
                Raw::Int(7.into()),
                schema_type(0, Raw::Text("Float".into())),
                uuid_raw(unit_id),
            ])
        };
        let schema = SchemaDescriptor::new(schema_for_type(
            quantity_type(unit_a),
            unit_definitions.clone(),
        ))
        .unwrap();
        let other_schema = SchemaDescriptor::new(schema_for_type(
            quantity_type(unit_b),
            unit_definitions.clone(),
        ))
        .unwrap();
        assert_ne!(schema, other_schema);
        let key = encode_raw(&Raw::Int(7.into())).unwrap();
        let quantity = |unit_id| {
            Raw::Tag(
                QUANTITY_TAG,
                Box::new(Raw::Array(vec![
                    Raw::Float(1.0_f64.to_bits()),
                    uuid_raw(unit_id),
                ])),
            )
        };
        let matching_fields =
            encode_canonical_storage_value(&Raw::Array(vec![quantity(unit_a)])).unwrap();
        assert!(
            schema
                .validate_stored_row([9; 16], &key, &matching_fields)
                .is_ok()
        );

        let different_dimension_fields =
            encode_canonical_storage_value(&Raw::Array(vec![quantity(unit_b)])).unwrap();
        assert!(
            schema
                .validate_stored_row([9; 16], &key, &different_dimension_fields)
                .is_err()
        );

        assert!(
            SchemaDescriptor::new(schema_for_type(
                quantity_type([3; 16]),
                unit_definitions.clone(),
            ))
            .is_err()
        );
        assert!(
            SchemaDescriptor::new(schema_for_type(
                quantity_type([3; 16]),
                vec![
                    unit_definitions[0].clone(),
                    record_definition(3, Vec::new())
                ],
            ))
            .is_err()
        );
    }

    #[test]
    fn schema_descriptor_validates_nested_stored_blob_references() {
        let key = encode_raw(&Raw::Int(7.into())).unwrap();
        let blob_type = schema_type(0, Raw::Text("Blob".into()));

        let list_schema =
            SchemaDescriptor::new(schema_for_type(schema_type(1, blob_type.clone()), vec![]))
                .unwrap();
        let list_fields = encode_canonical_storage_value(&Raw::Array(vec![Raw::Array(vec![
            stored_blob_reference("image/png", Raw::Null),
        ])]))
        .unwrap();
        assert!(
            list_schema
                .validate_stored_row([9; 16], &key, &list_fields)
                .is_ok()
        );

        let option_schema =
            SchemaDescriptor::new(schema_for_type(schema_type(2, blob_type), vec![])).unwrap();
        let option_fields = encode_canonical_storage_value(&Raw::Array(vec![Raw::Tag(
            OPTION_TAG,
            Box::new(Raw::Array(vec![
                Raw::Int(1.into()),
                stored_blob_reference("image/png", Raw::Null),
            ])),
        )]))
        .unwrap();
        assert!(
            option_schema
                .validate_stored_row([9; 16], &key, &option_fields)
                .is_ok()
        );

        let malformed_none = encode_canonical_storage_value(&Raw::Array(vec![Raw::Tag(
            OPTION_TAG,
            Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Null])),
        )]))
        .unwrap();
        assert!(
            option_schema
                .validate_stored_row([9; 16], &key, &malformed_none)
                .is_err()
        );
    }

    #[test]
    fn schema_descriptor_rejects_malformed_nested_datetime_payload() {
        let schema = SchemaDescriptor::new(schema_for_type(
            schema_type(0, Raw::Text("ZonedDateTime".into())),
            vec![],
        ))
        .unwrap();
        let key = encode_raw(&Raw::Int(7.into())).unwrap();
        let valid_instant = || {
            Raw::Tag(
                INSTANT_TAG,
                Box::new(Raw::Array(vec![Raw::Int(0.into()), Raw::Int(0.into())])),
            )
        };
        let valid_zone = || Raw::Tag(TIME_ZONE_TAG, Box::new(Raw::Text("UTC".into())));
        let valid_value = Raw::Tag(
            ZONED_DATE_TIME_TAG,
            Box::new(Raw::Array(vec![
                valid_instant(),
                valid_zone(),
                Raw::Int(0.into()),
            ])),
        );
        let valid_fields = encode_raw(&Raw::Array(vec![valid_value])).unwrap();
        assert!(
            schema
                .validate_stored_row([9; 16], &key, &valid_fields)
                .is_ok()
        );

        let malformed_values = [
            Raw::Tag(
                ZONED_DATE_TIME_TAG,
                Box::new(Raw::Array(vec![
                    Raw::Tag(INSTANT_TAG, Box::new(Raw::Int(0.into()))),
                    valid_zone(),
                    Raw::Int(0.into()),
                ])),
            ),
            Raw::Tag(
                ZONED_DATE_TIME_TAG,
                Box::new(Raw::Array(vec![
                    valid_instant(),
                    Raw::Tag(TIME_ZONE_TAG, Box::new(Raw::Int(0.into()))),
                    Raw::Int(0.into()),
                ])),
            ),
        ];
        for malformed_value in malformed_values {
            let mut malformed_fields = Vec::new();
            write_raw(&Raw::Array(vec![malformed_value]), &mut malformed_fields).unwrap();
            assert!(
                schema
                    .validate_stored_row([9; 16], &key, &malformed_fields)
                    .is_err()
            );
        }
    }

    #[test]
    fn canonical_uuid_text_is_lowercase_and_round_trips() {
        let bytes = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let text = canonical_uuid_text(bytes);
        assert_eq!(text, "00112233-4455-6677-8899-aabbccddeeff");
        assert_eq!(parse_canonical_uuid_text(&text).unwrap(), bytes);
        assert_eq!(
            Value::uuid(bytes).encode().unwrap(),
            h("d8255000112233445566778899aabbccddeeff")
        );
        for invalid in [
            "001122334455-6677-8899-aabbccddeeff",
            "00112233-4455-6677-8899-AABBCCDDEEFF",
            "00112233-4455-6677-8899-aabbccddeefg",
            "00112233-4455-6677-8899-aabbccddeeff00",
        ] {
            assert!(parse_canonical_uuid_text(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn argument_identity_normalizes_names_before_sorting_and_collision_check() {
        let bool_type = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Bool".into())]);
        let composed = "é".to_owned();
        let decomposed = "e\u{301}".to_owned();
        let left = argument_identity(vec![(composed, bool_type.clone(), Raw::Bool(true))]).unwrap();
        let right = argument_identity(vec![(
            decomposed.clone(),
            bool_type.clone(),
            Raw::Bool(true),
        )])
        .unwrap();
        assert_eq!(left, right);
        assert!(
            argument_identity(vec![
                ("é".to_owned(), bool_type.clone(), Raw::Bool(true)),
                (decomposed, bool_type, Raw::Bool(false)),
            ])
            .is_err()
        );
    }

    #[test]
    fn date_codec_rejects_year_zero_and_accepts_declared_bounds() {
        let date = |text: &str| Value::new(tag(DATE_TAG, Raw::Text(text.to_owned())));

        assert_eq!(date("0000-01-01"), Err(Error::InvalidTag));
        assert!(date("0001-01-01").is_ok());
        assert!(date("9999-12-31").is_ok());
    }

    #[test]
    fn repo_paths_reject_backslashes_without_restricting_host_paths() {
        let path = |flavour: &str, value: &str| {
            Value::new(tag(
                PATH_TAG,
                Raw::Array(vec![
                    Raw::Text(flavour.to_owned()),
                    Raw::Text(value.to_owned()),
                ]),
            ))
        };

        assert!(path("repo", "crates/orna-value/src/lib.rs").is_ok());
        assert!(matches!(
            path("repo", r"crates\orna-value\src\lib.rs"),
            Err(Error::InvalidTag)
        ));
        assert!(path("posix", r"/tmp/literal\backslash").is_ok());
        assert!(path("windows", r"C:\Users\Ada\file.orna").is_ok());
    }

    #[test]
    fn values_match_supplied_vectors() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/reference/tests/value-vectors.json"))
                .unwrap();
        for vector in fixture.as_array().unwrap() {
            let x = vector["hex"].as_str().unwrap();
            let b = h(x);
            let canonical_roundtrip = Value::decode(&b)
                .and_then(|decoded| decoded.encode())
                .is_ok_and(|encoded| encoded == b);
            assert_eq!(
                canonical_roundtrip,
                vector["canonical_roundtrip"].as_bool().unwrap(),
                "{x}"
            );
        }
    }
    #[test]
    fn rejects_noncanonical() {
        for x in [
            "1817",
            "c2490000000000000000",
            "fa3f800000",
            "fb7ff8000000000001",
            "9f00ff",
            "a101000100",
        ] {
            assert!(Value::decode(&h(x)).is_err(), "{x}")
        }
    }

    #[test]
    fn bignums_reject_aliases_and_admit_the_first_out_of_range_magnitude() {
        // ORNA-FORMAT-001: tags 2/3 are only the arbitrary-precision forms.
        // Empty, zero, leading-zero, and direct-major-range magnitudes are
        // aliases, so they cannot enter a canonical identity or digest.
        for x in [
            "c240",                   // tag 2, empty magnitude
            "c24100",                 // tag 2, zero magnitude
            "c24101",                 // tag 2, direct uint(1)
            "c248ffffffffffffffff",   // tag 2, direct uint(u64::MAX)
            "c24900ffffffffffffffff", // tag 2, leading-zero magnitude
            "c340",                   // tag 3, empty magnitude
            "c34100",                 // tag 3, zero magnitude
            "c34101",                 // tag 3, direct nint(-2)
            "c348ffffffffffffffff",   // tag 3, direct nint(-u64::MAX - 1)
            "c34900ffffffffffffffff", // tag 3, leading-zero magnitude
        ] {
            assert_eq!(Value::decode(&h(x)), Err(Error::NonCanonical), "{x}");
        }

        for x in [
            "c249010000000000000000", //  2^64
            "c349010000000000000000", // -2^64 - 1
        ] {
            let value = Value::decode(&h(x)).unwrap_or_else(|error| panic!("{x}: {error}"));
            assert_eq!(value.encode().unwrap(), h(x), "{x}");
        }
    }
    #[test]
    fn invocation_handle_requires_canonical_runtime_uuid() {
        let handle = |runtime| {
            Value::new(tag(
                LIVE_HANDLE_TAG,
                Raw::Array(vec![
                    uuid_raw([1; 16]),
                    runtime,
                    Raw::Null,
                    Raw::Bool(false),
                ]),
            ))
        };

        assert!(handle(uuid_raw([2; 16])).is_ok());
        assert!(handle(Raw::Text("runtime".into())).is_err());
        assert!(handle(Raw::Int(2.into())).is_err());
        assert!(handle(uuid_raw([0; 16])).is_ok());
        assert!(handle(tag(UUID_TAG, Raw::Bytes(vec![2; 15]))).is_err());
    }
    #[test]
    fn sys_revision_id_is_a_closed_32_byte_portable_identifier() {
        let revision_id = |name: Raw, representation: Raw| {
            tag(OPAQUE_SYSTEM_ID_TAG, Raw::Array(vec![name, representation]))
        };
        let valid = revision_id(Raw::Text("sys.RevisionId".into()), Raw::Bytes(vec![7; 32]));
        let canonical = |raw: Raw| {
            let value = Value::new(raw).expect("valid system value was rejected");
            let encoded = value.encode().unwrap();
            assert_eq!(Value::decode(&encoded).unwrap().raw(), value.raw());
            encoded
        };
        let encodings = [
            canonical(Raw::Text("revision".into())),
            canonical(revision_id(
                Raw::Text("sys.DatabaseId".into()),
                uuid_raw([7; 16]),
            )),
            canonical(tag(
                DIGEST_TAG,
                Raw::Array(vec![Raw::Text("sha256".into()), Raw::Bytes(vec![7; 32])]),
            )),
            canonical(valid.clone()),
        ];
        for (index, left) in encodings.iter().enumerate() {
            assert!(encodings[index + 1..].iter().all(|right| left != right));
        }

        let mut noncanonical = encodings[3].clone();
        noncanonical.splice(4..5, [0x78, 0x0e]);
        assert!(Value::decode(&noncanonical).is_err());

        let rejected = [
            revision_id(Raw::Text("sys.RevisionId".into()), Raw::Bytes(vec![7; 31])),
            revision_id(Raw::Text("sys.RevisionId".into()), Raw::Bytes(vec![7; 33])),
            revision_id(Raw::Text("Str".into()), Raw::Bytes(vec![7; 32])),
            revision_id(Raw::Text("Digest".into()), Raw::Bytes(vec![7; 32])),
            revision_id(Raw::Text("sys.Digest".into()), Raw::Bytes(vec![7; 32])),
            revision_id(Raw::Text("sys.RevisionId".into()), uuid_raw([7; 16])),
            revision_id(
                Raw::Text("sys.RevisionId".into()),
                tag(
                    DIGEST_TAG,
                    Raw::Array(vec![Raw::Text("sha256".into()), Raw::Bytes(vec![7; 32])]),
                ),
            ),
            tag(OPAQUE_SYSTEM_ID_TAG, Raw::Text("sys.RevisionId".into())),
            tag(
                OPAQUE_SYSTEM_ID_TAG,
                Raw::Array(vec![Raw::Text("sys.RevisionId".into())]),
            ),
            revision_id(
                Raw::Bytes(b"sys.RevisionId".to_vec()),
                Raw::Bytes(vec![7; 32]),
            ),
        ];
        for raw in rejected {
            assert!(Value::new(raw.clone()).is_err());
            let mut encoded = Vec::new();
            write_raw(&raw, &mut encoded).unwrap();
            assert!(Value::decode(&encoded).is_err());
        }
    }
    #[test]
    fn float_vectors() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/reference/tests/float-vectors.json"))
                .unwrap();
        let parse = |text: &str| u64::from_str_radix(text, 16).unwrap();
        let ordered: Vec<u64> = fixture["ascending_total_order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| parse(v["bits"].as_str().unwrap()))
            .collect();
        assert!(
            ordered
                .windows(2)
                .all(|p| float_total_cmp(p[0], p[1]) == Ordering::Less)
        );
        for vector in fixture["ordinary_equality"].as_array().unwrap() {
            assert_eq!(
                float_ordinary_eq(
                    parse(vector["left"].as_str().unwrap()),
                    parse(vector["right"].as_str().unwrap())
                ),
                vector["equal"].as_bool().unwrap()
            );
        }
        for vector in fixture["aggregate"].as_array().unwrap() {
            let values: Vec<u64> = vector["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| parse(v.as_str().unwrap()))
                .collect();
            let actual = if vector["operation"] == "min" {
                float_min(&values)
            } else {
                float_max(&values)
            };
            assert_eq!(actual, Some(parse(vector["result"].as_str().unwrap())));
        }
        for vector in fixture["parquet_statistics"].as_array().unwrap() {
            let values: Vec<u64> = vector["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| parse(v.as_str().unwrap()))
                .collect();
            assert_eq!(
                values.iter().filter(|x| is_nan_bits(**x)).count(),
                vector["nan_count"].as_u64().unwrap() as usize
            );
            let non_nan: Vec<u64> = values
                .iter()
                .copied()
                .filter(|x| !is_nan_bits(*x))
                .collect();
            let (min, max, key_min, key_max) = if non_nan.is_empty() {
                let canonical: Vec<u64> = values
                    .into_iter()
                    .map(|x| {
                        if is_nan_bits(x) {
                            CANONICAL_NAN_BITS
                        } else {
                            x
                        }
                    })
                    .collect();
                (
                    *canonical
                        .iter()
                        .min_by_key(|x| float_total_key(**x))
                        .unwrap(),
                    *canonical
                        .iter()
                        .max_by_key(|x| float_total_key(**x))
                        .unwrap(),
                    "min_total",
                    "max_total",
                )
            } else {
                (
                    *non_nan.iter().min_by_key(|x| float_total_key(**x)).unwrap(),
                    *non_nan.iter().max_by_key(|x| float_total_key(**x)).unwrap(),
                    "min_non_nan",
                    "max_non_nan",
                )
            };
            assert_eq!(min, parse(vector[key_min].as_str().unwrap()));
            assert_eq!(max, parse(vector[key_max].as_str().unwrap()));
        }
    }
    #[test]
    fn decimal_is_exact() {
        let a = Decimal::new(1.into(), (-1).into());
        let b = Decimal::new(2.into(), (-1).into());
        assert_eq!(a.try_add(&b).unwrap(), Decimal::new(3.into(), (-1).into()));
        assert_eq!(
            Value::decimal(12340.into(), (-3).into())
                .unwrap()
                .encode()
                .unwrap(),
            h("d9ea60821904d221")
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into())
                .divide_exact(&Decimal::new(8.into(), 0.into()))
                .unwrap(),
            Decimal::new(125.into(), (-3).into())
        );
        assert_eq!(
            Decimal::new(120.into(), (-2).into())
                .try_subtract(&Decimal::new(34.into(), (-2).into()))
                .unwrap(),
            Decimal::new(86.into(), (-2).into())
        );
        assert_eq!(
            Decimal::new(1.into(), (-1).into())
                .try_subtract(&Decimal::new(25.into(), (-2).into()))
                .unwrap(),
            Decimal::new((-15).into(), (-2).into())
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into())
                .try_subtract(&Decimal::new(1.into(), 0.into()))
                .unwrap(),
            Decimal::new(0.into(), 0.into())
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into()).try_subtract(&Decimal::new(
                1.into(),
                (-(Decimal::MAX_ABS_EXPONENT as i64 + 1)).into()
            )),
            Err(Error::DecimalLimit)
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into()).divide_exact(&Decimal::new(3.into(), 0.into())),
            Err(Error::NonFiniteDecimal)
        );
    }
    #[test]
    fn path_vectors() {
        for (a, b) in [
            ("alice-smith", "alice-smith"),
            ("a~b", "a~7eb"),
            ("foo/bar", "foo~2fbar"),
            ("é", "~c3~a9"),
            ("", "~ff"),
            ("con", "~63on"),
            (".git", "~2egit"),
        ] {
            assert_eq!(path_encode_component(a).unwrap(), b);
            assert_eq!(path_decode_component(b).unwrap(), a)
        }
        for x in ["~e", "~FF", "~61lice", "x~ff", "../x", "/absolute"] {
            assert!(path_decode_component(x).is_err())
        }
        assert_eq!(
            path_encode_key_components(&[
                "018f7f43-7b3a-7cc2-8c11-37e728c96f4a".into(),
                "2026-09-03".into()
            ])
            .unwrap(),
            vec!["018f7f43-7b3a-7cc2-8c11-37e728c96f4a", "2026-09-03.orna"]
        );
        assert!(path_validate_relative_components(&["..".into()]).is_err());
    }
    #[test]
    fn composite_path_decoding_is_canonical_and_bounded() {
        for keys in [
            vec![""],
            vec!["..", "foo/bar", "é"],
            vec!["first.orna", "last.orna"],
            vec!["con", "a\\b", ".git"],
        ] {
            let keys: Vec<String> = keys.into_iter().map(String::from).collect();
            let encoded = path_encode_key_components(&keys).unwrap();
            let decoded = path_decode_key_components(&encoded).unwrap();
            assert_eq!(decoded, keys);
            assert_eq!(path_encode_key_components(&decoded).unwrap(), encoded);
        }
        assert_eq!(
            path_decode_key_components(&["first.orna".into(), "last.orna.orna".into()]).unwrap(),
            vec!["first.orna", "last.orna"]
        );
        for encoded in [
            vec![],
            vec![""],
            vec![".orna"],
            vec!["a"],
            vec!["a.ORNA"],
            vec!["~61lice.orna"],
            vec!["~FF.orna"],
            vec!["x~ff.orna"],
            vec!["~e.orna"],
            vec!["..", "a.orna"],
            vec!["", "a.orna"],
            vec!["/a.orna"],
            vec!["a/b.orna"],
            vec!["a\\b.orna"],
        ] {
            let encoded: Vec<String> = encoded.into_iter().map(String::from).collect();
            assert!(path_decode_key_components(&encoded).is_err(), "{encoded:?}");
        }
        let mut boundary = vec!["a".repeat(200); 5];
        boundary.push(format!("{}.orna", "b".repeat(14)));
        assert_eq!(boundary.join("/").len(), 1024);
        assert!(path_decode_key_components(&boundary).is_ok());
        boundary[5].insert(0, 'b');
        assert!(path_decode_key_components(&boundary).is_err());
        assert!(path_decode_key_components(&[format!("{}.orna", "a".repeat(201))]).is_err());
    }
    #[test]
    fn construction_and_safe_containers_reject_aliases() {
        assert!(Value::new(Raw::Float(0x7ff8_0000_0000_0001)).is_err());
        assert_eq!(
            Value::float_bits(0x7ff8_0000_0000_0001).raw(),
            &Raw::Float(CANONICAL_NAN_BITS)
        );
        assert!(Value::new(tag(DIAGNOSTIC_TAG, Raw::Map(vec![]))).is_err());
        assert!(Value::new(tag(ERROR_TAG, Raw::Map(vec![]))).is_err());
        assert!(Value::new(tag(PRESENT_NODE_TAG, Raw::Array(vec![]))).is_err());
    }

    #[test]
    fn protected_payloads_cannot_enter_snapshot_key_serialization() {
        let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
        let snapshot = Snapshot::cwd([1; 16], [2; 16], 0.into()).unwrap();
        let row_reference = tag(
            PINNED_ROW_REFERENCE_TAG,
            Raw::Array(vec![
                uuid_raw([3; 16]),
                uuid_raw([4; 16]),
                tag(0, Raw::Text(fixture.to_owned())),
                snapshot.raw(),
            ]),
        );

        assert_eq!(
            Value::new(row_reference.clone()),
            Err(Error::ProtectedValue)
        );
        let mut unvalidated_bytes = Vec::new();
        write_raw(&row_reference, &mut unvalidated_bytes).unwrap();
        assert_eq!(
            Value::decode(&unvalidated_bytes),
            Err(Error::ProtectedValue)
        );
        assert!(!Error::ProtectedValue.to_string().contains(fixture));
    }

    #[test]
    fn raw_debug_redacts_protected_payloads_before_wire_validation() {
        let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
        let raw = Raw::Map(vec![(
            Raw::Text("credential".into()),
            Raw::Array(vec![Raw::Tag(0, Box::new(Raw::Text(fixture.to_owned())))]),
        )]);

        let debug = format!("{raw:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(fixture));
        assert_eq!(Value::new(raw), Err(Error::ProtectedValue));
    }

    #[test]
    fn raw_debug_redacts_diagnostic_present_and_sys_value_payloads() {
        let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
        let diagnostic = tag(
            DIAGNOSTIC_TAG,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Int(3.into())),
                (Raw::Int(2.into()), Raw::Text(fixture.into())),
                (Raw::Int(3.into()), Raw::Array(vec![])),
                (
                    Raw::Int(4.into()),
                    Raw::Array(vec![Raw::Text(fixture.into())]),
                ),
                (Raw::Int(5.into()), Raw::Array(vec![])),
                (Raw::Int(6.into()), Raw::Bool(false)),
            ]),
        );
        let present = tag(
            PRESENT_NODE_TAG,
            Raw::Array(vec![
                Raw::Text("value".into()),
                Raw::Null,
                Raw::Map(vec![(
                    Raw::Text("credential".into()),
                    Raw::Text(fixture.into()),
                )]),
                Raw::Array(vec![]),
            ]),
        );
        let sys_value = tag(
            SYS_VALUE_BOX_TAG,
            Raw::Array(vec![
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".into())]),
                Raw::Text(fixture.into()),
            ]),
        );

        for raw in [diagnostic, present, sys_value] {
            let value = Value::new(raw.clone()).expect("fixture wrapper is canonical");
            let debug = format!("{raw:?} {:?}", value);
            assert!(debug.contains("<redacted>"));
            assert!(!debug.contains(fixture));
        }
    }

    #[test]
    fn generic_trace_projection_hides_nested_wrappers_and_protected_map_keys() {
        let fixture = include_str!("../tests/fixtures/secret-surface.orna").trim();
        let diagnostic = tag(
            DIAGNOSTIC_TAG,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Int(3.into())),
                (Raw::Int(2.into()), Raw::Text(fixture.into())),
                (Raw::Int(3.into()), Raw::Array(vec![])),
                (
                    Raw::Int(4.into()),
                    Raw::Array(vec![Raw::Text(fixture.into())]),
                ),
                (Raw::Int(5.into()), Raw::Array(vec![])),
                (Raw::Int(6.into()), Raw::Bool(false)),
            ]),
        );
        let present = tag(
            PRESENT_NODE_TAG,
            Raw::Array(vec![
                Raw::Text("value".into()),
                Raw::Null,
                Raw::Map(vec![(
                    Raw::Text("credential".into()),
                    Raw::Text(fixture.into()),
                )]),
                Raw::Array(vec![]),
            ]),
        );
        let sys_value = tag(
            SYS_VALUE_BOX_TAG,
            Raw::Array(vec![
                Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".into())]),
                Raw::Text(fixture.into()),
            ]),
        );
        let value = Value::new(Raw::Array(vec![diagnostic.clone(), present, sys_value]))
            .expect("generic wrapper fixture is canonical");

        let trace_bytes = value.redacted_for_trace().unwrap().encode().unwrap();
        assert!(
            !trace_bytes
                .windows(fixture.len())
                .any(|part| part == fixture.as_bytes())
        );
        assert_eq!(
            Value::decode(&trace_bytes).unwrap().raw(),
            &Raw::Array(vec![
                Raw::Text("<redacted>".into()),
                Raw::Text("<redacted>".into()),
                Raw::Text("<redacted>".into()),
            ])
        );

        let keyed = Value::new(Raw::Map(vec![(diagnostic, Raw::Text(fixture.into()))]))
            .expect("a typed wrapper can be a canonical map key");
        assert_eq!(
            keyed.redacted_for_trace().unwrap().raw(),
            &Raw::Text("<redacted>".into())
        );
    }

    #[test]
    fn quantity_requires_a_numeric_amount() {
        let raw = tag(
            QUANTITY_TAG,
            Raw::Array(vec![Raw::Text("not-a-number".into()), uuid_raw([7; 16])]),
        );
        assert_eq!(Value::new(raw.clone()), Err(Error::InvalidTag));

        let mut bytes = Vec::new();
        write_raw(&raw, &mut bytes).unwrap();
        assert_eq!(Value::decode(&bytes), Err(Error::InvalidTag));
    }

    #[test]
    fn error_value_round_trips_nested_causes_and_safe_details() {
        let mut child_details = BTreeMap::new();
        child_details.insert("retryable".into(), Value::new(Raw::Bool(true)).unwrap());
        let child = ErrorValue::new("ORNA-E-CHILD", "child failure", [], child_details).unwrap();

        let mut details = BTreeMap::new();
        details.insert("attempt".into(), Value::int(3.into()));
        details.insert("zeta".into(), Value::new(Raw::Text("safe".into())).unwrap());
        let error = ErrorValue::new("ORNA-E-ROOT", "root failure", [child], details).unwrap();

        assert_eq!(error.code(), "ORNA-E-ROOT");
        assert_eq!(error.message(), "root failure");
        assert_eq!(error.causes().len(), 1);
        assert_eq!(error.causes()[0].code(), "ORNA-E-CHILD");
        assert_eq!(
            error.causes()[0].safe_details()["retryable"],
            Value::new(Raw::Bool(true)).unwrap()
        );
        assert_eq!(error.safe_details()["attempt"], Value::int(3.into()));
        assert_eq!(
            error.safe_details()["zeta"],
            Value::new(Raw::Text("safe".into())).unwrap()
        );

        let encoded = error.encode().unwrap();
        let decoded = ErrorValue::decode(&encoded).unwrap();
        assert_eq!(decoded, error);
        assert_eq!(decoded.value().raw(), error.value().raw());
    }

    #[test]
    fn error_trace_projection_redacts_nested_payloads_and_debug() {
        let fixture = include_str!("../tests/fixtures/error-trace-secret.orna").trim();
        let detail = Value::new(Raw::Text(fixture.into())).unwrap();
        let cause = ErrorValue::new(
            fixture,
            fixture,
            [],
            BTreeMap::from([("credential".into(), detail.clone())]),
        )
        .unwrap();
        let error = ErrorValue::new(
            fixture,
            fixture,
            [cause],
            BTreeMap::from([("credential".into(), detail)]),
        )
        .unwrap();

        // Local recovery remains lossless; trace serialization has its own
        // fail-closed projection because OVB cannot classify caller text.
        assert!(
            error
                .encode()
                .unwrap()
                .windows(fixture.len())
                .any(|part| part == fixture.as_bytes())
        );
        let trace_bytes = error.encode_for_trace().unwrap();
        assert!(
            !trace_bytes
                .windows(fixture.len())
                .any(|part| part == fixture.as_bytes())
        );
        let traced = ErrorValue::decode(&trace_bytes).unwrap();
        assert_eq!(traced.code(), "<redacted>");
        assert_eq!(traced.message(), "<redacted>");
        assert!(traced.safe_details().is_empty());
        assert_eq!(traced.causes()[0].code(), "<redacted>");
        assert!(traced.causes()[0].safe_details().is_empty());
        assert!(!format!("{error:?}").contains(fixture));
        assert!(!format!("{:?}", error.value()).contains(fixture));
        assert!(!format!("{:?}", error.value().raw()).contains(fixture));

        let nested = Value::new(Raw::Array(vec![error.value().raw().clone()])).unwrap();
        let nested_bytes = nested.redacted_for_trace().unwrap().encode().unwrap();
        assert!(
            !nested_bytes
                .windows(fixture.len())
                .any(|part| part == fixture.as_bytes())
        );
        assert!(!format!("{nested:?}").contains(fixture));
    }

    #[test]
    fn error_value_preserves_malformed_rejection() {
        let malformed = tag(
            ERROR_TAG,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Text("safe".into())),
                (
                    Raw::Int(2.into()),
                    Raw::Array(vec![Raw::Text("not-error".into())]),
                ),
                (
                    Raw::Int(3.into()),
                    Raw::Map(vec![(Raw::Int(4.into()), Raw::Bool(true))]),
                ),
            ]),
        );
        assert_eq!(Value::new(malformed.clone()), Err(Error::InvalidTag));

        let mut bytes = Vec::new();
        write_raw(&malformed, &mut bytes).unwrap();
        assert_eq!(ErrorValue::decode(&bytes), Err(Error::InvalidTag));
    }

    fn diagnostic_with_path(path: &str) -> Raw {
        tag(
            DIAGNOSTIC_TAG,
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Text("ORNA-E-TEST".into())),
                (Raw::Int(1.into()), Raw::Int(3.into())),
                (Raw::Int(2.into()), Raw::Text("safe message".into())),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![Raw::Array(vec![
                        Snapshot::cwd([1; 16], [2; 16], 0.into()).unwrap().raw(),
                        Raw::Text(path.into()),
                        Raw::Int(0.into()),
                        Raw::Int(0.into()),
                    ])]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
                (Raw::Int(5.into()), Raw::Array(vec![])),
                (Raw::Int(6.into()), Raw::Bool(false)),
            ]),
        )
    }

    #[test]
    fn diagnostic_paths_are_relative_utf8_or_redacted_at_both_ovb_boundaries() {
        for path in [
            "",
            "/absolute/main.orna",
            "../main.orna",
            "src/../main.orna",
            "src//main.orna",
            "./src/main.orna",
            "src\\main.orna",
            "C:/src/main.orna",
            "c:src/main.orna",
            "\\\\server\\share\\main.orna",
            "src/\0main.orna",
            "src/\nmain.orna",
        ] {
            let raw = diagnostic_with_path(path);
            assert!(Value::new(raw.clone()).is_err(), "unsafe path was admitted");
            let mut bytes = Vec::new();
            write_raw(&raw, &mut bytes).unwrap();
            assert!(Value::decode(&bytes).is_err(), "unsafe path was decoded");
        }

        for path in ["src/entrée/メイン.orna", "<redacted>"] {
            let value = Value::new(diagnostic_with_path(path)).expect("safe path rejected");
            let bytes = value.encode().unwrap();
            assert!(Value::decode(&bytes).is_ok(), "safe path was not decoded");
        }
    }

    #[test]
    fn sys_value_money_binds_its_currency_witness() {
        let currency = [7u8; 16];
        let other = [8u8; 16];
        let type_node = Raw::Array(vec![Raw::Int(8.into()), uuid_raw(currency)]);
        let amount = tag(
            DECIMAL_TAG,
            Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())]),
        );
        let valid = tag(
            SYS_VALUE_BOX_TAG,
            Raw::Array(vec![
                type_node.clone(),
                tag(
                    MONEY_TAG,
                    Raw::Array(vec![amount.clone(), uuid_raw(currency)]),
                ),
            ]),
        );
        assert!(Value::new(valid).is_ok());
        let invalid = tag(
            SYS_VALUE_BOX_TAG,
            Raw::Array(vec![
                type_node,
                tag(MONEY_TAG, Raw::Array(vec![amount, uuid_raw(other)])),
            ]),
        );
        assert!(Value::new(invalid).is_err());
    }

    #[test]
    fn schema_rejects_non_nfc_structural_type_names() {
        let table = [9u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let inner_record = Raw::Array(vec![
            Raw::Int(4.into()),
            Raw::Array(vec![Raw::Array(vec![
                Raw::Text("inner".into()),
                type_int.clone(),
            ])]),
        ]);
        let descriptor = |structural_type: Raw| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(vec![uuid_raw([1; 16])])),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw([1; 16]),
                            Raw::Text("id".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                        Raw::Array(vec![
                            uuid_raw([2; 16]),
                            Raw::Text("record".into()),
                            structural_type,
                            Raw::Int(1.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                    ]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
            ])
        };
        let record_type = |names: &[&str]| {
            Raw::Array(vec![
                Raw::Int(4.into()),
                Raw::Array(
                    names
                        .iter()
                        .map(|name| {
                            Raw::Array(vec![Raw::Text((*name).into()), inner_record.clone()])
                        })
                        .collect(),
                ),
            ])
        };

        assert!(SchemaDescriptor::new(descriptor(record_type(&["café"]))).is_ok());
        assert!(SchemaDescriptor::new(descriptor(record_type(&["cafe\u{301}"]))).is_err());
        assert!(SchemaDescriptor::new(descriptor(record_type(&["z", "a"]))).is_err());
    }

    #[test]
    fn schema_rejects_unconstructible_unreferenced_nominal_definitions() {
        let unused_id = [13; 16];
        let unconstructible = record_definition(
            13,
            vec![schema_field(23, "self", nominal_type(unused_id), 1)],
        );
        let valid_root_field = schema_type(0, Raw::Text("Int".into()));
        assert!(
            SchemaDescriptor::new(schema_for_type(
                valid_root_field.clone(),
                vec![unconstructible]
            ))
            .is_err()
        );

        let guarded = record_definition(
            14,
            vec![schema_field(
                24,
                "next",
                schema_type(2, nominal_type([14; 16])),
                1,
            )],
        );
        assert!(SchemaDescriptor::new(schema_for_type(valid_root_field, vec![guarded])).is_ok());
    }

    #[test]
    fn schema_rejects_unproductive_nominal_cycles_without_a_base_variant() {
        let direct_id = [10; 16];
        let direct = record_definition(
            10,
            vec![schema_field(20, "self", nominal_type(direct_id), 1)],
        );
        assert!(
            SchemaDescriptor::new(schema_for_type(nominal_type(direct_id), vec![direct])).is_err()
        );

        let a = [10; 16];
        let b = [11; 16];
        let mutual = vec![
            record_definition(10, vec![schema_field(20, "b", nominal_type(b), 1)]),
            record_definition(11, vec![schema_field(21, "a", nominal_type(a), 1)]),
        ];
        assert!(SchemaDescriptor::new(schema_for_type(nominal_type(a), mutual)).is_err());

        let enum_id = [12; 16];
        let recursive_variant = Raw::Array(vec![
            uuid_raw([30; 16]),
            Raw::Text("recursive".into()),
            Raw::Array(vec![schema_field(31, "next", nominal_type(enum_id), 1)]),
        ]);
        let recursive_enum = nominal_definition(12, 1, Raw::Array(vec![recursive_variant]));
        assert!(
            SchemaDescriptor::new(schema_for_type(nominal_type(enum_id), vec![recursive_enum]))
                .is_err()
        );
    }

    #[test]
    fn schema_accepts_guarded_recursion_enum_base_and_stored_reference() {
        let node_id = [10; 16];
        let node_type = nominal_type(node_id);
        let node = record_definition(
            10,
            vec![
                schema_field(20, "next", schema_type(2, node_type.clone()), 1),
                schema_field(21, "children", schema_type(1, node_type.clone()), 1),
                schema_field(22, "window", schema_type(9, node_type.clone()), 1),
            ],
        );
        assert!(SchemaDescriptor::new(schema_for_type(node_type, vec![node])).is_ok());

        let enum_id = [11; 16];
        let enum_definition = nominal_definition(
            11,
            1,
            Raw::Array(vec![
                Raw::Array(vec![
                    uuid_raw([30; 16]),
                    Raw::Text("empty".into()),
                    Raw::Array(vec![]),
                ]),
                Raw::Array(vec![
                    uuid_raw([31; 16]),
                    Raw::Text("next".into()),
                    Raw::Array(vec![schema_field(32, "previous", nominal_type(enum_id), 1)]),
                ]),
            ]),
        );
        assert!(
            SchemaDescriptor::new(schema_for_type(
                nominal_type(enum_id),
                vec![enum_definition]
            ))
            .is_ok()
        );

        let stored_reference = Raw::Array(vec![
            Raw::Int(6.into()),
            uuid_raw([8; 16]),
            uuid_raw([9; 16]),
            Raw::Array(vec![schema_type(0, Raw::Text("Int".into()))]),
        ]);
        assert!(SchemaDescriptor::new(schema_for_type(stored_reference, vec![])).is_ok());
    }

    #[test]
    fn schema_rejects_unconstructible_nominals_across_type_compositions() {
        let bad_id = [10; 16];
        let bad_type = nominal_type(bad_id);
        let bad_definition =
            record_definition(10, vec![schema_field(20, "self", bad_type.clone(), 1)]);
        let tuple = Raw::Array(vec![Raw::Int(3.into()), Raw::Array(vec![bad_type.clone()])]);
        let structural = Raw::Array(vec![
            Raw::Int(4.into()),
            Raw::Array(vec![Raw::Array(vec![
                Raw::Text("member".into()),
                bad_type.clone(),
            ])]),
        ]);
        for required in [tuple, structural] {
            assert!(
                SchemaDescriptor::new(schema_for_type(required, vec![bad_definition.clone()]))
                    .is_err()
            );
        }

        let refined = nominal_definition(
            11,
            2,
            Raw::Array(vec![bad_type.clone(), Raw::Array(vec![])]),
        );
        assert!(
            SchemaDescriptor::new(schema_for_type(
                nominal_type([11; 16]),
                vec![bad_definition.clone(), refined]
            ))
            .is_err()
        );

        let quantity = Raw::Array(vec![
            Raw::Int(7.into()),
            bad_type.clone(),
            uuid_raw([30; 16]),
        ]);
        assert!(
            SchemaDescriptor::new(schema_for_type(quantity, vec![bad_definition.clone()])).is_err()
        );

        let reference_with_unconstructible_key = Raw::Array(vec![
            Raw::Int(6.into()),
            uuid_raw([8; 16]),
            uuid_raw([9; 16]),
            Raw::Array(vec![bad_type.clone()]),
        ]);
        assert!(
            SchemaDescriptor::new(schema_for_type(
                reference_with_unconstructible_key,
                vec![bad_definition.clone()]
            ))
            .is_err()
        );

        for guarded in [
            schema_type(1, bad_type.clone()),
            schema_type(2, bad_type.clone()),
            schema_type(9, bad_type),
        ] {
            assert!(
                SchemaDescriptor::new(schema_for_type(guarded, vec![bad_definition.clone()]))
                    .is_err()
            );
        }
    }

    #[test]
    fn schema_frozen_fallback_is_type_directed() {
        let table = [9u8; 16];
        let key = [1u8; 16];
        let stored = [2u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let schema = |fallback: Raw| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(vec![uuid_raw(key)])),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw(key),
                            Raw::Text("id".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                        Raw::Array(vec![
                            uuid_raw(stored),
                            Raw::Text("value".into()),
                            type_int.clone(),
                            Raw::Int(1.into()),
                            fallback,
                        ]),
                    ]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
            ])
        };
        assert!(
            SchemaDescriptor::new(schema(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(7.into())
            ])))
            .is_ok()
        );
        assert!(
            SchemaDescriptor::new(schema(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Text("not-an-int".into())
            ])))
            .is_err()
        );
    }

    #[test]
    fn nested_field_fallbacks_follow_role_for_records_and_enum_payloads() {
        let table = [9u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let field = |id: u8, role: i64, fallback: Raw| {
            Raw::Array(vec![
                uuid_raw([id; 16]),
                Raw::Text(format!("field-{id}")),
                type_int.clone(),
                Raw::Int(role.into()),
                fallback,
            ])
        };
        let schema = |definition: Raw| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(vec![uuid_raw([1; 16])])),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![Raw::Array(vec![
                        uuid_raw([1; 16]),
                        Raw::Text("key".into()),
                        type_int.clone(),
                        Raw::Int(0.into()),
                        Raw::Array(vec![Raw::Int(0.into())]),
                    ])]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![definition])),
            ])
        };
        let record = |fields: Vec<Raw>| {
            Raw::Array(vec![
                uuid_raw([3; 16]),
                Raw::Int(0.into()),
                Raw::Array(fields),
            ])
        };
        let enum_payload = |fields: Vec<Raw>| {
            Raw::Array(vec![
                uuid_raw([4; 16]),
                Raw::Int(1.into()),
                Raw::Array(vec![Raw::Array(vec![
                    uuid_raw([5; 16]),
                    Raw::Text("Variant".into()),
                    Raw::Array(fields),
                ])]),
            ])
        };
        let valid_fields = || {
            vec![
                field(10, 0, Raw::Array(vec![Raw::Int(0.into())])),
                field(
                    11,
                    1,
                    Raw::Array(vec![Raw::Int(1.into()), Raw::Int(7.into())]),
                ),
                field(
                    12,
                    2,
                    Raw::Array(vec![Raw::Int(2.into()), Raw::Bytes(vec![8; 32])]),
                ),
            ]
        };

        assert!(SchemaDescriptor::new(schema(record(valid_fields()))).is_ok());
        assert!(SchemaDescriptor::new(schema(enum_payload(valid_fields()))).is_ok());
        assert!(
            SchemaDescriptor::new(schema(record(vec![field(
                10,
                0,
                Raw::Array(vec![Raw::Int(1.into()), Raw::Int(7.into())]),
            )])))
            .is_err()
        );
        assert!(
            SchemaDescriptor::new(schema(enum_payload(vec![field(
                10,
                1,
                Raw::Array(vec![Raw::Int(2.into()), Raw::Bytes(vec![8; 32])]),
            )])))
            .is_err()
        );
    }
    #[test]
    fn schema_preserves_key_order_independently_of_field_order() {
        let table = [9u8; 16];
        let first = [1u8; 16];
        let second = [2u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let schema = |keys: Vec<Raw>| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(keys)),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw(first),
                            Raw::Text("first".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                        Raw::Array(vec![
                            uuid_raw(second),
                            Raw::Text("second".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                    ]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
            ])
        };
        let key_order = |descriptor: &SchemaDescriptor| {
            let Raw::Map(entries) = descriptor.raw() else {
                unreachable!()
            };
            let keys = entries
                .iter()
                .find(|(key, _)| int_u64(key).ok() == Some(2))
                .map(|(_, value)| array(value).unwrap())
                .unwrap();
            keys.iter()
                .map(|key| uuid_array(key).unwrap())
                .collect::<Vec<_>>()
        };

        let forward =
            SchemaDescriptor::new(schema(vec![uuid_raw(first), uuid_raw(second)])).unwrap();
        let reverse =
            SchemaDescriptor::new(schema(vec![uuid_raw(second), uuid_raw(first)])).unwrap();
        assert_eq!(key_order(&forward), vec![first, second]);
        assert_eq!(key_order(&reverse), vec![second, first]);
        assert_ne!(
            schema_identity(&forward).unwrap(),
            schema_identity(&reverse).unwrap()
        );
    }

    #[test]
    fn schema_key_lists_reject_missing_duplicate_and_wrong_role_ids() {
        let table = [9u8; 16];
        let first = [1u8; 16];
        let second = [2u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let schema = |keys: Vec<Raw>, second_role: i64| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(keys)),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw(first),
                            Raw::Text("first".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                        Raw::Array(vec![
                            uuid_raw(second),
                            Raw::Text("second".into()),
                            type_int.clone(),
                            Raw::Int(second_role.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                    ]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
            ])
        };

        assert!(SchemaDescriptor::new(schema(vec![uuid_raw(first)], 0)).is_err());
        assert!(
            SchemaDescriptor::new(schema(
                vec![uuid_raw(first), uuid_raw(second), uuid_raw(first)],
                0
            ))
            .is_err()
        );
        assert!(SchemaDescriptor::new(schema(vec![uuid_raw(first), uuid_raw(second)], 1)).is_err());
    }

    #[test]
    fn snapshot_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "fixtures/reference/tests/snapshot-vectors.json"
        ))
        .unwrap();
        let db = h("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let rt = h("101112131415161718191a1b1c1d1e1f").try_into().unwrap();
        let s = Snapshot::cwd(db, rt, 1.into()).unwrap();
        assert_eq!(
            encode_raw(&s.raw()).unwrap(),
            h(
                "8500d82550000102030405060708090a0b0c0d0e0fd82550101112131415161718191a1b1c1d1e1f015820732adfe16f749548c7184797f01a99247b4633cc89d32d5f47d99df94e53727e"
            )
        );
        for name in ["cwd_1", "cwd_2", "commit_sha1"] {
            let vector = fixture[name].as_str().unwrap();
            let raw = Value::decode(&h(vector)).unwrap().raw().clone();
            assert_eq!(
                encode_raw(&Snapshot::decode(&raw).unwrap().raw()).unwrap(),
                h(vector)
            );
        }
        let commit_sha256 = h(
            "8401d82550000102030405060708090a0b0c0d0e0f6673686132353658200404040404040404040404040404040404040404040404040404040404040404",
        );
        let commit_sha256_raw = Value::decode(&commit_sha256).unwrap().raw().clone();
        assert_eq!(
            encode_raw(&Snapshot::decode(&commit_sha256_raw).unwrap().raw()).unwrap(),
            commit_sha256
        );

        let invalid_sha256 = Raw::Array(vec![
            Raw::Int(1.into()),
            uuid_raw(db),
            Raw::Text("sha256".into()),
            Raw::Bytes(vec![4; 31]),
        ]);
        assert!(Snapshot::decode(&invalid_sha256).is_err());
        let invalid = fixture["invalid_bare_cwd"].as_str().unwrap();
        assert!(Value::decode(&h(invalid)).is_ok());
        assert!(Snapshot::decode(Value::decode(&h(invalid)).unwrap().raw()).is_err());
    }
    #[test]
    fn snapshot_cwd_rejects_negative_generation() {
        let db = h("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let rt = h("101112131415161718191a1b1c1d1e1f").try_into().unwrap();
        assert!(Snapshot::cwd(db, rt, (-1).into()).is_err());
        let noncanonical = Raw::Array(vec![
            Raw::Int(0.into()),
            uuid_raw(db),
            uuid_raw(rt),
            Raw::Int((-1).into()),
            Raw::Bytes([0; 32].to_vec()),
        ]);
        assert!(Snapshot::decode(&noncanonical).is_err());
    }

    #[test]
    fn snapshot_byte_codec_preserves_full_generation_identity() {
        let db = h("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let rt = h("101112131415161718191a1b1c1d1e1f").try_into().unwrap();
        let generation = (BigInt::from(1_u8) << 130_usize) + BigInt::from(42_u8);
        let snapshot = Snapshot::cwd(db, rt, generation.clone()).unwrap();

        let encoded = snapshot.encode().unwrap();
        let decoded = Snapshot::decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, snapshot);
        assert_eq!(decoded.encode().unwrap(), encoded);

        let Snapshot::Cwd {
            id,
            generation: pinned,
            ..
        } = &decoded
        else {
            unreachable!();
        };
        assert_eq!(pinned, &generation);
        let next = Snapshot::cwd(db, rt, &generation + 1).unwrap();
        let Snapshot::Cwd { id: next_id, .. } = next else {
            unreachable!();
        };
        assert_ne!(*id, next_id, "each logical generation has its own id");

        let mut forged = snapshot;
        let Snapshot::Cwd { id, .. } = &mut forged else {
            unreachable!();
        };
        *id = [0xa5; 32];
        assert!(forged.encode().is_err());

        let mut trailing = encoded;
        trailing.push(0);
        assert_eq!(Snapshot::decode_bytes(&trailing), Err(Error::TrailingBytes));
    }

    #[test]
    fn fixture_path_vectors() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/reference/tests/path-vectors.json"))
                .unwrap();
        for vector in fixture["round_trip"].as_array().unwrap() {
            let source = vector["value"].as_str().unwrap();
            let encoded = vector["encoded"].as_str().unwrap();
            assert_eq!(path_encode_component(source).unwrap(), encoded);
            assert_eq!(path_decode_component(encoded).unwrap(), source);
            assert_eq!(
                path_decode_key_components(&[format!("{encoded}.orna")]).unwrap(),
                vec![source]
            );
        }
        for vector in fixture["composite"].as_array().unwrap() {
            let values: Vec<String> = vector["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect();
            let components: Vec<String> = vector["components"]
                .as_array()
                .unwrap()
                .iter()
                .map(|component| component.as_str().unwrap().to_owned())
                .collect();
            assert_eq!(path_encode_key_components(&values).unwrap(), components);
            assert_eq!(path_decode_key_components(&components).unwrap(), values);
        }
        for vector in fixture["reject_encoded"].as_array().unwrap() {
            assert!(path_decode_component(vector["encoded"].as_str().unwrap()).is_err());
            assert!(
                path_decode_key_components(&[format!(
                    "{}.orna",
                    vector["encoded"].as_str().unwrap()
                )])
                .is_err()
            );
        }
        for vector in fixture["portable_collisions"].as_array().unwrap() {
            let left = path_encode_component(vector["left"].as_str().unwrap()).unwrap();
            let right = path_encode_component(vector["right"].as_str().unwrap()).unwrap();
            assert_eq!(
                path_collision_key(&left),
                vector["collision_key"].as_str().unwrap()
            );
            assert_eq!(path_collision_key(&left), path_collision_key(&right));
        }
        for vector in fixture["limits"]["component"].as_array().unwrap() {
            let input = "a".repeat(vector["encoded_length"].as_u64().unwrap() as usize);
            assert_eq!(
                path_encode_component(&input).is_ok(),
                vector["expect"] == "pass"
            );
        }
        for vector in fixture["limits"]["table_relative_path"].as_array().unwrap() {
            let size = vector["encoded_length"].as_u64().unwrap() as usize;
            let mut remaining = size - 5;
            let mut components = Vec::new();
            while remaining > 200 {
                components.push("a".repeat(200));
                remaining -= 201;
            }
            components.push("a".repeat(remaining));
            assert_eq!(
                path_encode_key_components(&components).is_ok(),
                vector["expect"] == "pass"
            );
        }
    }
    #[test]
    fn fixture_numeric_vectors_are_exact() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "fixtures/reference/tests/numeric-vectors.json"
        ))
        .unwrap();
        assert_eq!(fixture["money_add"]["a"], "0.1");
        assert_eq!(fixture["money_add"]["b"], "0.2");
        assert_eq!(fixture["money_add"]["result"], "0.3");
        assert_eq!(fixture["mile_per_hour_mps"], "0.44704");
        let a = Decimal::new(1.into(), (-1).into());
        let b = Decimal::new(2.into(), (-1).into());
        assert_eq!(a.try_add(&b).unwrap(), Decimal::new(3.into(), (-1).into()));
        // 1609.344 / 3600 has the finite exact decimal supplied by the fixture.
        let mile = Decimal::new(1_609_344.into(), (-3).into());
        let seconds = Decimal::new(3_600.into(), 0.into());
        assert_eq!(
            mile.divide_exact(&seconds).unwrap(),
            Decimal::new(44_704.into(), (-5).into())
        );
    }
}

#[cfg(test)]
mod logical_primary_key_order_tests {
    use super::*;

    fn value(raw: Raw) -> Value {
        Value::new(raw).expect("valid key value")
    }

    fn uuid_value(byte: u8) -> Raw {
        uuid_raw([byte; 16])
    }

    fn decimal(coefficient: i64, exponent: i64) -> Value {
        Value::decimal(coefficient.into(), exponent.into()).expect("valid decimal")
    }

    #[test]
    fn compares_scalar_key_families_by_logical_value() {
        assert_eq!(
            compare_primary_keys(&Value::int((-12).into()), &Value::int((-2).into())).unwrap(),
            Ordering::Less
        );
        assert_eq!(
            compare_primary_keys(&decimal(-12, -1), &decimal(-11, -1)).unwrap(),
            Ordering::Less
        );
        assert_eq!(
            compare_primary_keys(&decimal(119, -2), &decimal(12, -1)).unwrap(),
            Ordering::Less
        );
        assert_eq!(
            compare_primary_keys(
                &value(Raw::Tag(DATE_TAG, Box::new(Raw::Text("2024-01-01".into())))),
                &value(Raw::Tag(DATE_TAG, Box::new(Raw::Text("2024-02-01".into())))),
            )
            .unwrap(),
            Ordering::Less
        );
        assert_eq!(
            compare_primary_keys(
                &value(Raw::Tag(
                    INSTANT_TAG,
                    Box::new(Raw::Array(vec![Raw::Int(7.into()), Raw::Int(99.into())])),
                )),
                &value(Raw::Tag(
                    INSTANT_TAG,
                    Box::new(Raw::Array(vec![Raw::Int(8.into()), Raw::Int(0.into())])),
                )),
            )
            .unwrap(),
            Ordering::Less
        );
        assert_eq!(
            compare_primary_keys(&Value::uuid([1; 16]), &Value::uuid([2; 16])).unwrap(),
            Ordering::Less
        );
    }

    #[test]
    fn compares_payload_free_enums_references_and_tuples_recursively() {
        let enum_key = |variant| {
            value(Raw::Tag(
                ENUM_TAG,
                Box::new(Raw::Array(vec![
                    uuid_value(1),
                    uuid_value(variant),
                    Raw::Null,
                ])),
            ))
        };
        assert_eq!(
            compare_primary_keys(&enum_key(2), &enum_key(3)).unwrap(),
            Ordering::Less
        );

        let reference_key = |key: i64| {
            value(Raw::Tag(
                STORED_TABLE_REFERENCE_TAG,
                Box::new(Raw::Array(vec![
                    uuid_value(4),
                    uuid_value(5),
                    Raw::Int(key.into()),
                ])),
            ))
        };
        assert_eq!(
            compare_primary_keys(&reference_key(-2), &reference_key(0)).unwrap(),
            Ordering::Less
        );

        let tuple = |first: i64, second: &str| {
            value(Raw::Tag(
                TUPLE_TAG,
                Box::new(Raw::Array(vec![
                    Raw::Int(first.into()),
                    Raw::Text(second.into()),
                ])),
            ))
        };
        assert_eq!(
            compare_primary_keys(&tuple(-3, "z"), &tuple(-2, "a")).unwrap(),
            Ordering::Less
        );
    }

    #[test]
    fn rejects_non_key_and_malformed_components() {
        assert!(
            compare_primary_keys(&Value::float_bits(1.0f64.to_bits()), &Value::int(1.into()))
                .is_err()
        );
        assert!(compare_primary_keys(&Value::unit(), &Value::unit()).is_err());
        let malformed_enum = value(Raw::Tag(
            ENUM_TAG,
            Box::new(Raw::Array(vec![uuid_value(1), uuid_value(2), Raw::Null])),
        ));
        let payload_enum = value(Raw::Tag(
            ENUM_TAG,
            Box::new(Raw::Array(vec![
                uuid_value(1),
                uuid_value(2),
                Raw::Tag(
                    RECORD_TAG,
                    Box::new(Raw::Array(vec![Raw::Null, Raw::Array(vec![])])),
                ),
            ])),
        ));
        assert!(compare_primary_keys(&malformed_enum, &payload_enum).is_err());
    }
}
#[cfg(test)]
mod exact_content_outcome_tests {
    use super::*;

    // A collision needs two digest-valid sides with differing bytes, which
    // SHA-256 forbids in practice; the classification is therefore exercised
    // through this pure seam rather than through `same_content_checked`.
    #[test]
    fn verified_digests_with_differing_bytes_are_a_collision() {
        assert!(matches!(
            Blob::exact_content_outcome(true, true, true),
            Err(Error::ContentCollision)
        ));
    }

    #[test]
    fn digest_failure_wins_over_differing_bytes() {
        assert!(matches!(
            Blob::exact_content_outcome(true, false, true),
            Err(Error::ContentDigestMismatch)
        ));
        assert!(matches!(
            Blob::exact_content_outcome(true, true, false),
            Err(Error::ContentDigestMismatch)
        ));
    }

    #[test]
    fn verified_equal_bytes_are_equal() {
        assert!(Blob::exact_content_outcome(false, true, true).unwrap());
    }
}

#[cfg(test)]
mod resolver_integrity_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct PinnedMediaResolver {
        descriptor: NativeOid,
        identity: ContentIdentity,
        bytes: Vec<u8>,
        chunk_digests: Vec<[u8; 32]>,
        reads: AtomicUsize,
        bytes_read: AtomicUsize,
    }

    impl BlobResolver for PinnedMediaResolver {
        fn read_range(
            &self,
            reference: &ContentReference,
            offset: u64,
            length: u64,
        ) -> Result<Vec<u8>> {
            self.reads.fetch_add(1, AtomicOrdering::SeqCst);
            if reference.descriptor_oid() != &self.descriptor {
                return Err(Error::MissingDescriptor);
            }
            if reference.identity() != self.identity {
                return Err(Error::ContentDigestMismatch);
            }
            let start = usize::try_from(offset).map_err(|_| Error::InvalidRange)?;
            let length = usize::try_from(length).map_err(|_| Error::InvalidRange)?;
            let end = start.checked_add(length).ok_or(Error::InvalidRange)?;
            if end > self.bytes.len() {
                return Err(Error::ContentUnavailable);
            }
            if start == end {
                return Ok(Vec::new());
            }

            let first_chunk = start / 2;
            let last_chunk = (end - 1) / 2;
            for chunk_index in first_chunk..=last_chunk {
                let chunk_start = chunk_index * 2;
                let chunk_end = (chunk_start + 2).min(self.bytes.len());
                let chunk = &self.bytes[chunk_start..chunk_end];
                self.bytes_read
                    .fetch_add(chunk.len(), AtomicOrdering::SeqCst);
                if ContentIdentity::from_bytes(chunk).sha256() != self.chunk_digests[chunk_index] {
                    return Err(Error::ContentDigestMismatch);
                }
            }
            Ok(self.bytes[start..end].to_vec())
        }
    }

    fn chunk_digests(bytes: &[u8]) -> Vec<[u8; 32]> {
        bytes
            .chunks(2)
            .map(|chunk| ContentIdentity::from_bytes(chunk).sha256())
            .collect()
    }

    fn resolver(
        descriptor: NativeOid,
        identity: ContentIdentity,
        bytes: &[u8],
        chunks: &[u8],
    ) -> Arc<PinnedMediaResolver> {
        Arc::new(PinnedMediaResolver {
            descriptor,
            identity,
            bytes: bytes.to_vec(),
            chunk_digests: chunk_digests(chunks),
            reads: AtomicUsize::new(0),
            bytes_read: AtomicUsize::new(0),
        })
    }

    fn context(resolver: Arc<dyn BlobResolver>) -> Format3Context {
        context_with_limit(resolver, 2)
    }

    fn context_with_limit(resolver: Arc<dyn BlobResolver>, max_read_bytes: u64) -> Format3Context {
        let database = [1; 16];
        Format3Context::from_persisted(Format3ContextInputs {
            database_uuid: database,
            pin: Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
            git_oid_algorithm: GitHash::Sha1,
            schema_root: NativeOid::from_bytes(&[9; 20]).unwrap(),
            store_root: NativeOid::from_bytes(&[8; 20]).unwrap(),
            availability: AvailabilityState::Available,
            owner: OwnerLifetime::new([3; 16]),
            quota: Format3Quota::new(max_read_bytes, 1024).unwrap(),
            resolver,
        })
        .unwrap()
    }

    #[test]
    fn value_layer_requests_only_the_selected_byte_range() {
        let descriptor = NativeOid::from_bytes(&[5; 20]).unwrap();
        let identity = ContentIdentity::from_bytes(b"abcdef");
        let resolver = resolver(descriptor.clone(), identity, b"abcdef", b"abcdef");
        let context = context(resolver.clone());
        let blob = Blob::from_reference(
            context
                .reference(identity, descriptor)
                .expect("issue pinned descriptor reference"),
        )
        .unwrap();

        assert_eq!(blob.read(2, 1).unwrap(), b"c");
        assert_eq!(resolver.reads.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(resolver.bytes_read.load(AtomicOrdering::SeqCst), 2);

        assert!(blob.read(6, 1).unwrap().is_empty());
        assert_eq!(resolver.reads.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(resolver.bytes_read.load(AtomicOrdering::SeqCst), 2);

        assert_eq!(blob.read_to_end().unwrap(), b"abcdef");
        assert_eq!(resolver.reads.load(AtomicOrdering::SeqCst), 5);
        assert_eq!(resolver.bytes_read.load(AtomicOrdering::SeqCst), 8);
    }

    #[test]
    fn resolver_rejects_replaced_descriptors_and_bad_returned_chunks() {
        let descriptor = NativeOid::from_bytes(&[7; 20]).unwrap();
        let identity = ContentIdentity::from_bytes(b"abc");
        let resolver = resolver(descriptor.clone(), identity, b"axc", b"abc");
        let context = context(resolver.clone());
        let blob = Blob::from_reference(
            context
                .reference(identity, descriptor)
                .expect("issue pinned descriptor reference"),
        )
        .unwrap();

        assert_eq!(blob.read(0, 1), Err(Error::ContentDigestMismatch));
        assert_eq!(resolver.reads.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(resolver.bytes_read.load(AtomicOrdering::SeqCst), 2);

        let replaced = Blob::from_reference(
            context
                .reference(identity, NativeOid::from_bytes(&[6; 20]).unwrap())
                .expect("issue replacement descriptor reference"),
        )
        .unwrap();
        assert_eq!(replaced.read(0, 1), Err(Error::MissingDescriptor));
        assert_eq!(replaced.read(0, 0), Err(Error::MissingDescriptor));
        assert_eq!(resolver.reads.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(resolver.bytes_read.load(AtomicOrdering::SeqCst), 2);
    }

    #[test]
    fn same_content_over_admitted_references_compares_exact_bytes() {
        let descriptor = NativeOid::from_bytes(&[6; 20]).unwrap();
        let identity = ContentIdentity::from_bytes(b"abcdef");
        let resolver = resolver(descriptor.clone(), identity, b"abcdef", b"abcdef");
        let context = context(resolver);
        let left =
            Blob::from_reference(context.reference(identity, descriptor.clone()).unwrap()).unwrap();
        let right = Blob::from_reference(context.reference(identity, descriptor).unwrap()).unwrap();

        assert!(left.same_content(&right).unwrap());
        assert!(left.same_content(&left).unwrap());
    }

    #[test]
    fn same_content_rejects_tampered_bytes_under_an_admitted_identity() {
        let identity = ContentIdentity::from_bytes(b"abc");
        let good_descriptor = NativeOid::from_bytes(&[4; 20]).unwrap();
        let tampered_descriptor = NativeOid::from_bytes(&[5; 20]).unwrap();
        let good_resolver = resolver(good_descriptor.clone(), identity, b"abc", b"abc");
        let tampered_resolver = resolver(tampered_descriptor.clone(), identity, b"axc", b"abc");
        let good = Blob::from_reference(
            context(good_resolver)
                .reference(identity, good_descriptor)
                .unwrap(),
        )
        .unwrap();
        let tampered = Blob::from_reference(
            context(tampered_resolver)
                .reference(identity, tampered_descriptor)
                .unwrap(),
        )
        .unwrap();

        assert_eq!(
            good.same_content(&tampered),
            Err(Error::ContentDigestMismatch)
        );
    }

    #[test]
    fn empty_reference_reads_still_verify_the_pinned_descriptor() {
        let descriptor = NativeOid::from_bytes(&[10; 20]).unwrap();
        let identity = ContentIdentity::from_bytes(b"");
        let resolver = resolver(descriptor.clone(), identity, b"", b"");
        let context = context(resolver.clone());
        let blob = Blob::from_reference(
            context
                .reference(identity, descriptor)
                .expect("issue pinned empty descriptor reference"),
        )
        .unwrap();

        assert!(blob.read_to_end().unwrap().is_empty());
        assert!(blob.read(0, 0).unwrap().is_empty());
        assert_eq!(resolver.reads.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(resolver.bytes_read.load(AtomicOrdering::SeqCst), 0);

        let replaced = Blob::from_reference(
            context
                .reference(identity, NativeOid::from_bytes(&[11; 20]).unwrap())
                .expect("issue replaced empty descriptor reference"),
        )
        .unwrap();
        assert_eq!(replaced.read_to_end(), Err(Error::MissingDescriptor));
        assert_eq!(resolver.reads.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(resolver.bytes_read.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn full_reads_still_verify_the_raw_content_commitment() {
        let descriptor = NativeOid::from_bytes(&[8; 20]).unwrap();
        let identity = ContentIdentity::from_bytes(b"abc");
        let resolver = resolver(descriptor.clone(), identity, b"axc", b"axc");
        let context = context_with_limit(resolver.clone(), 4);
        let blob = Blob::from_reference(
            context
                .reference(identity, descriptor)
                .expect("issue pinned descriptor reference"),
        )
        .unwrap();

        assert_eq!(blob.read(0, 1).unwrap(), b"a");
        assert_eq!(blob.read(0, 3), Err(Error::ContentDigestMismatch));
        assert_eq!(blob.read_to_end(), Err(Error::ContentDigestMismatch));
    }
}

#[cfg(test)]
mod anonymous_record_order_tests {
    use super::*;

    fn str_type() -> Raw {
        Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".into())])
    }

    fn int_type() -> Raw {
        Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())])
    }

    /// The descriptor declares `label` before `rank`, which is name order. Stored entries
    /// must follow encoded field-key order (`rank` before `label`), so matching is by key.
    fn descriptor_fields() -> Vec<Raw> {
        vec![
            Raw::Array(vec![Raw::Text("label".into()), str_type()]),
            Raw::Array(vec![Raw::Text("rank".into()), int_type()]),
        ]
    }

    fn record(entries: Vec<(&str, Raw)>) -> Raw {
        Raw::Tag(
            RECORD_TAG,
            Box::new(Raw::Array(vec![
                Raw::Null,
                Raw::Array(
                    entries
                        .into_iter()
                        .map(|(name, value)| Raw::Array(vec![Raw::Text(name.to_owned()), value]))
                        .collect(),
                ),
            ])),
        )
    }

    #[test]
    fn accepts_ascending_encoded_keys_in_any_descriptor_order() {
        let stored = record(vec![
            ("rank", Raw::Int(1.into())),
            ("label", Raw::Text("x".into())),
        ]);
        assert_eq!(
            validate_anonymous_record(&stored, &descriptor_fields(), &[]),
            Ok(())
        );
    }

    #[test]
    fn refuses_unordered_entries() {
        let stored = record(vec![
            ("label", Raw::Text("x".into())),
            ("rank", Raw::Int(1.into())),
        ]);
        assert_eq!(
            validate_anonymous_record(&stored, &descriptor_fields(), &[]),
            Err(Error::DuplicateOrUnorderedMapKey)
        );
    }

    #[test]
    fn refuses_duplicate_keys() {
        let stored = record(vec![
            ("rank", Raw::Int(1.into())),
            ("rank", Raw::Int(2.into())),
        ]);
        assert_eq!(
            validate_anonymous_record(&stored, &descriptor_fields(), &[]),
            Err(Error::DuplicateOrUnorderedMapKey)
        );
    }

    #[test]
    fn refuses_unknown_keys() {
        let stored = record(vec![
            ("rank", Raw::Int(1.into())),
            ("other", Raw::Text("x".into())),
        ]);
        assert_eq!(
            validate_anonymous_record(&stored, &descriptor_fields(), &[]),
            Err(Error::InvalidValue)
        );
    }
}
