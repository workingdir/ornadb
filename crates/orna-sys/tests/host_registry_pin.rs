//! The generated host-operation registry bytes must not drift.

use orna_sys::system_host_operation_registry_json;
use sha2::{Digest, Sha256};

/// SHA-256 of the pre-reform `system_host_operations.json` bytes.
///
/// S2 only changes how the facts are declared in Rust; the generated registry is
/// the same document byte for byte.
const HOST_REGISTRY_SHA256: &str =
    "168da41ada8c394f9545f0d5e6008ced6574bb2aefece57908e8096e95d6a023";

#[test]
fn generated_host_operation_registry_bytes_are_frozen() {
    let bytes = system_host_operation_registry_json();
    assert_eq!(
        format!("{:x}", Sha256::digest(bytes.as_bytes())),
        HOST_REGISTRY_SHA256,
        "generated host-operation registry bytes changed"
    );
}
