//! Durable automatic-key allocators (ORNA-AUTOID-002, ORNA-AUTOID-003,
//! ORNA-AUTOID-004; ORNA-META-004, ORNA-BRANCH-002).
//!
//! Each automatic-key table's high-water mark lives in the hidden ref
//! `refs/orna/ids/<table ObjectId>` (07-tables.md), outside every branch
//! tree, so deleting rows, `git reset`, checkout and branch switching never
//! rewind it. The ref names a blob holding the canonical decimal text of the
//! highest ID ever reserved for the table: digits only, no sign, no leading
//! zero except `0`, no newline, at most 64 digits. Writers only advance it,
//! with compare-and-swap, and make it durable before the rows that use the
//! reserved IDs can commit, so a failed commit leaves a gap and never a
//! reused ID.

use num_bigint::{BigInt, Sign};

use crate::{
    Repository, RepositoryError, Uuid,
    native_graph::{GitHashAlgorithm, NativeOid, sync_git_ref_durably, sync_object_files},
};

/// The namespace of the per-table allocator refs.
pub const AUTOMATIC_ID_REF_PREFIX: &str = "refs/orna/ids/";

/// The longest watermark text an allocator ref holds; Orna-aware transport
/// enforces the same bound.
const MAX_WATERMARK_DIGITS: usize = 64;

/// The allocator ref of one table: the prefix and the table ObjectId as 32
/// lowercase hexadecimal digits.
pub fn automatic_id_ref(table_object_id: &[u8; 16]) -> String {
    format!(
        "{AUTOMATIC_ID_REF_PREFIX}{}",
        Uuid::from_bytes(*table_object_id).simple()
    )
}

/// The blob bytes of a watermark, or `None` for a negative value or one
/// longer than the 64-digit bound.
pub fn encode_automatic_id_watermark(watermark: &BigInt) -> Option<Vec<u8>> {
    if watermark.sign() == Sign::Minus {
        return None;
    }
    let text = watermark.to_string();
    (text.len() <= MAX_WATERMARK_DIGITS).then(|| text.into_bytes())
}

/// The watermark an allocator blob holds, or `None` for any bytes that are
/// not its canonical decimal text. A malformed allocator is never read as 0.
pub fn decode_automatic_id_watermark(bytes: &[u8]) -> Option<BigInt> {
    let canonical = !bytes.is_empty()
        && bytes.len() <= MAX_WATERMARK_DIGITS
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes[0] != b'0');
    if !canonical {
        return None;
    }
    BigInt::parse_bytes(bytes, 10)
}

/// Failures to read or advance an automatic-key allocator.
#[derive(Debug)]
pub enum AllocatorError {
    /// The allocator ref names something other than a canonical watermark
    /// blob.
    InvalidWatermark,
    /// Another writer advanced the allocator after it was read; retry with
    /// fresh IDs.
    Moved,
    /// The watermark is negative or exceeds the 64-digit bound.
    OutOfRange,
    /// The reserved watermark could not be made durable.
    Durability,
    Repository(RepositoryError),
}

