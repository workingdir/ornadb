//! Repository-format-2 Blob reads (ORNA-ROW-013, ORNA-UPGRADE-001,
//! ORNA-UPGRADE-002).
//!
//! A format-2 Blob field is either an ORR-2 value (a storage record or inline
//! padded Base64) or a ROV-2 CBOR reference. A storage reference names a
//! content root under `.orna/blobs/<2>/<62>/` in the pinned commit. This module
//! is the Blob-field read API for those fields: it pins one selector, admits
//! only a repository-format-2 commit, decodes the field, resolves the root
//! through the repository's object reader, and verifies it with the OGB-1
//! verifier before returning any bytes. It is NOT yet called from a production
//! row read path; the follow-up beads are cross-linked from ornadb-nne7.
//!
//! Format-1 and format-3 commits are refused here. The format-2 record is
//! read-only: nothing in this module writes to the object database.

use std::cell::RefCell;
use std::fmt;

use crate::format2_ogb::{
    OGB_MAX_CHUNK, OGB_MAX_READ_CALL, OgbError, OgbObjects, OgbTreeEntry, read_ogb_root,
};
use crate::format2_orr::{Orr2BlobField, Orr2DecodeError, decode_orr2_blob_field};
use crate::format2_rov::{Rov2Blob, Rov2DecodeError, decode_rov2_blob};
use crate::native_graph::{hex_decode, hex_encode};
use crate::object_reader::{ObjectKind, ObjectReader, TreeEntry, TreeEntryMode};
use crate::{FormatContextError, RepositoryFormat};
use crate::{GitCommitRef, Repository, RepositoryError};

/// Why a format-2 Blob field could not be read. `Git` and `RootMissing` are
/// not evidence of corruption; `Ogb` and `RecordMismatch` are.
#[derive(Debug)]
pub enum Format2BlobError {
    /// The selector could not be pinned or its format metadata was refused.
    Context(FormatContextError),
    /// The pinned commit is not repository format 2. Format-2 Blob fields are
    /// never read from another format.
    NotFormat2(RepositoryFormat),
    /// The stored field is not a valid ORR-2 Blob value.
    OrrField(Orr2DecodeError),
    /// The stored value is not a valid ROV-2 reference.
    Rov2Value(Rov2DecodeError),
    /// The Blob content is larger than the caller's read limit. Checked before
    /// any content object is read.
    Limit { total: u64, limit: u64 },
    /// The pinned commit has no content root for the recorded digest.
    RootMissing { path: String },
    /// Git failed while reading an object. Not evidence of corruption.
    Git(RepositoryError),
    /// The OGB-1 structure or its content check failed.
    Ogb(OgbError),
    /// The verified root does not match the digest or length the field recorded.
    RecordMismatch(&'static str),
}

impl fmt::Display for Format2BlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context(error) => write!(f, "format-2 Blob read: {error}"),
            Self::NotFormat2(format) => write!(
                f,
                "format-2 Blob fields are read only from repository format 2, not {format:?}"
            ),
            Self::OrrField(error) => write!(f, "{error}"),
            Self::Rov2Value(error) => write!(f, "{error}"),
            Self::Limit { total, limit } => write!(
                f,
                "Blob content of {total} bytes exceeds the read limit of {limit} bytes"
            ),
            Self::RootMissing { path } => {
                write!(
                    f,
                    "Blob content root {path} is absent from the pinned commit"
                )
            }
            Self::Git(error) => write!(f, "{error}"),
            Self::Ogb(error) => write!(f, "{error}"),
            Self::RecordMismatch(why) => {
                write!(f, "Blob content root does not match its record: {why}")
            }
        }
    }
}

impl std::error::Error for Format2BlobError {}

