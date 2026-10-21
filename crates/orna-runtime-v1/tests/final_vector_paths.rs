//! Orna 1.1.0 `tests/final-vectors.json`, `paths`: the production VFS-1 key
//! component projection reproduces every published host spelling, including
//! the long `~key-` alias (`profiles/vfs-1.md`, "Row/key directory mapping").

use orna_runtime_v1::vfs::{VfsComponent, project_component, unproject_component};
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
                assert_eq!(hex(&digest), hex(&Sha256::digest(encoding)), "paths[{index}]");
            }
            Err(error) => panic!("paths[{index}] does not unproject: {error:?}"),
        }
    }
}
