//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

mod migration;

use migration::validate_migration_identity_component;
pub use migration::{
    LegacyAnnotationCandidate, MigrationAnnotationDefaults, MigrationContinuityRecord,
};

pub(crate) const JOURNAL_MAGIC: &[u8] = b"ORNA-PUB-JOURNAL\0";
pub(crate) const GIT_INDEX_LOCK_MAGIC: &[u8] = b"ORNA-GIT-INDEX-LOCK\0";
const MAX_MIGRATION_IDENTITY_RECORDS: usize = 65_536;
const MAX_MIGRATION_IDENTITY_COMPONENT_BYTES: usize = 1024 * 1024;
const MAX_MIGRATION_ANNOTATION_DEFAULTS: usize = 1_000_000_000;

/// The one canonical MIME-1 media type a legacy Blob row takes when its old
/// annotation is not representable as a format-3 annotated value. UPGRADE-004
/// fixes this default; a migration never chooses another one.
pub const LEGACY_ANNOTATION_DEFAULT_MEDIA_TYPE: &str = "application/octet-stream";

/// Whether an absent journal object is a refusal or an accepted loss.
/// `RequireObjects` is for publish; `AcceptLostObjects` is for recovery,
/// which classifies a lost candidate or base itself (ORNA-GIT-010).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationPinPolicy {
    RequireObjects,
    AcceptLostObjects,
}

impl PublicationPinPolicy {
    pub(crate) fn absent_object(self) -> Result<(), RepositoryError> {
        match self {
            Self::RequireObjects => Err(RepositoryError::PublicationObjectUnavailable),
            Self::AcceptLostObjects => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationMaterializationPhase {
    Clean,
    QuarantinePrepared,
    Quarantined,
    Installed,
}

/// What one publication entry needs next, decided from its journalled phase
/// and the bytes currently at its quarantine and target paths. Keeping the
/// decision separate makes every reachable state explicit instead of leaving
/// it implicit in a chain of `if` branches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationMaterializationStep {
    /// The target already holds the entry's next content.
    Done,
    /// The entry is clean and still holds its expected content: journal the
    /// quarantine step and let the materializer run.
    PrepareQuarantine,
    /// Quarantine holds the expected content and the target was installed:
    /// drop the quarantine and journal clean.
    DiscardQuarantine,
    /// A deletion's target is absent and its quarantine holds the expected
    /// content: journal installed, then drop the quarantine.
    InstallAbsent,
    /// A replacement's target is absent and its quarantine holds the expected
    /// content: install the next content, then drop the quarantine.
    InstallNext,
    /// No quarantine remains and the target holds the next content: journal
    /// clean.
    AdoptInstalled,
    /// No quarantine remains, the entry was mid-quarantine and holds its
    /// expected content: materialize it.
    Materialize,
}

/// Why the observed bytes refuse the journalled phase. A quarantine that does
/// not match, or one left behind by a clean entry, is a plain conflict; a
/// target that disagrees with the phase names the entry, because a migration
/// deletion reports the changed legacy path instead.
pub(crate) enum PublicationMaterializationConflict {
    Plain,
    Entry,
}

/// Decides the next step for one entry. `Err(_)` means the observed bytes are
/// inconsistent with the journalled phase.
pub(crate) fn publication_materialization_step(
    entry: &PublicationJournalEntry,
    phase: PublicationMaterializationPhase,
    quarantine: Option<&[u8]>,
    current: Option<&[u8]>,
) -> Result<PublicationMaterializationStep, PublicationMaterializationConflict> {
    if phase == PublicationMaterializationPhase::Clean {
        if quarantine.is_some() {
            return Err(PublicationMaterializationConflict::Plain);
        }
        if current == entry.next() {
            return Ok(PublicationMaterializationStep::Done);
        }
        if current != entry.expected() {
            return Err(PublicationMaterializationConflict::Entry);
        }
        return Ok(PublicationMaterializationStep::PrepareQuarantine);
    }
    if let Some(bytes) = quarantine {
        if Some(bytes) != entry.expected() {
            return Err(PublicationMaterializationConflict::Plain);
        }
        if current == entry.next() {
            return Ok(PublicationMaterializationStep::DiscardQuarantine);
        }
        if current.is_some() {
            return Err(PublicationMaterializationConflict::Entry);
        }
        return Ok(if entry.next().is_none() {
            PublicationMaterializationStep::InstallAbsent
        } else {
            PublicationMaterializationStep::InstallNext
        });
    }
    if current == entry.next() {
        return Ok(PublicationMaterializationStep::AdoptInstalled);
    }
    if phase != PublicationMaterializationPhase::QuarantinePrepared || current != entry.expected() {
        return Err(PublicationMaterializationConflict::Entry);
    }
    Ok(PublicationMaterializationStep::Materialize)
}

impl PublicationMaterializationPhase {
    fn code(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::QuarantinePrepared => 1,
            Self::Quarantined => 2,
            Self::Installed => 3,
        }
    }

    fn from_code(code: u8) -> Result<Self, RepositoryError> {
        match code {
            0 => Ok(Self::Clean),
            1 => Ok(Self::QuarantinePrepared),
            2 => Ok(Self::Quarantined),
            3 => Ok(Self::Installed),
            _ => Err(RepositoryError::InvalidPublicationJournal),
        }
    }
}

/// One managed path's recorded progress through its quarantine-and-install
/// sequence. Every phase past `Clean` names the quarantine file that holds the
/// displaced bytes, so a phase and its quarantine cannot disagree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PublicationMaterialization {
    Clean,
    QuarantinePrepared(String),
    Quarantined(String),
    Installed(String),
}