impl Repository {
    /// Reads the content of one ORR-2 Blob-slot value at a pinned selector.
    ///
    /// `field` is the stored text. An inline value returns its decoded bytes; a
    /// storage record resolves through `.orna/blobs/` in the pinned commit.
    /// `max_len` bounds the returned content and never exceeds 8 MiB (ORNA-BLOB-011).
    pub fn read_format2_orr_blob(
        &self,
        selector: &str,
        field: &str,
        max_len: u64,
    ) -> Result<Vec<u8>, Format2BlobError> {
        let commit = pin_format2(self, selector)?;
        match decode_orr2_blob_field(field).map_err(Format2BlobError::OrrField)? {
            Orr2BlobField::Inline(bytes) => {
                let total = bytes.len() as u64;
                let limit = max_len.min(OGB_MAX_READ_CALL);
                if total > limit {
                    return Err(Format2BlobError::Limit { total, limit });
                }
                Ok(bytes)
            }
            Orr2BlobField::Reference(reference) => read_root(
                self,
                &commit,
                &reference.content_sha256,
                reference.length,
                max_len,
            ),
        }
    }

    /// Reads the content of one ROV-2 storage reference at a pinned selector.
    /// `value` is the tagged CBOR reference. `max_len` bounds the content.
    pub fn read_format2_rov_blob(
        &self,
        selector: &str,
        value: &[u8],
        max_len: u64,
    ) -> Result<Vec<u8>, Format2BlobError> {
        let commit = pin_format2(self, selector)?;
        let Rov2Blob {
            content_sha256,
            total_length,
        } = decode_rov2_blob(value).map_err(Format2BlobError::Rov2Value)?;
        read_root(self, &commit, &content_sha256, total_length, max_len)
    }
}

/// Pins one selector once and admits it only as a repository-format-2 commit.
fn pin_format2(repository: &Repository, selector: &str) -> Result<GitCommitRef, Format2BlobError> {
    let (commit, context) = repository
        .open_pinned_format_context_with_commit(selector)
        .map_err(Format2BlobError::Context)?;
    match context.repository_format() {
        RepositoryFormat::Legacy2 => Ok(commit),
        other => Err(Format2BlobError::NotFormat2(other)),
    }
}

/// Resolves `.orna/blobs/<2>/<62>/` in the pinned commit, verifies the root
/// with OGB-1, and checks it against the digest and length the field recorded.
fn read_root(
    repository: &Repository,
    commit: &GitCommitRef,
    digest: &[u8; 32],
    length: u64,
    max_len: u64,
) -> Result<Vec<u8>, Format2BlobError> {
    if length > max_len {
        return Err(Format2BlobError::Limit {
            total: length,
            limit: max_len,
        });
    }
    let digest_hex = hex_encode(digest);
    let path = format!(".orna/blobs/{}/{}", &digest_hex[..2], &digest_hex[2..]);
    let objects = repository.objects();
    let root = objects
        .info(&format!("{}:{path}", commit.as_str()))
        .map_err(Format2BlobError::Git)?
        .ok_or_else(|| Format2BlobError::RootMissing { path: path.clone() })?;
    if root.kind() != ObjectKind::Tree {
        return Err(Format2BlobError::RecordMismatch(
            "content root is not a tree",
        ));
    }
    let root_entries = objects
        .tree_entries(root.oid())
        .map_err(Format2BlobError::Git)?
        .iter()
        .map(ogb_entry)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Format2BlobError::Ogb)?;

    let store = PinnedObjects {
        objects,
        failure: RefCell::new(None),
    };
    let outcome = read_ogb_root(&store, &root_entries, max_len);
    // A Git failure or a wrong-typed child is kept by the adapter; it is the
    // real cause, so it is reported before the verifier's generic result.
    if let Some(error) = store.failure.into_inner() {
        return Err(error);
    }
    let (verified, payload) = outcome.map_err(Format2BlobError::Ogb)?;
    if verified.content_sha256 != *digest {
        return Err(Format2BlobError::RecordMismatch("content digest"));
    }
    if verified.total_length != length || payload.len() as u64 != length {
        return Err(Format2BlobError::RecordMismatch("content length"));
    }
    Ok(payload)
}

/// Adapts the repository object reader to the OGB-1 verifier.
///
/// The verifier's trait can only say "absent". Git failures and wrong-typed
/// or oversized children are therefore stored here and reported in place of a
/// bare "missing", so an I/O failure is never presented as absence.
struct PinnedObjects<'a> {
    objects: &'a ObjectReader,
    failure: RefCell<Option<Format2BlobError>>,
}

