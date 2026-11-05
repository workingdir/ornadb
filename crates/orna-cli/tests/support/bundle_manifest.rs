//! Walks an offline copy bundle and renders a `sha256sum`-format manifest of its
//! files, so two bundles can be compared byte-for-byte.

use std::path::Path;

use sha2::{Digest, Sha256};

/// Lowercase hex of a digest, shared with the format-3 fixture builder.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

/// Returns the bundle's relative file paths, sorted: `index.tsv` plus every
/// file under `media/`.
pub fn bundle_files(directory: &Path) -> Vec<String> {
    let mut files = vec!["index.tsv".to_owned()];
    for entry in std::fs::read_dir(directory.join("media")).expect("bundle media directory") {
        let entry = entry.expect("media entry");
        files.push(format!(
            "media/{}",
            entry.file_name().to_str().expect("UTF-8 media name")
        ));
    }
    files.sort();
    files
}

/// Returns `sha256sum`-format bytes for every bundle file, in `bundle_files`
/// order: `<lowercase hex>  <relative path>\n` per file.
pub fn bundle_manifest(directory: &Path) -> Vec<u8> {
    let mut manifest = String::new();
    for name in bundle_files(directory) {
        let bytes = std::fs::read(directory.join(&name)).expect("bundle file is readable");
        let digest = Sha256::digest(&bytes);
        let hex = hex_encode(&digest);
        manifest.extend([format!("{hex}  {name}\n")]);
    }
    manifest.into_bytes()
}