impl PublicationMaterialization {
    /// The state `phase` records. A `Clean` path has no quarantine and every
    /// other phase has one; any other pairing is a corrupt journal.
    pub(crate) fn from_parts(
        phase: PublicationMaterializationPhase,
        quarantine: Option<String>,
    ) -> Result<Self, RepositoryError> {
        match (phase, quarantine) {
            (PublicationMaterializationPhase::Clean, None) => Ok(Self::Clean),
            (PublicationMaterializationPhase::QuarantinePrepared, Some(name)) => {
                Ok(Self::QuarantinePrepared(name))
            }
            (PublicationMaterializationPhase::Quarantined, Some(name)) => {
                Ok(Self::Quarantined(name))
            }
            (PublicationMaterializationPhase::Installed, Some(name)) => Ok(Self::Installed(name)),
            (PublicationMaterializationPhase::Clean, Some(_))
            | (
                PublicationMaterializationPhase::QuarantinePrepared
                | PublicationMaterializationPhase::Quarantined
                | PublicationMaterializationPhase::Installed,
                None,
            ) => Err(RepositoryError::InvalidPublicationJournal),
        }
    }

    pub(crate) fn phase(&self) -> PublicationMaterializationPhase {
        match self {
            Self::Clean => PublicationMaterializationPhase::Clean,
            Self::QuarantinePrepared(_) => PublicationMaterializationPhase::QuarantinePrepared,
            Self::Quarantined(_) => PublicationMaterializationPhase::Quarantined,
            Self::Installed(_) => PublicationMaterializationPhase::Installed,
        }
    }

    pub(crate) fn quarantine(&self) -> Option<&str> {
        match self {
            Self::Clean => None,
            Self::QuarantinePrepared(name) | Self::Quarantined(name) | Self::Installed(name) => {
                Some(name)
            }
        }
    }
}

/// The recovery stages persisted for one publication attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationJournalStage {
    Prepared,
    RefAdvanced,
    IndexReconciled,
    WorktreeReconciled,
    RuntimeCompleted,
    Complete,
}

impl PublicationJournalStage {
    fn code(self) -> u8 {
        match self {
            Self::Prepared => 1,
            Self::RefAdvanced => 2,
            Self::IndexReconciled => 3,
            Self::WorktreeReconciled => 4,
            Self::RuntimeCompleted => 5,
            Self::Complete => 6,
        }
    }