impl PinnedObjects<'_> {
    fn record(&self, error: Format2BlobError) {
        let mut slot = self.failure.borrow_mut();
        if slot.is_none() {
            *slot = Some(error);
        }
    }
}

impl OgbObjects for PinnedObjects<'_> {
    fn tree(&self, oid: &[u8]) -> Option<Vec<OgbTreeEntry>> {
        let hex = hex_encode(oid);
        let header = match self.objects.info(&hex) {
            Ok(Some(header)) => header,
            Ok(None) => return None,
            Err(error) => {
                self.record(Format2BlobError::Git(error));
                return None;
            }
        };
        if header.kind() != ObjectKind::Tree {
            self.record(Format2BlobError::Ogb(OgbError::Malformed(
                "child object is not a tree",
            )));
            return None;
        }
        let entries = match self.objects.tree_entries(&hex) {
            Ok(entries) => entries,
            Err(error) => {
                self.record(Format2BlobError::Git(error));
                return None;
            }
        };
        let mut converted = Vec::with_capacity(entries.len());
        for entry in &entries {
            match ogb_entry(entry) {
                Ok(entry) => converted.push(entry),
                Err(error) => {
                    self.record(Format2BlobError::Ogb(error));
                    return None;
                }
            }
        }
        Some(converted)
    }

    fn blob(&self, oid: &[u8]) -> Option<Vec<u8>> {
        let hex = hex_encode(oid);
        let header = match self.objects.info(&hex) {
            Ok(Some(header)) => header,
            Ok(None) => return None,
            Err(error) => {
                self.record(Format2BlobError::Git(error));
                return None;
            }
        };
        if header.kind() != ObjectKind::Blob {
            self.record(Format2BlobError::Ogb(OgbError::Malformed(
                "child object is not a blob",
            )));
            return None;
        }
        if header.size() > OGB_MAX_CHUNK {
            self.record(Format2BlobError::Ogb(OgbError::Malformed(
                "blob exceeds the OGB-1 chunk bound",
            )));
            return None;
        }
        // The bound above keeps the size within `usize` on supported targets.
        match self.objects.read_blob(&hex, header.size() as usize) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                self.record(Format2BlobError::Git(error));
                None
            }
        }
    }
}

/// Converts one Git tree entry to the OGB-1 view. Modes are Git's raw tree-object
/// spellings, which the OGB-1 verifier compares against.
fn ogb_entry(entry: &TreeEntry) -> Result<OgbTreeEntry, OgbError> {
    let name = std::str::from_utf8(entry.name())
        .map_err(|_| OgbError::Malformed("entry name is not UTF-8"))?;
    let oid = hex_decode(entry.oid()).ok_or(OgbError::Malformed("entry object id"))?;
    Ok(OgbTreeEntry {
        name: name.to_owned(),
        mode: raw_mode(entry.mode()).to_owned(),
        oid,
    })
}

