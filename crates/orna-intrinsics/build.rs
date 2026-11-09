//! Verifies the checked-in `std_intrinsics.json` is current.
//!
//! The digest covers every catalogue source file, so a descriptor edit that is
//! not followed by `cargo run --bin std-api-export` fails here with the exact
//! command to run. `tests/std_intrinsics_json.rs` additionally proves the
//! recorded artifact equals a fresh render from the compiled catalogue.

use std::{env, fs, path::PathBuf};

#[path = "src/digest.rs"]
mod digest;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=std_intrinsics.json");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let artifact = manifest.join("std_intrinsics.json");
    let text = fs::read_to_string(&artifact).unwrap_or_else(|error| {
        panic!(
            "{} is missing or unreadable ({error}); run `cargo run -p orna-intrinsics --bin std-api-export`",
            artifact.display()
        )
    });
    let recorded = recorded_digest(&text).unwrap_or_else(|| {
        panic!(
            "{} has no source_digest field; run `cargo run -p orna-intrinsics --bin std-api-export`",
            artifact.display()
        )
    });
    let expected = digest::source_digest();
    assert_eq!(
        recorded,
        expected.as_str(),
        "{} is stale; run `cargo run -p orna-intrinsics --bin std-api-export`",
        artifact.display()
    );
}

fn recorded_digest(text: &str) -> Option<&str> {
    let marker = "\"source_digest\": \"";
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}
