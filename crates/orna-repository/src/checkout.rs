//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;
use crate::wire::{
    decode_checkout_cwd, decode_optional_checkout_cwd, encode_checkout_cwd,
    encode_optional_checkout_cwd,
};

/// Version marker of the force-discard recovery journal.
pub(crate) const CHECKOUT_JOURNAL_MAGIC: &[u8] = b"ORNA-CHECKOUT-JOURNAL\0";

/// A generation supplied by the embedded runtime state layer.
///
/// The repository layer does not open `state.db`: doing so would conflate the
/// Git/index boundary with the single-writer runtime ownership boundary.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeGeneration(u64);

impl RuntimeGeneration {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Git's current index generation.  Both fields are retained so a `HEAD`
/// change is observable even when the index tree happens to be identical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexGeneration {
    pub(crate) head: Option<GitCommitRef>,
    pub(crate) tree: Option<IndexTreeRef>,
}

impl IndexGeneration {
    pub fn head(&self) -> Option<&GitCommitRef> {
        self.head.as_ref()
    }
    pub fn tree(&self) -> Option<&IndexTreeRef> {
        self.tree.as_ref()
    }
}

/// A lossless, deterministic representation of Git's ordinary worktree
/// status.  It deliberately is not a content hash; callers can compare it but
/// must not treat it as a cryptographic integrity assertion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeState(pub(crate) Vec<u8>, pub(crate) [u8; 32]);

/// Stat identity of one managed worktree path, captured before its bytes are read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedFileStamp {
    pub(crate) file_type: fs::FileType,
    pub(crate) len: u64,
    pub(crate) modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    pub(crate) change: (u64, u64, i64, i64, u32),
}

/// Stamp of one managed path; `None` records that the path is absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedStamp {
    pub(crate) path: ManagedPath,
    pub(crate) stamp: Option<ManagedFileStamp>,
}

/// Worktree status and content digest, plus the stamps of every digested path.
pub(crate) struct WorktreeScan {
    pub(crate) state: WorktreeState,
    pub(crate) stamps: Vec<ManagedStamp>,
}

/// The digest of one regular file's bytes, valid while its stamp is unchanged.
#[derive(Clone, Debug)]
pub(crate) struct CachedFileDigest {
    pub(crate) stamp: ManagedFileStamp,
    pub(crate) digest: [u8; 32],
}

/// One scanned worktree path: the stamp to record, and the cache entry to
/// carry forward when the path is a regular file whose section was reused.
pub(crate) struct ScannedWorktreePath {
    pub(crate) stamp: ManagedStamp,
    pub(crate) cache_entry: Option<(Vec<u8>, CachedFileDigest)>,
}

