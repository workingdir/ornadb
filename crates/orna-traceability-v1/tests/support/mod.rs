//! Shared publication-identity checks for the traceability integration tests.

use sha2::{Digest, Sha256};
use std::{fs, path::Path};

const MARKDOWN: &str = "Orna-1.1.0.md";

/// Lowercase SHA-256 of the canonical 1.1.0 markdown file under `root`.
fn canonical_markdown_digest(root: &Path) -> String {
    let bytes = fs::read(root.join(MARKDOWN)).expect("read canonical markdown");
    format!("{:x}", Sha256::digest(bytes))
}

/// The digest `SHA256SUMS` publishes for the canonical 1.1.0 markdown file.
fn published_markdown_digest(root: &Path) -> String {
    let sums = fs::read_to_string(root.join("SHA256SUMS")).expect("read SHA256SUMS");
    let mut entries = sums.lines().filter_map(|line| {
        let (digest, name) = line.split_once("  ")?;
        (name == MARKDOWN).then(|| digest.to_owned())
    });
    let digest = entries.next().expect("SHA256SUMS lists the canonical markdown");
    assert!(entries.next().is_none(), "SHA256SUMS lists the canonical markdown once");
    digest
}

/// Positive publication identity: the vendored markdown must hash to the
/// `SHA256SUMS` entry for the 1.1.0 publication.
pub fn assert_markdown_publication_digest(root: &Path) {
    assert_eq!(
        canonical_markdown_digest(root),
        published_markdown_digest(root),
        "canonical 1.1.0 markdown matches its SHA256SUMS publication entry"
    );
}
