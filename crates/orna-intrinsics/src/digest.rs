//! Deterministic source digest for the `std_intrinsics.json` freshness check.
//!
//! The digest covers every `.rs` file below `src/` except the export binary.
//! `build.rs` compiles this file by path and compares the digest recorded in
//! the checked-in JSON, so editing a descriptor without regenerating the export
//! fails the build with an actionable message.

use std::fs;
use std::path::{Path, PathBuf};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Returns the FNV-1a 64-bit digest of the catalogue sources, as lowercase hex.
pub fn source_digest() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_sources(&root, &root, &mut files);
    files.sort();

    let mut hash = FNV_OFFSET;
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .expect("source file below the crate source root")
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&file).unwrap_or_else(|error| {
            panic!("read {} for the catalogue digest: {error}", file.display())
        });
        hash_bytes(&mut hash, relative.as_bytes());
        hash_bytes(&mut hash, &[0]);
        hash_bytes(&mut hash, &bytes);
    }
    format!("{hash:016x}")
}

fn collect_rust_sources(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).unwrap_or_else(|error| {
        panic!(
            "read {} for the catalogue digest: {error}",
            directory.display()
        )
    });
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| {
            panic!(
                "walk {} for the catalogue digest: {error}",
                directory.display()
            )
        });
        let path = entry.path();
        if path.is_dir() {
            if path == root.join("bin") {
                continue;
            }
            collect_rust_sources(root, &path, output);
        } else if path
            .extension()
            .is_some_and(|extension| extension.to_str() == Some("rs"))
        {
            output.push(path);
        }
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}