/// How long before a read a file's mtime and ctime must lie for its stamp to
/// prove its bytes unchanged. Filesystems store timestamps coarsely, so a write
/// in the same tick as an earlier read can leave the stamp equal.
pub(crate) const CONTENT_RACY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(test)]
thread_local! {
    /// Regular files whose bytes a worktree observation read on this thread.
    pub(crate) static CONTENT_READS: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    /// A race an install test queues for the candidate-recheck window: after the
    /// candidate is written and before the target is rechecked (ORNA-PUB-010).
    pub(crate) static MANAGED_CANDIDATE_RECHECK_RACE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn managed_candidate_recheck_race() {
    let race = MANAGED_CANDIDATE_RECHECK_RACE.with(|slot| slot.borrow_mut().take());
    if let Some(race) = race {
        race();
    }
}

/// Queues one race for the candidate-recheck window. Dropping the guard clears the
/// slot, including on unwind, so a test that panics cannot leak its race into a later
/// install (ORNA-PUB-010).
#[cfg(test)]
pub(crate) struct ManagedCandidateRecheckRace;

#[cfg(test)]
impl ManagedCandidateRecheckRace {
    pub(crate) fn queue(race: impl FnOnce() + 'static) -> Self {
        MANAGED_CANDIDATE_RECHECK_RACE.with(|slot| *slot.borrow_mut() = Some(Box::new(race)));
        Self
    }
}

#[cfg(test)]
impl Drop for ManagedCandidateRecheckRace {
    fn drop(&mut self) {
        MANAGED_CANDIDATE_RECHECK_RACE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(not(test))]
pub(crate) fn managed_candidate_recheck_race() {}

impl WorktreeState {
    pub fn as_porcelain_v2_z(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn content_digest(&self) -> &[u8; 32] {
        &self.1
    }

    pub fn is_clean(&self) -> bool {
        self.0.is_empty()
    }
}

/// The four independently observable parts of an Orna CWD generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwdGeneration {
    pub(crate) head: Option<GitCommitRef>,
    pub(crate) branch: Option<String>,
    pub(crate) index: IndexGeneration,
    pub(crate) worktree: WorktreeState,
    pub(crate) runtime: RuntimeGeneration,
}

impl CwdGeneration {
    pub fn head(&self) -> Option<&GitCommitRef> {
        self.head.as_ref()
    }
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
    pub fn index(&self) -> &IndexGeneration {
        &self.index
    }
    pub fn worktree(&self) -> &WorktreeState {
        &self.worktree
    }
    pub const fn runtime(&self) -> RuntimeGeneration {
        self.runtime
    }
}

/// The immutable target selected by a checkout preview. A local branch keeps
/// its attachment; every other accepted selector is a detached commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckoutTarget {
    Branch { name: String, commit: GitCommitRef },
    Detached { commit: GitCommitRef },
}

impl CheckoutTarget {
    pub fn commit(&self) -> &GitCommitRef {
        match self {
            Self::Branch { commit, .. } | Self::Detached { commit } => commit,
        }
    }

    pub fn branch_name(&self) -> Option<&str> {
        match self {
            Self::Branch { name, .. } => Some(name),
            Self::Detached { .. } => None,
        }
    }
}

/// Checkout preconditions. The bounded same-commit operation can consume this
/// plan, but forceful/divergent checkout, logical validation, and durable
/// recovery remain separate boundaries owned by higher repository layers.
/// Capturing it may initialize private coordination metadata, but never
/// changes visible Git state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutPreflight {
    pub(crate) target: CheckoutTarget,
    pub(crate) expected_head: Option<GitCommitRef>,
    pub(crate) cwd: CwdGeneration,
    pub(crate) git: CheckoutGitSubplan,
    pub(crate) dirty_content_digest: [u8; 32],
}

/// The read-only Git portion of a checkout plan. Paths are repository-relative
/// and classified from the current worktree and target commit without
/// changing the ref, index, or worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutGitSubplan {
    pub(crate) affected_paths: Vec<ManagedPath>,
    pub(crate) conflicting_paths: Vec<ManagedPath>,
    pub(crate) discardable_paths: Vec<ManagedPath>,
}

impl CheckoutGitSubplan {
    pub fn affected_paths(&self) -> &[ManagedPath] {
        &self.affected_paths
    }

    pub fn conflicting_paths(&self) -> &[ManagedPath] {
        &self.conflicting_paths
    }

    pub fn discardable_paths(&self) -> &[ManagedPath] {
        &self.discardable_paths
    }
}

/// A domain-separated authorization witness for one exact checkout
/// preflight. It is opaque so callers cannot assemble a force token without
/// first observing the repository state it authorizes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CheckoutPlanToken(pub(crate) [u8; 32]);

impl CheckoutPlanToken {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An opaque admission capability for one exact force-discard checkout.
///
/// The capability can only be produced after the locked preflight, force
/// witness, logical validation, and canonical discard set all agree. Its
/// private contents prevent callers from replacing any of those bindings
/// before a future journaled executor consumes the capability.
#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedCheckoutDiscard {
    pub(crate) preflight: CheckoutPreflight,
    pub(crate) force_token: CheckoutPlanToken,
    pub(crate) discard_paths: Vec<ManagedPath>,
}

/// A durable, opaque record that fences recovery of a force-discard checkout.
///
/// It is written before an eventual destructive executor crosses a Git-visible
/// boundary. Until that executor exists, recovery can only prove that the
/// original preflight is still current and discard the no-mutation intent; it
/// must never infer permission to resume a discard after state drift.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckoutRecoveryJournal {
    pub(crate) target: CheckoutTarget,
    pub(crate) runtime: RuntimeGeneration,
    pub(crate) force_token: CheckoutPlanToken,
    pub(crate) discard_paths: Vec<ManagedPath>,
    pub(crate) state: CheckoutRecoveryState,
}

/// Durable Git-visible boundaries of one force-discard transition, each with
/// the CWD generations observed up to it.
///
/// Each state is written only after the preceding Git operation has reached a
/// fully observable CWD generation. Recovery therefore either recognizes an
/// exact recorded generation or stops without applying a broad reset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckoutRecoveryState {
    /// A version-1 record: the intent without any CWD generation. It cannot be
    /// written any more, and recovery keeps its historical no-mutation
    /// behavior.
    PreparedWithoutGeneration,
    Prepared {
        before: CwdGeneration,
    },
    Discarded {
        before: CwdGeneration,
        discarded: CwdGeneration,
    },
    Applied {
        before: CwdGeneration,
        discarded: CwdGeneration,
        after: CwdGeneration,
    },
}

