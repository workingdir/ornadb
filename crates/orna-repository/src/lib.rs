//! Git-valid repository state for Orna 1.0.
//!
//! This crate intentionally models the repository boundary. It can initialize
//! canonical `.orna/` metadata, but never advances an ordinary ref. In particular,
//! [`Repository::stage_managed`] delegates to Git's ordinary index and touches
//! only the supplied worktree-relative paths.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt, fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
};

#[cfg(target_os = "linux")]
use std::{
    ffi::{OsStr, OsString},
    mem::MaybeUninit,
    os::unix::ffi::OsStrExt,
};

#[cfg(target_os = "linux")]
use rustix::fs::StatExt;

use fs2::FileExt;
use sha2::{Digest, Sha256};
pub use uuid::Uuid;

use crate::cbor::{bytes as cbor_bytes, head as cbor_head, text as cbor_text};
use crate::hex::hex_encode;
use crate::wire::MAX_JOURNAL_BYTES;
use crate::wire::little::{
    put_optional_bytes, put_optional_string, put_string, put_u32, take_byte,
    take_fixed as take_fixed_array, take_optional_bytes, take_optional_string, take_string,
    take_u32,
};

mod adapter;
pub mod allocator;
mod blob_store;
mod cbor;
mod checkout;
mod commit;
mod compact;
pub mod complete_copy;
mod crash_barrier;
mod error;
pub mod format2_read;
mod format_context;
mod git_command;
mod hex;
mod index_lock;
mod init;
mod journal;
mod locks;
pub mod migration_copy;
mod mount;
mod native_graph;
mod object_reader;
pub mod offline_copy;
mod orp;
mod overflow;
mod paths;
pub mod publication_describe;
mod publication_transaction;
pub mod relation_admission;
mod repository;
mod row_store;
pub mod schema_evolution;
#[cfg(test)]
pub(crate) mod test_support;
mod transport;
mod wire;

pub use adapter::{OrnaRepositoryAdapter, OrnaRepositoryAdapterError};
pub use blob_store::{ContentIdentity, raw_content_bytes_digested};
#[cfg(test)]
pub(crate) use checkout::CHECKOUT_JOURNAL_MAGIC;
pub(crate) use checkout::{
    CONTENT_RACY_WINDOW, CachedFileDigest, CheckoutRecoveryJournal, CheckoutRecoveryState,
    CheckoutRecoveryStep, ManagedFileStamp, ManagedStamp, ScannedWorktreePath, WorktreeScan,
    case_folded_path_collides, is_loose_path_components, managed_candidate_recheck_race,
    record_case_folded_path_components, sorted_paths,
};
#[cfg(test)]
pub(crate) use checkout::{
    CONTENT_READS, MANAGED_CANDIDATE_RECHECK_RACE, ManagedCandidateRecheckRace,
};
pub use checkout::{
    CheckoutGitSubplan, CheckoutPlanToken, CheckoutPreflight, CheckoutTarget, CwdGeneration,
    IndexGeneration, RuntimeGeneration, ValidatedCheckoutDiscard, WorktreeState,
};
#[cfg(test)]
pub(crate) use commit::{AcceptAllAssertions, AcceptAllCheckoutAssertions};
pub use commit::{
    CandidateAssertionGate, CandidateAssertionRefusal, CheckoutAssertionGate, CommittedBranch,
    CommittedLogEntry, CommittedRevisionSnapshot, CommittedTreeEntry, CommittedTreeEntryKind,
    GitCommitRef, IndexTreeRef, ManagedFileChange, PrivateCommit, ReviewedCandidateReview,
};
pub(crate) use commit::{
    MAX_COMMITTED_REVISION_WALK, commit_gate_refusal, reviewed_candidate_runtime_intent_id,
    reviewed_candidate_token, split_committed_revision_record,
};
pub use compact::{
    COMPACT_MANIFEST_SHARD_LIMIT, COMPACT_MAX_UNCOMPRESSED_PAGE_BYTES, CompactCommittedRow,
    CompactCommittedSegmentProjection, CompactManifest, CompactManifestEntry,
    CompactManifestWitness, CompactPublicationPending, CompactRuntimeReceipt, CompactSegmentRole,
    LegacyAuthoritativeRow, LegacyAuthoritativeTable, validate_compact_page_uncompressed_sizes,
};
#[cfg(any(test, feature = "legacy-fixture-writer"))]
pub use compact::{
    CompactPublicationPlan, CompactPublicationRequest, CompactSegment, CompactSegmentRecord,
};
#[cfg(feature = "legacy-fixture-writer")]
pub use compact::{
    golden_compact_schemas, write_legacy_format1_fixture, write_legacy_format1_golden_fixture,
};
pub use crash_barrier::{publication_crash_barrier, publication_crash_pause_until_released};
pub use error::RepositoryError;
pub use format_context::{
    CaptureCapabilityError, CommittedTableBinding, CommittedTableBindingRequest,
    FINAL_REPOSITORY_FORMAT, FinalFormatCapability, FormatContextError, HistoricalFormatContext,
    LegacyWriteRefused, REPOSITORY_LAYOUT_VERSION, RepositoryFormat, RepositoryFormatContext,
    RepositorySnapshotPin, SchemaRootPin, StoreRootPin,
};
pub(crate) use git_command::{
    ConfigValue, GitListRecord, GitTreeRecord, PromisorReachability, git_command,
    git_command_at_root, git_command_in, git_stderr_indicates_malformed_object,
    parse_committed_tree_entry, parse_git_bool, parse_remote_orna_refs, remote_ref_records,
    require_native_object_id, run_bounded_listing, run_command, scrub_git_routing_environment,
    trim_output, valid_branch_name, valid_native_object_id, valid_orna_internal_ref,
    valid_remote_name,
};
#[cfg(test)]
pub(crate) use git_command::{bind_git_ceiling_above, bind_git_discovery_to_worktree};
pub(crate) use index_lock::GitIndexLock;
#[cfg(test)]
pub(crate) use index_lock::{GitIndexLockMarker, GitIndexLockOwnerLiveness};
#[cfg(all(test, target_os = "linux"))]
pub(crate) use index_lock::{
    GitIndexLockReclaimPoint, ProcessStartObservation, process_owner_liveness,
};
pub use init::{
    DatabaseId, RepositoryInitError, RepositoryInitialization, RepositoryMetadata, STORAGE_PROFILE,
    initialize_repository, inspect_legacy_source_metadata, inspect_metadata,
    inspect_metadata_for_runtime,
};
#[cfg(test)]
pub(crate) use journal::PublicationMaterialization;
pub use journal::{
    LEGACY_ANNOTATION_DEFAULT_MEDIA_TYPE, LegacyAnnotationCandidate, MigrationAnnotationDefaults,
    MigrationContinuityRecord, PublicationJournal, PublicationJournalEntry,
    PublicationJournalStage, PublicationLogicalRowRekey, StreamCheckpointIdentityPredecessor,
};
pub(crate) use journal::{
    PublicationMaterializationConflict, PublicationMaterializationPhase,
    PublicationMaterializationStep, PublicationPinPolicy, publication_materialization_step,
};
pub(crate) use locks::{
    COORDINATION_LOCK_WAIT, CoordinationLock, MaintenanceMode, ManagedIndexOperation, StagedFile,
    lock_before_deadline, open_lock_file, persist_runtime_owner_state, read_runtime_owner_state,
    write_atomically,
};
pub use locks::{MaintenanceLease, PublicationLease, RuntimeOwnerLock};
pub use mount::{MountError, MountRowBaselines, MountStatus, MountView};
pub use native_graph::RowRevision;
pub use native_graph::{
    AdmittedBlobReference, AdmittedRowField, AdmittedRowFields, CapturedBlobCandidate,
    GitHashAlgorithm, GraphError, IndexMaintenance, NativeGraphContext, NativeObjectKind,
    NativeOid, OrpBlobBinding, OrpGraphCandidate, ProtectedBlobMetadata, ProtectedContentPin,
    ProtectedContentTransfer, RangeVerification, RepositoryCaptureCapability, RepositoryReadScope,
    RowIndexMutation, VerifiedBlobRange,
};
pub use orna_value::BlobMetadata;
pub use paths::{
    GitDeclaredObjectSetState, GitObjectKind, GitObjectState, GitRepositoryCapabilities,
    GitRepositoryMode, ManagedPath, NativeObjectId, OrnaInternalRef, RemoteContinuity,
    RequiredInternalRef, RuntimePaths,
};
pub use publication_transaction::{
    ProtectedBlobRowInsert, PublicationTransactionError, commit_protected_blob_row,
};
pub use row_store::{
    AdmittedRow, KeyRange, KeyValue, RepositoryVfsFieldIdentity, RepositoryVfsRowIdentity,
    RowDependency, RowMapSnapshot, RowMapVersion, RowValue, SchemaGeneration, TypedKey,
};
pub use transport::{FetchError, FetchReport, FetchRequest, FetchedRef, PushRequest, RequestedRef};

/// A normal Git repository and its selected worktree.
///
/// Clones share one object reader, so a handle and every context opened from
/// it read committed objects through the same long-lived Git process.
#[derive(Clone, Debug)]
pub struct Repository {
    worktree: PathBuf,
    runtime: RuntimePaths,
    objects: std::sync::Arc<object_reader::ObjectReader>,
    coordination_lock_wait: std::time::Duration,
    /// Regular-file digest sections from the last worktree observation. Derived
    /// and disposable: a section is reused only while its file stamp is unchanged.
    section_cache: std::sync::Arc<std::sync::Mutex<HashMap<Vec<u8>, CachedFileDigest>>>,
}

/// How a private candidate commit binds its `.orna/store` tree.
#[derive(Clone, Copy)]
enum StoreBinding<'a> {
    /// A store root decoded through `graph`, which must share its algorithm.
    Validated {
        graph: &'a NativeGraphContext,
        root: &'a NativeOid,
    },
    /// A store root already read back through the store reader by the caller.
    Decoded(&'a NativeOid),
}

const STORE_PATH: &str = ".orna/store";