    fn from_code(code: u8) -> Result<Self, RepositoryError> {
        match code {
            1 => Ok(Self::Prepared),
            2 => Ok(Self::RefAdvanced),
            3 => Ok(Self::IndexReconciled),
            4 => Ok(Self::WorktreeReconciled),
            5 => Ok(Self::RuntimeCompleted),
            6 => Ok(Self::Complete),
            _ => Err(RepositoryError::InvalidPublicationJournal),
        }
    }
}

/// One managed path's captured and desired worktree bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationJournalEntry {
    pub(crate) path: ManagedPath,
    expected: Option<Vec<u8>>,
    next: Option<Vec<u8>>,
    materialization: PublicationMaterialization,
}

impl PublicationJournalEntry {
    pub fn new(path: ManagedPath, expected: Option<Vec<u8>>, next: Option<Vec<u8>>) -> Self {
        Self {
            path,
            expected,
            next,
            materialization: PublicationMaterialization::Clean,
        }
    }

    pub fn path(&self) -> &ManagedPath {
        &self.path
    }

    pub fn expected(&self) -> Option<&[u8]> {
        self.expected.as_deref()
    }

    pub fn next(&self) -> Option<&[u8]> {
        self.next.as_deref()
    }

    pub(crate) fn materialization(&self) -> &PublicationMaterialization {
        &self.materialization
    }
}

/// One checkpoint natural-key predecessor across the legacy-to-format-3
/// consumer identity transition. Source and partition values are their exact
/// canonical typed identity bytes; `None` remains distinct from every encoded
/// partition value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct StreamCheckpointIdentityPredecessor {
    legacy_consumer_identity: [u8; 32],
    legacy_source_identity: Vec<u8>,
    legacy_partition_identity: Option<Vec<u8>>,
    format3_consumer_identity: [u8; 32],
    format3_source_identity: Vec<u8>,
    format3_partition_identity: Option<Vec<u8>>,
}

impl StreamCheckpointIdentityPredecessor {
    pub fn new(
        legacy_consumer_identity: [u8; 32],
        legacy_source_identity: Vec<u8>,
        legacy_partition_identity: Option<Vec<u8>>,
        format3_consumer_identity: [u8; 32],
        format3_source_identity: Vec<u8>,
        format3_partition_identity: Option<Vec<u8>>,
    ) -> Result<Self, RepositoryError> {
        validate_migration_identity_component(&legacy_source_identity, false)?;
        validate_migration_identity_component(
            legacy_partition_identity.as_deref().unwrap_or_default(),
            legacy_partition_identity.is_none(),
        )?;
        validate_migration_identity_component(&format3_source_identity, false)?;
        validate_migration_identity_component(
            format3_partition_identity.as_deref().unwrap_or_default(),
            format3_partition_identity.is_none(),
        )?;
        Ok(Self {
            legacy_consumer_identity,
            legacy_source_identity,
            legacy_partition_identity,
            format3_consumer_identity,
            format3_source_identity,
            format3_partition_identity,
        })
    }

    pub const fn legacy_consumer_identity(&self) -> &[u8; 32] {
        &self.legacy_consumer_identity
    }

    pub fn legacy_source_identity(&self) -> &[u8] {
        &self.legacy_source_identity
    }

    pub fn legacy_partition_identity(&self) -> Option<&[u8]> {
        self.legacy_partition_identity.as_deref()
    }

    pub const fn format3_consumer_identity(&self) -> &[u8; 32] {
        &self.format3_consumer_identity
    }

    pub fn format3_source_identity(&self) -> &[u8] {
        &self.format3_source_identity
    }

    pub fn format3_partition_identity(&self) -> Option<&[u8]> {
        self.format3_partition_identity.as_deref()
    }

    fn legacy_key(&self) -> ([u8; 32], Vec<u8>, Option<Vec<u8>>) {
        (
            self.legacy_consumer_identity,
            self.legacy_source_identity.clone(),
            self.legacy_partition_identity.clone(),
        )
    }

    fn format3_key(&self) -> ([u8; 32], Vec<u8>, Option<Vec<u8>>) {
        (
            self.format3_consumer_identity,
            self.format3_source_identity.clone(),
            self.format3_partition_identity.clone(),
        )
    }
}