impl std::fmt::Display for AllocatorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidWatermark => {
                formatter.write_str("an automatic-key allocator ref is not a watermark")
            }
            Self::Moved => formatter.write_str(
                "another writer reserved automatic IDs for the table at the same time; retry",
            ),
            Self::OutOfRange => {
                formatter.write_str("the automatic-key watermark is outside its 64-digit range")
            }
            Self::Durability => {
                formatter.write_str("the automatic-key allocator could not be made durable")
            }
            Self::Repository(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for AllocatorError {}

impl From<RepositoryError> for AllocatorError {
    fn from(error: RepositoryError) -> Self {
        Self::Repository(error)
    }
}

impl Repository {
    /// The highest automatic ID ever reserved for `table_object_id` in this
    /// repository, `None` before its first reservation. Every allocation
    /// starts above it, whatever the checked-out rows are (ORNA-AUTOID-002).
    pub fn automatic_id_watermark(
        &self,
        table_object_id: &[u8; 16],
    ) -> Result<Option<BigInt>, AllocatorError> {
        Ok(self
            .allocator_ref(table_object_id)?
            .map(|(_, watermark)| watermark))
    }

    /// Advances the allocator of `table_object_id` from `expected`, the
    /// watermark the caller allocated above, to `watermark`, and makes the
    /// ref durable. A watermark that does not exceed `expected` changes
    /// nothing. The update is a compare-and-swap: when another writer moved
    /// the allocator first, nothing changes and [`AllocatorError::Moved`] is
    /// returned, because IDs allocated above `expected` may now be taken.
    pub fn advance_automatic_id_watermark(
        &self,
        table_object_id: &[u8; 16],
        expected: Option<&BigInt>,
        watermark: &BigInt,
    ) -> Result<(), AllocatorError> {
        if expected.is_some_and(|expected| watermark <= expected) {
            return Ok(());
        }
        let bytes = encode_automatic_id_watermark(watermark).ok_or(AllocatorError::OutOfRange)?;
        let reference = automatic_id_ref(table_object_id);
        let current = self.allocator_ref(table_object_id)?;
        if current.as_ref().map(|(_, value)| value) != expected {
            return Err(AllocatorError::Moved);
        }
        let blob = self.hash_object(&bytes)?;
        let algorithm = match blob.len() {
            40 => GitHashAlgorithm::Sha1,
            64 => GitHashAlgorithm::Sha256,
            _ => return Err(AllocatorError::Durability),
        };
        let blob_oid =
            NativeOid::from_hex(algorithm, &blob).map_err(|_| AllocatorError::Durability)?;
        let objects = self.git_path("objects")?;
        sync_object_files(&objects, [&blob_oid], || Ok(()))
            .map_err(|_| AllocatorError::Durability)?;
        let previous = current.map_or_else(|| "0".repeat(blob.len()), |(oid, _)| oid);
        let mut command = self.command();
        command.args(["update-ref", &reference, &blob, &previous]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            // The expected old value no longer holds: another writer moved
            // the allocator, unless it reserved exactly this watermark.
            return match self.allocator_ref(table_object_id)? {
                Some((oid, _)) if oid == blob => Ok(()),
                _ => Err(AllocatorError::Moved),
            };
        }
        sync_git_ref_durably(self, &reference).map_err(|_| AllocatorError::Durability)
    }

    /// The object id and watermark the allocator ref of a table names.
    fn allocator_ref(
        &self,
        table_object_id: &[u8; 16],
    ) -> Result<Option<(String, BigInt)>, AllocatorError> {
        let reference = automatic_id_ref(table_object_id);
        let mut command = self.command();
        command.args(["rev-parse", "--verify", "--quiet", &reference]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            // The quiet probe's sole absence case; anything else fails closed.
            if output.status.code() == Some(1) && output.stderr.is_empty() {
                return Ok(None);
            }
            return Err(RepositoryError::GitOperationFailed.into());
        }
        let oid = crate::trim_output(&output.stdout);
        let mut kind = self.command();
        kind.args(["cat-file", "-t", &oid]);
        if crate::trim_output(&self.run(kind)?.stdout) != "blob" {
            return Err(AllocatorError::InvalidWatermark);
        }
        let mut size = self.command();
        size.args(["cat-file", "-s", &oid]);
        let length = crate::trim_output(&self.run(size)?.stdout)
            .parse::<usize>()
            .map_err(|_| AllocatorError::InvalidWatermark)?;
        if length > MAX_WATERMARK_DIGITS {
            return Err(AllocatorError::InvalidWatermark);
        }
        let mut contents = self.command();
        contents.args(["cat-file", "blob", &oid]);
        let watermark = decode_automatic_id_watermark(&self.run(contents)?.stdout)
            .ok_or(AllocatorError::InvalidWatermark)?;
        Ok(Some((oid, watermark)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &std::path::Path, arguments: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn watermark_text_is_canonical_decimal() {
        for (value, text) in [(0, "0"), (7, "7"), (1_000, "1000")] {
            let value = BigInt::from(value);
            assert_eq!(
                encode_automatic_id_watermark(&value).unwrap(),
                text.as_bytes()
            );
            assert_eq!(decode_automatic_id_watermark(text.as_bytes()), Some(value));
        }
        for malformed in ["", "007", "-1", "+1", "1\n", " 1", "1e3"] {
            assert_eq!(decode_automatic_id_watermark(malformed.as_bytes()), None);
        }
        assert_eq!(encode_automatic_id_watermark(&BigInt::from(-1)), None);
        let largest = BigInt::parse_bytes("9".repeat(64).as_bytes(), 10).unwrap();
        assert!(encode_automatic_id_watermark(&largest).is_some());
        assert_eq!(encode_automatic_id_watermark(&(largest + 1)), None);
        assert_eq!(
            automatic_id_ref(&[0xab; 16]),
            format!("refs/orna/ids/{}", "ab".repeat(16))
        );
    }

    /// The allocator lives outside the branch: committing, resetting and
    /// switching branches leave it where it is, and a stale writer cannot
    /// lower or overwrite it.
    #[test]
    fn allocators_advance_only_by_compare_and_swap_outside_branches() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--quiet", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root);
        git(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("main.orna"), "pub table Note { text: Str, }\n").unwrap();
        git(root, &["add", "main.orna"]);
        git(root, &["commit", "--quiet", "-m", "first"]);
        let repository = Repository::discover(root).unwrap();
        let table = [0x5a; 16];

        assert_eq!(repository.automatic_id_watermark(&table).unwrap(), None);
        repository
            .advance_automatic_id_watermark(&table, None, &BigInt::from(3))
            .unwrap();
        assert_eq!(
            repository.automatic_id_watermark(&table).unwrap(),
            Some(BigInt::from(3))
        );
        // A writer that read an older watermark loses the race.
        assert!(matches!(
            repository.advance_automatic_id_watermark(&table, None, &BigInt::from(9)),
            Err(AllocatorError::Moved)
        ));
        // Not exceeding the watermark it read changes nothing.
        repository
            .advance_automatic_id_watermark(&table, Some(&BigInt::from(3)), &BigInt::from(2))
            .unwrap();
        repository
            .advance_automatic_id_watermark(&table, Some(&BigInt::from(3)), &BigInt::from(10))
            .unwrap();

        // Git history operations never rewind it.
        std::fs::write(root.join("notes.txt"), "second").unwrap();
        git(root, &["add", "notes.txt"]);
        git(root, &["commit", "--quiet", "-m", "second"]);
        git(root, &["reset", "--quiet", "--hard", "HEAD~1"]);
        git(root, &["checkout", "--quiet", "-b", "other"]);
        assert_eq!(
            repository.automatic_id_watermark(&table).unwrap(),
            Some(BigInt::from(10))
        );
        assert_eq!(
            git(root, &["cat-file", "-p", &automatic_id_ref(&table)]),
            "10"
        );
        // The allocator ref is not part of any branch tree.
        assert!(!git(root, &["ls-tree", "-r", "--name-only", "HEAD"]).contains("ids"));

        // A malformed allocator fails closed instead of reading as 0.
        let malformed = {
            let mut child = std::process::Command::new("git")
                .current_dir(root)
                .args(["hash-object", "-w", "--stdin"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write as _;
            child.stdin.take().unwrap().write_all(b"010").unwrap();
            String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
        };
        git(
            root,
            &["update-ref", &automatic_id_ref(&table), malformed.trim()],
        );
        assert!(matches!(
            repository.automatic_id_watermark(&table),
            Err(AllocatorError::InvalidWatermark)
        ));
    }
}
