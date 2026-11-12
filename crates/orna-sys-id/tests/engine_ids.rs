//! The committed engine identity table must equal the sealed engine catalogue byte-for-byte.
//!
//! Each `u128` is compared through its 32-digit lowercase hex spelling, which is the
//! exact text the catalogue stores. A mismatch in any row, order or count fails here.

use std::collections::BTreeSet;

use orna_sys_id::engine_ids::{ENGINE_FUNCTION_IDS, ENGINE_ONLY_TYPE_IDS, SIGNATURE_TYPE_IDS};
use serde_json::Value;

const ENGINE: &str = include_str!("../catalogue/engine-1.1.0.json");

fn catalogue() -> Value {
    serde_json::from_str(ENGINE).expect("engine-1.1.0.json parses")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("catalogue id is a string")
}

fn hex(id: u128) -> String {
    format!("{id:032x}")
}

fn signature_ids(catalogue: &Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for function in catalogue["sealed_functions"]
        .as_array()
        .expect("sealed_functions array")
    {
        let signature = &function["signature"];
        for key in [
            "result_type",
            "parameter_id",
            "request_type",
            "event_stream_item_type",
            "stream_item_type",
        ] {
            if let Some(id) = signature[key].as_str() {
                ids.insert(id.to_owned());
            }
        }
        for id in signature["parameter_types"]
            .as_array()
            .into_iter()
            .flatten()
        {
            ids.insert(text(id).to_owned());
        }
    }
    ids
}

#[test]
fn engine_function_ids_match_catalogue_byte_for_byte() {
    let catalogue = catalogue();
    let sealed = catalogue["sealed_functions"]
        .as_array()
        .expect("sealed_functions array");
    assert_eq!(ENGINE_FUNCTION_IDS.len(), sealed.len());
    for ((name, id), function) in ENGINE_FUNCTION_IDS.iter().zip(sealed) {
        assert_eq!(*name, text(&function["name"]));
        assert_eq!(
            hex(*id),
            text(&function["function_id"]),
            "function id for {name}"
        );
    }
}

#[test]
fn engine_only_type_ids_match_catalogue_byte_for_byte() {
    let catalogue = catalogue();
    let types = catalogue["engine_only_types"]
        .as_array()
        .expect("engine_only_types array");
    assert_eq!(ENGINE_ONLY_TYPE_IDS.len(), types.len());
    for ((name, id), entry) in ENGINE_ONLY_TYPE_IDS.iter().zip(types) {
        assert_eq!(*name, text(&entry["name"]));
        assert_eq!(hex(*id), text(&entry["type_id"]), "type id for {name}");
    }
}

#[test]
fn signature_type_ids_are_exactly_the_catalogue_set() {
    let catalogue = catalogue();
    let expected = signature_ids(&catalogue);
    let actual: BTreeSet<String> = SIGNATURE_TYPE_IDS.iter().map(|id| hex(*id)).collect();
    assert_eq!(SIGNATURE_TYPE_IDS.len(), expected.len());
    assert_eq!(actual, expected);
    let ascending = SIGNATURE_TYPE_IDS.windows(2).all(|pair| pair[0] < pair[1]);
    assert!(ascending, "SIGNATURE_TYPE_IDS must be strictly ascending");
}