/// One stable relation identity's exact logical address change retained for
/// publication recovery and downstream replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationLogicalRowRekey {
    relation_id: [u8; 16],
    old_address: Vec<u8>,
    new_address: Vec<u8>,
}

impl PublicationLogicalRowRekey {
    pub fn new(
        relation_id: [u8; 16],
        old_address: Vec<u8>,
        new_address: Vec<u8>,
    ) -> Result<Self, RepositoryError> {
        crate::row_store::TypedKey::decode_canonical(&old_address)
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        crate::row_store::TypedKey::decode_canonical(&new_address)
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        if old_address == new_address {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(Self {
            relation_id,
            old_address,
            new_address,
        })
    }

    pub const fn relation_id(&self) -> [u8; 16] {
        self.relation_id
    }

    pub fn old_address(&self) -> &[u8] {
        &self.old_address
    }

    pub fn new_address(&self) -> &[u8] {
        &self.new_address
    }
}
/// A restart-safe publication record stored in the private runtime area.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationJournal {
    old_head: GitCommitRef,
    pub(crate) new_head: GitCommitRef,
    pub(crate) base_index_tree: Option<IndexTreeRef>,
    runtime_intent_id: Option<[u8; 16]>,
    compact_manifest: Option<CompactManifestWitness>,
    store_root: Option<IndexTreeRef>,
    migration_continuity: Option<MigrationContinuityRecord>,
    migration_annotation_defaults: Option<MigrationAnnotationDefaults>,
    logical_rekeys: Vec<PublicationLogicalRowRekey>,
    entries: Vec<PublicationJournalEntry>,
    stage: PublicationJournalStage,
    wire_version: u8,
}

impl PublicationJournal {
    pub fn new(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        Self::new_with_base_index_tree(old_head, new_head, None, entries)
    }

    pub fn new_with_index_tree(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: IndexTreeRef,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        Self::new_with_base_index_tree(old_head, new_head, Some(base_index_tree), entries)
    }

