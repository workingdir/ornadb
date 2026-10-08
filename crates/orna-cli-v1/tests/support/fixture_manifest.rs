//! Renders the `sha256sum` manifest for a directory of fixture files, so a test
//! can compare it byte-for-byte with the committed `manifest.sha256`.

use std::path::Path;

use sha2::{Digest, Sha256};

/// Returns `sha256sum`-format bytes for the named files under `directory`, in
/// the order given: `<lowercase hex>  <name>\n` per file.
pub fn manifest_bytes(directory: &Path, names: &[&str]) -> Vec<u8> {
    let mut manifest = String::new();
    for name in names {
        let bytes = std::fs::read(directory.join(name)).expect("fixture file is readable");
        let digest = Sha256::digest(&bytes);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        manifest.push_str(&format!("{hex}  {name}\n"));
    }
    manifest.into_bytes()
}
