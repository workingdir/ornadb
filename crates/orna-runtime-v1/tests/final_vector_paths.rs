//! Orna 1.1.0 `tests/final-vectors.json`, `paths`: the production VFS-1 key
//! component projection reproduces every published host spelling, including
//! the long `~key-` alias (`profiles/vfs-1.md`, "Row/key directory mapping").

use orna_runtime_v1::vfs::{
    VfsComponent, project_component, project_key_component, unproject_component,
};
use sha2::{Digest, Sha256};

/// A byte-identical copy of the Orna 1.1.0 release file, pinned to its release
/// `SHA256SUMS` value so drift from the publication fails here.
const FINAL_VECTORS_JSON: &str = include_str!("fixtures/reference/tests/final-vectors.json");
const FINAL_VECTORS_SHA256: &str =
    "985140787cde755ae53034de28d46cb4f5696d8bc95e385876e95ec0c953899d";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn final_vectors() -> serde_json::Value {
    assert_eq!(
        hex(&Sha256::digest(FINAL_VECTORS_JSON.as_bytes())),
        FINAL_VECTORS_SHA256,
        "the vendored final vectors no longer match the release"
    );
    serde_json::from_str(FINAL_VECTORS_JSON).expect("the vendored final vectors are JSON")
}

#[test]
fn final_vector_paths_project_to_the_published_components() {
    let vectors = final_vectors();
    let paths = vectors["paths"].as_array().expect("path vectors");
    assert_eq!(paths.len(), 11);
    for (index, case) in paths.iter().enumerate() {
        let input = case["input"].as_str().expect("path input");
        let component = case["component"].as_str().expect("published component");
        assert_eq!(project_component(input), component, "paths[{index}]");
        match unproject_component(component) {
            Ok(VfsComponent::Canonical(text)) => assert_eq!(text, input, "paths[{index}]"),
            Ok(VfsComponent::LongAlias { digest }) => {
                // The alias digests the canonical typed key component, which
                // for a Str key is its OVB-1 encoding, not its raw UTF-8 bytes.
                let encoding =
                    orna_value_v1::encode_typed(&input.to_owned()).expect("a Str key encodes");
                assert_eq!(
                    hex(&digest),
                    hex(&Sha256::digest(encoding)),
                    "paths[{index}]"
                );
            }
            Err(error) => panic!("paths[{index}] does not unproject: {error:?}"),
        }
    }
}

/// The long alias digests the canonical typed key component, so 250 digits
/// keyed as an integer and as a Str project to different names (VFS-1, "Row/key
/// directory mapping": a text `1` and an integer 1 cannot collide).
#[test]
fn typed_long_key_aliases_digest_the_typed_encoding() {
    let digits = "9".repeat(250);
    let integer = num_bigint::BigInt::parse_bytes(digits.as_bytes(), 10).expect("decimal digits");
    let integer_encoding = orna_value_v1::Value::int(integer)
        .encode()
        .expect("an Int encodes");
    let integer_alias = project_key_component(&digits, &integer_encoding);
    assert_eq!(
        integer_alias,
        format!("~key-{}", hex(&Sha256::digest(&integer_encoding)))
    );

    let text_encoding = orna_value_v1::encode_typed(&digits).expect("a Str encodes");
    assert_eq!(
        project_key_component(&digits, &text_encoding),
        project_component(&digits)
    );
    assert_ne!(integer_alias, project_component(&digits));

    // Under the bound the spelling is the canonical text whatever the type.
    assert_eq!(project_key_component("42", &[0x18, 0x2a]), "42");
}
