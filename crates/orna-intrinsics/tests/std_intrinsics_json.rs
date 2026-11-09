//! The checked-in `std_intrinsics.json` must equal a fresh render.

use orna_intrinsics::{IntrinsicId, catalogue, export};

#[test]
fn artifact_is_current() {
    let artifact = include_str!("../std_intrinsics.json");
    let rendered = export::json();
    assert_eq!(
        artifact, rendered,
        "std_intrinsics.json is stale; run `cargo run -p orna-intrinsics --bin std-api-export`"
    );
}

#[test]
fn artifact_mentions_every_id_exactly_once() {
    let artifact = include_str!("../std_intrinsics.json");
    assert!(artifact.starts_with("{\n"));
    assert!(artifact.ends_with("]\n}\n"));
    assert!(artifact.contains("\"schema\": \"orna.std_intrinsics.v1\""));
    for id in IntrinsicId::ALL {
        let marker = format!("\"id\": \"{}\",", id.name());
        assert_eq!(
            artifact.matches(&marker).count(),
            1,
            "{} is missing or duplicated in the artifact",
            id.name()
        );
    }
    assert_eq!(catalogue().len(), IntrinsicId::ALL.len());
}

#[test]
fn artifact_is_deterministic_across_calls() {
    assert_eq!(export::json(), export::json());
}

#[test]
fn artifact_digest_matches_the_sources() {
    let artifact = include_str!("../std_intrinsics.json");
    let marker = "\"source_digest\": \"";
    let start = artifact.find(marker).expect("source_digest field") + marker.len();
    let rest = &artifact[start..];
    let end = rest.find('"').expect("closing quote");
    assert_eq!(&rest[..end], orna_intrinsics::digest::source_digest());
}
