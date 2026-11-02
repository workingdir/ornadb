//! Renders the `sha256sum` manifest for a directory of fixture files, so a test
//! can compare it byte-for-byte with the committed `manifest.sha256`.

use std::fmt::Write as _;

use std::path::Path;

use sha2::{Digest, Sha256};

/// Returns `sha256sum`-format bytes for the named files under `directory`, in
/// the order given: `<lowercase hex>  <name>\n` per file.
pub fn manifest_bytes(directory: &Path, names: &[&str]) -> Vec<u8> {
    let mut manifest = String::new();
    for name in names {
        let bytes = std::fs::read(directory.join(name)).expect("fixture file is readable");
        let digest = Sha256::digest(&bytes);
        let mut hex = String::with_capacity(64);
        for byte in &digest {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
        }
        manifest.extend([format!("{hex}  {name}\n")]);
    }
    manifest.into_bytes()
}