impl CheckoutRecoveryState {
    /// Ensures every journalled generation belongs to the transition it
    /// claims to record. Recovery must not treat independently decodable CWD
    /// snapshots as interchangeable: the discard boundary retains the old
    /// attachment, while the applied boundary must carry the selected target
    /// attachment. This is deliberately checked before recovery can clear a
    /// journal or invoke Git.
    fn validate(&self, target: &CheckoutTarget) -> Result<(), RepositoryError> {
        let valid = match self {
            Self::PreparedWithoutGeneration | Self::Prepared { .. } => true,
            Self::Discarded { before, discarded } => {
                discarded.head == before.head && discarded.branch == before.branch
            }
            Self::Applied {
                before,
                discarded,
                after,
            } => {
                discarded.head == before.head
                    && discarded.branch == before.branch
                    && after.head.as_ref() == Some(target.commit())
                    && after.branch.as_deref() == target.branch_name()
            }
        };
        if valid {
            Ok(())
        } else {
            Err(RepositoryError::InvalidCheckoutJournal)
        }
    }
}

/// What restart recovery does with a retained force-discard record once it
/// has compared the record with the current CWD.
pub(crate) enum CheckoutRecoveryStep {
    /// A version-1 record: prove the intent is still current and release it.
    ReleaseLegacyIntent,
    /// Nothing was switched and the CWD is the recorded one: prove the
    /// original preflight still holds and release the record.
    ReleasePrepared,
    /// The recorded transition finished and the CWD is the recorded result.
    ReleaseApplied,
    /// The scoped discard is recorded and current: select the target.
    SelectTarget,
    /// Git selected the target but the process stopped before journalling the
    /// result; the observed generation proves it is the recorded transition.
    AdoptSelection(CwdGeneration),
}

impl CheckoutRecoveryJournal {
    pub(crate) fn from_validated(discard: &ValidatedCheckoutDiscard) -> Self {
        Self {
            target: discard.preflight.target.clone(),
            runtime: discard.preflight.cwd.runtime,
            force_token: discard.force_token,
            discard_paths: discard.discard_paths.clone(),
            state: CheckoutRecoveryState::Prepared {
                before: discard.preflight.cwd.clone(),
            },
        }
    }

    /// The CWD recorded before the discard, absent only in a version-1 record.
    fn before(&self) -> Option<&CwdGeneration> {
        match &self.state {
            CheckoutRecoveryState::PreparedWithoutGeneration => None,
            CheckoutRecoveryState::Prepared { before }
            | CheckoutRecoveryState::Discarded { before, .. }
            | CheckoutRecoveryState::Applied { before, .. } => Some(before),
        }
    }

    pub(crate) fn matches_validated(&self, discard: &ValidatedCheckoutDiscard) -> bool {
        self.target == discard.preflight.target
            && self.runtime == discard.preflight.cwd.runtime
            && self.force_token == discard.force_token
            && self.discard_paths == discard.discard_paths
            && self.before() == Some(&discard.preflight.cwd)
    }

    pub(crate) fn record_discarded(
        &mut self,
        discarded: CwdGeneration,
    ) -> Result<(), RepositoryError> {
        let CheckoutRecoveryState::Prepared { before } = &self.state else {
            return Err(RepositoryError::InvalidCheckoutJournal);
        };
        let next = CheckoutRecoveryState::Discarded {
            before: before.clone(),
            discarded,
        };
        next.validate(&self.target)?;
        self.state = next;
        Ok(())
    }