    pub fn new_with_runtime_intent(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: IndexTreeRef,
        runtime_intent_id: [u8; 16],
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        let mut journal =
            Self::new_with_base_index_tree(old_head, new_head, Some(base_index_tree), entries)?;
        if runtime_intent_id == [0; 16] {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        journal.runtime_intent_id = Some(runtime_intent_id);
        Ok(journal)
    }

    fn new_with_base_index_tree(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: Option<IndexTreeRef>,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        if entries.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let mut paths = HashSet::new();
        if entries.iter().any(|entry| {
            entry.path.as_path().to_str().is_none() || !paths.insert(entry.path.clone())
        }) {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(Self {
            old_head,
            new_head,
            base_index_tree,
            runtime_intent_id: None,
            compact_manifest: None,
            store_root: None,
            migration_continuity: None,
            migration_annotation_defaults: None,
            logical_rekeys: Vec::new(),
            entries,
            stage: PublicationJournalStage::Prepared,
            wire_version: 5,
        })
    }

    pub fn old_head(&self) -> &GitCommitRef {
        &self.old_head
    }

    pub fn new_head(&self) -> &GitCommitRef {
        &self.new_head
    }

    pub fn base_index_tree(&self) -> Option<&IndexTreeRef> {
        self.base_index_tree.as_ref()
    }

    pub fn entries(&self) -> &[PublicationJournalEntry] {
        &self.entries
    }

    pub const fn runtime_intent_id(&self) -> Option<[u8; 16]> {
        self.runtime_intent_id
    }

    /// Binds a compact manifest identity and all referenced immutable objects
    /// into this restart record. Only the test-only legacy fixture writer
    /// still records one; recovery refuses such journals.
    #[cfg(any(test, feature = "legacy-fixture-writer"))]
    pub fn with_compact_manifest(
        mut self,
        witness: CompactManifestWitness,
    ) -> Result<Self, RepositoryError> {
        witness.validate(None)?;
        self.compact_manifest = Some(witness);
        Ok(self)
    }

    pub fn compact_manifest(&self) -> Option<&CompactManifestWitness> {
        self.compact_manifest.as_ref()
    }
    /// Binds the format-3 store tree selected for index reconciliation and
    /// candidate recovery. Older journal versions remain store-root-free.
    pub fn with_store_root(mut self, store_root: &NativeOid) -> Result<Self, RepositoryError> {
        let store_root = store_root.to_hex();
        if self.store_root.is_some()
            || self.stage != PublicationJournalStage::Prepared
            || store_root.len() != self.new_head.as_str().len()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        self.store_root = Some(IndexTreeRef(store_root));
        self.wire_version = self.wire_version.max(8);
        Ok(self)
    }

    pub fn store_root(&self) -> Option<&IndexTreeRef> {
        self.store_root.as_ref()
    }
    /// Retains exact rekey identities in the restart journal for recovery and replay.
    pub fn with_logical_rekeys(
        mut self,
        logical_rekeys: Vec<PublicationLogicalRowRekey>,
    ) -> Result<Self, RepositoryError> {
        if self.runtime_intent_id.is_none()
            || self.stage != PublicationJournalStage::Prepared
            || !self.logical_rekeys.is_empty()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if logical_rekeys.is_empty() {
            return Ok(self);
        }
        self.logical_rekeys = logical_rekeys;
        self.wire_version = self.wire_version.max(9);
        Ok(self)
    }

    pub fn logical_rekeys(&self) -> &[PublicationLogicalRowRekey] {
        &self.logical_rekeys
    }

    /// Binds the complete stream/checkpoint predecessor inventory to an
    /// explicit legacy-format migration journal. This promotes only that
    /// journal to wire version 6; ordinary publication remains version 5.
    pub fn with_migration_continuity(
        mut self,
        continuity: MigrationContinuityRecord,
    ) -> Result<Self, RepositoryError> {
        if self.migration_continuity.is_some()
            || self.compact_manifest.is_some()
            || self.stage != PublicationJournalStage::Prepared
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        self.migration_continuity = Some(continuity);
        self.wire_version = self.wire_version.max(6);
        Ok(self)
    }

    pub fn migration_continuity(&self) -> Option<&MigrationContinuityRecord> {
        self.migration_continuity.as_ref()
    }

    /// Binds the reported annotation-default transition to an explicit legacy
    /// migration journal. A representable legacy annotation is carried
    /// unchanged, so only the newly defaulted rows are reported; UPGRADE-005
    /// requires that report to accompany preservation of the raw bytes.
    ///
    /// This promotes the journal to wire version 7 and requires the consumer
    /// identity map to be present as well, because a migration that changes
    /// annotated value identity cannot be published without both coordinates.
    pub fn with_migration_annotation_defaults(
        mut self,
        defaults: MigrationAnnotationDefaults,
    ) -> Result<Self, RepositoryError> {
        if self.migration_annotation_defaults.is_some()
            || self.compact_manifest.is_some()
            || self.stage != PublicationJournalStage::Prepared
            || self.migration_continuity.is_none()
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        if defaults.payload_inspected() {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        self.migration_annotation_defaults = Some(defaults);
        self.wire_version = self.wire_version.max(7);
        Ok(self)
    }

    /// Re-admits a migration journal before runtime completion whose
    /// annotation-default coordinate was already durably recorded, so a
    /// resumed publisher reports the same semantic transition without
    /// re-deriving it.
    ///
    /// This is the resume counterpart of
    /// [`Self::with_migration_annotation_defaults`]: only a wire-version-7
    /// legacy migration that already carries a non-inspecting coordinate is
    /// admitted, and the coordinate must be supplied exactly as it was
    /// persisted. It returns the record's own value so a caller cannot
    /// substitute a second, unrecorded transition for the durable evidence.
    pub fn resume_migration_annotation_defaults(
        &self,
        defaults: MigrationAnnotationDefaults,
    ) -> Result<MigrationAnnotationDefaults, RepositoryError> {
        let recorded = self
            .migration_annotation_defaults
            .ok_or(RepositoryError::InvalidFormatMigration)?;
        if self.wire_version != 7
            || !self.is_resumable_migration_stage()
            || self.compact_manifest.is_some()
            || self.migration_continuity.is_none()
            || defaults.payload_inspected()
            || recorded != defaults
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        Ok(recorded)
    }
    fn is_resumable_migration_stage(&self) -> bool {
        matches!(
            self.stage,
            PublicationJournalStage::Prepared
                | PublicationJournalStage::RefAdvanced
                | PublicationJournalStage::IndexReconciled
                | PublicationJournalStage::WorktreeReconciled
        )
    }

    /// The reported annotation-default coordinate of this migration. `None`
    /// means the caller has not declared one, and publication refuses.
    pub const fn migration_annotation_defaults(&self) -> Option<MigrationAnnotationDefaults> {
        self.migration_annotation_defaults
    }

    /// Re-admits a migration journal before runtime completion whose
    /// stream/checkpoint identity inventory was already durably recorded, so
    /// a resumed publisher resolves the same successor identities instead
    /// of re-deriving them.
    ///
    /// This is the resume counterpart of [`Self::with_migration_continuity`],
    /// which refuses a journal that already carries an inventory. A resumed
    /// publisher must supply the inventory exactly as it was persisted, and a
    /// substituted one is rejected: re-deriving a successor identity is the
    /// reset this coordinate exists to prevent. It returns the record's own
    /// value, so the resumed publisher reports the durable evidence rather
    /// than the copy the caller supplied.
    ///
    /// The returned record is the only supported source of successor
    /// identities; [`MigrationContinuityRecord::successor_for`] answers the
    /// legacy-format checkpoint key this migration carried, and answers nothing
    /// for a key it did not carry.
    pub fn resume_migration_continuity(
        &self,
        continuity: MigrationContinuityRecord,
    ) -> Result<MigrationContinuityRecord, RepositoryError> {
        let recorded = self
            .migration_continuity
            .as_ref()
            .ok_or(RepositoryError::InvalidFormatMigration)?;
        if self.compact_manifest().is_some()
            || !self.is_resumable_migration_stage()
            || recorded != &continuity
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        Ok(recorded.clone())
    }

    pub const fn stage(&self) -> PublicationJournalStage {
        self.stage
    }

    pub fn advance(&mut self, next: PublicationJournalStage) -> Result<(), RepositoryError> {
        if next.code() != self.stage.code() + 1 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        self.stage = next;
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, RepositoryError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(JOURNAL_MAGIC);
        bytes.push(self.wire_version);
        put_string(&mut bytes, self.old_head.as_str())?;
        put_string(&mut bytes, self.new_head.as_str())?;
        put_optional_string(
            &mut bytes,
            self.base_index_tree.as_ref().map(IndexTreeRef::as_str),
        )?;
        match self.runtime_intent_id {
            Some(intent_id) => {
                bytes.push(1);
                bytes.extend_from_slice(&intent_id);
            }
            None => bytes.push(0),
        }
        match &self.compact_manifest {
            Some(witness) => {
                bytes.push(1);
                witness.encode(&mut bytes)?;
            }
            None => bytes.push(0),
        }
        if self.wire_version >= 8 {
            match &self.migration_continuity {
                Some(continuity) => {
                    bytes.push(1);
                    continuity.encode(&mut bytes)?;
                }
                None => bytes.push(0),
            }
        } else if self.wire_version >= 6 {
            self.migration_continuity
                .as_ref()
                .ok_or(RepositoryError::InvalidPublicationJournal)?
                .encode(&mut bytes)?;
        } else if self.migration_continuity.is_some() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if self.wire_version >= 8 {
            match &self.migration_annotation_defaults {
                Some(defaults) => {
                    bytes.push(1);
                    defaults.encode(&mut bytes)?;
                }
                None => bytes.push(0),
            }
            put_optional_string(
                &mut bytes,
                self.store_root.as_ref().map(IndexTreeRef::as_str),
            )?;
        } else if self.wire_version >= 7 {
            self.migration_annotation_defaults
                .ok_or(RepositoryError::InvalidPublicationJournal)?
                .encode(&mut bytes)?;
            if self.store_root.is_some() {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
        } else if self.migration_annotation_defaults.is_some() || self.store_root.is_some() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if self.wire_version >= 9 {
            put_u32(&mut bytes, self.logical_rekeys.len())?;
            for rekey in &self.logical_rekeys {
                bytes.extend_from_slice(&rekey.relation_id);
                put_optional_bytes(&mut bytes, Some(rekey.old_address()))?;
                put_optional_bytes(&mut bytes, Some(rekey.new_address()))?;
            }
        } else if !self.logical_rekeys.is_empty() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        bytes.push(self.stage.code());
        put_u32(&mut bytes, self.entries.len())?;
        for entry in &self.entries {
            let path = entry
                .path
                .as_path()
                .to_str()
                .ok_or(RepositoryError::InvalidPublicationJournal)?;
            put_string(&mut bytes, path)?;
            put_optional_bytes(&mut bytes, entry.expected.as_deref())?;
            put_optional_bytes(&mut bytes, entry.next.as_deref())?;
            bytes.push(entry.materialization.phase().code());
            put_optional_string(&mut bytes, entry.materialization.quarantine())?;
        }
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(bytes)
    }

    pub(crate) fn lock_binding(&self) -> Result<[u8; 32], RepositoryError> {
        let mut stable = self.clone();
        stable.stage = PublicationJournalStage::Prepared;
        for entry in &mut stable.entries {
            entry.materialization = PublicationMaterialization::Clean;
        }
        Ok(Sha256::digest(stable.encode()?).into())
    }

    pub(crate) fn decode(bytes: &[u8], object_id_length: usize) -> Result<Self, RepositoryError> {
        if bytes.len() > MAX_JOURNAL_BYTES || !bytes.starts_with(JOURNAL_MAGIC) {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut cursor = JOURNAL_MAGIC.len();
        let version = take_byte(bytes, &mut cursor)?;
        if version != 5 && version != 6 && version != 7 && version != 8 && version != 9 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let old_head =
            GitCommitRef::from_verified_commit(take_string(bytes, &mut cursor)?, object_id_length)?;
        let new_head =
            GitCommitRef::from_verified_commit(take_string(bytes, &mut cursor)?, object_id_length)?;
        let base_index_tree = take_optional_string(bytes, &mut cursor)?
            .map(|value| IndexTreeRef::from_verified_tree(value, object_id_length))
            .transpose()?;
        let runtime_intent_id = match take_byte(bytes, &mut cursor)? {
            0 => None,
            1 => Some(take_fixed_array::<16>(bytes, &mut cursor)?),
            _ => return Err(RepositoryError::InvalidPublicationJournal),
        };
        let compact_manifest = match take_byte(bytes, &mut cursor)? {
            0 => None,
            1 => Some(CompactManifestWitness::decode(
                bytes,
                &mut cursor,
                object_id_length,
            )?),
            _ => return Err(RepositoryError::InvalidPublicationJournal),
        };
        let migration_continuity = if version >= 8 {
            match take_byte(bytes, &mut cursor)? {
                0 => None,
                1 => Some(MigrationContinuityRecord::decode(bytes, &mut cursor)?),
                _ => return Err(RepositoryError::InvalidPublicationJournal),
            }
        } else if version >= 6 {
            Some(MigrationContinuityRecord::decode(bytes, &mut cursor)?)
        } else {
            None
        };
        let migration_annotation_defaults = if version >= 8 {
            match take_byte(bytes, &mut cursor)? {
                0 => None,
                1 => Some(MigrationAnnotationDefaults::decode(bytes, &mut cursor)?),
                _ => return Err(RepositoryError::InvalidPublicationJournal),
            }
        } else if version >= 7 {
            Some(MigrationAnnotationDefaults::decode(bytes, &mut cursor)?)
        } else {
            None
        };
        let store_root = if version >= 8 {
            take_optional_string(bytes, &mut cursor)?
                .map(|value| IndexTreeRef::from_verified_tree(value, object_id_length))
                .transpose()?
        } else {
            None
        };
        let logical_rekeys = if version >= 9 {
            let count = take_u32(bytes, &mut cursor)? as usize;
            if count == 0 {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            let mut rekeys = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                let relation_id = take_fixed_array::<16>(bytes, &mut cursor)?;
                let old_address = take_optional_bytes(bytes, &mut cursor)?
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                let new_address = take_optional_bytes(bytes, &mut cursor)?
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                rekeys.push(PublicationLogicalRowRekey::new(
                    relation_id,
                    old_address,
                    new_address,
                )?);
            }
            if runtime_intent_id.is_none() {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            rekeys
        } else {
            Vec::new()
        };
        if migration_continuity.is_some() && compact_manifest.is_some() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if migration_annotation_defaults.is_some() && migration_continuity.is_none() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let stage = PublicationJournalStage::from_code(take_byte(bytes, &mut cursor)?)?;
        let count = take_u32(bytes, &mut cursor)? as usize;
        if count == 0 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut entries = Vec::with_capacity(count.min(1024));
        let mut paths = HashSet::new();
        for _ in 0..count {
            let path = ManagedPath::new(take_string(bytes, &mut cursor)?)?;
            let expected = take_optional_bytes(bytes, &mut cursor)?;
            let next = take_optional_bytes(bytes, &mut cursor)?;
            if !paths.insert(path.clone()) {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            let phase = PublicationMaterializationPhase::from_code(take_byte(bytes, &mut cursor)?)?;
            let quarantine = take_optional_string(bytes, &mut cursor)?;
            let mut entry = PublicationJournalEntry::new(path, expected, next);
            entry.materialization = PublicationMaterialization::from_parts(phase, quarantine)?;
            entries.push(entry);
        }
        if cursor != bytes.len() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let journal = Self {
            old_head,
            new_head,
            base_index_tree,
            runtime_intent_id,
            compact_manifest,
            store_root,
            migration_continuity,
            migration_annotation_defaults,
            logical_rekeys,
            entries,
            stage,
            wire_version: version,
        };
        let quarantine_binding = journal.quarantine_binding()?;
        for (index, entry) in journal.entries.iter().enumerate() {
            if let Some(name) = entry.materialization.quarantine()
                && name != Self::quarantine_name_from(quarantine_binding.clone(), index)
            {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
        }
        Ok(journal)
    }

    /// The SHA-256 state every quarantine name for this journal extends with
    /// the entry index. Encoding the journal once here keeps decoding many
    /// entries linear instead of re-encoding the journal for each name.
    fn quarantine_binding(&self) -> Result<Sha256, RepositoryError> {
        let mut stable = self.clone();
        stable.stage = PublicationJournalStage::Prepared;
        for entry in &mut stable.entries {
            entry.materialization = PublicationMaterialization::Clean;
        }
        let mut digest = Sha256::new();
        digest.update(stable.encode()?);
        Ok(digest)
    }

    fn quarantine_name_from(mut binding: Sha256, index: usize) -> String {
        binding.update(index.to_le_bytes());
        format!(
            ".orna-quarantine-journal-{}",
            hex_encode(&binding.finalize())
        )
    }

    pub(crate) fn quarantine_name(&self, index: usize) -> Result<String, RepositoryError> {
        Ok(Self::quarantine_name_from(
            self.quarantine_binding()?,
            index,
        ))
    }

    /// Records that `index` reached `phase`. A phase past `Clean` carries the
    /// quarantine name derived from this journal's binding, the only name
    /// decode accepts.
    pub(crate) fn set_materialization(
        &mut self,
        index: usize,
        phase: PublicationMaterializationPhase,
    ) -> Result<(), RepositoryError> {
        let quarantine = (phase != PublicationMaterializationPhase::Clean)
            .then(|| self.quarantine_name(index))
            .transpose()?;
        let materialization = PublicationMaterialization::from_parts(phase, quarantine)?;
        let entry = self
            .entries
            .get_mut(index)
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        entry.materialization = materialization;
        Ok(())
    }
}