/// Binds a candidate tree entry at its exact index position.
///
/// A regular entry keeps the ordinary `--cacheinfo` path. A tree entry cannot:
/// Git refuses a directory entry that overlaps the file entries a base commit
/// already contributed under the same path, and a `--cacheinfo` directory entry
/// is re-expanded from the worktree. The base's entries are removed first, then
/// the subtree is bound leaf by leaf with `read-tree --prefix`, which reproduces
/// the tree exactly for later index and tree reads.
pub(crate) fn bind_tree_entry_in_index(
    mut git: impl FnMut() -> Command,
    index: &Path,
    path: &str,
    mode: &str,
    object: &str,
) -> Result<(), RepositoryError> {
    if mode != "040000" {
        let cacheinfo = format!("{mode},{object},{path}");
        let mut update = git();
        update.env("GIT_INDEX_FILE", index).args([
            "update-index",
            "--add",
            "--cacheinfo",
            &cacheinfo,
        ]);
        run_command(update)?;
        return Ok(());
    }
    let mut listed = git();
    listed
        .env("GIT_INDEX_FILE", index)
        .args(["ls-files", "-z", "--", path]);
    let output = run_command(listed)?;
    for stale in output.stdout.split(|byte| *byte == 0) {
        if stale.is_empty() {
            continue;
        }
        let stale = std::str::from_utf8(stale).map_err(|_| RepositoryError::GitOperationFailed)?;
        let mut remove = git();
        remove
            .env("GIT_INDEX_FILE", index)
            .args(["update-index", "--force-remove", "--", stale]);
        run_command(remove)?;
    }
    let prefix = format!("{path}/");
    let mut bind = git();
    bind.env("GIT_INDEX_FILE", index)
        .args(["read-tree", "--prefix", &prefix, object]);
    run_command(bind)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CONTENT_RACY_WINDOW, CONTENT_READS};
    use crate::{AcceptAllAssertions, AcceptAllCheckoutAssertions};
    use std::{collections::BTreeMap, fs, path::Path, process::Command};

    #[cfg(target_os = "linux")]
    use std::{env, os::unix::ffi::OsStringExt, path::PathBuf};

    use super::{
        GitCommitRef, GitIndexLock, ManagedCandidateRecheckRace, ManagedFileChange, ManagedPath,
        PublicationJournal, PublicationJournalEntry, PublicationJournalStage,
        PublicationMaterializationPhase, PublicationPinPolicy, RemoteContinuity, Repository,
        RepositoryError, RuntimeGeneration, parse_remote_orna_refs,
    };

    #[cfg(target_os = "linux")]
    use super::{
        GitIndexLockMarker, GitIndexLockOwnerLiveness, GitIndexLockReclaimPoint,
        ProcessStartObservation, process_owner_liveness,
    };

    #[test]
    fn publication_journal_round_trips_durable_logical_rekeys() {
        let mut journal = PublicationJournal::new_with_runtime_intent(
            GitCommitRef("a".repeat(40)),
            GitCommitRef("b".repeat(40)),
            super::IndexTreeRef::from_verified_tree("c".repeat(40), 40).unwrap(),
            [1; 16],
            vec![PublicationJournalEntry::new(
                ManagedPath::new(".orna/database.orna").unwrap(),
                None,
                Some(b"committed".to_vec()),
            )],
        )
        .unwrap();
        let event = super::PublicationLogicalRowRekey::new(
            [2; 16],
            super::row_store::TypedKey::UInt(10)
                .canonical_bytes()
                .unwrap(),
            super::row_store::TypedKey::UInt(50)
                .canonical_bytes()
                .unwrap(),
        )
        .unwrap();
        journal = journal.with_logical_rekeys(vec![event.clone()]).unwrap();

        let recovered = PublicationJournal::decode(&journal.encode().unwrap(), 40).unwrap();
        assert_eq!(recovered.runtime_intent_id(), Some([1; 16]));
        assert_eq!(recovered.logical_rekeys(), &[event]);
    }

    /// Builds `parent/healthy` as a real repository and `parent/healthy/child`
    /// with a corrupt `HEAD`, so discovery from `child` must not reach `healthy`.
    #[cfg(unix)]
    pub(crate) fn corrupt_child_under_healthy_parent(root: &Path) -> std::path::PathBuf {
        let healthy = root.join("healthy");
        fs::create_dir_all(&healthy).unwrap();
        let init = Command::new("git")
            .current_dir(&healthy)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(init.success());
        let child = healthy.join("child");
        fs::create_dir_all(child.join(".git")).unwrap();
        fs::write(child.join(".git").join("HEAD"), b"garbage\n").unwrap();
        child
    }

    /// Runs `rev-parse HEAD` in `directory` under the production discovery bind.
    #[cfg(unix)]
    fn head_probe_succeeds_bound(directory: &Path) -> bool {
        let mut command = Command::new("git");
        command.current_dir(directory).args(["rev-parse", "HEAD"]);
        super::scrub_git_routing_environment(&mut command);
        super::bind_git_discovery_to_worktree(&mut command, directory);
        command
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    #[test]
    fn git_discovery_symlinked_worktree_does_not_fall_through_to_real_parent() {
        let root = tempfile::tempdir().unwrap();
        let child = corrupt_child_under_healthy_parent(root.path());
        let healthy = child.parent().unwrap();
        let init = Command::new("git")
            .current_dir(healthy)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "healthy",
            ])
            .status()
            .unwrap();
        assert!(init.success());
        let plain = root.path().join("x");
        fs::create_dir_all(&plain).unwrap();
        let link = plain.join("L");
        std::os::unix::fs::symlink(&child, &link).unwrap();
        assert!(!head_probe_succeeds_bound(&link));
    }

    #[cfg(unix)]
    #[test]
    fn git_discovery_relative_path_to_corrupt_child_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let _child = corrupt_child_under_healthy_parent(root.path());
        let previous = env::current_dir().unwrap();
        env::set_current_dir(root.path()).unwrap();
        let relative = Path::new("healthy").join("child");
        let ok = head_probe_succeeds_bound(&relative);
        env::set_current_dir(previous).unwrap();
        assert!(!ok, "relative corrupt child must not adopt healthy parent");
    }

    #[cfg(unix)]
    #[test]
    fn git_at_corrupt_root_under_healthy_parent_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let child = corrupt_child_under_healthy_parent(root.path());
        assert!(super::Repository::git_at(&child, ["rev-parse", "HEAD"]).is_err());
    }

    /// Replaces one length-prefixed string in an encoded journal with another,
    /// keeping the surrounding wire format valid.
    pub(crate) fn splice_length_prefixed(
        bytes: &[u8],
        old: &str,
        new: &str,
        length_prefix: fn(u32) -> [u8; 4],
    ) -> Vec<u8> {
        let mut needle = length_prefix(old.len() as u32).to_vec();
        needle.extend_from_slice(old.as_bytes());
        let mut replacement = length_prefix(new.len() as u32).to_vec();
        replacement.extend_from_slice(new.as_bytes());
        let at = bytes
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
            .expect("old path must be encoded in the journal");
        let mut out = bytes[..at].to_vec();
        out.extend_from_slice(&replacement);
        out.extend_from_slice(&bytes[at + needle.len()..]);
        out
    }

    #[test]
    fn publication_journal_decode_refuses_hostile_managed_paths() {
        let journal = PublicationJournal::new_with_runtime_intent(
            GitCommitRef("a".repeat(40)),
            GitCommitRef("b".repeat(40)),
            super::IndexTreeRef::from_verified_tree("c".repeat(40), 40).unwrap(),
            [1; 16],
            vec![PublicationJournalEntry::new(
                ManagedPath::new(".orna/database.orna").unwrap(),
                None,
                Some(b"committed".to_vec()),
            )],
        )
        .unwrap();
        let encoded = journal.encode().unwrap();
        assert!(PublicationJournal::decode(&encoded, 40).is_ok());
        for hostile in ["../escape", "/etc/passwd", "a\\b", "a\0b", "a//b"] {
            let spliced =
                splice_length_prefixed(&encoded, ".orna/database.orna", hostile, u32::to_le_bytes);
            assert!(
                PublicationJournal::decode(&spliced, 40).is_err(),
                "journal path {hostile:?} must not decode"
            );
        }
    }

    #[test]
    fn checkout_journal_v1_legacy_bytes_decode_and_hostile_discard_paths_refuse() {
        let mut bytes = super::CHECKOUT_JOURNAL_MAGIC.to_vec();
        bytes.push(1);
        bytes.extend_from_slice(&7_u64.to_be_bytes());
        bytes.push(2);
        super::put_string(&mut bytes, &"a".repeat(40)).unwrap();
        bytes.extend_from_slice(&[9; 32]);
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        super::put_string(&mut bytes, "generated/row.orna").unwrap();
        assert!(super::CheckoutRecoveryJournal::decode(&bytes, 40).is_ok());
        let hostile =
            splice_length_prefixed(&bytes, "generated/row.orna", "../escape", u32::to_le_bytes);
        assert!(super::CheckoutRecoveryJournal::decode(&hostile, 40).is_err());
    }

    /// The force-discard record advances Prepared, Discarded, Applied one
    /// boundary at a time. No step can be skipped or repeated, and a boundary
    /// is refused when its generation does not belong to the transition.
    #[test]
    fn checkout_journal_records_each_boundary_once_and_in_order() {
        use super::{
            CheckoutPlanToken, CheckoutRecoveryJournal, CheckoutRecoveryState, CheckoutTarget,
            CwdGeneration, IndexGeneration, WorktreeState,
        };

        let cwd = |head: char, branch: &str| {
            let head = GitCommitRef(head.to_string().repeat(40));
            CwdGeneration {
                head: Some(head.clone()),
                branch: Some(branch.to_owned()),
                index: IndexGeneration {
                    head: Some(head),
                    tree: None,
                },
                worktree: WorktreeState(Vec::new(), [0; 32]),
                runtime: RuntimeGeneration::new(7),
            }
        };
        let mut journal = CheckoutRecoveryJournal {
            target: CheckoutTarget::Branch {
                name: "main".to_owned(),
                commit: GitCommitRef("b".repeat(40)),
            },
            runtime: RuntimeGeneration::new(7),
            force_token: CheckoutPlanToken([9; 32]),
            discard_paths: vec![ManagedPath::new("generated/row.orna").unwrap()],
            state: CheckoutRecoveryState::Prepared {
                before: cwd('a', "old"),
            },
        };
        let prepared = journal.clone();

        assert!(matches!(
            journal.record_applied(cwd('b', "main")),
            Err(RepositoryError::InvalidCheckoutJournal)
        ));
        assert!(matches!(
            journal.record_discarded(cwd('c', "old")),
            Err(RepositoryError::InvalidCheckoutJournal)
        ));
        assert!(matches!(
            journal.record_discarded(cwd('a', "other")),
            Err(RepositoryError::InvalidCheckoutJournal)
        ));
        assert_eq!(
            journal, prepared,
            "a refused boundary leaves the record as it was"
        );

        journal.record_discarded(cwd('a', "old")).unwrap();
        assert!(matches!(
            journal.record_discarded(cwd('a', "old")),
            Err(RepositoryError::InvalidCheckoutJournal)
        ));
        assert!(matches!(
            journal.record_applied(cwd('a', "old")),
            Err(RepositoryError::InvalidCheckoutJournal)
        ));
        journal.record_applied(cwd('b', "main")).unwrap();
        assert!(matches!(
            journal.state,
            CheckoutRecoveryState::Applied { .. }
        ));
        assert!(matches!(
            journal.record_applied(cwd('b', "main")),
            Err(RepositoryError::InvalidCheckoutJournal)
        ));

        let encoded = journal.encode().unwrap();
        assert_eq!(
            CheckoutRecoveryJournal::decode(&encoded, 40).unwrap(),
            journal
        );
    }

    /// A version-1 record has no CWD generation to write back, so it can be
    /// read for recovery but never encoded again.
    #[test]
    fn checkout_journal_v1_record_is_read_only() {
        let mut bytes = super::CHECKOUT_JOURNAL_MAGIC.to_vec();
        bytes.push(1);
        bytes.extend_from_slice(&7_u64.to_be_bytes());
        bytes.push(2);
        super::put_string(&mut bytes, &"a".repeat(40)).unwrap();
        bytes.extend_from_slice(&[9; 32]);
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        super::put_string(&mut bytes, "generated/row.orna").unwrap();
        let journal = super::CheckoutRecoveryJournal::decode(&bytes, 40).unwrap();
        assert_eq!(
            journal.state,
            super::CheckoutRecoveryState::PreparedWithoutGeneration
        );
        assert!(matches!(
            journal.encode(),
            Err(RepositoryError::InvalidCheckoutJournal)
        ));
    }

    #[test]
    fn publication_object_present_fails_closed_on_git_error() {
        let root = tempfile::TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        let repository = Repository::discover(root.path()).unwrap();
        let mut hash = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(root.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(hash.stdin.as_mut().unwrap(), b"pinned").unwrap();
        let present = String::from_utf8(hash.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        let absent = "0123456789abcdef0123456789abcdef01234567";
        assert!(repository.publication_object_present(&present).unwrap());
        assert!(!repository.publication_object_present(absent).unwrap());

        // A corrupt HEAD makes git exit 128. The object may still exist, so
        // the answer is an error, never "absent" (which would skip the pin).
        fs::write(root.path().join(".git/HEAD"), "garbage\n").unwrap();
        assert!(matches!(
            repository.publication_object_present(&present),
            Err(RepositoryError::GitOperationFailed)
        ));
    }

    /// ORNA-GIT-010 and ORNA-CAP-006: the pin policy decides an absent journal
    /// object. Publish (`RequireObjects`) refuses it and creates no pin, so a
    /// candidate whose object is gone is never published. Recovery
    /// (`AcceptLostObjects`) classifies the loss itself, so it accepts the
    /// absence and still creates no pin. A present object is pinned under
    /// either policy.
    #[test]
    fn pin_policy_refuses_absent_object_only_for_publish() {
        let root = tempfile::TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        let repository = Repository::discover(root.path()).unwrap();
        let absent = "0123456789abcdef0123456789abcdef01234567";
        let absent_ref = Repository::publication_candidate_pin_ref(absent);
        let pin_exists = |reference: &str| {
            Command::new("git")
                .args(["show-ref", "--verify", "--quiet", reference])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        };

        assert!(matches!(
            repository.pin_publication_object_locked(
                &absent_ref,
                absent,
                PublicationPinPolicy::RequireObjects
            ),
            Err(RepositoryError::PublicationObjectUnavailable)
        ));
        assert!(!pin_exists(&absent_ref), "a refused pin must not exist");

        repository
            .pin_publication_object_locked(
                &absent_ref,
                absent,
                PublicationPinPolicy::AcceptLostObjects,
            )
            .expect("recovery accepts a lost object");
        assert!(!pin_exists(&absent_ref), "an accepted loss must not pin");

        let mut hash = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(root.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(hash.stdin.as_mut().unwrap(), b"pinned").unwrap();
        let present = String::from_utf8(hash.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        let present_ref = Repository::publication_candidate_pin_ref(&present);
        repository
            .pin_publication_object_locked(
                &present_ref,
                &present,
                PublicationPinPolicy::RequireObjects,
            )
            .expect("publish pins a present object");
        assert!(pin_exists(&present_ref), "a present object must be pinned");
    }

    /// A child repository nested inside a parent repository must not let a
    /// corrupt child HEAD fall through to the parent. Git would then report the
    /// child's pinned object as missing, and the publication pin check would
    /// skip the pin (ORNA-PUB-006). The probe must fail closed instead.
    #[test]
    fn publication_probe_does_not_fall_through_a_corrupt_child_head() {
        let parent = tempfile::TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(parent.path())
                .status()
                .unwrap()
                .success()
        );
        let child = parent.path().join("child");
        fs::create_dir(&child).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&child)
                .status()
                .unwrap()
                .success()
        );
        let repository = Repository::discover(&child).unwrap();
        let mut hash = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(&child)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(hash.stdin.as_mut().unwrap(), b"pinned").unwrap();
        let present = String::from_utf8(hash.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        assert!(repository.publication_object_present(&present).unwrap());

        fs::write(child.join(".git/HEAD"), "garbage\n").unwrap();
        assert!(matches!(
            repository.publication_object_present(&present),
            Err(RepositoryError::GitOperationFailed)
        ));
    }
    #[test]
    fn managed_path_rejects_tracked_runtime_cache_path() {
        assert!(matches!(
            ManagedPath::new(".orna/cache.db"),
            Err(RepositoryError::UnsafeManagedPath)
        ));
        assert_eq!(
            ManagedPath::new(".orna/format.orna").unwrap().as_path(),
            Path::new(".orna/format.orna")
        );
    }

    #[test]
    fn only_verified_remote_continuity_permits_a_continuity_claim() {
        assert!(RemoteContinuity::Continuous.permits_continuity_claim());
        assert!(!RemoteContinuity::Missing.permits_continuity_claim());
        assert!(!RemoteContinuity::Stale.permits_continuity_claim());
        assert!(!RemoteContinuity::InvalidEvidence.permits_continuity_claim());
        assert!(!RemoteContinuity::Unverifiable.permits_continuity_claim());
    }

    #[test]
    fn remote_ref_parser_rejects_blank_or_unexpected_records() {
        let expected = BTreeMap::from([(
            "refs/orna/ids/0123456789abcdef",
            "0123456789012345678901234567890123456789",
        )]);
        assert!(parse_remote_orna_refs("\n", 40, &expected).is_err());
        assert!(
            parse_remote_orna_refs(
                "0123456789012345678901234567890123456789\trefs/orna/ids/unexpected\n",
                40,
                &expected,
            )
            .is_err()
        );
        assert!(
            parse_remote_orna_refs(
                "0123456789012345678901234567890123456789\trefs/orna/ids/0123456789abcdef\n\n",
                40,
                &expected,
            )
            .is_err()
        );
    }

    #[test]
    fn runtime_owner_lock_excludes_a_second_owner_until_drop() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let first = repository.acquire_runtime_owner_lock([1; 16]).unwrap();
        assert!(matches!(
            repository.acquire_runtime_owner_lock([2; 16]),
            Err(RepositoryError::RepositoryBusy)
        ));
        drop(first);
        let second = repository.acquire_runtime_owner_lock([2; 16]).unwrap();
        assert_eq!(second.previous_owner(), Some([1; 16]));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_owner_chain_survives_repeated_startup_crashes() {
        use std::os::unix::process::ExitStatusExt;

        const REPOSITORY_ENV: &str = "ORNA_RUNTIME_OWNER_CRASH_REPOSITORY";
        const OWNER_ENV: &str = "ORNA_RUNTIME_OWNER_CRASH_ID";
        const TEST_FILTER: &str = "runtime_owner_chain_survives_repeated_startup_crashes";
        if let (Ok(path), Ok(owner)) = (std::env::var(REPOSITORY_ENV), std::env::var(OWNER_ENV)) {
            let owner = owner.parse::<u8>().unwrap();
            let repository = Repository::discover(path).unwrap();
            let _lock = repository.acquire_runtime_owner_lock([owner; 16]).unwrap();
            std::process::abort();
        }

        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let binary = std::env::current_exe().unwrap();
        for owner in [1_u8, 2, 3] {
            let status = Command::new("/bin/sh")
                .arg("-c")
                .arg("ulimit -c 0 2>/dev/null; exec \"$0\" \"$@\"")
                .arg(&binary)
                .args([TEST_FILTER, "--nocapture", "--test-threads=1"])
                .env(REPOSITORY_ENV, root.path())
                .env(OWNER_ENV, owner.to_string())
                .status()
                .unwrap();
            assert_eq!(status.signal(), Some(6), "startup child must abort");
        }

        let repository = Repository::discover(root.path()).unwrap();
        let recovered = repository.acquire_runtime_owner_lock([4; 16]).unwrap();
        assert_eq!(recovered.departed_owners(), &[[1; 16], [2; 16], [3; 16]]);
        assert_eq!(
            recovered
                .reclaim_departed_owner_pins(&BTreeMap::new())
                .unwrap(),
            Some(0)
        );
        drop(recovered);

        let retried = repository.acquire_runtime_owner_lock([5; 16]).unwrap();
        assert_eq!(retried.departed_owners(), &[[4; 16]]);
    }

    use crate::test_support::git;

    #[test]
    fn index_candidate_refuses_to_overwrite_an_existing_path() {
        let root = tempfile::tempdir().unwrap();
        let candidate = root.path().join("index-candidate-prepared");
        fs::write(&candidate, b"attacker-owned").unwrap();

        assert!(matches!(
            super::StagedFile::create(candidate.clone(), b"index-bytes"),
            Err(RepositoryError::LocalStateUnavailable)
        ));
        assert_eq!(fs::read(&candidate).unwrap(), b"attacker-owned");
    }

    #[test]
    fn staged_file_is_removed_on_drop_and_left_alone_once_installed() {
        let root = tempfile::tempdir().unwrap();
        let candidate = root.path().join("index-candidate-staged");
        let staged = super::StagedFile::create(candidate.clone(), b"index-bytes").unwrap();
        assert_eq!(fs::read(&candidate).unwrap(), b"index-bytes");
        drop(staged);
        assert!(
            !candidate.exists(),
            "dropping the guard removes the candidate"
        );

        let staged = super::StagedFile::create(candidate.clone(), b"index-bytes").unwrap();
        let live = root.path().join("index");
        fs::rename(staged.path(), &live).unwrap();
        drop(staged);
        assert_eq!(
            fs::read(&live).unwrap(),
            b"index-bytes",
            "a candidate that was installed is not touched by the guard"
        );
    }

    #[test]
    fn commit_optional_treats_option_shaped_expressions_as_revisions() {
        let root = repository_with_ignored_big_directory();
        let repository = Repository::discover(root.path()).unwrap();
        // Without `--end-of-options`, rev-parse would print this option's value
        // and succeed; as a revision name it is simply absent.
        assert!(matches!(
            repository.commit_optional("--git-dir=/nonexistent"),
            Ok(None)
        ));
    }

    fn repository_with_ignored_big_directory() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        fs::write(root.path().join(".gitignore"), "big/\n").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", ".gitignore", "main.orna"]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=Orna Test",
                "-c",
                "user.email=orna@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        );
        fs::create_dir(root.path().join("big")).unwrap();
        root
    }

    #[test]
    fn cwd_generation_ignores_git_ignored_content() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("big/app.log"), b"one").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let before = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        fs::write(root.path().join("big/app.log"), b"two, longer").unwrap();
        let after = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        assert!(
            before == after,
            "git-ignored content must not enter the CWD observation"
        );
    }

    /// Opens a repository whose HEAD names a commit nesting one file
    /// `depth` directories deep: a tree the committed walk refuses by depth.
    fn repository_on_deep_head(depth: usize) -> (tempfile::TempDir, Repository, GitCommitRef) {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        let commit = crate::test_support::deep_nested_commit(root.path(), depth);
        git(root.path(), &["update-ref", "HEAD", &commit]);
        let repository = Repository::discover(root.path()).unwrap();
        let head = repository.head().unwrap().unwrap();
        (root, repository, head)
    }

    /// G1: the private-commit read-tree of HEAD must be preceded by the
    /// bounded expansion pre-pass, so a too-deep HEAD is refused.
    #[test]
    fn private_commit_refuses_a_head_tree_nested_beyond_the_walk_limit() {
        let (_root, repository, head) = repository_on_deep_head(300);
        let change = ManagedFileChange::new(
            ManagedPath::new("src/x.txt").unwrap(),
            Some(b"x\n".to_vec()),
        );
        assert!(matches!(
            repository.build_private_commit(&head, &[change], "orna: test deep head"),
            Err(RepositoryError::SourceTreeLimit)
        ));
    }

    /// G3: with a current head present, the target tree is still checked
    /// before the diff against the current tree expands it.
    #[test]
    fn checkout_subplan_refuses_a_target_tree_nested_beyond_the_walk_limit() {
        let (root, repository, deep) = repository_on_deep_head(300);
        let shallow = git(
            root.path(),
            &[
                "commit-tree",
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
                "-m",
                "shallow",
            ],
        );
        git(root.path(), &["update-ref", "refs/heads/shallow", &shallow]);
        let current = repository.resolve_snapshot(&shallow).unwrap();
        assert!(matches!(
            repository.checkout_git_subplan(Some(&current), &deep),
            Err(RepositoryError::SourceTreeLimit)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cwd_generation_survives_non_utf8_git_ignored_names() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let root = repository_with_ignored_big_directory();
        fs::write(
            root.path().join("big").join(OsStr::from_bytes(b"\xff.bin")),
            b"x",
        )
        .unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
    }

    #[test]
    fn cwd_generation_detects_managed_dirty_content_change() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("main.orna"), "module main;\n// one\n").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let before = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n// two\n").unwrap();
        let after = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        assert!(
            before != after,
            "a dirty managed file must change the CWD observation"
        );
    }

    /// Takes the dirty-file reads this thread recorded, clearing the record.
    fn content_reads_take() -> Vec<PathBuf> {
        CONTENT_READS.with(|reads| std::mem::take(&mut *reads.borrow_mut()))
    }

    /// A settled dirty file is read once across the CWD observations of one
    /// operation: the first observation reads it, the rest reuse its section.
    #[test]
    fn settled_dirty_file_is_read_once_across_cwd_observations() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("main.orna"), "module main;\n// settled\n").unwrap();
        std::thread::sleep(CONTENT_RACY_WINDOW + std::time::Duration::from_millis(200));
        let repository = Repository::discover(root.path()).unwrap();
        content_reads_take();
        let first = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        repository.verify_cwd(&first).unwrap();
        repository.worktree_state().unwrap();
        let reads = content_reads_take();
        assert_eq!(
            reads.len(),
            1,
            "a settled dirty file must be read once: {reads:?}"
        );
        assert!(reads[0].ends_with("main.orna"));
    }

    /// Git-ignored bytes never enter the CWD observation, so no read names them.
    #[test]
    fn cwd_observation_reads_no_git_ignored_file() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("big/app.log"), b"ignored").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        content_reads_take();
        repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        repository.worktree_state().unwrap();
        let reads = content_reads_take();
        assert!(
            reads.iter().all(|path| !path.ends_with("app.log")),
            "git-ignored bytes must not be read: {reads:?}"
        );
    }

    /// A file written within the racy window is not settled, so its bytes are
    /// read again and a same-size rewrite still changes the observation.
    #[test]
    fn cwd_generation_detects_same_size_rewrite_within_racy_window() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("main.orna"), "module main;\n// aaaa\n").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        content_reads_take();
        let before = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        assert_eq!(content_reads_take().len(), 1);
        fs::write(root.path().join("main.orna"), "module main;\n// bbbb\n").unwrap();
        let after = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        assert_eq!(
            content_reads_take().len(),
            1,
            "an unsettled dirty file must be read again"
        );
        assert!(
            before != after,
            "a same-size rewrite must change the CWD observation"
        );
    }

    /// Deleting a settled dirty file changes the observation; its cached
    /// section is never reused for a path that no longer exists.
    #[test]
    fn settled_dirty_file_deletion_changes_cwd_observation() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("main.orna"), "module main;\n// keep\n").unwrap();
        std::thread::sleep(CONTENT_RACY_WINDOW + std::time::Duration::from_millis(200));
        let repository = Repository::discover(root.path()).unwrap();
        let before = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        fs::remove_file(root.path().join("main.orna")).unwrap();
        let after = repository
            .cwd_generation(RuntimeGeneration::new(0))
            .unwrap();
        assert!(
            before != after,
            "a deleted dirty file must change the CWD observation"
        );
    }

    #[cfg(target_os = "linux")]
    fn dead_publisher_lock_bytes(binding: [u8; 32], nonce: [u8; 16]) -> Vec<u8> {
        let mut marker = GitIndexLockMarker::new(binding).unwrap();
        marker.process_start = if marker.process_start == u64::MAX {
            marker.process_start - 1
        } else {
            marker.process_start + 1
        };
        marker.nonce = nonce;
        marker.encode()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owner_liveness_fails_closed_when_procfs_is_indeterminate() {
        assert_eq!(
            process_owner_liveness(7, ProcessStartObservation::Unknown),
            GitIndexLockOwnerLiveness::Unknown
        );
        assert_eq!(
            process_owner_liveness(7, ProcessStartObservation::Observed(7)),
            GitIndexLockOwnerLiveness::Live
        );
        assert_eq!(
            process_owner_liveness(7, ProcessStartObservation::Observed(8)),
            GitIndexLockOwnerLiveness::ProvenDead
        );
        assert_eq!(
            process_owner_liveness(7, ProcessStartObservation::Missing),
            GitIndexLockOwnerLiveness::ProvenDead
        );
    }

    #[test]
    fn recovery_refuses_a_live_publishers_matching_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [7; 32];
        let lock = GitIndexLock::acquire_owned(lock_path.clone(), binding).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert!(lock_path.is_file());

        drop(lock);
        assert!(!lock_path.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unbound_orphan_reclaim_takes_only_a_dead_orna_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        fs::write(&lock_path, dead_publisher_lock_bytes([10; 32], [3; 16])).unwrap();
        GitIndexLock::reclaim_unbound_orphan(lock_path.clone()).unwrap();
        assert!(!lock_path.exists());

        let foreign = b"foreign Git index lock bytes\n".to_vec();
        fs::write(&lock_path, &foreign).unwrap();
        assert!(matches!(
            GitIndexLock::reclaim_unbound_orphan(lock_path.clone()),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), foreign);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_owner_orna_lock_is_never_reclaimed() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        // The marker names this live process, so its owner is not provably dead.
        let live = GitIndexLockMarker::new([10; 32]).unwrap().encode();
        fs::write(&lock_path, &live).unwrap();
        assert!(matches!(
            GitIndexLock::reclaim_unbound_orphan(lock_path.clone()),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), [10; 32]),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), live);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dead_ordinary_writer_lock_with_zero_binding_is_never_reclaimed() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        // All-zero binding marks an ordinary Orna writer, not a publisher.
        let ordinary = dead_publisher_lock_bytes([0; 32], [4; 16]);
        fs::write(&lock_path, &ordinary).unwrap();
        assert!(matches!(
            GitIndexLock::reclaim_unbound_orphan(lock_path.clone()),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), ordinary);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_refuses_a_lock_with_a_mismatched_journal_binding() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let lock_binding = [8; 32];
        let journal_binding = [7; 32];
        let original = dead_publisher_lock_bytes(lock_binding, [1; 16]);
        fs::write(&lock_path, &original).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), journal_binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), original);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_preserves_a_malformed_or_foreign_canonical_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [18; 32];
        let original = b"foreign Git index lock bytes\0\xff\n".to_vec();
        fs::write(&lock_path, &original).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), original);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_preserves_a_replacement_that_arrives_before_lock_transfer() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [9; 32];
        let stale = dead_publisher_lock_bytes(binding, [2; 16]);
        let claim = GitIndexLock::claim_path(&lock_path, &stale).unwrap();
        fs::write(&lock_path, stale).unwrap();
        let replacement = b"ordinary Git writer lock\n".to_vec();

        let result =
            GitIndexLock::reclaim_abandoned_with_hook(lock_path.clone(), binding, |point, path| {
                if point == GitIndexLockReclaimPoint::BeforeTransfer {
                    fs::write(path, &replacement).unwrap();
                }
            });
        assert!(matches!(result, Err(RepositoryError::GitIndexLockPresent)));
        assert_eq!(fs::read(&lock_path).unwrap(), replacement);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(!claim.exists());
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(lock_path).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_preserves_a_replacement_that_arrives_after_lock_transfer() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [10; 32];
        fs::write(&lock_path, dead_publisher_lock_bytes(binding, [3; 16])).unwrap();
        let replacement = b"new publisher lock\n".to_vec();

        let result =
            GitIndexLock::reclaim_abandoned_with_hook(lock_path.clone(), binding, |point, path| {
                if point == GitIndexLockReclaimPoint::AfterTransfer {
                    fs::write(path, &replacement).unwrap();
                }
            });
        assert!(matches!(result, Err(RepositoryError::GitIndexLockPresent)));
        assert_eq!(fs::read(&lock_path).unwrap(), replacement);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(lock_path).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_preserves_a_replacement_before_private_claim_disposal() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [13; 32];
        let stale = dead_publisher_lock_bytes(binding, [5; 16]);
        let claim = GitIndexLock::claim_path(&lock_path, &stale).unwrap();
        fs::write(&lock_path, stale).unwrap();
        let replacement = b"replacement private claim\n".to_vec();

        let result =
            GitIndexLock::reclaim_abandoned_with_hook(lock_path.clone(), binding, |point, path| {
                if point == GitIndexLockReclaimPoint::BeforePrivateClaimDisposal {
                    fs::write(path, &replacement).unwrap();
                }
            });
        assert!(matches!(result, Err(RepositoryError::GitIndexLockPresent)));
        assert!(!lock_path.exists());
        assert_eq!(fs::read(&claim).unwrap(), replacement);
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(claim).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_reclaims_its_interrupted_private_claim_only() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [11; 32];
        let bytes = dead_publisher_lock_bytes(binding, [4; 16]);
        let claim = GitIndexLock::claim_path(&lock_path, &bytes).unwrap();
        fs::write(&claim, &bytes).unwrap();

        GitIndexLock::reclaim_abandoned(lock_path.clone(), binding).unwrap();
        assert!(!lock_path.exists());
        assert!(!claim.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_reclaims_an_interrupted_private_disposal_tombstone() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [19; 32];
        let bytes = dead_publisher_lock_bytes(binding, [20; 16]);
        let disposal = lock_path.with_file_name(GitIndexLock::disposal_name(
            lock_path.file_name().unwrap(),
            &bytes,
        ));
        fs::write(&disposal, &bytes).unwrap();

        GitIndexLock::reclaim_abandoned(lock_path.clone(), binding).unwrap();
        assert!(!disposal.exists());
        assert!(!lock_path.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_retains_an_unknown_private_claim() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let mut claim = GitIndexLock::private_prefix(lock_path.file_name().unwrap());
        claim.push("unknown");
        let claim = root.path().join(claim);
        let unknown = b"do not delete\n".to_vec();
        fs::write(&claim, &unknown).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, [12; 32]),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(claim).unwrap(), unknown);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_retains_a_replacement_at_the_private_disposal_name() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [17; 32];
        let stale = dead_publisher_lock_bytes(binding, [10; 16]);
        let disposal = lock_path.with_file_name(GitIndexLock::disposal_name(
            lock_path.file_name().unwrap(),
            &stale,
        ));
        let replacement = b"replacement private tombstone\n".to_vec();
        fs::write(&disposal, &replacement).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&disposal).unwrap(), replacement);
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(disposal).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_retains_multiple_private_claims() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [14; 32];
        let first = dead_publisher_lock_bytes(binding, [6; 16]);
        let second = dead_publisher_lock_bytes(binding, [7; 16]);
        let first_claim = GitIndexLock::claim_path(&lock_path, &first).unwrap();
        let second_claim = GitIndexLock::claim_path(&lock_path, &second).unwrap();
        fs::write(&first_claim, &first).unwrap();
        fs::write(&second_claim, &second).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(first_claim).unwrap(), first);
        assert_eq!(fs::read(second_claim).unwrap(), second);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_rejects_a_symlinked_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let target = root.path().join("lock-target");
        let binding = [15; 32];
        let bytes = dead_publisher_lock_bytes(binding, [8; 16]);
        fs::write(&target, &bytes).unwrap();
        std::os::unix::fs::symlink(&target, &lock_path).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(target).unwrap(), bytes);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_supports_non_utf8_lock_basenames() {
        let root = tempfile::TempDir::new().unwrap();
        let mut name = vec![b'i'; 240];
        name[120] = 0xff;
        let lock_path = root.path().join(std::ffi::OsString::from_vec(name));
        let binding = [16; 32];
        fs::write(&lock_path, dead_publisher_lock_bytes(binding, [9; 16])).unwrap();

        GitIndexLock::reclaim_abandoned(lock_path.clone(), binding).unwrap();
        assert!(!lock_path.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn publisher_index_lock_child() {
        let Some(path) = env::var_os("ORNA_TEST_GIT_INDEX_LOCK_PATH") else {
            return;
        };
        let encoded = env::var("ORNA_TEST_GIT_INDEX_LOCK_BINDING").unwrap();
        let bytes = (0..32)
            .map(|index| u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16).unwrap())
            .collect::<Vec<_>>();
        let binding: [u8; 32] = bytes.try_into().unwrap();
        let lock = GitIndexLock::acquire_owned(PathBuf::from(path), binding).unwrap();
        std::mem::forget(lock);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_reclaims_a_dead_publisher_index_lock() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(root.path().join("ordinary.txt"), "base\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();
        let managed = super::ManagedPath::new("generated/row.orna").unwrap();
        let head = repository.head().unwrap().unwrap();
        let index_before = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[super::ManagedFileChange::new(
                    managed.clone(),
                    Some(b"candidate row\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            candidate.commit().clone(),
            index_before.tree().unwrap().clone(),
            [7; 16],
            vec![PublicationJournalEntry::new(
                managed.clone(),
                None,
                Some(b"candidate row\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository.advance_current_ref(&head, &candidate).unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        let lock_path = repository.git_path("index").unwrap().with_extension("lock");
        let binding = journal.lock_binding().unwrap();
        let encoded_binding = binding
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::publisher_index_lock_child",
                "--nocapture",
            ])
            .env("ORNA_TEST_GIT_INDEX_LOCK_PATH", &lock_path)
            .env("ORNA_TEST_GIT_INDEX_LOCK_BINDING", encoded_binding)
            .status()
            .unwrap();
        assert!(child.success());
        assert!(lock_path.is_file());

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        assert!(!lock_path.exists());
        assert_eq!(repository.head().unwrap(), Some(candidate.commit().clone()));
        assert_eq!(
            git(root.path(), &["show", ":ordinary.txt"]),
            "staged human edit"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("main.orna")).unwrap(),
            "unstaged human edit\n"
        );
        assert_eq!(
            fs::read(root.path().join(managed.as_path())).unwrap(),
            b"candidate row\n"
        );
        let mut completed = repository.read_publication_journal().unwrap().unwrap();
        repository
            .mark_runtime_complete([7; 16], &mut completed)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
    }

    #[test]
    fn fresh_recovery_resumes_persisted_index_and_completion_phases() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"first\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [31; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                None,
                Some(b"first\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository.advance_current_ref(&head, &candidate).unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .reconcile_published_index(&index, &candidate, std::slice::from_ref(&path))
            .unwrap();
        journal
            .advance(PublicationJournalStage::IndexReconciled)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"first\n"
        );
        let mut completed = repository.read_publication_journal().unwrap().unwrap();
        repository
            .mark_runtime_complete([31; 16], &mut completed)
            .unwrap();

        let old_head = repository.head().unwrap().unwrap();
        let old_index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &old_head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"second\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head.clone(),
            candidate.commit().clone(),
            old_index.tree().unwrap().clone(),
            [32; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                Some(b"first\n".to_vec()),
                Some(b"second\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .advance_current_ref(&old_head, &candidate)
            .unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .reconcile_published_index(&old_index, &candidate, std::slice::from_ref(&path))
            .unwrap();
        journal
            .advance(PublicationJournalStage::IndexReconciled)
            .unwrap();
        repository
            .materialize_publication_entry(&mut journal, 0)
            .unwrap();
        journal
            .advance(PublicationJournalStage::WorktreeReconciled)
            .unwrap();
        journal
            .advance(PublicationJournalStage::RuntimeCompleted)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        assert!(repository.recover_publication().unwrap().is_some());
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"second\n"
        );
    }

    /// ORNA-PUB-007: a publisher killed after installing the reconciled index
    /// but before IndexReconciled is durable leaves `.orna/store` bound as
    /// leaf entries. Recovery must recognise that index as the candidate's
    /// rather than report it as stale.
    #[test]
    fn recovery_recognises_an_installed_store_index_before_its_journal_stage() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        let database = include_str!("../tests/fixtures/format-context/database-final.orna");
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/database.orna"), database).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/store/data").unwrap(),
                        Some(b"store root two".to_vec()),
                    ),
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/store/refs/node/data").unwrap(),
                        Some(b"child node".to_vec()),
                    ),
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/store/refs/node/chunk").unwrap(),
                        Some(b"payload chunk".to_vec()),
                    ),
                ],
                "orna: publish runtime data",
            )
            .unwrap();
        let store_root = git(
            root.path(),
            &[
                "rev-parse",
                &format!("{}:.orna/store", candidate.commit().as_str()),
            ],
        );
        let algorithm = if store_root.len() == 64 {
            super::GitHashAlgorithm::Sha256
        } else {
            super::GitHashAlgorithm::Sha1
        };
        let marker = ManagedPath::new(".orna/database.orna").unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [61; 16],
            vec![PublicationJournalEntry::new(
                marker.clone(),
                Some(database.as_bytes().to_vec()),
                Some(database.as_bytes().to_vec()),
            )],
        )
        .unwrap()
        .with_store_root(&super::NativeOid::from_hex(algorithm, &store_root).unwrap())
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository.advance_current_ref(&head, &candidate).unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        let installed = repository
            .reconcile_published_index(
                &index,
                &candidate,
                &[marker, ManagedPath::new(".orna/store").unwrap()],
            )
            .unwrap();
        assert!(
            git(root.path(), &["ls-files", "--stage", "--", ".orna/store"])
                .lines()
                .count()
                > 1
        );

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        let mut recovered = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(
            recovered.stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(repository.index_generation().unwrap(), installed);
        assert_eq!(
            fs::read(root.path().join(".orna/store/data")).unwrap(),
            b"store root two"
        );
        repository
            .mark_runtime_complete([61; 16], &mut recovered)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
    }

    #[test]
    fn recovery_finalizes_runtime_completed_descendant_head() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(root.path().join("ordinary.txt"), "base\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let old_head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &old_head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"published\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head,
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [33; 16],
            vec![PublicationJournalEntry::new(
                path,
                None,
                Some(b"published\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .advance_current_ref(&journal.old_head().clone(), &candidate)
            .unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        journal
            .advance(PublicationJournalStage::IndexReconciled)
            .unwrap();
        fs::create_dir_all(root.path().join("generated")).unwrap();
        fs::write(root.path().join("generated/row.orna"), "published\n").unwrap();
        git(root.path(), &["add", "generated/row.orna"]);
        journal
            .advance(PublicationJournalStage::WorktreeReconciled)
            .unwrap();
        journal
            .advance(PublicationJournalStage::RuntimeCompleted)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        fs::write(root.path().join("ordinary.txt"), "later\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        git(root.path(), &["commit", "-m", "ordinary follow-up"]);
        let follow_up = repository.head().unwrap().unwrap();

        // Spec ORNA-PUB-009 / recovery table row "Ref no longer H or N": the
        // ordinary descendant proves the batch is present, so cleanup finishes
        // under the new ref without rewriting it (ORNA-PUB-017).
        assert!(repository.recover_publication().unwrap().is_some());
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(repository.head().unwrap().unwrap(), follow_up);
    }

    /// Builds a publication whose candidate is reconciled into the ordinary
    /// index and worktree (stage `WorktreeReconciled`) and whose ref is the
    /// candidate, as a crash after reconciliation leaves it.
    fn publication_reconciled_at_candidate(root: &Path) -> (Repository, PublicationJournal) {
        git(root, &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root);
        git(root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("main.orna"), "module main;\n").unwrap();
        fs::write(root.join("ordinary.txt"), "base\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "initial"]);

        let repository = Repository::discover(root).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let old_head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &old_head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"published\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head,
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [33; 16],
            vec![PublicationJournalEntry::new(
                path,
                None,
                Some(b"published\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .advance_current_ref(&journal.old_head().clone(), &candidate)
            .unwrap();
        let row = root.join("generated");
        fs::create_dir_all(&row).unwrap();
        fs::write(row.join("row.orna"), "published\n").unwrap();
        git(root, &["add", "generated/row.orna"]);
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        journal
            .advance(PublicationJournalStage::IndexReconciled)
            .unwrap();
        journal
            .advance(PublicationJournalStage::WorktreeReconciled)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        (repository, journal)
    }

    #[test]
    fn worktree_reconciled_descendant_head_completes_via_runtime_completion() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        fs::write(root.path().join("ordinary.txt"), "later\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        git(root.path(), &["commit", "-m", "ordinary follow-up"]);
        let follow_up = repository.head().unwrap().unwrap();

        // The runtime transaction must still complete before cleanup.
        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        repository
            .mark_runtime_complete([33; 16], &mut journal)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(repository.head().unwrap().unwrap(), follow_up);
        assert_eq!(
            fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
            "later\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("generated/row.orna")).unwrap(),
            "published\n"
        );
    }

    /// Recovery table row "Ref no longer H or N" without ancestry proof: a
    /// HEAD that does not contain the candidate is a recovery conflict. The
    /// journal and the newer ref are both preserved.
    #[test]
    fn recovery_fences_head_that_does_not_contain_candidate() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        let old_head = journal.old_head().clone();
        let tree = git(
            root.path(),
            &["rev-parse", &format!("{}^{{tree}}", old_head.as_str())],
        );
        let sibling = git(
            root.path(),
            &[
                "commit-tree",
                tree.trim(),
                "-p",
                old_head.as_str(),
                "-m",
                "sibling",
            ],
        );
        git(
            root.path(),
            &["update-ref", "refs/heads/main", sibling.trim()],
        );

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::StaleHead)
        ));
        assert!(matches!(
            repository.mark_runtime_complete([33; 16], &mut journal),
            Err(RepositoryError::StaleHead)
        ));
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(repository.head().unwrap().unwrap().as_str(), sibling.trim());
    }

    /// Recovery row "Index or affected path differs from both recorded
    /// states": a managed entry dropped from the index after reconciliation is
    /// a typed conflict even when a descendant commit exists. The journal and
    /// the managed bytes are preserved (ORNA-PUB-011, ORNA-PUB-017).
    #[test]
    fn descendant_completion_refuses_index_that_dropped_managed_entry() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        git(root.path(), &["rm", "--cached", "-q", "generated/row.orna"]);
        fs::write(root.path().join("ordinary.txt"), "later\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        git(root.path(), &["commit", "-m", "follow-up after index edit"]);

        let result = repository.mark_runtime_complete([33; 16], &mut journal);
        assert!(
            matches!(result, Err(RepositoryError::StaleIndex { .. })),
            "{result:?}"
        );
        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::StaleIndex { .. })
        ));
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(
            fs::read_to_string(root.path().join("generated/row.orna")).unwrap(),
            "published\n"
        );
    }

    /// An unrelated staged edit is outside the published batch and survives
    /// runtime completion.
    #[test]
    fn runtime_completion_preserves_unrelated_staged_change() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        fs::write(root.path().join("ordinary.txt"), "staged\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        repository
            .mark_runtime_complete([33; 16], &mut journal)
            .unwrap();

        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(
            git(root.path(), &["diff", "--cached", "--name-only"]).trim(),
            "ordinary.txt"
        );
    }

    /// A rewound HEAD (an ancestor of the candidate) does not contain the
    /// batch: the journal and the managed bytes are preserved.
    #[test]
    fn recovery_fences_rewound_head_and_keeps_managed_bytes() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        let old_head = journal.old_head().clone();
        git(
            root.path(),
            &["update-ref", "refs/heads/main", old_head.as_str()],
        );

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::StaleHead)
        ));
        assert!(matches!(
            repository.mark_runtime_complete([33; 16], &mut journal),
            Err(RepositoryError::StaleHead)
        ));
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(
            fs::read_to_string(root.path().join("generated/row.orna")).unwrap(),
            "published\n"
        );
    }

    /// A same-tree commit with a different parent (a rebase) is a different
    /// commit: containment is by commit id, so it is refused.
    #[test]
    fn recovery_fences_rebased_same_tree_commit() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        let old_head = journal.old_head().clone();
        let tree = git(
            root.path(),
            &[
                "rev-parse",
                &format!("{}^{{tree}}", journal.new_head().as_str()),
            ],
        );
        let rebased = git(
            root.path(),
            &[
                "commit-tree",
                tree.trim(),
                "-p",
                old_head.as_str(),
                "-m",
                "rebased",
            ],
        );
        git(
            root.path(),
            &["update-ref", "refs/heads/main", rebased.trim()],
        );

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::StaleHead)
        ));
        assert!(matches!(
            repository.mark_runtime_complete([33; 16], &mut journal),
            Err(RepositoryError::StaleHead)
        ));
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::WorktreeReconciled
        );
    }

    /// A merge whose first parent is the candidate descends from it, so the
    /// batch is present and completion leaves ref, index and bytes untouched.
    #[test]
    fn runtime_completion_accepts_merge_descending_from_candidate() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        let old_head = journal.old_head().clone();
        let candidate = journal.new_head().clone();
        let tree = git(
            root.path(),
            &["rev-parse", &format!("{}^{{tree}}", candidate.as_str())],
        );
        let side = git(
            root.path(),
            &[
                "commit-tree",
                tree.trim(),
                "-p",
                old_head.as_str(),
                "-m",
                "side",
            ],
        );
        let merge = git(
            root.path(),
            &[
                "commit-tree",
                tree.trim(),
                "-p",
                candidate.as_str(),
                "-p",
                side.trim(),
                "-m",
                "merge",
            ],
        );
        git(
            root.path(),
            &["update-ref", "refs/heads/main", merge.trim()],
        );

        repository
            .mark_runtime_complete([33; 16], &mut journal)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(repository.head().unwrap().unwrap().as_str(), merge.trim());
        assert_eq!(
            fs::read_to_string(root.path().join("generated/row.orna")).unwrap(),
            "published\n"
        );
    }

    #[test]
    fn checkout_preflight_rejects_same_commit_branch_attachment_drift() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());
        git(root.path(), &["branch", "target"]);
        git(root.path(), &["branch", "interleaved"]);

        let repository = Repository::discover(root.path()).unwrap();
        let plan = repository
            .plan_checkout("target", RuntimeGeneration::new(41))
            .unwrap();
        let token = plan.force_token();

        git(root.path(), &["switch", "interleaved"]);

        assert!(matches!(
            repository.verify_checkout_preflight(&plan),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        assert!(matches!(
            repository.authorize_checkout_force(&plan, true, Some(&token)),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        assert_eq!(
            Command::new("git")
                .current_dir(root.path())
                .args(["branch", "--show-current"])
                .output()
                .unwrap()
                .stdout,
            b"interleaved\n"
        );
    }

    #[test]
    fn checkout_force_witness_rejects_same_status_content_drift() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(root.path().join("ordinary.txt"), "base\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        git(root.path(), &["branch", "experiment"]);
        git(root.path(), &["switch", "experiment"]);
        fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
        fs::write(root.path().join("ignored.txt"), "ignored target\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        git(root.path(), &["add", "-f", "ignored.txt"]);
        git(root.path(), &["commit", "-m", "target"]);
        git(root.path(), &["switch", "main"]);
        fs::write(root.path().join("ordinary.txt"), "first local edit\n").unwrap();
        fs::write(root.path().join("ignored.txt"), "first ignored edit\n").unwrap();

        let repository = Repository::discover(root.path()).unwrap();
        let plan = repository
            .plan_checkout("experiment", RuntimeGeneration::new(51))
            .unwrap();
        let token = plan.force_token();
        let discard = repository
            .validate_checkout_discard_set(
                &plan,
                true,
                Some(&token),
                plan.git().discardable_paths(),
                &AcceptAllCheckoutAssertions,
            )
            .unwrap();

        fs::write(root.path().join("ordinary.txt"), "second local edit\n").unwrap();
        assert!(matches!(
            repository.verify_validated_checkout_discard(&discard),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        fs::write(root.path().join("ordinary.txt"), "first local edit\n").unwrap();
        repository
            .persist_validated_checkout_discard(&discard)
            .unwrap();

        fs::write(root.path().join("ignored.txt"), "second ignored edit\n").unwrap();
        let head_before = repository.head().unwrap();
        assert!(matches!(
            repository.verify_validated_checkout_discard(&discard),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        assert!(matches!(
            repository.recover_pre_execution_checkout(),
            Err(RepositoryError::CheckoutRecoveryRequired)
        ));
        assert_eq!(repository.head().unwrap(), head_before);
        assert_eq!(
            repository.current_branch().unwrap().as_deref(),
            Some("main")
        );
        assert_eq!(
            fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
            "first local edit\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("ignored.txt")).unwrap(),
            "second ignored edit\n"
        );
        assert!(repository.has_pending_pre_execution_checkout().unwrap());
    }

    #[test]
    fn journaled_materialization_replaces_and_deletes_without_quarantine_leaks() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let head = repository.head().unwrap().unwrap();
        let tree = repository
            .index_generation()
            .unwrap()
            .tree()
            .unwrap()
            .clone();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            GitCommitRef("b".repeat(40)),
            tree.clone(),
            [21; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                None,
                Some(b"first".to_vec()),
            )],
        )
        .unwrap();
        repository
            .materialize_publication_entry(&mut journal, 0)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"first"
        );

        let mut replacement = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            GitCommitRef("c".repeat(40)),
            tree.clone(),
            [22; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                Some(b"first".to_vec()),
                Some(b"second".to_vec()),
            )],
        )
        .unwrap();
        let replacement_name = replacement.quarantine_name(0).unwrap();
        repository
            .materialize_publication_entry(&mut replacement, 0)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"second"
        );
        assert!(
            !root
                .path()
                .join("generated")
                .join(replacement_name)
                .exists()
        );

        let mut deletion = PublicationJournal::new_with_runtime_intent(
            head,
            GitCommitRef("d".repeat(40)),
            tree,
            [23; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                Some(b"second".to_vec()),
                None,
            )],
        )
        .unwrap();
        repository
            .materialize_publication_entry(&mut deletion, 0)
            .unwrap();
        assert!(!root.path().join(path.as_path()).exists());
    }

    #[test]
    fn journaled_materialization_preserves_quarantine_on_install_conflict_and_deletion_cleanup() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let conflict_path = ManagedPath::new("generated/conflict.orna").unwrap();
        let conflict_target = root.path().join(conflict_path.as_path());
        fs::create_dir_all(conflict_target.parent().unwrap()).unwrap();
        fs::write(&conflict_target, b"captured").unwrap();
        let head = repository.head().unwrap().unwrap();
        let tree = repository
            .index_generation()
            .unwrap()
            .tree()
            .unwrap()
            .clone();
        let mut conflict = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            GitCommitRef("f".repeat(40)),
            tree.clone(),
            [25; 16],
            vec![PublicationJournalEntry::new(
                conflict_path.clone(),
                Some(b"captured".to_vec()),
                Some(b"published".to_vec()),
            )],
        )
        .unwrap();
        let conflict_name = conflict.quarantine_name(0).unwrap();
        conflict
            .set_materialization(0, PublicationMaterializationPhase::QuarantinePrepared)
            .unwrap();
        repository.write_publication_journal(&conflict).unwrap();
        let mut after_boundary = |phase| {
            conflict.set_materialization(0, phase)?;
            repository.write_publication_journal_locked(&conflict)?;
            if phase == PublicationMaterializationPhase::Quarantined {
                fs::write(&conflict_target, b"external").unwrap();
            }
            Ok(())
        };
        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &conflict_path,
                Some(b"captured"),
                Some(b"published"),
                Some(&conflict_target.parent().unwrap().join(&conflict_name)),
                None,
                Some(&mut after_boundary),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        let conflict_quarantine = conflict_target.parent().unwrap().join(conflict_name);
        assert_eq!(fs::read(&conflict_quarantine).unwrap(), b"captured");
        assert_eq!(fs::read(&conflict_target).unwrap(), b"external");
        let persisted = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(
            persisted.entries()[0].materialization().phase(),
            PublicationMaterializationPhase::Quarantined
        );

        let delete_path = ManagedPath::new("generated/delete.orna").unwrap();
        let delete_target = root.path().join(delete_path.as_path());
        let mut deletion = PublicationJournal::new_with_runtime_intent(
            head,
            GitCommitRef("1".repeat(40)),
            tree,
            [26; 16],
            vec![PublicationJournalEntry::new(
                delete_path,
                Some(b"to-delete".to_vec()),
                None,
            )],
        )
        .unwrap();
        let delete_name = deletion.quarantine_name(0).unwrap();
        let delete_quarantine = delete_target.parent().unwrap().join(&delete_name);
        fs::write(&delete_quarantine, b"to-delete").unwrap();
        deletion
            .set_materialization(0, PublicationMaterializationPhase::Installed)
            .unwrap();
        repository.write_publication_journal(&deletion).unwrap();
        let mut recovered = repository.read_publication_journal().unwrap().unwrap();
        repository
            .materialize_publication_entry(&mut recovered, 0)
            .unwrap();
        assert!(!delete_target.exists());
        assert!(!delete_quarantine.exists());
        assert_eq!(
            recovered.entries()[0].materialization().phase(),
            PublicationMaterializationPhase::Clean
        );
    }

    /// Repository with one committed file. Each per-guard test below builds one
    /// crafted journal state, so a refusal can only come from the guard it names.
    fn managed_guard_fixture() -> (tempfile::TempDir, Repository) {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());
        let repository = Repository::discover(root.path()).unwrap();
        (root, repository)
    }

    fn managed_guard_journal(
        repository: &Repository,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        id: u8,
    ) -> PublicationJournal {
        let head = repository.head().unwrap().unwrap();
        let tree = repository
            .index_generation()
            .unwrap()
            .tree()
            .unwrap()
            .clone();
        PublicationJournal::new_with_runtime_intent(
            head,
            GitCommitRef("e".repeat(40)),
            tree,
            [id; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                expected.map(<[u8]>::to_vec),
                next.map(<[u8]>::to_vec),
            )],
        )
        .unwrap()
    }

    /// Clean phase with quarantine residue: the guard at the
    /// `quarantine_bytes.is_some()` check in materialize_publication_entry_phases_locked.
    /// The target already holds `next`, which the Clean branch accepts as complete,
    /// so only this guard refuses.
    #[test]
    fn managed_guard_clean_quarantine_residue_is_refused_before_completion_check() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let mut journal =
            managed_guard_journal(&repository, &path, Some(b"expected"), Some(b"next"), 31);
        let parent = root.path().join("generated");
        let quarantine = parent.join(journal.quarantine_name(0).unwrap());
        let target = root.path().join(path.as_path());
        fs::create_dir_all(&parent).unwrap();
        fs::write(&target, b"next").unwrap();
        fs::write(&quarantine, b"expected").unwrap();
        repository.write_publication_journal(&journal).unwrap();

        assert!(matches!(
            repository.materialize_publication_entry(&mut journal, 0),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"next");
        assert_eq!(fs::read(&quarantine).unwrap(), b"expected");
    }

    /// Clean phase with a target matching neither expected nor next: the guard at
    /// the `current != entry.expected()` check. The impl-level check would also refuse,
    /// but only after the phase had advanced, so this asserts the journal never advances.
    #[test]
    fn managed_guard_clean_unexpected_target_is_refused_without_advancing_the_journal() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let mut journal =
            managed_guard_journal(&repository, &path, Some(b"expected"), Some(b"next"), 32);
        let target = root.path().join(path.as_path());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"foreign").unwrap();
        repository.write_publication_journal(&journal).unwrap();

        assert!(matches!(
            repository.materialize_publication_entry(&mut journal, 0),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"foreign");
        let persisted = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(
            persisted.entries()[0].materialization().phase(),
            PublicationMaterializationPhase::Clean
        );
    }

    /// Quarantine-prepared phase with a foreign quarantine file: the guard at the
    /// `Some(bytes) != entry.expected()` check. The target is absent, so without this
    /// guard the next-content install would write `next` into the target.
    #[test]
    fn managed_guard_foreign_quarantine_is_refused_before_installing_next() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let mut journal =
            managed_guard_journal(&repository, &path, Some(b"expected"), Some(b"next"), 33);
        let name = journal.quarantine_name(0).unwrap();
        journal
            .set_materialization(0, PublicationMaterializationPhase::QuarantinePrepared)
            .unwrap();
        let parent = root.path().join("generated");
        let quarantine = parent.join(&name);
        let target = root.path().join(path.as_path());
        fs::create_dir_all(&parent).unwrap();
        fs::write(&quarantine, b"foreign").unwrap();
        repository.write_publication_journal(&journal).unwrap();

        assert!(matches!(
            repository.materialize_publication_entry(&mut journal, 0),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert!(!target.exists());
        assert_eq!(fs::read(&quarantine).unwrap(), b"foreign");
    }

    /// Installed phase with no quarantine file and the target still at `expected`: the
    /// guard at the phase check that follows the quarantine branch. Only this guard
    /// refuses; without it the managed install would move `expected` aside and write `next`.
    #[test]
    fn managed_guard_installed_phase_without_quarantine_does_not_rewind_the_target() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let mut journal =
            managed_guard_journal(&repository, &path, Some(b"expected"), Some(b"next"), 34);
        journal
            .set_materialization(0, PublicationMaterializationPhase::Installed)
            .unwrap();
        let target = root.path().join(path.as_path());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"expected").unwrap();
        repository.write_publication_journal(&journal).unwrap();

        assert!(matches!(
            repository.materialize_publication_entry(&mut journal, 0),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"expected");
        let persisted = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(
            persisted.entries()[0].materialization().phase(),
            PublicationMaterializationPhase::Installed
        );
    }

    /// Managed install entry check: removal (`next` None) with the expected bytes absent.
    /// The removal branch returns Ok for an absent target, so only the entry guard refuses.
    #[test]
    fn managed_guard_install_refuses_missing_target_for_expected_removal() {
        let (_root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let _lock = repository.acquire_coordination_lock().unwrap();
        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine_locked(
                &path,
                Some(b"expected"),
                None,
                None,
                None,
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
    }

    /// Repository with `.orna/store/data` committed as `committed`. The layered
    /// reconcile guards run through `reconcile_store_worktree_transition`, so each
    /// test edits one leaf state and names the single guard it expects to refuse.
    fn managed_guard_store_fixture(committed: &[u8]) -> (tempfile::TempDir, Repository) {
        managed_guard_store_leaf_fixture("data", committed)
    }

    /// Repository with `.orna/store/<leaf>` committed as `committed`. A leaf other than
    /// `data` is an OGB-2 payload chunk (`is_store_payload_blob_path`).
    fn managed_guard_store_leaf_fixture(
        leaf: &str,
        committed: &[u8],
    ) -> (tempfile::TempDir, Repository) {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        let relative = format!(".orna/store/{leaf}");
        fs::write(root.path().join(&relative), committed).unwrap();
        git(root.path(), &["add", relative.as_str()]);
        git(root.path(), &["commit", "-m", "base"]);
        let repository = Repository::discover(root.path()).unwrap();
        (root, repository)
    }

    /// The current index as a private candidate: the reconcile target when the
    /// store did not change, or the commit after a changed store leaf.
    fn managed_guard_committed_candidate(repository: &Repository) -> super::PrivateCommit {
        let index = repository.index_generation().unwrap();
        super::PrivateCommit {
            tree: index.tree().unwrap().clone(),
            commit: index.head().unwrap().clone(),
        }
    }

    /// Layered guard at line 4086 of `reconcile_store_worktree_transition`: a
    /// skipped, unchanged store leaf whose worktree bytes differ from the committed
    /// bytes is an external edit and is refused, not reconciled.
    #[test]
    fn managed_guard_layered_skipped_unchanged_leaf_edit_is_refused() {
        let (root, repository) = managed_guard_store_fixture(b"committed\n");
        git(
            root.path(),
            &["update-index", "--skip-worktree", ".orna/store/data"],
        );
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);
        fs::write(root.path().join(".orna/store/data"), b"edited\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(
            fs::read(root.path().join(".orna/store/data")).unwrap(),
            b"edited\n"
        );
    }

    /// Layered guard at line 4096: an unchanged, unskipped node-metadata leaf whose worktree
    /// bytes differ from the committed bytes is an external edit and is refused.
    #[test]
    fn managed_guard_layered_unchanged_metadata_leaf_edit_is_refused() {
        let (root, repository) = managed_guard_store_fixture(b"committed\n");
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);
        fs::write(root.path().join(".orna/store/data"), b"edited\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(
            fs::read(root.path().join(".orna/store/data")).unwrap(),
            b"edited\n"
        );
    }

    /// Positive control for the metadata guard: a worktree leaf that already holds the
    /// committed bytes passes the unchanged-metadata check and the transition succeeds.
    #[test]
    fn managed_guard_layered_unchanged_metadata_leaf_matching_commit_is_admitted() {
        let (root, repository) = managed_guard_store_fixture(b"committed\n");
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);

        repository
            .reconcile_store_worktree_transition(&expected, &candidate, false)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(".orna/store/data")).unwrap(),
            b"committed\n"
        );
    }

    /// Layered guard at line 4093: a skipped store leaf that is absent from the
    /// worktree may be restored only from its committed bytes, so a quarantine that
    /// holds other bytes is refused and left in place.
    #[test]
    fn managed_guard_layered_skipped_absent_leaf_with_foreign_quarantine_is_refused() {
        let (root, repository) = managed_guard_store_fixture(b"committed\n");
        git(
            root.path(),
            &["update-index", "--skip-worktree", ".orna/store/data"],
        );
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);
        let store = root.path().join(".orna/store");
        fs::remove_file(store.join("data")).unwrap();
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-data",
            candidate.commit().as_str()
        ));
        fs::write(&quarantine, b"foreign\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&quarantine).unwrap(), b"foreign\n");
        assert!(!store.join("data").exists());
    }

    /// Layered guard at line 4121: a changed store leaf whose displaced quarantine
    /// holds bytes other than the old committed leaf is refused, even when the
    /// worktree already holds the new committed bytes.
    #[test]
    fn managed_guard_layered_changed_leaf_with_foreign_quarantine_is_refused() {
        let (root, repository) = managed_guard_store_fixture(b"old\n");
        let expected = repository.index_generation().unwrap();
        fs::write(root.path().join(".orna/store/data"), b"new\n").unwrap();
        git(root.path(), &["commit", "-a", "-m", "next"]);
        let candidate = managed_guard_committed_candidate(&repository);
        let store = root.path().join(".orna/store");
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-data",
            candidate.commit().as_str()
        ));
        fs::write(&quarantine, b"foreign\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(store.join("data")).unwrap(), b"new\n");
        assert_eq!(fs::read(&quarantine).unwrap(), b"foreign\n");
    }

    /// Layered guard at line 4133: a changed store leaf whose worktree bytes are
    /// neither the old nor the new committed bytes, with no quarantine, is an
    /// external edit and is refused.
    #[test]
    fn managed_guard_layered_changed_leaf_with_foreign_content_is_refused() {
        let (root, repository) = managed_guard_store_fixture(b"old\n");
        let expected = repository.index_generation().unwrap();
        fs::write(root.path().join(".orna/store/data"), b"new\n").unwrap();
        git(root.path(), &["commit", "-a", "-m", "next"]);
        let candidate = managed_guard_committed_candidate(&repository);
        fs::write(root.path().join(".orna/store/data"), b"editor\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(
            fs::read(root.path().join(".orna/store/data")).unwrap(),
            b"editor\n"
        );
    }

    /// Payload guard (ORNA-PUB-010): a payload leaf present both at the target and in its
    /// displaced quarantine is ambiguous and is refused, even when both copies hold the
    /// committed bytes and nothing would be installed.
    #[test]
    fn managed_guard_store_payload_with_target_and_quarantine_both_present_is_refused() {
        let (root, repository) = managed_guard_store_leaf_fixture("chunk", b"committed\n");
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);
        let store = root.path().join(".orna/store");
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-chunk",
            candidate.commit().as_str()
        ));
        fs::write(&quarantine, b"committed\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(store.join("chunk")).unwrap(), b"committed\n");
        assert_eq!(fs::read(&quarantine).unwrap(), b"committed\n");
    }

    /// Payload guard, target disjunct: a payload leaf whose worktree bytes differ from
    /// the committed bytes is refused, so no install starts over the edit.
    #[test]
    fn managed_guard_store_payload_with_edited_target_is_refused() {
        let (root, repository) = managed_guard_store_leaf_fixture("chunk", b"committed\n");
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);
        let target = root.path().join(".orna/store/chunk");
        fs::write(&target, b"editor\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"editor\n");
    }

    /// Payload guard, quarantine disjunct: a displaced payload copy with foreign bytes
    /// and no target is refused and left in place.
    #[test]
    fn managed_guard_store_payload_with_foreign_quarantine_and_no_target_is_refused() {
        let (root, repository) = managed_guard_store_leaf_fixture("chunk", b"committed\n");
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);
        let store = root.path().join(".orna/store");
        fs::remove_file(store.join("chunk")).unwrap();
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-chunk",
            candidate.commit().as_str()
        ));
        fs::write(&quarantine, b"foreign\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&quarantine).unwrap(), b"foreign\n");
        assert!(!store.join("chunk").exists());
    }

    /// Positive control for the payload guards: a payload leaf holding exactly the
    /// committed bytes, with no quarantine, is admitted and left unchanged.
    #[test]
    fn managed_guard_store_payload_matching_committed_bytes_is_admitted() {
        let (root, repository) = managed_guard_store_leaf_fixture("chunk", b"committed\n");
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);

        repository
            .reconcile_store_worktree_transition(&expected, &candidate, false)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(".orna/store/chunk")).unwrap(),
            b"committed\n"
        );
    }

    /// Positive control for the layered skipped-leaf path: a skipped metadata leaf that
    /// matches its committed bytes is admitted, so the edit guard refuses only edits.
    #[test]
    fn managed_guard_layered_skipped_leaf_matching_committed_is_admitted() {
        let (root, repository) = managed_guard_store_fixture(b"committed\n");
        git(
            root.path(),
            &["update-index", "--skip-worktree", ".orna/store/data"],
        );
        let expected = repository.index_generation().unwrap();
        let candidate = managed_guard_committed_candidate(&repository);

        repository
            .reconcile_store_worktree_transition(&expected, &candidate, false)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(".orna/store/data")).unwrap(),
            b"committed\n"
        );
    }

    /// Positive control for the changed-leaf install (ORNA-PUB-010): a worktree leaf that
    /// still holds the old committed bytes is replaced with the new bytes, and no
    /// quarantine is left behind.
    #[test]
    fn managed_guard_layered_changed_leaf_at_old_bytes_is_installed_to_new() {
        let (root, repository) = managed_guard_store_fixture(b"old\n");
        let expected = repository.index_generation().unwrap();
        fs::write(root.path().join(".orna/store/data"), b"new\n").unwrap();
        git(root.path(), &["commit", "-a", "-m", "next"]);
        let candidate = managed_guard_committed_candidate(&repository);
        let store = root.path().join(".orna/store");
        fs::write(store.join("data"), b"old\n").unwrap();

        repository
            .reconcile_store_worktree_transition(&expected, &candidate, true)
            .unwrap();
        assert_eq!(fs::read(store.join("data")).unwrap(), b"new\n");
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-data",
            candidate.commit().as_str()
        ));
        assert!(!quarantine.exists());
    }

    /// Positive control for the install path with an explicit quarantine: an expected
    /// target is moved aside and replaced, and the quarantine is removed afterwards.
    #[test]
    fn managed_guard_install_with_explicit_quarantine_replaces_expected_target() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target.parent().unwrap().join(".orna-quarantine-explicit");

        repository
            .materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&quarantine),
                None,
                None,
            )
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
        assert!(!quarantine.exists());
    }

    /// Store-reconcile guard in `materialize_store_worktree_file` (ORNA-PUB-010): a
    /// displaced copy that is not the expected bytes is refused before anything is
    /// installed, even when the target is absent and the next bytes are present.
    #[test]
    fn managed_guard_store_materialize_refuses_foreign_displaced_copy_before_install() {
        let (root, repository) = managed_guard_store_fixture(b"old\n");
        let candidate = managed_guard_committed_candidate(&repository);
        let path = ManagedPath::new(".orna/store/data").unwrap();
        let store = root.path().join(".orna/store");
        fs::remove_file(store.join("data")).unwrap();
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-data",
            candidate.commit().as_str()
        ));
        fs::write(&quarantine, b"foreign\n").unwrap();

        assert!(matches!(
            repository.materialize_store_worktree_file(
                &path,
                Some(b"old\n"),
                Some(b"new\n"),
                &candidate,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert!(!store.join("data").exists());
        assert_eq!(fs::read(&quarantine).unwrap(), b"foreign\n");
    }

    /// Store-reconcile guard in `materialize_store_worktree_file` (ORNA-PUB-010): when the
    /// displaced copy and the target both hold the expected bytes but the next bytes
    /// differ, the install is refused before a candidate is written, so the editor race
    /// hook is never reached.
    #[test]
    fn managed_guard_store_materialize_refuses_expected_pair_before_candidate_write() {
        let (root, repository) = managed_guard_store_fixture(b"old\n");
        let candidate = managed_guard_committed_candidate(&repository);
        let path = ManagedPath::new(".orna/store/data").unwrap();
        let store = root.path().join(".orna/store");
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-data",
            candidate.commit().as_str()
        ));
        fs::write(&quarantine, b"old\n").unwrap();
        let raced = std::rc::Rc::new(std::cell::Cell::new(false));
        let raced_in_race = raced.clone();
        let _race = ManagedCandidateRecheckRace::queue(move || raced_in_race.set(true));

        assert!(matches!(
            repository.materialize_store_worktree_file(
                &path,
                Some(b"old\n"),
                Some(b"new\n"),
                &candidate,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert!(!raced.get());
        assert_eq!(fs::read(store.join("data")).unwrap(), b"old\n");
        assert_eq!(fs::read(&quarantine).unwrap(), b"old\n");
    }

    /// Store-reconcile guard in `materialize_store_worktree_file` (ORNA-PUB-010): a displaced
    /// copy holding the expected bytes while the target already holds the next bytes is
    /// verified and removed, and the target keeps the next bytes.
    #[test]
    fn managed_guard_store_materialize_removes_displaced_copy_when_target_holds_next() {
        let (root, repository) = managed_guard_store_fixture(b"old\n");
        let candidate = managed_guard_committed_candidate(&repository);
        let path = ManagedPath::new(".orna/store/data").unwrap();
        let store = root.path().join(".orna/store");
        fs::write(store.join("data"), b"new\n").unwrap();
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-data",
            candidate.commit().as_str()
        ));
        fs::write(&quarantine, b"old\n").unwrap();

        repository
            .materialize_store_worktree_file(&path, Some(b"old\n"), Some(b"new\n"), &candidate)
            .unwrap();
        assert_eq!(fs::read(store.join("data")).unwrap(), b"new\n");
        assert!(!quarantine.exists());
    }

    /// Store-reconcile guard in `materialize_store_worktree_file` (ORNA-PUB-010): a displaced
    /// copy holding the expected bytes while the target is absent installs the next bytes,
    /// then removes the verified displaced copy.
    #[test]
    fn managed_guard_store_materialize_installs_next_when_target_absent() {
        let (root, repository) = managed_guard_store_fixture(b"old\n");
        let candidate = managed_guard_committed_candidate(&repository);
        let path = ManagedPath::new(".orna/store/data").unwrap();
        let store = root.path().join(".orna/store");
        fs::remove_file(store.join("data")).unwrap();
        let quarantine = store.join(format!(
            ".orna-store-reconcile-{}-data",
            candidate.commit().as_str()
        ));
        fs::write(&quarantine, b"old\n").unwrap();

        repository
            .materialize_store_worktree_file(&path, Some(b"old\n"), Some(b"new\n"), &candidate)
            .unwrap();
        assert_eq!(fs::read(store.join("data")).unwrap(), b"new\n");
        assert!(!quarantine.exists());
    }

    /// Store-reconcile guard in `materialize_store_worktree_file` (ORNA-PUB-010): a foreign
    /// target with no displaced copy is refused and left untouched. The install entry
    /// guard refuses the same state, so this test pins the outcome rather than the guard.
    #[test]
    fn managed_guard_store_materialize_refuses_foreign_target_without_quarantine() {
        let (root, repository) = managed_guard_store_fixture(b"old\n");
        let candidate = managed_guard_committed_candidate(&repository);
        let path = ManagedPath::new(".orna/store/data").unwrap();
        let target = root.path().join(".orna/store/data");
        fs::write(&target, b"editor\n").unwrap();

        assert!(matches!(
            repository.materialize_store_worktree_file(
                &path,
                Some(b"old\n"),
                Some(b"new\n"),
                &candidate,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"editor\n");
    }

    /// The candidate that writes `path` from absent to `published`, with the
    /// Prepared journal that names it. The index it journals is the current one.
    fn managed_guard_publication(
        repository: &Repository,
        path: &ManagedPath,
    ) -> (super::PrivateCommit, PublicationJournal) {
        let old_head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &old_head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"published\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let journal = PublicationJournal::new_with_runtime_intent(
            old_head,
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [41; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                None,
                Some(b"published\n".to_vec()),
            )],
        )
        .unwrap();
        (candidate, journal)
    }

    /// Pre-advance guard at line 5393 of `publish_candidate_body` (ORNA-PUB-010): a
    /// managed target that holds neither the expected nor the next bytes refuses
    /// before the ref CAS, so no journal is written and HEAD does not move.
    #[test]
    fn managed_guard_pre_advance_foreign_target_is_refused_before_ref_advance() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let old_head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let (candidate, mut journal) = managed_guard_publication(&repository, &path);
        let target = root.path().join(path.as_path());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"editor\n").unwrap();

        assert!(matches!(
            repository.publish_candidate(&index, &candidate, &mut journal),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(repository.head().unwrap().unwrap(), old_head);
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(fs::read(&target).unwrap(), b"editor\n");
    }

    /// Pre-advance guard at line 5382: a target that holds the expected bytes (here
    /// absent, matching an install's `None`) is admitted, so the publication the
    /// guard above refuses goes through and advances HEAD.
    #[test]
    fn managed_guard_pre_advance_target_matching_expected_is_admitted() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let index = repository.index_generation().unwrap();
        let (candidate, mut journal) = managed_guard_publication(&repository, &path);

        repository
            .publish_candidate(&index, &candidate, &mut journal)
            .unwrap();
        assert_eq!(
            repository.head().unwrap().unwrap(),
            candidate.commit().clone()
        );
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"published\n"
        );
    }

    /// Pre-advance guard (ORNA-PUB-010): a managed target that already holds the next
    /// bytes is complete, not a conflict, so the publication advances HEAD.
    #[test]
    fn managed_guard_pre_advance_target_already_at_next_is_admitted() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let index = repository.index_generation().unwrap();
        let (candidate, mut journal) = managed_guard_publication(&repository, &path);
        let target = root.path().join(path.as_path());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"published\n").unwrap();

        repository
            .publish_candidate(&index, &candidate, &mut journal)
            .unwrap();
        assert_eq!(
            repository.head().unwrap().unwrap(),
            candidate.commit().clone()
        );
        assert_eq!(fs::read(&target).unwrap(), b"published\n");
    }

    /// Race-only install guard at line 8382 (ORNA-PUB-010): a quarantine path that
    /// already exists is refused before the target is moved, so the install hook is
    /// never reached.
    #[test]
    fn managed_guard_install_refuses_existing_quarantine_path_before_moving_target() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target
            .parent()
            .unwrap()
            .join(".orna-quarantine-preexisting");
        fs::write(&quarantine, b"unexpected").unwrap();
        let mut hook_calls = 0;
        let mut before_install = || hook_calls += 1;

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&quarantine),
                Some(&mut before_install),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(hook_calls, 0);
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(fs::read(&quarantine).unwrap(), b"unexpected");
    }

    /// Race-only install guard at line 8398 (ORNA-PUB-010): a target that vanishes
    /// between the quarantine check and the rename is a managed conflict, not a
    /// local-state failure, and nothing is left at the quarantine path.
    #[test]
    fn managed_guard_install_refuses_target_removed_at_quarantine_boundary() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target.parent().unwrap().join(".orna-quarantine-vanished");
        let mut remove_target = || fs::remove_file(&target).unwrap();

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&quarantine),
                Some(&mut remove_target),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert!(!target.exists());
        assert!(!quarantine.exists());
    }

    /// Race-only install guard at line 8376 (ORNA-PUB-010): an editor write in the
    /// window between the candidate write and the target recheck is refused before
    /// the target is quarantined or the install hook runs.
    #[test]
    fn managed_guard_install_recheck_refuses_editor_race_before_quarantine() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let race_target = target.clone();
        let _race = ManagedCandidateRecheckRace::queue(move || {
            fs::write(&race_target, b"editor").unwrap();
        });
        let mut hook_calls = 0;
        let mut before_install = || hook_calls += 1;

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                Some(b"second"),
                None,
                Some(&mut before_install),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(hook_calls, 0);
        assert_eq!(fs::read(&target).unwrap(), b"editor");
    }

    /// Removal entry guard (ORNA-PUB-010): an expected target holding other bytes is
    /// refused before the install hook runs, so no quarantine is attempted.
    #[test]
    fn managed_guard_removal_refuses_foreign_target_before_install_hook() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let target = root.path().join(path.as_path());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"editor").unwrap();
        let mut hook_calls = 0;
        let mut before_install = || hook_calls += 1;

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                None,
                None,
                Some(&mut before_install),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(hook_calls, 0);
        assert_eq!(fs::read(&target).unwrap(), b"editor");
    }

    /// Removal recheck guard (ORNA-PUB-010): a target that an editor rewrites after the
    /// first read is refused before the quarantine move, and the second hook never runs.
    #[test]
    fn managed_guard_removal_recheck_refuses_editor_write_before_quarantine() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let race_target = target.clone();
        let mut hook_calls = 0;
        let mut before_install = || {
            hook_calls += 1;
            fs::write(&race_target, b"editor").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                None,
                None,
                Some(&mut before_install),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(hook_calls, 1);
        assert_eq!(fs::read(&target).unwrap(), b"editor");
    }

    /// Removal post-move guard (ORNA-PUB-010): an editor write that lands before the
    /// move is moved into quarantine, found not to hold the expected bytes, and restored
    /// to the target, leaving no residue.
    #[test]
    fn managed_guard_removal_restores_editor_write_moved_into_quarantine() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let race_target = target.clone();
        let mut hook_calls = 0;
        let mut before_install = || {
            hook_calls += 1;
            if hook_calls == 2 {
                fs::write(&race_target, b"editor").unwrap();
            }
        };

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                None,
                None,
                Some(&mut before_install),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(hook_calls, 2);
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 1);
    }

    /// Removal boundary guard (ORNA-PUB-010): a target that reappears after the move to
    /// quarantine is a managed conflict. The quarantined copy is kept for inspection.
    #[test]
    fn managed_guard_removal_refuses_target_reappearing_after_quarantine() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target.parent().unwrap().join(".orna-quarantine-reappear");
        let race_target = target.clone();
        let mut after_boundary =
            |phase: PublicationMaterializationPhase| -> Result<(), RepositoryError> {
                if phase == PublicationMaterializationPhase::Quarantined {
                    fs::write(&race_target, b"editor").unwrap();
                }
                Ok(())
            };

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                None,
                Some(&quarantine),
                None,
                Some(&mut after_boundary),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        assert_eq!(fs::read(&quarantine).unwrap(), b"first");
    }

    /// Removal quarantine guard (ORNA-PUB-010): an existing quarantine path is refused
    /// before the install hook runs a second time, and the foreign quarantine is kept.
    #[test]
    fn managed_guard_removal_refuses_existing_quarantine_before_second_hook() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target
            .parent()
            .unwrap()
            .join(".orna-quarantine-preexisting-removal");
        fs::write(&quarantine, b"unexpected").unwrap();
        let mut hook_calls = 0;
        let mut before_install = || hook_calls += 1;

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                None,
                Some(&quarantine),
                Some(&mut before_install),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(hook_calls, 1);
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(fs::read(&quarantine).unwrap(), b"unexpected");
    }

    /// Positive control for the removal branch: an expected target still holding the
    /// expected bytes is moved aside and deleted, leaving no quarantine residue.
    #[test]
    fn managed_guard_removal_of_expected_target_leaves_no_residue() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());

        repository
            .materialize_managed_file(&path, Some(b"first"), None)
            .unwrap();
        assert!(!target.exists());
        assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 0);
    }

    /// Install rename guard (ORNA-PUB-010): a quarantine path that appears between the
    /// path check and the move is refused as a managed conflict, not a local-state
    /// failure, and the target keeps its expected bytes.
    #[test]
    fn managed_guard_install_refuses_quarantine_created_before_move() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target.parent().unwrap().join(".orna-quarantine-created");
        let race_quarantine = quarantine.clone();
        let mut create_quarantine = || fs::write(&race_quarantine, b"foreign").unwrap();

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&quarantine),
                Some(&mut create_quarantine),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(fs::read(&quarantine).unwrap(), b"foreign");
    }

    /// Removal rename guard (ORNA-PUB-010): a quarantine path that appears between the
    /// path check and the move is refused as a managed conflict, not a local-state
    /// failure, and the target keeps its expected bytes.
    #[test]
    fn managed_guard_removal_refuses_quarantine_created_before_move() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target
            .parent()
            .unwrap()
            .join(".orna-quarantine-removal-created");
        let race_quarantine = quarantine.clone();
        // The first hook call precedes the quarantine path check; the second runs
        // after it, immediately before the move, so only the second creates the path.
        let mut hook_calls = 0;
        let mut create_quarantine = || {
            hook_calls += 1;
            if hook_calls == 2 {
                fs::write(&race_quarantine, b"foreign").unwrap();
            }
        };

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                None,
                Some(&quarantine),
                Some(&mut create_quarantine),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(hook_calls, 2);
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(fs::read(&quarantine).unwrap(), b"foreign");
    }

    /// Install restore guard (ORNA-PUB-010): an editor rewrite before the move is moved to
    /// quarantine, found not to hold the expected bytes, and restored to the target.
    #[test]
    fn managed_guard_install_restores_editor_write_moved_into_quarantine() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target.parent().unwrap().join(".orna-quarantine-restored");
        let race_target = target.clone();
        let mut overwrite_target = || fs::write(&race_target, b"editor").unwrap();

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&quarantine),
                Some(&mut overwrite_target),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        assert!(!quarantine.exists());
    }

    /// RAII guard (ORNA-PUB-010): a race queued by a test that unwinds is cleared on drop,
    /// so the next install on the same thread is not raced.
    #[test]
    fn managed_guard_race_slot_is_cleared_when_the_test_guard_unwinds() {
        let (root, repository) = managed_guard_fixture();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let race_target = target.clone();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _race = ManagedCandidateRecheckRace::queue(move || {
                fs::write(&race_target, b"editor").unwrap();
            });
            panic!("unwind while the race is queued");
        }));
        assert!(unwound.is_err());
        assert!(super::MANAGED_CANDIDATE_RECHECK_RACE.with(|slot| slot.borrow().is_none()));

        repository
            .materialize_managed_file(&path, Some(b"first"), Some(b"second"))
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn final_quarantine_replacement_is_preserved_and_reports_conflict() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let parent = root.path().join("generated");
        fs::create_dir_all(&parent).unwrap();
        let quarantine = parent.join(".orna-quarantine-boundary");
        let target = parent.join("row.orna");
        fs::write(&quarantine, b"captured").unwrap();
        fs::write(&target, b"published").unwrap();
        let mut replacement_at_boundary = || {
            fs::write(&quarantine, b"external").unwrap();
            Ok(())
        };

        assert!(matches!(
            repository.remove_verified_quarantine_with_boundary(
                &parent,
                &quarantine,
                &target,
                Some(b"captured"),
                Some(b"published"),
                Some(&mut replacement_at_boundary),
            ),
            Err(RepositoryError::PublicationRecoveryConflict)
        ));
        assert_eq!(fs::read(&quarantine).unwrap(), b"external");
        assert!(
            !repository
                .runtime_paths()
                .root()
                .join("publication-cleanup")
                .read_dir()
                .unwrap()
                .any(|entry| entry.is_ok())
        );
    }

    #[test]
    fn journaled_materialization_recovers_a_persisted_quarantine_phase() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let head = repository.head().unwrap().unwrap();
        let tree = repository
            .index_generation()
            .unwrap()
            .tree()
            .unwrap()
            .clone();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head,
            GitCommitRef("e".repeat(40)),
            tree,
            [24; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                Some(b"captured".to_vec()),
                Some(b"published".to_vec()),
            )],
        )
        .unwrap();
        let quarantine = root
            .path()
            .join("generated")
            .join(journal.quarantine_name(0).unwrap());
        fs::create_dir_all(quarantine.parent().unwrap()).unwrap();
        fs::write(&quarantine, b"captured").unwrap();
        journal
            .set_materialization(0, PublicationMaterializationPhase::Quarantined)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        let mut recovered = repository.read_publication_journal().unwrap().unwrap();

        repository
            .materialize_publication_entry(&mut recovered, 0)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"published"
        );
        assert!(!quarantine.exists());
        assert_eq!(
            recovered.entries()[0].materialization().phase(),
            PublicationMaterializationPhase::Clean
        );
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .entries()[0]
                .materialization()
                .phase(),
            PublicationMaterializationPhase::Clean
        );
    }

    #[test]
    fn replacement_preserves_editor_race_after_final_check() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let path = super::ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let mut editor_race = || {
            fs::write(&target, b"editor").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&mut editor_race),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        assert_eq!(
            fs::read_dir(target.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".orna-quarantine-")
                })
                .count(),
            0
        );
        repository
            .materialize_managed_file(&path, Some(b"editor"), Some(b"second"))
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn quarantine_restore_preserves_newer_editor_race_after_absence_check() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let parent = root.path().join("generated");
        fs::create_dir_all(&parent).unwrap();
        let target = parent.join("row.orna");
        let quarantine = parent.join(".orna-quarantine-test");
        fs::write(&quarantine, b"captured editor").unwrap();
        let mut newer_editor = || {
            fs::write(&target, b"newer editor").unwrap();
        };

        assert!(matches!(
            repository.restore_managed_quarantine_impl(
                &quarantine,
                &target,
                Some(&mut newer_editor),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"newer editor");
        assert_eq!(fs::read(&quarantine).unwrap(), b"captured editor");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn quarantine_replacement_race_preserves_both_files() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target
            .parent()
            .unwrap()
            .join(".orna-quarantine-replacement-race");
        let mut replacement = || {
            fs::write(&quarantine, b"unexpected replacement").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&quarantine),
                Some(&mut replacement),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(fs::read(&quarantine).unwrap(), b"unexpected replacement");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn quarantine_deletion_race_preserves_both_files() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target
            .parent()
            .unwrap()
            .join(".orna-quarantine-deletion-race");
        let mut deletion = || {
            fs::write(&quarantine, b"unexpected deletion race").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                None,
                Some(&quarantine),
                Some(&mut deletion),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(fs::read(&quarantine).unwrap(), b"unexpected deletion race");
    }

    #[test]
    fn fresh_materialization_invokes_before_install_once_at_absence_boundary() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let target = root.path().join(path.as_path());
        let mut calls = 0;
        let mut before_install = || {
            calls += 1;
            assert!(!target.exists());
            fs::write(&target, b"external editor").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl(
                &path,
                None,
                Some(b"generated"),
                Some(&mut before_install),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(calls, 1);
        assert_eq!(fs::read(&target).unwrap(), b"external editor");
    }

    #[test]
    fn deletion_revalidates_editor_race_and_retries_idempotently() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let path = super::ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let head_before = repository.head().unwrap();
        let index_before = repository.index_generation().unwrap();
        let target = root.path().join(path.as_path());
        let mut editor_race = || {
            fs::write(&target, b"editor").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl(
                &path,
                Some(b"first"),
                None,
                Some(&mut editor_race),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        assert_eq!(repository.head().unwrap(), head_before);
        assert_eq!(repository.index_generation().unwrap(), index_before);

        repository
            .materialize_managed_file(&path, Some(b"editor"), None)
            .unwrap();
        repository
            .materialize_managed_file(&path, None, None)
            .unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn reviewed_candidate_publication_recovers_and_clears_its_journal() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let staged = b"module main;\n// reviewed\n";
        fs::write(root.path().join("main.orna"), staged).unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([4; 16]),
                super::Uuid::from_bytes([5; 16]),
                11,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(12),
                vec![0x81, 0x00],
            )
            .unwrap();
        let head = review.cwd.head().unwrap().clone();
        let path = ManagedPath::new("main.orna").unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(path.clone(), Some(staged.to_vec()))],
                "reviewed",
            )
            .unwrap();
        assert_eq!(candidate.tree, review.index_tree);
        let intent = [6; 16];
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head,
            candidate.commit().clone(),
            review.index_tree.clone(),
            intent,
            vec![PublicationJournalEntry::new(
                path,
                Some(staged.to_vec()),
                Some(staged.to_vec()),
            )],
        )
        .unwrap();
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            repository
                .publish_candidate_impl_with_migration(
                    &review.cwd.index,
                    &candidate,
                    &mut journal,
                    false,
                    Some(&review),
                    |stage| {
                        if stage == PublicationJournalStage::RefAdvanced {
                            panic!("simulated interruption")
                        }
                    },
                )
                .unwrap();
        }));
        assert!(interrupted.is_err());
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::RefAdvanced
        );
        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        let mut recovered = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(
            recovered.stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(recovered.runtime_intent_id(), Some(intent));
        assert!(recovered.compact_manifest().is_none());
        repository
            .mark_runtime_complete(intent, &mut recovered)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(repository.head().unwrap(), Some(candidate.commit().clone()));
    }

    fn overwrite_keeping_mtime(path: &std::path::Path, bytes: &[u8]) {
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        fs::write(path, bytes).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn reviewed_candidate_detects_same_size_same_mtime_edit_before_ref_advance() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let staged = b"module main;\n// reviewed\n";
        fs::write(root.path().join("main.orna"), staged).unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([40; 16]),
                super::Uuid::from_bytes([41; 16]),
                21,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(22),
                vec![0x81, 0x00],
            )
            .unwrap();
        let head_before = repository.head().unwrap();

        // Same length, restored mtime: only a content compare can see this edit.
        overwrite_keeping_mtime(
            &root.path().join("main.orna"),
            b"module main;\n// edited!\n",
        );
        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions),
            Err(RepositoryError::StaleReviewedCandidate)
        ));
        assert_eq!(
            repository.head().unwrap(),
            head_before,
            "a stale review must not advance the ref"
        );
        assert_eq!(git(root.path(), &["for-each-ref", "refs/orna/"]), "");
    }

    #[test]
    fn reviewed_candidate_detects_same_size_same_mtime_edit_after_ref_advance() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let staged = b"module main;\n// reviewed\n";
        fs::write(root.path().join("main.orna"), staged).unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([42; 16]),
                super::Uuid::from_bytes([43; 16]),
                23,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(24),
                vec![0x81, 0x00],
            )
            .unwrap();
        let head = review.cwd.head().unwrap().clone();
        let path = ManagedPath::new("main.orna").unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(path.clone(), Some(staged.to_vec()))],
                "reviewed",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head,
            candidate.commit().clone(),
            review.index_tree.clone(),
            [44; 16],
            vec![PublicationJournalEntry::new(
                path,
                Some(staged.to_vec()),
                Some(staged.to_vec()),
            )],
        )
        .unwrap();
        let root_path = root.path().join("main.orna");
        let result = repository.publish_candidate_impl_with_migration(
            &review.cwd.index,
            &candidate,
            &mut journal,
            false,
            Some(&review),
            |stage| {
                if stage == PublicationJournalStage::RefAdvanced {
                    overwrite_keeping_mtime(&root_path, b"module main;\n// edited!\n");
                }
            },
        );
        // The edit lands after RefAdvanced is durable. Reconciliation must refuse to
        // materialise over bytes that match neither the batch nor the expected state
        // (ORNA-PUB-007: recovery is required, the publication is not reported as complete).
        assert!(
            matches!(result, Err(RepositoryError::ManagedContentConflict)),
            "post-advance same-size edit must fail with ManagedContentConflict, got {result:?}"
        );
        // ORNA-PUB-005: the ref names the candidate because it advanced before the edit.
        assert!(
            repository.head().unwrap().as_ref() == Some(journal.new_head()),
            "ref must stay advanced"
        );
        // The journal is durable at IndexReconciled: the edit hook runs at RefAdvanced
        // (lib.rs:4796), index reconciliation then succeeds and IndexReconciled is
        // persisted (lib.rs:4807-4809), and the managed-file conflict is raised while
        // materialising (lib.rs:4811). Recovery resumes from IndexReconciled.
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::IndexReconciled
        );
        // The user's edited bytes are retained, not overwritten by the batch.
        assert_eq!(
            fs::read(root.path().join("main.orna")).unwrap(),
            b"module main;\n// edited!\n"
        );
    }

    #[test]
    fn reviewed_candidate_commit_preserves_unstaged_work_and_replays_result() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(
            root.path().join(".orna/database.orna"),
            include_str!("../tests/fixtures/format-context/database-final.orna"),
        )
        .unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let staged = b"module main;\n// admitted\n";
        fs::write(root.path().join("main.orna"), staged).unwrap();
        git(root.path(), &["add", "main.orna"]);
        fs::write(root.path().join("unrelated.txt"), b"unstaged").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([10; 16]),
                super::Uuid::from_bytes([11; 16]),
                3,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(4),
                vec![0x81, 0x00],
            )
            .unwrap();
        let committed = repository
            .commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions)
            .unwrap();
        assert_eq!(repository.head().unwrap(), Some(committed.clone()));
        assert_eq!(
            repository.index_generation().unwrap().tree(),
            Some(&review.index_tree)
        );
        assert_eq!(
            repository
                .read_committed_file(&committed, "main.orna", 1 << 20)
                .unwrap(),
            staged
        );
        assert_eq!(
            fs::read(root.path().join("unrelated.txt")).unwrap(),
            b"unstaged"
        );
        assert_eq!(
            git(root.path(), &["for-each-ref", "refs/orna/"]),
            "",
            "a completed reviewed commit leaves no private candidate ref behind"
        );
        assert!(!git(root.path(), &["status", "--porcelain"]).is_empty());
        git(
            root.path(),
            &["commit", "--allow-empty", "-m", "later unrelated commit"],
        );
        assert_ne!(repository.head().unwrap(), Some(committed.clone()));
        let replay = repository
            .commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions)
            .unwrap();
        assert_eq!(replay, committed);
    }

    #[test]
    fn reviewed_candidate_is_validated_before_any_commit_object_exists() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(
            root.path().join(".orna/database.orna"),
            include_str!("../tests/fixtures/format-context/database-final.orna"),
        )
        .unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        // The staged candidate drops the store root every format-3 snapshot has.
        git(
            root.path(),
            &["rm", "-r", "--cached", "--quiet", ".orna/store"],
        );
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([50; 16]),
                super::Uuid::from_bytes([51; 16]),
                2,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(3),
                vec![0x81, 0x00],
            )
            .unwrap();
        let objects = || {
            git(
                root.path(),
                &[
                    "cat-file",
                    "--batch-all-objects",
                    "--batch-check=%(objectname)",
                ],
            )
        };
        let objects_before = objects();
        let head_before = repository.head().unwrap();

        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "", &AcceptAllAssertions),
            Err(RepositoryError::InvalidCommitMessage)
        ));
        assert!(matches!(
            repository.commit_reviewed_candidate(
                &review,
                "drop the store root",
                &AcceptAllAssertions
            ),
            Err(RepositoryError::InvalidCommitCandidate)
        ));
        assert_eq!(
            objects(),
            objects_before,
            "a refused candidate creates no Git object"
        );
        assert_eq!(git(root.path(), &["for-each-ref", "refs/orna/"]), "");
        assert_eq!(repository.head().unwrap(), head_before);
    }

    #[test]
    fn reviewed_candidate_replay_requires_the_original_message() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(
            root.path().join(".orna/database.orna"),
            include_str!("../tests/fixtures/format-context/database-final.orna"),
        )
        .unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        fs::write(root.path().join("main.orna"), "module main;\n// admitted\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([10; 16]),
                super::Uuid::from_bytes([11; 16]),
                3,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(4),
                vec![0x81, 0x00],
            )
            .unwrap();
        let committed = repository
            .commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions)
            .unwrap();

        let replay = repository
            .commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions)
            .unwrap();
        assert_eq!(replay, committed);
        assert!(matches!(
            repository.commit_reviewed_candidate(
                &review,
                "corrected message",
                &AcceptAllAssertions
            ),
            Err(RepositoryError::StaleReviewedCandidate)
        ));
    }
    #[test]
    fn reviewed_candidate_retry_recovers_after_publish_before_result_write() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(
            root.path().join(".orna/database.orna"),
            include_str!("../tests/fixtures/format-context/database-final.orna"),
        )
        .unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        fs::write(root.path().join("main.orna"), "module main;\n// admitted\n").unwrap();
        git(root.path(), &["add", "main.orna"]);

        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([40; 16]),
                super::Uuid::from_bytes([41; 16]),
                12,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(13),
                vec![0x81, 0x00],
            )
            .unwrap();
        let intent = super::reviewed_candidate_runtime_intent_id(&review, "reviewed");
        let old_head = repository.head().unwrap();
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            repository
                .commit_reviewed_candidate_with_stage_hook(
                    &review,
                    "reviewed",
                    &AcceptAllAssertions,
                    |stage| {
                        if stage == PublicationJournalStage::WorktreeReconciled {
                            panic!("simulated crash before reviewed result recording");
                        }
                    },
                )
                .unwrap();
        }));
        assert!(crashed.is_err());
        let published = repository.head().unwrap().unwrap();
        assert_ne!(Some(published.clone()), old_head);
        let journal = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(journal.stage(), PublicationJournalStage::WorktreeReconciled);
        assert_eq!(journal.runtime_intent_id(), Some(intent));
        let result_path = repository.runtime.root().join(format!(
            "reviewed-result-{}",
            intent
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        assert!(!result_path.exists());

        let retry_review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([40; 16]),
                super::Uuid::from_bytes([41; 16]),
                12,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(13),
                vec![0x81, 0x00],
            )
            .unwrap();
        assert!(retry_review.candidate_paths.is_empty());
        assert_eq!(
            super::reviewed_candidate_runtime_intent_id(&retry_review, "reviewed"),
            intent
        );
        let retried = repository
            .commit_reviewed_candidate(&retry_review, "reviewed", &AcceptAllAssertions)
            .unwrap();
        assert_eq!(retried, published);
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert!(result_path.exists());
    }
    #[test]
    fn reviewed_candidate_retries_after_index_lock_contention() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(
            root.path().join(".orna/database.orna"),
            include_str!("../tests/fixtures/format-context/database-final.orna"),
        )
        .unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        fs::write(
            root.path().join("main.orna"),
            "module main;\n// lock retry\n",
        )
        .unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([20; 16]),
                super::Uuid::from_bytes([21; 16]),
                5,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(6),
                vec![0x81, 0x00],
            )
            .unwrap();
        let index = repository.git_path("index").unwrap();
        let lock = index.with_extension("lock");
        fs::write(&lock, b"occupied").unwrap();
        assert!(
            repository
                .commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions)
                .is_err()
        );
        fs::remove_file(&lock).unwrap();
        assert!(
            repository
                .commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions)
                .is_ok()
        );
        assert!(repository.read_publication_journal().unwrap().is_none());
    }

    #[test]
    fn reviewed_candidate_empty_commit_returns_nothing_to_commit() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([30; 16]),
                super::Uuid::from_bytes([31; 16]),
                8,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(9),
                vec![0x81, 0x00],
            )
            .unwrap();
        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions),
            Err(RepositoryError::SysGitNothingToCommit)
        ));
    }

    #[test]
    fn reviewed_candidate_rejects_forged_and_stale_index_reviews() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());
        fs::write(root.path().join("main.orna"), "module main;\n// staged\n").unwrap();
        git(root.path(), &["add", "main.orna"]);

        let repository = Repository::discover(root.path()).unwrap();
        let head_before = repository.head().unwrap();
        let index_before = repository.index_generation().unwrap();
        let worktree_before = repository.worktree_state().unwrap();
        let review = repository
            .review_staged_candidate(
                super::Uuid::from_bytes([1; 16]),
                super::Uuid::from_bytes([2; 16]),
                7,
                super::RepositoryFormat::Format3,
                RuntimeGeneration::new(9),
                vec![0x81, 0x00],
            )
            .unwrap();
        assert_eq!(repository.head().unwrap(), head_before);
        assert_eq!(repository.index_generation().unwrap(), index_before);
        assert_eq!(repository.worktree_state().unwrap(), worktree_before);
        assert_eq!(review.index_tree, index_before.tree().unwrap().clone());
        assert_ne!(review.token(), &[0; 32]);

        let mut forged = review.clone();
        forged.pending_generation += 1;
        assert!(matches!(
            repository.commit_reviewed_candidate(&forged, "reviewed", &AcceptAllAssertions),
            Err(RepositoryError::StaleReviewedCandidate)
        ));

        fs::write(
            root.path().join("main.orna"),
            "module main;\n// newer staged\n",
        )
        .unwrap();
        git(root.path(), &["add", "main.orna"]);
        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions),
            Err(RepositoryError::StaleReviewedCandidate)
        ));
        assert_eq!(repository.head().unwrap(), head_before);
    }

    /// ORNA-PUB-010: an unchanged `.orna/store/data` node leaf that was edited in the worktree
    /// is an external change and must refuse the reconcile rather than be left in place.
    #[test]
    fn reconcile_refuses_edited_unchanged_store_data_leaf() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"committed\n").unwrap();
        git(root.path(), &["add", ".orna/store/data"]);
        git(root.path(), &["commit", "-m", "base"]);

        let repository = Repository::discover(root.path()).unwrap();
        let expected = repository.index_generation().unwrap();
        let candidate = super::PrivateCommit {
            tree: expected.tree().unwrap().clone(),
            commit: expected.head().unwrap().clone(),
        };
        fs::write(root.path().join(".orna/store/data"), b"edited\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(
            fs::read(root.path().join(".orna/store/data")).unwrap(),
            b"edited\n"
        );
    }

    #[test]
    fn deletion_preserves_editor_race_after_final_check() {
        let root = tempfile::TempDir::new().unwrap();
        crate::test_support::initialize_fixture_repository(root.path());
        crate::test_support::commit_initial_module(root.path());

        let repository = Repository::discover(root.path()).unwrap();
        let path = super::ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let mut calls = 0;
        let mut editor_race = || {
            calls += 1;
            if calls == 2 {
                fs::write(&target, b"editor").unwrap();
            }
        };

        assert!(matches!(
            repository.materialize_managed_file_impl(
                &path,
                Some(b"first"),
                None,
                Some(&mut editor_race),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(calls, 2);
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        repository
            .materialize_managed_file(&path, Some(b"editor"), None)
            .unwrap();
        assert!(!target.exists());
    }

    /// A journal entry is `Clean` with no quarantine or past `Clean` with one;
    /// the other pairings are not states the recovery protocol can be in.
    #[test]
    fn materialization_phase_and_quarantine_must_agree() {
        use super::PublicationMaterialization;
        use PublicationMaterializationPhase::{Clean, Installed, QuarantinePrepared, Quarantined};

        assert_eq!(
            PublicationMaterialization::from_parts(Clean, None).unwrap(),
            PublicationMaterialization::Clean
        );
        assert!(matches!(
            PublicationMaterialization::from_parts(Clean, Some("q".to_owned())),
            Err(RepositoryError::InvalidPublicationJournal)
        ));
        for phase in [QuarantinePrepared, Quarantined, Installed] {
            assert!(matches!(
                PublicationMaterialization::from_parts(phase, None),
                Err(RepositoryError::InvalidPublicationJournal)
            ));
            let state =
                PublicationMaterialization::from_parts(phase, Some("q".to_owned())).unwrap();
            assert_eq!(state.phase(), phase);
            assert_eq!(state.quarantine(), Some("q"));
        }
    }
}

#[cfg(test)]
mod git_binding_tests {
    use super::bind_git_discovery_to_worktree;
    use std::process::Command;

    #[test]
    fn discovery_bind_makes_a_relative_ceiling_absolute() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        std::fs::create_dir_all(child.join(".git")).unwrap();
        let mut command = Command::new("git");
        bind_git_discovery_to_worktree(&mut command, &child);
        let ceiling = command
            .get_envs()
            .find(|(name, _)| *name == "GIT_CEILING_DIRECTORIES")
            .and_then(|(_, value)| value)
            .map(std::path::PathBuf::from)
            .expect("discovery ceiling is bound");
        assert!(ceiling.is_absolute());
        assert_eq!(ceiling, root.path().canonicalize().unwrap());
    }

    #[test]
    fn discovery_bind_leaves_a_plain_subdirectory_free_to_discover_its_repository() {
        let root = tempfile::tempdir().unwrap();
        let plain = root.path().join(".orna");
        std::fs::create_dir_all(&plain).unwrap();
        let mut command = Command::new("git");
        bind_git_discovery_to_worktree(&mut command, &plain);
        assert!(
            command
                .get_envs()
                .all(|(name, _)| name != "GIT_CEILING_DIRECTORIES")
        );
    }

    /// A relative worktree path must still bind to an absolute ceiling. The root
    /// is created inside the process directory so its path can be given relative.
    #[test]
    fn discovery_bind_absolutizes_a_relative_worktree_path() {
        let cwd = std::env::current_dir().unwrap();
        let root = tempfile::Builder::new().tempdir_in(&cwd).unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        let relative = std::path::PathBuf::from(root.path().file_name().unwrap());
        let mut command = Command::new("git");
        bind_git_discovery_to_worktree(&mut command, &relative);
        let ceiling = command
            .get_envs()
            .find(|(name, _)| *name == "GIT_CEILING_DIRECTORIES")
            .and_then(|(_, value)| value)
            .map(std::path::PathBuf::from)
            .expect("discovery ceiling is bound");
        assert!(ceiling.is_absolute());
        assert_eq!(ceiling, cwd.canonicalize().unwrap());
    }

    /// The unconditional ceiling binds a plain directory's parent, with no `.git`.
    #[test]
    fn unconditional_ceiling_binds_a_plain_directory_parent() {
        let root = tempfile::tempdir().unwrap();
        let plain = root.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let mut command = Command::new("git");
        super::bind_git_ceiling_above(&mut command, &plain);
        let ceiling = command
            .get_envs()
            .find(|(name, _)| *name == "GIT_CEILING_DIRECTORIES")
            .and_then(|(_, value)| value)
            .map(std::path::PathBuf::from)
            .expect("discovery ceiling is bound");
        assert_eq!(ceiling, root.path().canonicalize().unwrap());
    }
}