const fn raw_mode(mode: TreeEntryMode) -> &'static str {
    match mode {
        TreeEntryMode::File => "100644",
        TreeEntryMode::Executable => "100755",
        TreeEntryMode::Symlink => "120000",
        TreeEntryMode::Tree => "40000",
        TreeEntryMode::Gitlink => "160000",
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::format2_ogb::OgbTreeEntry;
    use crate::format2_ogb::tests::{MemStore, abc_root};
    use crate::test_support::configure_fixture_git_identity;

    const FORMAT2_RECORD: &str = "{\n  repository_format: 2,\n  row_profile: \"ORR-2\",\n  blob_profile: \"OGB-1\",\n  semantic_profile: \"SOV-2\",\n  compact_profile: \"compact-storage-v2\",\n}\n";
    const FORMAT1_RECORD: &str =
        "{\n  repository_format: 1,\n  storage_profile: \"compact-storage-v1\",\n}\n";

    /// Runs one `git -C dir` command, feeding `input` on stdin, and returns trimmed stdout.
    fn git(dir: &Path, args: &[&str], input: &[u8]) -> String {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("git starts");
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(input)
            .expect("stdin is written");
        let output = child.wait_with_output().expect("git finishes");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output is UTF-8")
            .trim()
            .to_owned()
    }

    /// Writes one OGB-1 object and everything below it into the repository.
    fn write_object(dir: &Path, store: &MemStore, oid: &[u8]) -> String {
        if let Some(entries) = store.tree(oid) {
            return write_tree(dir, store, &entries);
        }
        let data = store.blob(oid).expect("object is in the fixture store");
        git(dir, &["hash-object", "-w", "--stdin"], &data)
    }

    fn write_tree(dir: &Path, store: &MemStore, entries: &[OgbTreeEntry]) -> String {
        let mut listing = String::new();
        for entry in entries {
            let child = write_object(dir, store, &entry.oid);
            let (mode, kind) = if entry.mode == "40000" {
                ("040000", "tree")
            } else {
                (entry.mode.as_str(), "blob")
            };
            listing.push_str(&format!("{mode} {kind} {child}\t{}\n", entry.name));
        }
        git(dir, &["mktree"], listing.as_bytes())
    }

    /// One content root placed at `.orna/blobs/<2>/<62>/` for `digest`.
    struct Root<'a> {
        store: &'a MemStore,
        entries: &'a [OgbTreeEntry],
        digest: [u8; 32],
    }

    /// A repository whose HEAD commit holds `.orna/format.orna` with `format_record`
    /// and, when given, the content root. Returns the temp dir to keep it alive.
    fn commit_fixture(
        format_record: &str,
        root: Option<Root<'_>>,
    ) -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path();
        git(path, &["init", "-q"], b"");
        configure_fixture_git_identity(path);
        let format_oid = git(
            path,
            &["hash-object", "-w", "--stdin"],
            format_record.as_bytes(),
        );
        let mut orna = format!("100644 blob {format_oid}\tformat.orna\n");
        if let Some(root) = root {
            let digest_hex = hex_encode(&root.digest);
            let content = write_tree(path, root.store, root.entries);
            let rest_dir = git(
                path,
                &["mktree"],
                format!("040000 tree {content}\t{}\n", &digest_hex[2..]).as_bytes(),
            );
            let blobs_listing = format!("040000 tree {rest_dir}\t{}\n", &digest_hex[..2]);
            let blobs = git(path, &["mktree"], blobs_listing.as_bytes());
            orna.push_str(&format!("040000 tree {blobs}\tblobs\n"));
        }
        let orna_tree = git(path, &["mktree"], orna.as_bytes());
        let root_tree = git(
            path,
            &["mktree"],
            format!("040000 tree {orna_tree}\t.orna\n").as_bytes(),
        );
        let commit = git(path, &["commit-tree", &root_tree, "-m", "fixture"], b"");
        git(path, &["update-ref", "HEAD", &commit], b"");
        let repository = Repository::discover(path).expect("fixture repository opens");
        (dir, repository)
    }

    fn abc_digest() -> [u8; 32] {
        Sha256::digest(b"abc").into()
    }

    fn orr_record(digest: &[u8; 32], length: u64) -> String {
        format!(
            "{{blob: \"sha256:{}\", length: {length}}}",
            hex_encode(digest)
        )
    }

    #[test]
    fn reads_inline_orr2_value_from_format2_pin() {
        let (_dir, repository) = commit_fixture(FORMAT2_RECORD, None);
        let bytes = repository
            .read_format2_orr_blob("HEAD", "YWJj", 16)
            .expect("inline value reads");
        assert_eq!(bytes, b"abc");
    }

    #[test]
    fn reads_orr2_reference_through_pinned_content_root() {
        let (store, entries) = abc_root(b"abc");
        let (_dir, repository) = commit_fixture(
            FORMAT2_RECORD,
            Some(Root {
                store: &store,
                entries: &entries,
                digest: abc_digest(),
            }),
        );
        let bytes = repository
            .read_format2_orr_blob("HEAD", &orr_record(&abc_digest(), 3), 16)
            .expect("reference reads through the verified root");
        assert_eq!(bytes, b"abc");
    }

    #[test]
    fn reads_rov2_reference_through_pinned_content_root() {
        let (store, entries) = abc_root(b"abc");
        let (_dir, repository) = commit_fixture(
            FORMAT2_RECORD,
            Some(Root {
                store: &store,
                entries: &entries,
                digest: abc_digest(),
            }),
        );
        // CBOR tag 60100 around [digest bytes32, length 3].
        let mut value = vec![0xd9, 0xea, 0xc4, 0x82, 0x58, 0x20];
        value.extend_from_slice(&abc_digest());
        value.push(0x03);
        let bytes = repository
            .read_format2_rov_blob("HEAD", &value, 16)
            .expect("ROV-2 reference reads through the verified root");
        assert_eq!(bytes, b"abc");
    }

    #[test]
    fn refuses_format1_pin_for_format2_blob_fields() {
        let (_dir, repository) = commit_fixture(FORMAT1_RECORD, None);
        let error = repository
            .read_format2_orr_blob("HEAD", "YWJj", 16)
            .expect_err("format-1 commit is refused");
        assert!(matches!(
            error,
            Format2BlobError::NotFormat2(RepositoryFormat::Legacy1)
        ));
    }

    #[test]
    fn rejects_content_root_whose_chunk_was_tampered() {
        // The root records sha256("abc") but its chunk holds "abd".
        let (store, entries) = abc_root(b"abd");
        let (_dir, repository) = commit_fixture(
            FORMAT2_RECORD,
            Some(Root {
                store: &store,
                entries: &entries,
                digest: abc_digest(),
            }),
        );
        let error = repository
            .read_format2_orr_blob("HEAD", &orr_record(&abc_digest(), 3), 16)
            .expect_err("tampered content is refused");
        assert!(matches!(
            error,
            Format2BlobError::Ogb(OgbError::Integrity(_))
        ));
    }

    #[test]
    fn missing_content_root_is_reported_not_as_corruption() {
        let (_dir, repository) = commit_fixture(FORMAT2_RECORD, None);
        let error = repository
            .read_format2_orr_blob("HEAD", &orr_record(&abc_digest(), 3), 16)
            .expect_err("absent root is refused");
        assert!(matches!(error, Format2BlobError::RootMissing { .. }));
    }

    #[test]
    fn refuses_record_length_that_disagrees_with_verified_root() {
        let (store, entries) = abc_root(b"abc");
        let (_dir, repository) = commit_fixture(
            FORMAT2_RECORD,
            Some(Root {
                store: &store,
                entries: &entries,
                digest: abc_digest(),
            }),
        );
        let error = repository
            .read_format2_orr_blob("HEAD", &orr_record(&abc_digest(), 4), 16)
            .expect_err("length mismatch is refused");
        assert!(matches!(
            error,
            Format2BlobError::RecordMismatch("content length")
        ));
    }

    #[test]
    fn refuses_content_above_read_limit_before_reading_chunks() {
        let (store, entries) = abc_root(b"abc");
        let (_dir, repository) = commit_fixture(
            FORMAT2_RECORD,
            Some(Root {
                store: &store,
                entries: &entries,
                digest: abc_digest(),
            }),
        );
        let error = repository
            .read_format2_orr_blob("HEAD", &orr_record(&abc_digest(), 3), 2)
            .expect_err("content above the limit is refused");
        assert!(matches!(
            error,
            Format2BlobError::Limit { total: 3, limit: 2 }
        ));
    }

    #[test]
    fn rejects_orr2_record_with_unknown_field() {
        let (_dir, repository) = commit_fixture(FORMAT2_RECORD, None);
        let field = format!(
            "{{blob: \"sha256:{}\", length: 3, extra: 1}}",
            hex_encode(&abc_digest())
        );
        let error = repository
            .read_format2_orr_blob("HEAD", &field, 16)
            .expect_err("unknown field is refused");
        assert!(matches!(error, Format2BlobError::OrrField(_)));
    }

    #[test]
    fn rejects_truncated_rov2_value() {
        let (_dir, repository) = commit_fixture(FORMAT2_RECORD, None);
        let error = repository
            .read_format2_rov_blob("HEAD", &[0xd9, 0xea, 0xc4, 0x82], 16)
            .expect_err("truncated ROV-2 value is refused");
        assert!(matches!(error, Format2BlobError::Rov2Value(_)));
    }
}