    pub(crate) fn record_applied(&mut self, after: CwdGeneration) -> Result<(), RepositoryError> {
        let CheckoutRecoveryState::Discarded { before, discarded } = &self.state else {
            return Err(RepositoryError::InvalidCheckoutJournal);
        };
        let next = CheckoutRecoveryState::Applied {
            before: before.clone(),
            discarded: discarded.clone(),
            after,
        };
        next.validate(&self.target)?;
        self.state = next;
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, RepositoryError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(CHECKOUT_JOURNAL_MAGIC);
        bytes.push(3);
        bytes.extend_from_slice(&self.runtime.get().to_be_bytes());
        match &self.target {
            CheckoutTarget::Branch { name, commit } => {
                bytes.push(1);
                put_string(&mut bytes, name)?;
                put_string(&mut bytes, commit.as_str())?;
            }
            CheckoutTarget::Detached { commit } => {
                bytes.push(2);
                put_string(&mut bytes, commit.as_str())?;
            }
        }
        bytes.extend_from_slice(self.force_token.as_bytes());
        put_u32(&mut bytes, self.discard_paths.len())?;
        for path in &self.discard_paths {
            put_string(
                &mut bytes,
                path.as_path()
                    .to_str()
                    .ok_or(RepositoryError::InvalidCheckoutJournal)?,
            )?;
        }
        let (phase, before, discarded, after) = match &self.state {
            CheckoutRecoveryState::PreparedWithoutGeneration => {
                return Err(RepositoryError::InvalidCheckoutJournal);
            }
            CheckoutRecoveryState::Prepared { before } => (1, before, None, None),
            CheckoutRecoveryState::Discarded { before, discarded } => {
                (2, before, Some(discarded), None)
            }
            CheckoutRecoveryState::Applied {
                before,
                discarded,
                after,
            } => (3, before, Some(discarded), Some(after)),
        };
        bytes.push(phase);
        encode_checkout_cwd(&mut bytes, before)?;
        encode_optional_checkout_cwd(&mut bytes, discarded)?;
        encode_optional_checkout_cwd(&mut bytes, after)?;
        self.state.validate(&self.target)?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8], object_id_length: usize) -> Result<Self, RepositoryError> {
        if bytes.len() > MAX_JOURNAL_BYTES || !bytes.starts_with(CHECKOUT_JOURNAL_MAGIC) {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let mut cursor = CHECKOUT_JOURNAL_MAGIC.len();
        let version = take_byte(bytes, &mut cursor)?;
        if version != 1 && version != 2 && version != 3 {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let runtime = RuntimeGeneration::new(u64::from_be_bytes(take_fixed_array::<8>(
            bytes,
            &mut cursor,
        )?));
        let target = match take_byte(bytes, &mut cursor)? {
            1 => CheckoutTarget::Branch {
                name: take_string(bytes, &mut cursor)?,
                commit: GitCommitRef::from_verified_commit(
                    take_string(bytes, &mut cursor)?,
                    object_id_length,
                )?,
            },
            2 => CheckoutTarget::Detached {
                commit: GitCommitRef::from_verified_commit(
                    take_string(bytes, &mut cursor)?,
                    object_id_length,
                )?,
            },
            _ => return Err(RepositoryError::InvalidCheckoutJournal),
        };
        let force_token = CheckoutPlanToken(take_fixed_array::<32>(bytes, &mut cursor)?);
        let count = usize::try_from(take_u32(bytes, &mut cursor)?)
            .map_err(|_| RepositoryError::InvalidCheckoutJournal)?;
        if count > bytes.len().saturating_sub(cursor) {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let mut discard_paths = Vec::with_capacity(count);
        for _ in 0..count {
            discard_paths.push(ManagedPath::new(take_string(bytes, &mut cursor)?)?);
        }
        if discard_paths.is_empty()
            || discard_paths
                .windows(2)
                .any(|paths| paths[0].as_path() >= paths[1].as_path())
        {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        if version == 1 {
            if cursor != bytes.len() {
                return Err(RepositoryError::InvalidCheckoutJournal);
            }
            return Ok(Self {
                target,
                runtime,
                force_token,
                discard_paths,
                state: CheckoutRecoveryState::PreparedWithoutGeneration,
            });
        }
        let phase = match take_byte(bytes, &mut cursor)? {
            phase @ 1..=3 => phase,
            _ => return Err(RepositoryError::InvalidCheckoutJournal),
        };
        let before = decode_checkout_cwd(bytes, &mut cursor, object_id_length, version >= 3)?;
        let discarded =
            decode_optional_checkout_cwd(bytes, &mut cursor, object_id_length, version >= 3)?;
        let after =
            decode_optional_checkout_cwd(bytes, &mut cursor, object_id_length, version >= 3)?;
        if cursor != bytes.len()
            || before.runtime != runtime
            || discarded.as_ref().is_some_and(|cwd| cwd.runtime != runtime)
            || after.as_ref().is_some_and(|cwd| cwd.runtime != runtime)
        {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let state = match (phase, discarded, after) {
            (1, None, None) => CheckoutRecoveryState::Prepared { before },
            (2, Some(discarded), None) => CheckoutRecoveryState::Discarded { before, discarded },
            (3, Some(discarded), Some(after)) => CheckoutRecoveryState::Applied {
                before,
                discarded,
                after,
            },
            _ => return Err(RepositoryError::InvalidCheckoutJournal),
        };
        state.validate(&target)?;
        Ok(Self {
            target,
            runtime,
            force_token,
            discard_paths,
            state,
        })
    }
}

impl CheckoutPreflight {
    pub fn target(&self) -> &CheckoutTarget {
        &self.target
    }

    pub fn expected_head(&self) -> Option<&GitCommitRef> {
        self.expected_head.as_ref()
    }

    pub fn cwd(&self) -> &CwdGeneration {
        &self.cwd
    }

    pub fn git(&self) -> &CheckoutGitSubplan {
        &self.git
    }

    /// Derives a stable authorization witness over the complete preflight,
    /// including target attachment kind, Git state, raw worktree status, and
    /// runtime generation.
    pub fn force_token(&self) -> CheckoutPlanToken {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ORNA-CHECKOUT-PLAN-TOKEN\0");
        bytes.push(2);
        match &self.target {
            CheckoutTarget::Branch { name, commit } => {
                bytes.push(1);
                token_string(&mut bytes, name);
                token_string(&mut bytes, commit.as_str());
            }
            CheckoutTarget::Detached { commit } => {
                bytes.push(2);
                token_string(&mut bytes, commit.as_str());
            }
        }
        token_optional_string(
            &mut bytes,
            self.expected_head.as_ref().map(GitCommitRef::as_str),
        );
        token_optional_string(&mut bytes, self.cwd.branch.as_deref());
        token_optional_string(
            &mut bytes,
            self.cwd.index.head.as_ref().map(GitCommitRef::as_str),
        );
        token_optional_string(
            &mut bytes,
            self.cwd.index.tree.as_ref().map(IndexTreeRef::as_str),
        );
        token_bytes(&mut bytes, self.cwd.worktree.as_porcelain_v2_z());
        token_bytes(&mut bytes, self.cwd.worktree.content_digest());
        token_bytes(&mut bytes, &self.dirty_content_digest);
        bytes.extend_from_slice(&self.cwd.runtime.get().to_be_bytes());
        token_paths(&mut bytes, &self.git.affected_paths);
        token_paths(&mut bytes, &self.git.conflicting_paths);
        token_paths(&mut bytes, &self.git.discardable_paths);
        CheckoutPlanToken(Sha256::digest(bytes).into())
    }

    /// Requires explicit force authorization for this exact, still-current
    /// preflight. A missing or stale witness never permits checkout mutation.
    pub fn authorize_force(
        &self,
        force: bool,
        token: Option<&CheckoutPlanToken>,
    ) -> Result<(), RepositoryError> {
        if force && token == Some(&self.force_token()) {
            Ok(())
        } else {
            Err(RepositoryError::CheckoutPlanStale)
        }
    }
}

pub(crate) fn token_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

pub(crate) fn token_string(output: &mut Vec<u8>, value: &str) {
    token_bytes(output, value.as_bytes());
}

pub(crate) fn token_optional_string(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            token_string(output, value);
        }
        None => output.push(0),
    }
}

pub(crate) fn token_paths(output: &mut Vec<u8>, paths: &[ManagedPath]) {
    output.extend_from_slice(&(paths.len() as u64).to_be_bytes());
    for path in paths {
        token_string(
            output,
            path.as_path()
                .to_str()
                .expect("validated checkout path is UTF-8"),
        );
    }
}

pub(crate) fn sorted_paths(paths: HashSet<ManagedPath>) -> Vec<ManagedPath> {
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| left.as_path().cmp(right.as_path()));
    paths
}

pub(crate) fn is_loose_path_components(components: &[Vec<u8>]) -> bool {
    components.len() >= 2
        && !components[0].starts_with(b".")
        && components
            .last()
            .is_some_and(|component| component.ends_with(b".orna"))
}

pub(crate) fn record_case_folded_path_components(
    prefixes: &mut BTreeMap<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>,
    components: &[Vec<u8>],
) {
    let mut prefix = Vec::new();
    for component in components {
        prefix.push(component.to_ascii_lowercase());
        prefixes
            .entry(prefix.clone())
            .or_default()
            .insert(component.clone());
    }
}

pub(crate) fn case_folded_path_collides(
    prefixes: &BTreeMap<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>,
    components: &[Vec<u8>],
) -> bool {
    let mut prefix = Vec::new();
    for component in components {
        prefix.push(component.to_ascii_lowercase());
        if prefixes
            .get(&prefix)
            .is_some_and(|siblings| siblings.iter().any(|sibling| sibling != component))
        {
            return true;
        }
    }
    false
}
