use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    Config, Errno, FileAttr, FileType, Filesystem, Generation, INodeNo, MountOption, RenameFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyWrite, ReplyXattr, Request,
};
use orna_conformance_v1::transactional_loaded_project_candidate_validator;
use orna_evaluator_v1::Limits as EvaluatorLimits;
use orna_project_v1::{
    ObjectIdentityManifest, ObjectKind, ProjectLoader, ResolvedTableSchema,
    admit_source_row_with_owner_fields,
};
use orna_repository_v1::{
    AdmittedBlobReference, CommittedTableBinding, ContentIdentity, GitCommitRef, GitHashAlgorithm,
    HistoricalFormatContext, KeyRange, NativeGraphContext, NativeOid, OrpBlobBinding, Repository,
    RowMapSnapshot, RuntimeGeneration, TypedKey,
    MountRowBaselines,
};
use orna_runtime_v1::{
    NoFault, RuntimeState, RuntimeTableActivationSnapshot, RuntimeTableIdentity, RuntimeTableRows,
    TableObjectId,
    vfs::{
        EditDraft, ManagedFile, SnapshotPin, TemporaryRenameOutcome, TemporarySave, VfsDurableSave,
        VfsFileSnapshot, VfsRepositoryCache, VfsRowProjection, VfsSaveCandidate, VfsSaveCoordinates,
        VfsSaveError, VfsStoredField, VfsTableRowReplacement, project_component,
    },
};
use orna_syntax_v1::{Expr, parse_row};
use orna_value_v1::{Format3Context, Raw, SchemaDescriptor, decode_canonical_storage_value};
use sha2::{Digest, Sha256};
const VFS_SCRATCH_DIRECTORY: &str = "vfs-editor-scratch-v1";
const VFS_SCRATCH_TEMP_PREFIX: &str = ".scratch-save-";
const VFS_SCRATCH_RECORD_MAGIC: &[u8; 8] = b"ORNAVFS1";

#[derive(Clone, Debug)]
struct TemporaryScratchIdentity {
    logical_path: String,
    relation_id: [u8; 16],
    key: Vec<u8>,
    name: String,
    blob_field_name: Option<String>,
}

struct PersistedTemporaryScratch {
    identity: TemporaryScratchIdentity,
    baseline: Option<Vec<u8>>,
    bytes: Vec<u8>,
}

struct VfsScratchStore {
    directory: PathBuf,
}

impl VfsScratchStore {
    fn open(runtime_root: &Path) -> Result<Self, String> {
        let directory = runtime_root.join(VFS_SCRATCH_DIRECTORY);
        let created = match fs::create_dir(&directory) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => {
                return Err(format!(
                    "temporary scratch directory could not be created: {error}"
                ));
            }
        };
        if created {
            Self::sync_directory(runtime_root)?;
        } else {
            let metadata = fs::symlink_metadata(&directory).map_err(|error| {
                format!("temporary scratch directory could not be inspected: {error}")
            })?;
            if !metadata.file_type().is_dir() {
                return Err("temporary scratch directory is not a real directory".into());
            }
        }
        Self::sync_directory(&directory)?;
        Ok(Self { directory })
    }

    fn persist(
        &self,
        identity: &TemporaryScratchIdentity,
        baseline: Option<&[u8]>,
        bytes: &[u8],
    ) -> Result<(), String> {
        Self::validate_identity(identity)?;
        let mut staged = tempfile::Builder::new()
            .prefix(VFS_SCRATCH_TEMP_PREFIX)
            .tempfile_in(&self.directory)
            .map_err(|error| format!("temporary scratch record could not be staged: {error}"))?;
        staged
            .write_all(VFS_SCRATCH_RECORD_MAGIC)
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))?;
        Self::write_identity(staged.as_file_mut(), identity)?;
        Self::write_optional(staged.as_file_mut(), baseline)?;
        Self::write_bytes(staged.as_file_mut(), bytes)?;
        staged.as_file().sync_all().map_err(|error| {
            format!("temporary scratch record could not be synchronized: {error}")
        })?;
        staged
            .persist(self.record_path(identity))
            .map_err(|error| {
                format!(
                    "temporary scratch record could not be published: {}",
                    error.error
                )
            })?;
        Self::sync_directory(&self.directory)
    }

    fn load(&self) -> Result<Vec<PersistedTemporaryScratch>, String> {
        let mut records = Vec::new();
        let mut removed_staging = false;
        for entry in fs::read_dir(&self.directory)
            .map_err(|error| format!("temporary scratch records could not be listed: {error}"))?
        {
            let entry = entry.map_err(|error| {
                format!("temporary scratch record could not be inspected: {error}")
            })?;
            let file_type = entry.file_type().map_err(|error| {
                format!("temporary scratch record type could not be read: {error}")
            })?;
            if !file_type.is_file() {
                return Err("temporary scratch store contains a non-file entry".into());
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "temporary scratch record name is not UTF-8".to_owned())?;
            if name.starts_with(VFS_SCRATCH_TEMP_PREFIX) {
                fs::remove_file(entry.path()).map_err(|error| {
                    format!("stale temporary scratch staging file could not be removed: {error}")
                })?;
                removed_staging = true;
                continue;
            }
            if !name.ends_with(".scratch") {
                return Err("temporary scratch store contains an unknown file".into());
            }
            let record = Self::read_record(&entry.path())?;
            Self::validate_identity(&record.identity)?;
            if Self::record_name(&record.identity) != name {
                return Err("temporary scratch record identity does not match its filename".into());
            }
            records.push(record);
        }
        if removed_staging {
            Self::sync_directory(&self.directory)?;
        }
        records.sort_by(|left, right| {
            left.identity
                .logical_path
                .cmp(&right.identity.logical_path)
                .then_with(|| left.identity.name.cmp(&right.identity.name))
        });
        Ok(records)
    }

    fn remove(&self, identity: &TemporaryScratchIdentity) -> Result<(), String> {
        Self::validate_identity(identity)?;
        match fs::remove_file(self.record_path(identity)) {
            Ok(()) => Self::sync_directory(&self.directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "temporary scratch record could not be removed: {error}"
            )),
        }
    }

    fn record_path(&self, identity: &TemporaryScratchIdentity) -> PathBuf {
        self.directory.join(Self::record_name(identity))
    }

    fn record_name(identity: &TemporaryScratchIdentity) -> String {
        let mut digest = Sha256::new();
        Self::update_digest(&mut digest, identity.logical_path.as_bytes());
        digest.update(identity.relation_id);
        Self::update_digest(&mut digest, &identity.key);
        Self::update_digest(&mut digest, identity.name.as_bytes());
        match &identity.blob_field_name {
            Some(name) => {
                digest.update([1]);
                Self::update_digest(&mut digest, name.as_bytes());
            }
            None => digest.update([0]),
        }
        let mut name = String::with_capacity(64 + ".scratch".len());
        for byte in digest.finalize() {
            let _ = write!(name, "{byte:02x}");
        }
        name.push_str(".scratch");
        name
    }

    fn update_digest(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }

    fn validate_identity(identity: &TemporaryScratchIdentity) -> Result<(), String> {
        if identity.logical_path.is_empty()
            || Path::new(&identity.logical_path).is_absolute()
            || identity.logical_path.contains('\0')
            || identity
                .logical_path
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err("temporary scratch row path is invalid".into());
        }
        if identity.name.is_empty()
            || identity.name == "."
            || identity.name == ".."
            || identity.name.contains('/')
            || identity.name.contains('\0')
        {
            return Err("temporary scratch name is invalid".into());
        }
        if identity
            .blob_field_name
            .as_deref()
            .is_some_and(|name| name.is_empty() || name.contains('\0'))
        {
            return Err("temporary scratch Blob field identity is invalid".into());
        }
        Ok(())
    }

    fn write_identity(file: &mut File, identity: &TemporaryScratchIdentity) -> Result<(), String> {
        Self::write_bytes(file, identity.logical_path.as_bytes())?;
        file.write_all(&identity.relation_id)
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))?;
        Self::write_bytes(file, &identity.key)?;
        Self::write_bytes(file, identity.name.as_bytes())?;
        Self::write_optional(file, identity.blob_field_name.as_deref().map(str::as_bytes))
    }

    fn write_optional(file: &mut File, value: Option<&[u8]>) -> Result<(), String> {
        match value {
            Some(bytes) => {
                file.write_all(&[1]).map_err(|error| {
                    format!("temporary scratch record could not be written: {error}")
                })?;
                Self::write_bytes(file, bytes)
            }
            None => file
                .write_all(&[0])
                .map_err(|error| format!("temporary scratch record could not be written: {error}")),
        }
    }

    fn write_bytes(file: &mut File, bytes: &[u8]) -> Result<(), String> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| "temporary scratch record field is too large".to_owned())?;
        file.write_all(&length.to_le_bytes())
            .and_then(|()| file.write_all(bytes))
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))
    }

    fn read_record(path: &Path) -> Result<PersistedTemporaryScratch, String> {
        let mut file = File::open(path)
            .map_err(|error| format!("temporary scratch record could not be opened: {error}"))?;
        let mut magic = [0; 8];
        file.read_exact(&mut magic)
            .map_err(|error| format!("temporary scratch record header is incomplete: {error}"))?;
        if &magic != VFS_SCRATCH_RECORD_MAGIC {
            return Err("temporary scratch record has an unsupported format".into());
        }
        let identity = Self::read_identity(&mut file)?;
        let baseline = Self::read_optional(&mut file)?;
        let bytes = Self::read_bytes(&mut file)?;
        let mut trailing = [0];
        if file
            .read(&mut trailing)
            .map_err(|error| format!("temporary scratch record could not be checked: {error}"))?
            != 0
        {
            return Err("temporary scratch record contains trailing data".into());
        }
        Ok(PersistedTemporaryScratch {
            identity,
            baseline,
            bytes,
        })
    }

    fn read_identity(file: &mut File) -> Result<TemporaryScratchIdentity, String> {
        let logical_path = Self::read_string(file)?;
        let mut relation_id = [0; 16];
        file.read_exact(&mut relation_id)
            .map_err(|error| format!("temporary scratch record identity is incomplete: {error}"))?;
        let key = Self::read_bytes(file)?;
        let name = Self::read_string(file)?;
        let blob_field_name = Self::read_optional(file)?
            .map(String::from_utf8)
            .transpose()
            .map_err(|error| {
                format!("temporary scratch Blob field identity is invalid: {error}")
            })?;
        Ok(TemporaryScratchIdentity {
            logical_path,
            relation_id,
            key,
            name,
            blob_field_name,
        })
    }

    fn read_string(file: &mut File) -> Result<String, String> {
        String::from_utf8(Self::read_bytes(file)?)
            .map_err(|error| format!("temporary scratch record string is invalid: {error}"))
    }

    fn read_optional(file: &mut File) -> Result<Option<Vec<u8>>, String> {
        let mut present = [0];
        file.read_exact(&mut present)
            .map_err(|error| format!("temporary scratch record field is incomplete: {error}"))?;
        match present[0] {
            0 => Ok(None),
            1 => Self::read_bytes(file).map(Some),
            _ => Err("temporary scratch record has an invalid field marker".into()),
        }
    }

    fn read_bytes(file: &mut File) -> Result<Vec<u8>, String> {
        let mut encoded_length = [0; 8];
        file.read_exact(&mut encoded_length).map_err(|error| {
            format!("temporary scratch record field length is incomplete: {error}")
        })?;
        let length = u64::from_le_bytes(encoded_length);
        if length
            > file
                .metadata()
                .map_err(|error| {
                    format!("temporary scratch record size could not be read: {error}")
                })?
                .len()
        {
            return Err("temporary scratch record field exceeds its file size".into());
        }
        let length = usize::try_from(length).map_err(|_| {
            "temporary scratch record field is too large for this platform".to_owned()
        })?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|error| {
            format!("temporary scratch record field could not be allocated: {error}")
        })?;
        bytes.resize(length, 0);
        file.read_exact(&mut bytes)
            .map_err(|error| format!("temporary scratch record field is incomplete: {error}"))?;
        Ok(bytes)
    }

    fn sync_directory(path: &Path) -> Result<(), String> {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!("temporary scratch directory could not be synchronized: {error}")
            })
    }
}

struct MountWriter {
    runtime: tokio::runtime::Runtime,
    state: RuntimeState,
    repository: Repository,
    project: Arc<orna_project_v1::LoadedProject>,
    runtime_commit: GitCommitRef,
    mount_snapshot: OnceLock<RuntimeTableActivationSnapshot>,
    cache: Arc<VfsRepositoryCache>,
    scratch_store: VfsScratchStore,
    owner_id: [u8; 16],
    row_baselines: Option<MountRowBaselines>,
}

impl MountWriter {
    fn open(
        repository: &Repository,
        commit: &GitCommitRef,
        row_baselines: Option<MountRowBaselines>,
    ) -> Result<Arc<Self>, String> {
        let (identity, initial_digest) = super::runtime_identity(repository)
            .map_err(|error| format!("repository runtime identity is unavailable: {error}"))?;
        let owner_id = super::invocation_owner_id(identity);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| format!("mounted editor runtime could not be created: {error}"))?;
        let state = runtime
            .block_on(RuntimeState::open(repository, identity, initial_digest))
            .map_err(|error| format!("mounted editor runtime could not be opened: {error:?}"))?;
        let runtime_commit = repository
            .head()
            .map_err(|error| format!("mounted editor runtime source commit is unavailable: {error:?}"))?
            .unwrap_or_else(|| commit.clone());
        let project = ProjectLoader::default()
            .load_committed_snapshot(repository, commit)
            .map_err(|error| format!("committed source project could not be loaded: {error:?}"))?;
        let scratch_store = VfsScratchStore::open(repository.runtime_paths().root())?;
        Ok(Arc::new(Self {
            runtime,
            state,
            repository: repository.clone(),
            project: Arc::new(project),
            runtime_commit,
            mount_snapshot: OnceLock::new(),
            cache: Arc::new(VfsRepositoryCache::new()),
            scratch_store,
            owner_id,
            row_baselines,
        }))
    }

    fn capture_mount_activation(&self, tables: &[RuntimeTableIdentity]) -> Result<(), String> {
        let snapshot = self
            .runtime
            .block_on(self.state.begin_admitted_table_activation(tables))
            .map_err(|error| format!("mounted editor row admission failed: {error:?}"))?;
        self.mount_snapshot
            .set(snapshot)
            .map_err(|_| "mounted editor runtime snapshot was already captured".to_owned())
    }

    fn cwd_generation(&self) -> Result<orna_repository_v1::CwdGeneration, String> {
        let capture = self
            .runtime
            .block_on(self.state.capture())
            .map_err(|error| format!("mounted editor CWD could not be captured: {error:?}"))?;
        let generation = u64::try_from(capture.generation())
            .map_err(|_| "mounted editor runtime generation is invalid".to_owned())?;
        self.repository
            .cwd_generation(RuntimeGeneration::new(generation))
            .map_err(|error| {
                format!("mounted editor repository generation is unavailable: {error:?}")
            })
    }

    fn row_baseline<'a>(
        &'a self,
        table: &RuntimeTableIdentity,
        key: &[u8],
    ) -> Result<Option<&'a [u8]>, String> {
        let snapshot = self
            .mount_snapshot
            .get()
            .ok_or_else(|| "mounted editor runtime snapshot was not captured".to_owned())?;
        runtime_row_baseline_from_rows(snapshot.table_rows(), table, key)
            .ok_or_else(|| format!("runtime table {} was not admitted", table.table()))
    }

    fn candidate(
        &self,
        row: &RowSaveContext,
        coordinates: &VfsSaveCoordinates,
        bytes: &[u8],
        replacement_binding: Option<(String, OrpBlobBinding)>,
    ) -> Result<(VfsSaveCandidate, Vec<u8>), orna_runtime_v1::TableActivationError> {
        let source = std::str::from_utf8(bytes).map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let blob_fields = row
            .blob_fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let omitted_blob_fields = blob_fields
            .iter()
            .map(|field| field.name.clone())
            .collect::<Vec<_>>();
        let source = source_without_fields(source, &omitted_blob_fields).map_err(|error| {
            eprintln!("mounted row source projection failed: {error}");
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let mut replacement_binding = replacement_binding;
        let mut blob_bindings = Vec::with_capacity(blob_fields.len());
        if !blob_fields.is_empty() {
            let scope = row.source.graph.open_read_scope().map_err(|_| {
                orna_runtime_v1::TableActivationError::Runtime(
                    orna_runtime_v1::RuntimeError::InvalidTableMutation,
                )
            })?;
            for field in &blob_fields {
                let binding = if replacement_binding
                    .as_ref()
                    .is_some_and(|(name, _)| name == &field.name)
                {
                    replacement_binding
                        .take()
                        .map(|(_, binding)| binding)
                        .ok_or(orna_runtime_v1::TableActivationError::Runtime(
                            orna_runtime_v1::RuntimeError::InvalidTableMutation,
                        ))?
                } else if let Some(bytes) = &field.replacement_bytes {
                    let capture = self
                        .repository
                        .capture_capability(*row.source.binding.relation_id())
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    let candidate = capture
                        .capture_blob_candidate(bytes.as_ref(), bytes.len() as u64)
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    capture
                        .accept_captured_blob(candidate, &field.media_type, field.suffix.as_deref())
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?
                } else {
                    let pin = row
                        .source
                        .graph
                        .protect_admitted_blob_reference(&field.reference, &scope)
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    row.source
                        .graph
                        .accept_protected_blob_pin_with_annotation(
                            pin,
                            &field.media_type,
                            field.suffix.as_deref(),
                        )
                        .map_err(|_| {
                            orna_runtime_v1::TableActivationError::Runtime(
                                orna_runtime_v1::RuntimeError::InvalidTableMutation,
                            )
                        })?
                };
                blob_bindings.push((field.name.clone(), binding));
            }
        }
        if replacement_binding.is_some() {
            return Err(orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            ));
        }
        let mut mutation_id = [0_u8; 16];
        getrandom::fill(&mut mutation_id).map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let admitted = admit_source_row_with_owner_fields(
            &self.project,
            &row.logical_path,
            &source,
            &row.source.schema,
            &row.source.value_context,
            blob_bindings,
            Vec::new(),
            mutation_id,
        )
        .map_err(|error| {
            eprintln!("mounted row source admission failed: {error:?}");
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let (source_mutation, bindings, resolved_schema) = admitted.into_mutation_and_bindings();
        // Text primary keys use their UTF-8 bytes in admitted mutations;
        // other key kinds keep the canonical typed-key encoding.
        let mutation_key_matches = match &row.key {
            TypedKey::Text(key) => source_mutation.key() == key.as_bytes(),
            _ => source_mutation.key() == coordinates.key(),
        };
        if resolved_schema.table_object_id() != row.source.schema.table_object_id()
            || !mutation_key_matches
            || source_mutation.table() != coordinates.table().table()
            || source_mutation.table_object_id() != Some(coordinates.table().object_id())
        {
            return Err(orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            ));
        }
        // Source-row admission creates insert-only mutations; this durable
        // save already admitted the existing baseline, so write a replacement.
        let replacement_value =
            source_mutation
                .value()
                .ok_or(orna_runtime_v1::TableActivationError::Runtime(
                    orna_runtime_v1::RuntimeError::InvalidTableMutation,
                ))?;
        let mut mutation = orna_runtime_v1::TableMutation::new(
            source_mutation.id(),
            source_mutation.table().to_owned(),
            coordinates.key().to_vec(),
            Some(replacement_value.to_vec()),
        )
        .map_err(orna_runtime_v1::TableActivationError::Runtime)?
        .with_table_object_id(coordinates.table().object_id());
        let mut pins = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let (_, _, pin) = binding.into_parts();
            mutation = mutation
                .with_protected_content_pin(&pin)
                .map_err(orna_runtime_v1::TableActivationError::Runtime)?;
            pins.push(pin);
        }
        let content_transfers = pins
            .iter()
            .map(|pin| pin.transfer_record())
            .collect::<Vec<_>>();
        let replacement_bytes = mutation
            .value()
            .ok_or(orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            ))?
            .to_vec();
        let replacement = VfsTableRowReplacement::from_admitted_mutation(
            coordinates.table().clone(),
            mutation,
            pins,
        )
        .map_err(orna_runtime_v1::TableActivationError::Runtime)?;
        let validator = transactional_loaded_project_candidate_validator(
            &self.project,
            coordinates.cwd_generation().clone(),
            Arc::clone(&row.table_object_ids),
            EvaluatorLimits::default(),
        )
        .map_err(|_| {
            orna_runtime_v1::TableActivationError::Runtime(
                orna_runtime_v1::RuntimeError::InvalidTableMutation,
            )
        })?;
        let candidate = VfsSaveCandidate::new(replacement, validator)
            .with_protected_content_transfers(replacement_bytes.clone(), content_transfers);
        Ok((candidate, replacement_bytes))
    }
}

#[derive(Clone)]
struct BlobField {
    name: String,
    file_name: String,
    media_type: String,
    suffix: Option<String>,
    reference: AdmittedBlobReference,
    replacement_bytes: Option<Arc<[u8]>>,
}
struct RowSaveContext {
    writer: Arc<MountWriter>,
    source: Arc<TableSource>,
    key: TypedKey,
    target: Arc<ManagedFile<RowMapSnapshot>>,
    table: RuntimeTableIdentity,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    logical_path: String,
    expected_baseline_row: Mutex<Option<Vec<u8>>>,
    blob_fields: Mutex<Vec<BlobField>>,
}

// Source-row mutations store text primary keys as UTF-8; other keys use their
// canonical typed-key encoding.
fn runtime_table_key_bytes(key: &TypedKey) -> Result<Vec<u8>, String> {
    match key {
        TypedKey::Text(key) => Ok(key.as_bytes().to_vec()),
        _ => key
            .canonical_bytes()
            .map_err(|error| format!("committed row key cannot be encoded: {error:?}")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredFieldShape {
    field_id: [u8; 16],
    field_type: Raw,
}

#[derive(Clone, Debug)]
struct StoredFieldLayout {
    fields: Vec<StoredFieldShape>,
    indexes: HashMap<[u8; 16], usize>,
}

impl StoredFieldLayout {
    fn field_type(&self, field_id: &[u8; 16]) -> Option<&Raw> {
        self.indexes
            .get(field_id)
            .and_then(|index| self.fields.get(*index))
            .map(|field| &field.field_type)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum RowProjectionSource {
    RuntimeSnapshot,
    PinnedSnapshot,
}

fn runtime_row_for_exact_key<'a>(
    rows: &'a [(Vec<u8>, Vec<u8>)],
    key: &[u8],
) -> Option<&'a [u8]> {
    rows.binary_search_by(|(candidate, _)| candidate.as_slice().cmp(key))
        .ok()
        .map(|index| rows[index].1.as_slice())
}

fn runtime_row_baseline_from_rows<'a>(
    rows: &'a RuntimeTableRows,
    table: &RuntimeTableIdentity,
    key: &[u8],
) -> Option<Option<&'a [u8]>> {
    Some(runtime_row_for_exact_key(rows.get(table.table())?, key))
}

fn runtime_row_matches_layout(
    runtime_fields: &Raw,
    pinned_layout: &[StoredFieldShape],
    runtime_layout: &[StoredFieldShape],
    row_field_ids: impl Iterator<Item = [u8; 16]>,
) -> bool {
    let Raw::Array(values) = runtime_fields else {
        return false;
    };
    runtime_layout == pinned_layout
        && values.len() == pinned_layout.len()
        && pinned_layout
            .iter()
            .map(|field| field.field_id)
            .eq(row_field_ids)
}

fn select_runtime_row_fields(
    runtime_row: Option<&[u8]>,
    pinned_fields: Raw,
    pinned_layout: &[StoredFieldShape],
    runtime_layout: Option<&[StoredFieldShape]>,
    row_field_ids: impl Iterator<Item = [u8; 16]>,
) -> (Raw, RowProjectionSource) {
    if let (Some(runtime_row), Some(runtime_layout)) = (runtime_row, runtime_layout)
        && let Ok(runtime_fields) = decode_canonical_storage_value(runtime_row)
        && runtime_row_matches_layout(
            &runtime_fields,
            pinned_layout,
            runtime_layout,
            row_field_ids,
        )
    {
        return (runtime_fields, RowProjectionSource::RuntimeSnapshot);
    }
    (pinned_fields, RowProjectionSource::PinnedSnapshot)
}

fn row_save_context(
    writer: &Arc<MountWriter>,
    source: &Arc<TableSource>,
    key: &TypedKey,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    document_bytes: &[u8],
    blob_fields: Vec<BlobField>,
) -> Result<Arc<RowSaveContext>, String> {
    let declaration = source
        .binding
        .declaration_name()
        .ok_or_else(|| "committed table has no source declaration name".to_owned())?;
    let table = RuntimeTableIdentity::new(
        declaration,
        TableObjectId::new(*source.binding.relation_id()),
    )
    .map_err(|error| format!("committed table identity is invalid: {error:?}"))?;
    let key_bytes = runtime_table_key_bytes(key)?;
    let live_baseline_row = source
        .runtime_table
        .as_ref()
        .map(|runtime_table| writer.row_baseline(runtime_table, &key_bytes))
        .transpose()?
        .flatten()
        .map(|row| row.to_vec());
    // A row this view already accepted keeps that baseline across the processes
    // that save through it; the live durable row is only the first baseline.
    let expected_baseline_row = match (&writer.row_baselines, &source.runtime_table) {
        (Some(baselines), Some(table)) => baselines
            .accepted_row(table.table(), &key_bytes)
            .map_err(|error| format!("mounted row baseline is unavailable: {error}"))?
            .or(live_baseline_row),
        _ => live_baseline_row,
    };
    let mut components = Vec::new();
    flatten_key_components(key, &mut components)?;
    let Some((leaf, prefixes)) = components.split_last() else {
        return Err("committed row key has no path components".into());
    };
    let mut logical_path = declaration.to_owned();
    for prefix in prefixes {
        logical_path.push('/');
        logical_path.push_str(&key_component(prefix)?);
    }
    logical_path.push('/');
    logical_path.push_str(&key_component(leaf)?);
    logical_path.push_str(".orna");
    let image = Arc::new(VfsFileSnapshot::new(
        SnapshotPin::capture(Arc::clone(&source.rows)),
        Arc::<[u8]>::from(document_bytes.to_vec()),
    ));
    let target = writer
        .runtime
        .block_on(writer.cache.managed_file(image))
        .map_err(|error| format!("mounted editor row image could not be admitted: {error:?}"))?;
    Ok(Arc::new(RowSaveContext {
        writer: Arc::clone(writer),
        source: Arc::clone(source),
        key: key.clone(),
        target,
        table,
        logical_path,
        table_object_ids,
        expected_baseline_row: Mutex::new(expected_baseline_row),
        blob_fields: Mutex::new(blob_fields),
    }))
}
/// Why a document save did not commit. `NoCandidate` means the handle never
/// submitted bytes, so there is nothing to accept or discard. `Stale` means the
/// mounted baseline no longer matches the durable row and nothing was written.
/// `Stale` and `Failed` both mean a save was attempted and did not commit, so
/// the draft stays unresolved.
#[derive(Debug)]
enum DocumentCommitError {
    NoCandidate,
    Stale,
    Failed(String),
}

impl From<String> for DocumentCommitError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl std::fmt::Display for DocumentCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCandidate => formatter.write_str("mounted row save failed: NoCandidate"),
            Self::Stale => formatter.write_str("mounted save is stale against its captured baseline"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl RowSaveContext {
    fn durable_save(&self) -> Result<VfsDurableSave, String> {
        let expected_baseline_row = self
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.durable_save_with_baseline(expected_baseline_row)
    }

    fn durable_save_with_baseline(
        &self,
        expected_baseline_row: Option<Vec<u8>>,
    ) -> Result<VfsDurableSave, String> {
        let cwd_generation = self.writer.cwd_generation()?;
        let key = runtime_table_key_bytes(&self.key)?;
        Ok(VfsDurableSave::new(
            self.writer.owner_id,
            cwd_generation,
            self.table.clone(),
            key,
        )
        .with_expected_baseline_row(expected_baseline_row))
    }

    fn record_accepted_row(&self, row: Option<Vec<u8>>) {
        let Some(row) = row else {
            return;
        };
        if let Some(baselines) = &self.writer.row_baselines {
            let recorded = runtime_table_key_bytes(&self.key).and_then(|key| {
                baselines
                    .record_accepted_row(self.table.table(), &key, &row)
                    .map_err(|error| error.to_string())
            });
            if let Err(error) = recorded {
                eprintln!("mounted row baseline could not be recorded: {error}");
            }
        }
        *self
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row);
    }
    fn temporary_target(
        &self,
        blob_field: Option<&BlobField>,
    ) -> Result<Arc<ManagedFile<RowMapSnapshot>>, String> {
        if blob_field.is_none() {
            return Ok(Arc::clone(&self.target));
        }
        let image = Arc::new(VfsFileSnapshot::new(
            SnapshotPin::capture(Arc::clone(&self.source.rows)),
            Arc::<[u8]>::from([]),
        ));
        self.writer
            .runtime
            .block_on(self.writer.cache.managed_file(image))
            .map_err(|error| {
                format!("mounted replacement scratch target could not be admitted: {error:?}")
            })
    }

    fn fsync_temporary(
        &self,
        scratch: &TemporarySave<RowMapSnapshot>,
        identity: &TemporaryScratchIdentity,
        baseline: Option<&[u8]>,
    ) -> Result<(), String> {
        let store = &self.writer.scratch_store;
        self.writer
            .runtime
            .block_on(scratch.fsync_scratch_with(|_, bytes| async move {
                store.persist(identity, baseline, bytes.as_ref())
            }))
            .map(|_| ())
    }

    fn commit_document(
        self: &Arc<Self>,
        draft: &EditDraft<RowMapSnapshot>,
    ) -> Result<(), DocumentCommitError> {
        let save = self.durable_save().map_err(DocumentCommitError::Failed)?;
        let row_context = Arc::clone(self);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self
            .writer
            .runtime
            .block_on(self.target.commit_durable_document_save(
                draft,
                &self.writer.state,
                save,
                move |coordinates, bytes| async move {
                    let (candidate, row_bytes) =
                        row_context
                            .writer
                            .candidate(&row_context, &coordinates, &bytes, None)?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                let accepted_row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                self.record_accepted_row(accepted_row);
                Ok(())
            }
            Ok(TemporaryRenameOutcome::Rejected { .. }) => Err(DocumentCommitError::Failed(
                "mounted row document was rejected by semantic admission".into(),
            )),
            Ok(TemporaryRenameOutcome::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(VfsSaveError::NoCandidate) => Err(DocumentCommitError::NoCandidate),
            Err(VfsSaveError::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(error) => Err(DocumentCommitError::Failed(format!(
                "mounted row save failed: {error:?}"
            ))),
        }
    }

    fn commit_blob_temporary(
        self: &Arc<Self>,
        target: &Arc<ManagedFile<RowMapSnapshot>>,
        scratch: &TemporarySave<RowMapSnapshot>,
        field: &BlobField,
        expected_baseline_row: Option<Vec<u8>>,
    ) -> Result<(), DocumentCommitError> {
        let replacement_bytes = self.writer.runtime.block_on(scratch.candidate_bytes());
        let length = u64::try_from(replacement_bytes.len())
            .map_err(|_| "replacement Blob length is invalid".to_owned())?;
        let digest: [u8; 32] = Sha256::digest(&replacement_bytes).into();
        let expected_identity = ContentIdentity::new(length, digest)
            .map_err(|error| format!("replacement Blob identity is invalid: {error:?}"))?;
        let capture = self
            .writer
            .repository
            .capture_capability(*self.source.binding.relation_id())
            .map_err(|error| format!("replacement Blob capture is unavailable: {error:?}"))?;
        let document = self.writer.runtime.block_on(self.target.open_read());
        let document_length = usize::try_from(document.len())
            .map_err(|_| "mounted row document length is invalid".to_owned())?;
        let document_bytes: Arc<[u8]> = Arc::from(document.read_at(0, document_length));
        let save = self.durable_save_with_baseline(expected_baseline_row)?;
        let row_context = Arc::clone(self);
        let field_name = field.name.clone();
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self
            .writer
            .runtime
            .block_on(self.writer.cache.commit_durable_blob_rename(
                target,
                scratch,
                &self.writer.state,
                save,
                &capture,
                expected_identity,
                &field.media_type,
                field.suffix.as_deref(),
                move |coordinates, _blob_bytes, binding| async move {
                    let (candidate, row_bytes) = row_context.writer.candidate(
                        &row_context,
                        &coordinates,
                        &document_bytes,
                        Some((field_name, binding)),
                    )?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                if let Some(field) = self
                    .blob_fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter_mut()
                    .find(|candidate| candidate.name == field.name)
                {
                    field.replacement_bytes = Some(replacement_bytes);
                } else {
                    return Err(DocumentCommitError::Failed(
                        "replaced Blob field disappeared from its row".into(),
                    ));
                }
                let row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                self.record_accepted_row(row);
                Ok(())
            }
            Ok(TemporaryRenameOutcome::Rejected { .. }) => Err(DocumentCommitError::Failed(
                "mounted Blob replacement was rejected by semantic admission".into(),
            )),
            Ok(TemporaryRenameOutcome::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(VfsSaveError::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(error) => Err(DocumentCommitError::Failed(format!(
                "mounted Blob replacement failed: {error:?}"
            ))),
        }
    }

    fn commit_temporary(
        self: &Arc<Self>,
        scratch: &TemporarySave<RowMapSnapshot>,
        expected_baseline_row: Option<Vec<u8>>,
    ) -> Result<(), String> {
        let save = self.durable_save_with_baseline(expected_baseline_row)?;
        let row_context = Arc::clone(self);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self
            .writer
            .runtime
            .block_on(self.target.commit_durable_temporary_rename(
                scratch,
                &self.writer.state,
                save,
                move |coordinates, bytes| async move {
                    let (candidate, row_bytes) =
                        row_context
                            .writer
                            .candidate(&row_context, &coordinates, &bytes, None)?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                let accepted_row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                self.record_accepted_row(accepted_row);
                Ok(())
            }
            Ok(TemporaryRenameOutcome::Rejected { .. }) => {
                Err("temporary mounted row document was rejected by semantic admission".into())
            }
            Ok(TemporaryRenameOutcome::Stale { .. }) => {
                Err("temporary mounted row document is stale against its captured baseline".into())
            }
            Err(error) => Err(format!("temporary mounted row save failed: {error:?}")),
        }
    }
}
const ROOT_INODE: u64 = 1;
const ROW_PAGE_LIMIT: usize = 256;
// Do not let cached attributes hide ENOTCONN when the FUSE session dies.
const ENTRY_TTL: Duration = Duration::ZERO;
// Private editor drafts must bypass the shared kernel page cache.
const EDITABLE_OPEN_FLAGS: fuser::FopenFlags = fuser::FopenFlags::FOPEN_DIRECT_IO;
pub(super) fn spawn_read_only(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    mountpoint: &Path,
    source: String,
) -> Result<fuser::BackgroundSession, String> {
    let filesystem = WorkspaceFilesystem::from_snapshot(repository, commit, context, None)?;
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName(source),
        MountOption::RO,
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoExec,
    ];
    fuser::spawn_mount(filesystem, mountpoint, &config)
        .map_err(|error| format!("FUSE mount could not be created: {error}"))
}
pub(super) fn spawn_editor(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    mountpoint: &Path,
    source: String,
    row_baselines: MountRowBaselines,
) -> Result<fuser::BackgroundSession, String> {
    let writer = MountWriter::open(repository, commit, Some(row_baselines))?;
    let filesystem = WorkspaceFilesystem::from_snapshot(repository, commit, context, Some(writer))?;
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName(source),
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoExec,
    ];
    fuser::spawn_mount(filesystem, mountpoint, &config)
        .map_err(|error| format!("FUSE editor mount could not be created: {error}"))
}

/// Why an explicit edit was not accepted. `Stale` means the mounted baseline no
/// longer matches the durable row: nothing was written and the draft is retained.
#[derive(Debug)]
pub(super) enum EditError {
    Stale(String),
    Failed(String),
}

impl From<String> for EditError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl From<&str> for EditError {
    fn from(error: &str) -> Self {
        Self::Failed(error.to_owned())
    }
}

pub(super) fn edit_path(
    repository: &Repository,
    commit: &GitCommitRef,
    context: &HistoricalFormatContext,
    path: &str,
    grouped_row: bool,
    row_baselines: Option<MountRowBaselines>,
) -> Result<(), EditError> {
    let writer = MountWriter::open(repository, commit, row_baselines)?;
    let filesystem =
        WorkspaceFilesystem::from_snapshot(repository, commit, context, Some(writer.clone()))?;
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err("edit path must be relative to the projected row tree".into());
    }
    let mut inode = ROOT_INODE;
    {
        let mut builder = filesystem
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err("edit path contains an unsupported component".into());
            };
            inode = builder
                .lookup_child(inode, name)?
                .ok_or_else(|| format!("edit path does not exist: {path}"))?;
        }
    }
    let (save, initial_bytes, blob_edit) = {
        let mut builder = filesystem
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let node = builder
            .nodes
            .get(&inode)
            .ok_or_else(|| "edit target disappeared".to_owned())?;
        match &node.kind {
            NodeKind::Document { save, bytes } => (Arc::clone(save), bytes.clone(), None),
            NodeKind::Blob { graph, reference } => {
                let parent = node.parent;
                let graph = Arc::clone(graph);
                let reference = reference.clone();
                let document = builder
                    .lookup_child(parent, OsStr::new("data.orna"))?
                    .ok_or_else(|| "Blob row has no data.orna document".to_owned())?;
                let save = match &builder
                    .nodes
                    .get(&document)
                    .ok_or_else(|| "Blob row document disappeared".to_owned())?
                    .kind
                {
                    NodeKind::Document { save, .. } => Arc::clone(save),
                    _ => return Err("Blob row document is not editable".into()),
                };
                let file_name = relative
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                let field = save
                    .blob_fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .find(|field| field.file_name == file_name)
                    .cloned()
                    .ok_or_else(|| "Blob edit target is not an admitted row field".to_owned())?;
                let scope = graph
                    .open_read_scope()
                    .map_err(|error| format!("Blob read scope could not be opened: {error:?}"))?;
                let length = usize::try_from(reference.content_identity().length())
                    .map_err(|_| "Blob length is not addressable".to_owned())?;
                let initial = graph
                    .read_blob_range(&reference, 0..length as u64, &scope)
                    .map_err(|error| format!("Blob content could not be read: {error:?}"))?
                    .bytes()
                    .to_vec();
                let expected = save
                    .expected_baseline_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let image = Arc::new(VfsFileSnapshot::new(
                    SnapshotPin::capture(Arc::clone(&save.source.rows)),
                    Arc::<[u8]>::from([]),
                ));
                let target = writer
                    .runtime
                    .block_on(writer.cache.managed_file(image))
                    .map_err(|error| format!("Blob edit target could not be opened: {error:?}"))?;
                let scratch = Arc::new(
                    writer
                        .runtime
                        .block_on(target.begin_temporary_replacement(usize::MAX)),
                );
                (save, initial, Some((target, scratch, field, expected)))
            }
            _ => {
                return Err(if grouped_row {
                "edit --row requires a projected row document or Blob path".into()
            } else {
                "edit target is not a managed row document or Blob".into()
                });
            }
        }
    };
    let draft = if blob_edit.is_none() {
        Some(Arc::new(
            writer
                .runtime
                .block_on(save.target.open_draft(64 * 1024 * 1024)),
        ))
    } else {
        None
    };
    let draft_path = std::env::temp_dir().join(format!(
        "orna-edit-{}-{}.draft",
        std::process::id(),
        super::cli_request_nonce()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&draft_path)
        .map_err(|error| format!("explicit edit draft could not be created: {error}"))?;
    use std::io::Write as _;
    file.write_all(&initial_bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            format!(
                "explicit edit draft could not be initialized: {error}; retained at {}",
                draft_path.display()
            )
        })?;
    drop(file);
    let Some(editor) = std::env::var_os("EDITOR") else {
        return Err(format!(
            "EDITOR is not set; initial draft retained at {}",
            draft_path.display()
        )
        .into());
    };
    let status = std::process::Command::new(editor)
        .arg(&draft_path)
        .status()
        .map_err(|error| {
            format!(
                "editor could not be started: {error}; draft retained at {}",
                draft_path.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "editor exited unsuccessfully; draft retained at {}",
            draft_path.display()
        )
        .into());
    }
    let candidate = std::fs::read(&draft_path).map_err(|error| {
        format!(
            "edited candidate could not be read: {error}; draft retained at {}",
            draft_path.display()
        )
    })?;
    let admission =
        if let Some((target, scratch, field, expected)) = blob_edit {
            writer
                .runtime
                .block_on(async {
                    scratch.truncate(0).await.map_err(|error| {
                        format!("Blob candidate exceeds draft limits: {error:?}")
                    })?;
                    scratch
                        .write_at(0, &candidate)
                        .await
                        .map_err(|error| format!("Blob candidate exceeds draft limits: {error:?}"))
        })
        .map_err(|error| format!("{error}; draft retained at {}", draft_path.display()))?;
        save.commit_blob_temporary(&target, &scratch, &field, expected)
    } else {
        let draft = draft.expect("document route creates an editor draft before launch");
            writer
                .runtime
                .block_on(async {
                    draft.truncate(0).await.map_err(|error| {
                        format!("edited candidate exceeds draft limits: {error:?}")
                    })?;
                    draft.write_at(0, &candidate).await.map_err(|error| {
                        format!("edited candidate exceeds draft limits: {error:?}")
                    })
        })
        .map_err(|error| format!("{error}; draft retained at {}", draft_path.display()))?;
        save.commit_document(&draft)
    };
    match admission {
        Ok(()) => std::fs::remove_file(&draft_path).map_err(|error| {
            EditError::Failed(format!("edit was accepted but its explicit draft could not be removed: {error}"))
        }),
        Err(DocumentCommitError::Stale) => Err(EditError::Stale(format!(
            "the mounted baseline is stale against the durable row; nothing was written; draft retained at {}",
            draft_path.display()
        ))),
        Err(error) => Err(EditError::Failed(format!(
            "edit candidate was rejected: {error}; draft retained at {}",
            draft_path.display()
        ))),
    }
}
#[derive(Debug)]
pub(super) enum UnmountPreparationError {
    UnresolvedDrafts,
    ReadOnlyMount,
    Disconnected,
    Operation(String),
}

pub(super) fn begin_unmount(mountpoint: &Path) -> Result<(), UnmountPreparationError> {
    rustix::fs::setxattr(
        mountpoint,
        "user.orna.unmount-quiesce",
        b"1",
        rustix::fs::XattrFlags::empty(),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::BUSY {
            UnmountPreparationError::UnresolvedDrafts
        } else if error == rustix::io::Errno::ROFS {
            UnmountPreparationError::ReadOnlyMount
        } else if error == rustix::io::Errno::NOTCONN {
            UnmountPreparationError::Disconnected
        } else {
            UnmountPreparationError::Operation(format!("could not quiesce Orna mount: {error}"))
        }
    })
}

pub(super) fn cancel_unmount(mountpoint: &Path) -> Result<(), String> {
    rustix::fs::setxattr(
        mountpoint,
        "user.orna.unmount-quiesce",
        b"0",
        rustix::fs::XattrFlags::empty(),
    )
    .map_err(|error| format!("could not resume Orna mount after failed unmount: {error}"))
}
pub(super) fn has_unresolved_drafts(mountpoint: &Path) -> Result<bool, String> {
    let mut value = [0_u8; 1];
    let length = rustix::fs::getxattr(mountpoint, "user.orna.unresolved-drafts", &mut value)
    .map_err(|error| format!("could not read Orna mount draft state: {error}"))?;
    if length != 1 {
        return Err("Orna mount returned an invalid draft-state length".to_owned());
    }
    match value[0] {
        b'0' => Ok(false),
        b'1' => Ok(true),
        _ => Err("Orna mount returned invalid draft state".to_owned()),
    }
}

pub(super) fn unmount_mountpoint(mountpoint: &Path) -> Result<(), String> {
    let output = Command::new(fusermount_program())
        .args(["-u", "--"])
        .arg(mountpoint)
        .output()
        .map_err(|error| format!("FUSE unmount helper could not be started: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(())
}

fn fusermount_program() -> OsString {
    if let Some(program) = std::env::var_os("FUSERMOUNT_PATH") {
        return program;
    }
    for program in [
        "fusermount3",
        "fusermount",
        "mount_fusefs",
        "/sbin/fusermount3",
        "/sbin/fusermount",
        "/sbin/mount_fusefs",
        "/bin/fusermount3",
        "/bin/fusermount",
    ] {
        if Command::new(program).arg("-h").output().is_ok() {
            return OsString::from(program);
        }
    }
    OsString::from("fusermount3")
}

#[derive(Clone)]
enum OpenFile {
    Document(Arc<RowSaveContext>, Arc<EditDraft<RowMapSnapshot>>),
    Temporary(
        Arc<RowSaveContext>,
        Arc<ManagedFile<RowMapSnapshot>>,
        Arc<TemporarySave<RowMapSnapshot>>,
        Option<BlobField>,
    ),
}
/// Unsynced Document drafts are discarded on release; a Document whose fsync was
/// rejected stays unresolved until a later sync of the same file is accepted.
/// Replacement scratch drafts remain unresolved until their rename is accepted.
#[derive(Default)]
struct DraftState {
    pending_truncates: HashMap<u64, u64>,
    unresolved_handles: HashMap<u64, u64>,
    retained_scratch: HashSet<u64>,
    rejected_documents: HashSet<u64>,
    /// Scratch files whose rename was refused; kept until a rename of them is accepted.
    rejected_scratch: HashSet<u64>,
    quiescing: bool,
}

impl DraftState {
    fn has_unresolved_drafts(&self) -> bool {
        !self.pending_truncates.is_empty()
            || !self.unresolved_handles.is_empty()
            || !self.retained_scratch.is_empty()
            || !self.rejected_documents.is_empty()
            || !self.rejected_scratch.is_empty()
    }
}

struct WorkspaceFilesystem {
    state: Mutex<FsBuilder>,
    open_files: Mutex<HashMap<u64, OpenFile>>,
    draft_state: Mutex<DraftState>,
    next_handle: AtomicU64,
}

impl WorkspaceFilesystem {
    fn from_snapshot(
        repository: &Repository,
        commit: &GitCommitRef,
        context: &HistoricalFormatContext,
        writer: Option<Arc<MountWriter>>,
    ) -> Result<Self, String> {
        // The caller passes the commit and its context from the same pinned view.
        let identities = ObjectIdentityManifest::read_committed(repository, commit)
            .map_err(|error| format!("committed table identities could not be read: {error:?}"))?;
        let tables = identities
            .records()
            .iter()
            .filter(|identity| identity.kind() == ObjectKind::Table && !identity.is_retired())
            .map(|identity| (*identity.id(), identity.qualified_name().to_owned()))
            .collect::<Vec<_>>();
        let loader = ProjectLoader::default();
        let mut builder = FsBuilder::new();
        builder.writer.clone_from(&writer);
        builder.table_object_ids = Arc::new(
            tables
                .iter()
                .map(|(relation_id, qualified_name)| {
                    (qualified_name.clone(), TableObjectId::new(*relation_id))
                })
                .collect(),
        );
        let mut runtime_tables = BTreeMap::new();
        if let Some(writer) = &builder.writer {
            // Every committed table is admitted under its pinned declaration name,
            // the name its row mutations address. The HEAD layout only gates the
            // runtime projection; an unresolved HEAD schema leaves that table pinned.
            let mut activation_tables = Vec::with_capacity(tables.len());
            for (relation_id, _) in &tables {
                let table_id = TableObjectId::new(*relation_id);
                let pinned_schema = loader
                    .resolve_committed_source_table_schema(repository, commit, table_id)
                    .map_err(|error| format!("committed table schema is unavailable: {error:?}"))?;
                let runtime_table =
                    RuntimeTableIdentity::new(pinned_schema.declaration_name(), table_id)
                        .map_err(|error| format!("runtime table identity is invalid: {error:?}"))?;
                let runtime_field_layout = loader
                    .resolve_committed_source_table_schema(
                        repository,
                        &writer.runtime_commit,
                        table_id,
                    )
                    .ok()
                    .map(|runtime_schema| stored_field_layout(runtime_schema.descriptor()))
                    .transpose()?;
                activation_tables.push(runtime_table.clone());
                runtime_tables.insert(*relation_id, (runtime_table, runtime_field_layout));
            }
            writer.capture_mount_activation(&activation_tables)?;
        }

        for (relation_id, qualified_name) in tables {
            let schema = loader
                .resolve_committed_source_table_schema(
                    repository,
                    commit,
                    TableObjectId::new(relation_id),
                )
                .map_err(|error| format!("committed table schema is unavailable: {error:?}"))?;
            let table_directory = builder.table_directory(
                schema.binding().module_name().unwrap_or_default(),
                schema.declaration_name(),
            )?;
            let field_names = field_names_for_table(&identities, &qualified_name);
            let rows = context
                .load_row_map(relation_id)
                .map_err(|error| format!("committed row map could not be opened: {error}"))?;
            let graph =
                Arc::new(context.open_native_graph(&rows).map_err(|error| {
                    format!("committed native graph could not be opened: {error}")
                })?);
            let value_context = context.open_value_context(relation_id).map_err(|error| {
                format!("committed row value context could not be opened: {error:?}")
            })?;
            let (runtime_table, runtime_field_layout) = runtime_tables
                .remove(&relation_id)
                .map(|(table, layout)| (Some(table), layout))
                .unwrap_or((None, None));
            builder.add_table_rows(
                table_directory,
                graph,
                Arc::new(rows),
                &schema,
                value_context,
                &field_names,
                runtime_table,
                runtime_field_layout,
            )?;
        }
        let mut retained_scratch = HashSet::new();
        if let Some(writer) = &writer {
            for record in writer.scratch_store.load()? {
                let logical_path = record.identity.logical_path.clone();
                match Self::restore_temporary_scratch(&mut builder, writer, record) {
                    Ok(inode) => {
                        retained_scratch.insert(inode);
                    }
                    Err(error) => eprintln!(
                        "skipped unresolved persisted temporary scratch {logical_path}: {error}"
                    ),
                }
            }
        }

        Ok(Self {
            state: Mutex::new(builder),
            open_files: Mutex::new(HashMap::new()),
            draft_state: Mutex::new(DraftState {
                retained_scratch,
                ..DraftState::default()
            }),
            next_handle: AtomicU64::new(1),
        })
    }
    fn restore_temporary_scratch(
        builder: &mut FsBuilder,
        writer: &Arc<MountWriter>,
        record: PersistedTemporaryScratch,
    ) -> Result<u64, String> {
        let table_inode = builder
            .nodes
            .iter()
            .find_map(|(inode, node)| match &node.kind {
                NodeKind::Table(table)
                    if *table.source.binding.relation_id() == record.identity.relation_id =>
                {
                    Some(*inode)
                }
                _ => None,
            })
            .ok_or_else(|| "persisted temporary scratch target table is unavailable".to_owned())?;
        let mut path_components = record.identity.logical_path.split('/').skip(1).peekable();
        let mut parent_inode = table_inode;
        while let Some(component) = path_components.next() {
            let component = if path_components.peek().is_none() {
                component.strip_suffix(".orna").ok_or_else(|| {
                    "persisted temporary scratch target path is invalid".to_owned()
                })?
            } else {
                component
            };
            parent_inode = builder
                .lookup_child(parent_inode, OsStr::new(component))?
                .ok_or_else(|| {
                    "persisted temporary scratch target row is unavailable".to_owned()
                })?;
        }
        if !matches!(
            builder.nodes.get(&parent_inode).map(|node| &node.kind),
            Some(NodeKind::Row(_))
        ) {
            return Err("persisted temporary scratch target row is unavailable".into());
        }
        let document_inode = builder
            .lookup_child(parent_inode, OsStr::new("data.orna"))?
            .ok_or_else(|| {
                "persisted temporary scratch target document is unavailable".to_owned()
            })?;
        let save = match builder.nodes.get(&document_inode).map(|node| &node.kind) {
            Some(NodeKind::Document { save, .. }) => Arc::clone(save),
            _ => return Err("persisted temporary scratch target document is unavailable".into()),
        };
        if *save.source.binding.relation_id() != record.identity.relation_id
            || runtime_table_key_bytes(&save.key)? != record.identity.key
        {
            return Err("persisted temporary scratch target identity has changed".into());
        }
        let parent = builder
            .nodes
            .get(&document_inode)
            .ok_or_else(|| "persisted temporary scratch row node is unavailable".to_owned())?
            .parent;
        let blob_field = match record.identity.blob_field_name.as_deref() {
            Some(name) => Some(
                save.blob_fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .find(|field| field.name.as_str() == name)
                    .cloned()
                    .ok_or_else(|| {
                        "persisted temporary scratch Blob field is unavailable".to_owned()
                    })?,
            ),
            None => None,
        };
        let target = save.temporary_target(blob_field.as_ref())?;
        let scratch = Arc::new(
            writer
                .runtime
                .block_on(target.begin_temporary_replacement(usize::MAX)),
        );
        writer
            .runtime
            .block_on(scratch.write_at(0, &record.bytes))
            .map_err(|error| {
                format!("persisted temporary scratch bytes could not be restored: {error:?}")
            })?;
        let PersistedTemporaryScratch {
            identity,
            baseline,
            bytes,
        } = record;
        let name = identity.name.clone();
        builder.add_file(
            parent,
            &name,
            NodeKind::Temporary {
                bytes,
                save,
                target,
                scratch,
                blob_field,
                baseline,
                identity: Arc::new(identity),
            },
        )
    }

    fn update_writable_bytes(&self, inode: u64, bytes: &[u8]) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(node) = state.nodes.get_mut(&inode) else {
            return false;
        };
        let current = match &mut node.kind {
            NodeKind::Document { bytes, .. } | NodeKind::Temporary { bytes, .. } => bytes,
            _ => return false,
        };
        current.clear();
        current.extend_from_slice(bytes);
        true
    }
    fn has_unresolved_drafts(&self) -> bool {
        self.draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .has_unresolved_drafts()
    }
}

impl Filesystem for WorkspaceFilesystem {
    fn getxattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        if inode.0 != ROOT_INODE || name != OsStr::new("user.orna.unresolved-drafts") {
            reply.error(Errno::ENODATA);
            return;
        }
        let value = if self.has_unresolved_drafts() {
            b"1"
        } else {
            b"0"
        };
        if size == 0 {
            reply.size(1);
        } else if size >= 1 {
            reply.data(value);
        } else {
            reply.error(Errno::ERANGE);
        }
    }
    fn setxattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        if inode.0 != ROOT_INODE || name != OsStr::new("user.orna.unmount-quiesce") {
            reply.error(Errno::ENODATA);
            return;
        }
        if flags != 0 || position != 0 {
            reply.error(Errno::EINVAL);
            return;
        }
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if value == b"1" {
            if draft_state.has_unresolved_drafts() {
                reply.error(Errno::EBUSY);
                return;
            }
            draft_state.quiescing = true;
            reply.ok();
        } else if value == b"0" {
            draft_state.quiescing = false;
            reply.ok();
        } else {
            reply.error(Errno::EINVAL);
        }
    }
    fn init(
        &mut self,
        _request: &Request,
        config: &mut fuser::KernelConfig,
    ) -> std::io::Result<()> {
        let editor = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .writer
            .is_some();
        if editor {
            config
                .add_capabilities(fuser::InitFlags::FUSE_ATOMIC_O_TRUNC)
                .map_err(|unsupported| {
                    std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        format!("editor mount requires atomic O_TRUNC support: {unsupported:?}"),
                    )
                })?;
        }
        Ok(())
    }
    fn lookup(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.lookup_child(parent.0, name) {
            Ok(Some(inode)) => match state.attr(inode) {
                Some(attr) => reply.entry(&ENTRY_TTL, &attr, Generation(1)),
                None => reply.error(Errno::EIO),
            },
            Ok(None) => reply.error(Errno::ENOENT),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn getattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: Option<fuser::FileHandle>,
        reply: ReplyAttr,
    ) {
        let candidate_size = handle.and_then(|handle| {
            let open_file = self
                .open_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&handle.0)
                .cloned()?;
            let bytes = match open_file {
                OpenFile::Document(save, draft) => {
                    save.writer.runtime.block_on(draft.candidate_bytes())
                }
                OpenFile::Temporary(save, _, scratch, _) => {
                    save.writer.runtime.block_on(scratch.candidate_bytes())
                }
            };
            Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        });
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.attr(inode.0) {
            Some(mut attr) => {
                if let Some(size) = candidate_size {
                    attr.size = size;
                    attr.blocks = size.saturating_add(511) / 512;
                }
                reply.attr(&ENTRY_TTL, &attr);
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn open(&self, _request: &Request, inode: INodeNo, flags: fuser::OpenFlags, reply: ReplyOpen) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let replaced_blob = matches!(
            state.nodes.get(&inode.0).map(|node| &node.kind),
            Some(NodeKind::BlobBytes(_))
        );
        drop(state);
        if replaced_blob {
            reply.opened(fuser::FileHandle(0), fuser::FopenFlags::empty());
            return;
        }
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let (save, target, scratch, blob_field) = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.nodes.get(&inode.0).map(|node| &node.kind) {
                Some(NodeKind::Document { save, .. }) => {
                    (Arc::clone(save), Arc::clone(&save.target), None, None)
                }
                Some(NodeKind::Temporary {
                    save,
                    target,
                    scratch,
                    blob_field,
                    ..
                }) => (
                    Arc::clone(save),
                    Arc::clone(target),
                    Some(Arc::clone(scratch)),
                    blob_field.clone(),
                ),
                Some(NodeKind::Bytes(_) | NodeKind::BlobBytes(_) | NodeKind::Blob { .. }) => {
                    reply.opened(fuser::FileHandle(0), fuser::FopenFlags::empty());
                    return;
                }
                Some(_) => {
                    reply.error(Errno::EISDIR);
                    return;
                }
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            }
        };
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let pending_truncate = draft_state.pending_truncates.get(&inode.0).copied();
        let truncate_size = if flags.0 & libc::O_TRUNC != 0 {
            Some(0)
        } else {
            pending_truncate
        };
        let is_scratch = scratch.is_some();
        let had_retained_scratch = draft_state.retained_scratch.contains(&inode.0);
        if truncate_size.is_some() {
            draft_state.unresolved_handles.insert(handle, inode.0);
            if is_scratch {
                draft_state.retained_scratch.insert(inode.0);
            }
        }
        if let Some(scratch) = scratch {
            if let Some(size) = truncate_size
                && save
                    .writer
                    .runtime
                    .block_on(scratch.truncate(size))
                    .is_err()
            {
                draft_state.unresolved_handles.remove(&handle);
                if is_scratch && !had_retained_scratch {
                    draft_state.retained_scratch.remove(&inode.0);
                }
                reply.error(Errno::EIO);
                return;
            }
            self.open_files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    handle,
                    OpenFile::Temporary(save, target, scratch, blob_field),
                );
            if truncate_size.is_some() {
                draft_state.pending_truncates.remove(&inode.0);
            }
            reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS);
            return;
        }
        let draft = Arc::new(
            save.writer
                .runtime
                .block_on(save.target.open_draft(usize::MAX)),
        );
        if let Some(size) = truncate_size
            && save.writer.runtime.block_on(draft.truncate(size)).is_err()
        {
            draft_state.unresolved_handles.remove(&handle);
            reply.error(Errno::EIO);
            return;
        }
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle, OpenFile::Document(save, draft));
        if truncate_size.is_some() {
            draft_state.pending_truncates.remove(&inode.0);
        }
        reply.opened(fuser::FileHandle(handle), EDITABLE_OPEN_FLAGS);
    }
    fn create(
        &self,
        _request: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let Some(name) = name.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let (attr, save, target, scratch, blob_field) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(
                state.nodes.get(&parent.0).map(|node| &node.kind),
                Some(NodeKind::Row(_))
            ) {
                reply.error(Errno::EROFS);
                return;
            }
            let destination = match state.lookup_child(parent.0, OsStr::new("data.orna")) {
                Ok(Some(inode)) => inode,
                Ok(None) | Err(_) => {
                    reply.error(Errno::EIO);
                    return;
                }
            };
            let save = match state.nodes.get(&destination).map(|node| &node.kind) {
                Some(NodeKind::Document { save, .. }) => Arc::clone(save),
                _ => {
                    reply.error(Errno::EROFS);
                    return;
                }
            };
            let blob_field = if is_editor_temporary_name(name) {
                None
            } else {
                save.blob_fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .find(|field| is_replacement_scratch_for(name, &field.file_name))
                    .cloned()
            };
            if !is_editor_temporary_name(name) && blob_field.is_none() {
                reply.error(Errno::EROFS);
                return;
            }
            let baseline = save
                .expected_baseline_row
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let Ok(key) = runtime_table_key_bytes(&save.key) else {
                reply.error(Errno::EIO);
                return;
            };
            let identity = Arc::new(TemporaryScratchIdentity {
                logical_path: save.logical_path.clone(),
                relation_id: *save.source.binding.relation_id(),
                key,
                name: name.to_owned(),
                blob_field_name: blob_field.as_ref().map(|field| field.name.clone()),
            });
            let Ok(target) = save.temporary_target(blob_field.as_ref()) else {
                reply.error(Errno::EIO);
                return;
            };
            let scratch = Arc::new(
                save.writer
                    .runtime
                    .block_on(target.begin_temporary_replacement(usize::MAX)),
            );
            let inode = match state.add_file(
                parent.0,
                name,
                NodeKind::Temporary {
                    bytes: Vec::new(),
                    save: Arc::clone(&save),
                    target: Arc::clone(&target),
                    scratch: Arc::clone(&scratch),
                    blob_field: blob_field.clone(),
                    baseline,
                    identity: Arc::clone(&identity),
                },
            ) {
                Ok(inode) => inode,
                Err(_) => {
                    reply.error(Errno::EEXIST);
                    return;
                }
            };
            let Some(attr) = state.attr(inode) else {
                reply.error(Errno::EIO);
                return;
            };
            (attr, save, target, scratch, blob_field)
        };
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                handle,
                OpenFile::Temporary(save, target, scratch, blob_field),
            );
        reply.created(
            &ENTRY_TTL,
            &attr,
            Generation(1),
            fuser::FileHandle(handle),
            EDITABLE_OPEN_FLAGS,
        );
    }
    fn rename(
        &self,
        _request: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if parent != newparent || flags != RenameFlags::empty() {
            reply.error(Errno::EROFS);
            return;
        }
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let (
            source_inode,
            destination_inode,
            save,
            target,
            scratch,
            blob_field,
            baseline,
            identity,
        ) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let source_inode = match state.lookup_child(parent.0, name) {
                Ok(Some(inode)) => inode,
                Ok(None) | Err(_) => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };
            let Some(NodeKind::Temporary {
                save,
                target,
                scratch,
                blob_field,
                baseline,
                identity,
                ..
            }) = state.nodes.get(&source_inode).map(|node| &node.kind)
            else {
                reply.error(Errno::EROFS);
                return;
            };
            let (save, target, scratch, blob_field, baseline, identity) = (
                Arc::clone(save),
                Arc::clone(target),
                Arc::clone(scratch),
                blob_field.clone(),
                baseline.clone(),
                Arc::clone(identity),
            );
            let expected_name = blob_field
                .as_ref()
                .map_or("data.orna", |field| field.file_name.as_str());
            if newname.to_str() != Some(expected_name) {
                reply.error(Errno::EROFS);
                return;
            }
            let destination_inode = match state.lookup_child(newparent.0, newname) {
                Ok(Some(inode)) => inode,
                Ok(None) | Err(_) => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            };
            match (
                blob_field.as_ref(),
                state.nodes.get(&destination_inode).map(|node| &node.kind),
            ) {
                (
                    None,
                    Some(NodeKind::Document {
                        save: destination_save,
                        ..
                    }),
                ) => {
                    if !Arc::ptr_eq(&save, destination_save) {
                        reply.error(Errno::EXDEV);
                        return;
                    }
                }
                (Some(_), Some(NodeKind::Blob { .. } | NodeKind::BlobBytes(_))) => {}
                _ => {
                    reply.error(Errno::EROFS);
                    return;
                }
            }
            (
                source_inode,
                destination_inode,
                save,
                target,
                scratch,
                blob_field,
                baseline,
                identity,
            )
        };
        let committed = match &blob_field {
            Some(field) => save
                .commit_blob_temporary(&target, &scratch, field, baseline)
                .map_err(|error| error.to_string()),
            None => save.commit_temporary(&scratch, baseline),
        };
        if let Err(error) = committed {
            eprintln!("mounted replacement failed: {error}");
            draft_state.rejected_scratch.insert(source_inode);
            reply.error(Errno::EIO);
            return;
        }
        let accepted_bytes = save.writer.runtime.block_on(scratch.candidate_bytes());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let destination = state
            .nodes
            .get_mut(&destination_inode)
            .map(|node| &mut node.kind);
        match (blob_field, destination) {
            (Some(_), Some(kind @ (NodeKind::Blob { .. } | NodeKind::BlobBytes(_)))) => {
                *kind = NodeKind::BlobBytes(accepted_bytes);
            }
            (None, Some(NodeKind::Document { bytes, .. })) => {
                *bytes = accepted_bytes.to_vec();
            }
            _ => {
                reply.error(Errno::EIO);
                return;
            }
        }
        if let Err(error) = save.writer.scratch_store.remove(&identity) {
            eprintln!("mounted replacement scratch cleanup failed: {error}");
            reply.error(Errno::EIO);
            return;
        }
        let removed = state
            .nodes
            .get_mut(&parent.0)
            .and_then(|node| node.kind.directory_mut())
            .and_then(|directory| directory.remove_entry(name));
        if removed.is_none() {
            reply.error(Errno::EIO);
            return;
        }
        draft_state
            .unresolved_handles
            .retain(|_, dirty_inode| *dirty_inode != source_inode);
        draft_state.retained_scratch.remove(&source_inode);
        draft_state.rejected_scratch.remove(&source_inode);
        draft_state.rejected_documents.remove(&destination_inode);
        draft_state.pending_truncates.remove(&source_inode);
        state.nodes.remove(&source_inode);
        reply.ok();
    }
    fn unlink(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(name) = name.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(
            state.nodes.get(&parent.0).map(|node| &node.kind),
            Some(NodeKind::Row(_))
        ) {
            reply.error(Errno::EROFS);
            return;
        }
        let inode = match state.lookup_child(parent.0, OsStr::new(name)) {
            Ok(Some(inode)) => inode,
            Ok(None) => {
                reply.error(Errno::ENOENT);
                return;
            }
            Err(_) => {
                reply.error(Errno::EIO);
                return;
            }
        };
        if !matches!(
            state.nodes.get(&inode).map(|node| &node.kind),
            Some(NodeKind::Temporary { .. })
        ) {
            reply.error(Errno::EROFS);
            return;
        }
        if draft_state
            .unresolved_handles
            .values()
            .any(|open| *open == inode)
        {
            reply.error(Errno::EBUSY);
            return;
        }
        if draft_state.rejected_scratch.contains(&inode) {
            // A rejected replacement is the only copy of its draft until a rename of
            // it is accepted; removing its name must not discard it.
            reply.error(Errno::EBUSY);
            return;
        }
        let (save, identity) = match state.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Temporary { save, identity, .. }) => {
                (Arc::clone(save), Arc::clone(identity))
            }
            _ => {
                reply.error(Errno::EROFS);
                return;
            }
        };
        if let Err(error) = save.writer.scratch_store.remove(&identity) {
            eprintln!("mounted temporary scratch removal failed: {error}");
            reply.error(Errno::EIO);
            return;
        }
        if state
            .directory_mut(parent.0)
            .and_then(|directory| directory.remove_entry(OsStr::new(name)))
            .is_none()
        {
            reply.error(Errno::EIO);
            return;
        }
        state.nodes.remove(&inode);
        draft_state.retained_scratch.remove(&inode);
        reply.ok();
    }
    fn read(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle.0)
            .cloned();
        let private_bytes = match open_file {
            Some(OpenFile::Document(save, draft)) => {
                Some(save.writer.runtime.block_on(draft.candidate_bytes()))
            }
            Some(OpenFile::Temporary(save, _, scratch, _)) => {
                Some(save.writer.runtime.block_on(scratch.candidate_bytes()))
            }
            None => None,
        };
        if let Some(bytes) = private_bytes {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let end = start.saturating_add(size as usize).min(bytes.len());
            reply.data(bytes.get(start..end).unwrap_or(&[]));
            return;
        }
        let blob = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(node) = state.nodes.get(&inode.0) else {
                reply.error(Errno::ENOENT);
                return;
            };
            match &node.kind {
                NodeKind::Bytes(bytes)
                | NodeKind::Document { bytes, .. }
                | NodeKind::Temporary { bytes, .. } => {
                    let start = match usize::try_from(offset) {
                        Ok(start) if start < bytes.len() => start,
                        _ => {
                            reply.data(&[]);
                            return;
                        }
                    };
                    let end = start.saturating_add(size as usize).min(bytes.len());
                    reply.data(&bytes[start..end]);
                    return;
                }
                NodeKind::BlobBytes(bytes) => {
                    let start = match usize::try_from(offset) {
                        Ok(start) if start < bytes.len() => start,
                        _ => {
                            reply.data(&[]);
                            return;
                        }
                    };
                    let end = start.saturating_add(size as usize).min(bytes.len());
                    reply.data(&bytes[start..end]);
                    return;
                }
                NodeKind::Blob { graph, reference } => Some((
                    Arc::clone(graph),
                    reference.clone(),
                    reference.content_identity().length(),
                )),
                NodeKind::Directory(_)
                | NodeKind::Table(_)
                | NodeKind::KeyDirectory(_)
                | NodeKind::Row(_) => {
                    reply.error(Errno::EISDIR);
                    return;
                }
            }
        };
        let Some((graph, reference, length)) = blob else {
            reply.error(Errno::EIO);
            return;
        };
        if offset >= length {
            reply.data(&[]);
            return;
        }
        let end = offset.saturating_add(u64::from(size)).min(length);
        let scope = match graph.open_read_scope() {
            Ok(scope) => scope,
            Err(_) => {
                reply.error(Errno::EIO);
                return;
            }
        };
        match graph.read_blob_range(&reference, offset..end, &scope) {
            Ok(range) => reply.data(range.bytes()),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn write(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle.0)
            .cloned();
        let is_scratch = matches!(&open_file, Some(OpenFile::Temporary(_, _, _, _)));
        let had_unresolved_handle = draft_state.unresolved_handles.contains_key(&handle.0);
        let had_retained_scratch = draft_state.retained_scratch.contains(&inode.0);
        // A scratch unlinked or renamed away while this handle stayed open is no
        // longer in the namespace; writing through it is not draft state.
        let is_tracked = !is_scratch
            || self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nodes
                .contains_key(&inode.0);
        if !data.is_empty() && is_tracked {
            draft_state.unresolved_handles.insert(handle.0, inode.0);
            if is_scratch {
                draft_state.retained_scratch.insert(inode.0);
            }
        }
        let (result, candidate_bytes) = match open_file {
            Some(OpenFile::Document(save, draft)) => (
                save.writer.runtime.block_on(draft.write_at(offset, data)),
                None,
            ),
            Some(OpenFile::Temporary(save, _, scratch, _)) => {
                let result = save.writer.runtime.block_on(scratch.write_at(offset, data));
                let bytes = result
                    .is_ok()
                    .then(|| save.writer.runtime.block_on(scratch.candidate_bytes()));
                (result, bytes)
            }
            None => {
                if !had_unresolved_handle {
                    draft_state.unresolved_handles.remove(&handle.0);
                }
                reply.error(Errno::EBADF);
                return;
            }
        };
        if result.is_err() {
            if !had_unresolved_handle {
                draft_state.unresolved_handles.remove(&handle.0);
            }
            if is_scratch && !had_retained_scratch {
                draft_state.retained_scratch.remove(&inode.0);
            }
            reply.error(Errno::EIO);
        } else {
            if let Some(bytes) = candidate_bytes {
                self.update_writable_bytes(inode.0, &bytes);
            }
            reply.written(u32::try_from(data.len()).unwrap_or(u32::MAX));
        }
    }

    fn fsync(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: fuser::FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if draft_state.quiescing {
            reply.error(Errno::EBUSY);
            return;
        }
        let open_file = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&handle.0)
            .cloned();
        let is_document = matches!(&open_file, Some(OpenFile::Document(_, _)));
        let result: Result<(), DocumentCommitError> = match open_file {
            Some(OpenFile::Document(save, draft)) => {
                let result = save.commit_document(&draft);
                if result.is_ok() {
                    let bytes = save.writer.runtime.block_on(draft.candidate_bytes());
                    if let Some(NodeKind::Document { bytes: current, .. }) = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .nodes
                        .get_mut(&inode.0)
                        .map(|node| &mut node.kind)
                    {
                        *current = bytes.to_vec();
                    }
                }
                result
            }
            Some(OpenFile::Temporary(save, _, scratch, _)) => {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match state.nodes.get(&inode.0).map(|node| &node.kind) {
                    Some(NodeKind::Temporary {
                        identity, baseline, ..
                    }) => save
                        .fsync_temporary(&scratch, identity, baseline.as_deref())
                        .map_err(DocumentCommitError::Failed),
                    // Rename already accepted and removed this scratch node.
                    None => Ok(()),
                    _ => Err(DocumentCommitError::Failed(
                        "open scratch node is unavailable".to_owned(),
                    )),
                }
            }
            None => Err(DocumentCommitError::Failed(
                "file handle is not open".to_owned(),
            )),
        };
        match result {
            Ok(()) => {
                // A successful fsync makes this handle's bytes durable; nothing is
                // unresolved for it, so unlink must not wait on its RELEASE.
                draft_state.unresolved_handles.remove(&handle.0);
                if is_document {
                    draft_state.rejected_documents.remove(&inode.0);
                }
                reply.ok();
            }
            Err(DocumentCommitError::NoCandidate) => {
                // The handle submitted nothing, so there is nothing to accept or
                // reject and it cannot block unmount. POSIX requires fsync of an
                // unmodified handle to succeed.
                draft_state.unresolved_handles.remove(&handle.0);
                reply.ok();
            }
            Err(DocumentCommitError::Stale | DocumentCommitError::Failed(_)) => {
                // A submission that did not commit (rejected, stale or failed I/O)
                // stays unresolved until a later sync of the same file is accepted.
                if is_document {
                    draft_state.rejected_documents.insert(inode.0);
                }
                reply.error(Errno::EIO);
            }
        }
    }

    fn flush(
        &self,
        _request: &Request,
        _inode: INodeNo,
        _handle: fuser::FileHandle,
        _lock_owner: fuser::LockOwner,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn release(
        &self,
        _request: &Request,
        _inode: INodeNo,
        handle: fuser::FileHandle,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let mut draft_state = self
            .draft_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let released = self
            .open_files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&handle.0);
        if let Some(inode) = draft_state.unresolved_handles.remove(&handle.0)
            && matches!(&released, Some(OpenFile::Temporary(_, _, _, _)))
        {
            draft_state.retained_scratch.insert(inode);
        }
        reply.ok();
    }

    fn setattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        handle: Option<fuser::FileHandle>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        if mode.is_some() || uid.is_some() || gid.is_some() || flags.is_some() {
            reply.error(Errno::EIO);
            return;
        }
        if let Some(size) = size {
            let mut draft_state = self
                .draft_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if draft_state.quiescing {
                reply.error(Errno::EBUSY);
                return;
            }
            if let Some(handle) = handle {
                let open_file = self
                    .open_files
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&handle.0)
                    .cloned();
                let is_scratch = matches!(&open_file, Some(OpenFile::Temporary(_, _, _, _)));
                let had_unresolved_handle = draft_state.unresolved_handles.contains_key(&handle.0);
                let had_retained_scratch = draft_state.retained_scratch.contains(&inode.0);
                let Some(open_file) = open_file else {
                    reply.error(Errno::EBADF);
                    return;
                };
                // A scratch unlinked or renamed away while this handle stayed open is
                // no longer in the namespace; truncating it is not draft state.
                let is_tracked = !is_scratch
                    || self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .nodes
                        .contains_key(&inode.0);
                if is_tracked {
                    draft_state.unresolved_handles.insert(handle.0, inode.0);
                    if is_scratch {
                        draft_state.retained_scratch.insert(inode.0);
                    }
                }
                let (result, candidate_bytes) = match open_file {
                    OpenFile::Document(save, draft) => {
                        (save.writer.runtime.block_on(draft.truncate(size)), None)
                    }
                    OpenFile::Temporary(save, _, scratch, _) => {
                        let result = save.writer.runtime.block_on(scratch.truncate(size));
                        let bytes = result
                            .is_ok()
                            .then(|| save.writer.runtime.block_on(scratch.candidate_bytes()));
                        (result, bytes)
                    }
                };
                if result.is_err() {
                    if !had_unresolved_handle {
                        draft_state.unresolved_handles.remove(&handle.0);
                    }
                    if is_scratch && !had_retained_scratch {
                        draft_state.retained_scratch.remove(&inode.0);
                    }
                    reply.error(Errno::EIO);
                    return;
                }
                if let Some(bytes) = candidate_bytes {
                    self.update_writable_bytes(inode.0, &bytes);
                }
            } else {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !matches!(
                    state.nodes.get(&inode.0).map(|node| &node.kind),
                    Some(NodeKind::Document { .. })
                ) {
                    reply.error(Errno::EROFS);
                    return;
                }
                draft_state.pending_truncates.insert(inode.0, size);
            }
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.attr(inode.0) {
            Some(mut attr) => {
                if let Some(size) = size {
                    attr.size = size;
                    attr.blocks = size.saturating_add(511) / 512;
                }
                reply.attr(&ENTRY_TTL, &attr);
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(
        &self,
        _request: &Request,
        inode: INodeNo,
        _handle: fuser::FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if state.ensure_readdir(inode.0, start).is_err() {
            reply.error(Errno::EIO);
            return;
        }
        let Some(directory) = state.directory(inode.0) else {
            if state.nodes.contains_key(&inode.0) {
                reply.error(Errno::ENOTDIR);
            } else {
                reply.error(Errno::ENOENT);
            }
            return;
        };
        let Some(node) = state.nodes.get(&inode.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        if start == 0 && reply.add(INodeNo(inode.0), 1, FileType::Directory, OsStr::new(".")) {
            reply.ok();
            return;
        }
        if start <= 1
            && reply.add(
                INodeNo(node.parent),
                2,
                FileType::Directory,
                OsStr::new(".."),
            )
        {
            reply.ok();
            return;
        }
        for (index, entry) in directory
            .entries
            .iter()
            .enumerate()
            .skip(start.saturating_sub(2))
        {
            if reply.add(
                INodeNo(entry.inode),
                u64::try_from(index.saturating_add(3)).unwrap_or(u64::MAX),
                entry.kind,
                entry.name.as_os_str(),
            ) {
                break;
            }
        }
        reply.ok();
    }
}

struct FsBuilder {
    nodes: HashMap<u64, FsNode>,
    next_inode: u64,
    writer: Option<Arc<MountWriter>>,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
}

impl FsBuilder {
    fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(
            ROOT_INODE,
            FsNode {
                parent: ROOT_INODE,
                kind: NodeKind::Directory(Directory::new()),
            },
        );
        Self {
            nodes,
            next_inode: ROOT_INODE + 1,
            writer: None,
            table_object_ids: Arc::new(BTreeMap::new()),
        }
    }

    fn attr(&self, inode: u64) -> Option<FileAttr> {
        let node = self.nodes.get(&inode)?;
        let (kind, size, permissions, links) = match &node.kind {
            NodeKind::Directory(_)
            | NodeKind::Table(_)
            | NodeKind::KeyDirectory(_)
            | NodeKind::Row(_) => {
                let directory = node.kind.directory()?;
                (
                    FileType::Directory,
                    0,
                    0o555,
                    2 + u32::try_from(
                        directory
                            .entries
                            .iter()
                            .filter(|entry| entry.kind == FileType::Directory)
                            .count(),
                    )
                    .unwrap_or(u32::MAX - 2),
                )
            }
            NodeKind::Bytes(bytes) => (FileType::RegularFile, bytes.len() as u64, 0o444, 1),
            NodeKind::BlobBytes(bytes) => (FileType::RegularFile, bytes.len() as u64, 0o444, 1),
            NodeKind::Document { bytes, .. } => {
                (FileType::RegularFile, bytes.len() as u64, 0o644, 1)
            }
            NodeKind::Temporary { bytes, .. } => {
                (FileType::RegularFile, bytes.len() as u64, 0o644, 1)
            }
            NodeKind::Blob { reference, .. } => (
                FileType::RegularFile,
                reference.content_identity().length(),
                0o444,
                1,
            ),
        };
        Some(FileAttr {
            ino: INodeNo(inode),
            size,
            blocks: size.saturating_add(511) / 512,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind,
            perm: permissions,
            nlink: links,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }

    fn directory(&self, inode: u64) -> Option<&Directory> {
        self.nodes.get(&inode)?.kind.directory()
    }

    fn directory_mut(&mut self, inode: u64) -> Option<&mut Directory> {
        self.nodes.get_mut(&inode)?.kind.directory_mut()
    }

    fn table_directory(&mut self, module_name: &str, table_name: &str) -> Result<u64, String> {
        let mut parent = ROOT_INODE;
        if !module_name.is_empty() {
            for component in module_name.split('.') {
                parent = self.ensure_directory(parent, &project_component(component))?;
            }
        }
        self.ensure_directory(parent, &project_component(table_name))
    }

    fn add_table_rows(
        &mut self,
        table_directory: u64,
        graph: Arc<NativeGraphContext>,
        rows: Arc<RowMapSnapshot>,
        schema: &ResolvedTableSchema,
        value_context: Format3Context,
        field_names: &HashMap<[u8; 16], String>,
        runtime_table: Option<RuntimeTableIdentity>,
        runtime_field_layout: Option<StoredFieldLayout>,
    ) -> Result<(), String> {
        let pinned_field_layout = stored_field_layout(schema.descriptor())?;
        let source = Arc::new(TableSource {
            graph,
            rows,
            binding: schema.binding().clone(),
            schema: schema.clone(),
            value_context,
            field_names: field_names.clone(),
            pinned_field_layout,
            runtime_table,
            runtime_field_layout,
        });
        let node = self
            .nodes
            .get_mut(&table_directory)
            .ok_or_else(|| "mount table directory is missing".to_owned())?;
        let NodeKind::Directory(directory) =
            std::mem::replace(&mut node.kind, NodeKind::Directory(Directory::new()))
        else {
            return Err("mount table path conflicts with another table".into());
        };
        node.kind = NodeKind::Table(TableDirectory {
            directory,
            source,
            lower: None,
            exhausted: false,
            error: None,
        });
        Ok(())
    }

    fn ensure_directory(&mut self, parent: u64, name: &str) -> Result<u64, String> {
        if let Some(existing) = self
            .directory(parent)
            .and_then(|directory| directory.entry(OsStr::new(name)))
            .map(|entry| entry.inode)
        {
            if self
                .nodes
                .get(&existing)
                .is_some_and(|node| node.kind.is_directory())
            {
                return Ok(existing);
            }
            return Err(format!(
                "mount path component conflicts with a file: {name}"
            ));
        }
        self.insert_node(
            parent,
            name,
            NodeKind::Directory(Directory::new()),
            FileType::Directory,
        )
    }

    fn lookup_child(&mut self, parent: u64, name: &OsStr) -> Result<Option<u64>, String> {
        let parent_kind = self
            .nodes
            .get(&parent)
            .map(|node| &node.kind)
            .ok_or_else(|| "mount lookup parent is missing".to_owned())?;
        if matches!(parent_kind, NodeKind::Row(_)) {
            self.ensure_row_loaded(parent)?;
        }
        if let Some(inode) = self
            .directory(parent)
            .and_then(|directory| directory.entry(name))
            .map(|entry| entry.inode)
        {
            return Ok(Some(inode));
        }
        let table_inode = match self.nodes.get(&parent).map(|node| &node.kind) {
            Some(NodeKind::Table(_)) => Some(parent),
            Some(NodeKind::KeyDirectory(directory)) => Some(directory.table_inode),
            _ => None,
        };
        let Some(table_inode) = table_inode else {
            return Ok(None);
        };
        loop {
            if let Some(inode) = self
                .directory(parent)
                .and_then(|directory| directory.entry(name))
                .map(|entry| entry.inode)
            {
                return Ok(Some(inode));
            }
            let (exhausted, error) = match self.nodes.get(&table_inode).map(|node| &node.kind) {
                Some(NodeKind::Table(table)) => (table.exhausted, table.error.clone()),
                _ => return Err("mount table directory is missing".into()),
            };
            if let Some(error) = error {
                return Err(error);
            }
            if exhausted {
                return Ok(None);
            }
            self.load_next_key_page(table_inode)?;
        }
    }

    fn ensure_readdir(&mut self, inode: u64, start: usize) -> Result<(), String> {
        if matches!(
            self.nodes.get(&inode).map(|node| &node.kind),
            Some(NodeKind::Row(_))
        ) {
            return self.ensure_row_loaded(inode);
        }
        let table_inode = match self.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Table(_)) => Some(inode),
            Some(NodeKind::KeyDirectory(directory)) => Some(directory.table_inode),
            _ => None,
        };
        let Some(table_inode) = table_inode else {
            return Ok(());
        };
        let child_start = start.saturating_sub(2);
        let loaded = self
            .directory(inode)
            .ok_or_else(|| "mount directory is missing".to_owned())?
            .entries
            .len();
        if child_start > loaded {
            return Ok(());
        }
        let target = child_start.saturating_add(ROW_PAGE_LIMIT);
        loop {
            let loaded = self
                .directory(inode)
                .ok_or_else(|| "mount directory is missing".to_owned())?
                .entries
                .len();
            let (exhausted, error) = match self.nodes.get(&table_inode).map(|node| &node.kind) {
                Some(NodeKind::Table(table)) => (table.exhausted, table.error.clone()),
                _ => return Err("mount table directory is missing".into()),
            };
            if loaded >= target || exhausted {
                return Ok(());
            }
            if let Some(error) = error {
                return Err(error);
            }
            self.load_next_key_page(table_inode)?;
        }
    }

    fn load_next_key_page(&mut self, table_inode: u64) -> Result<(), String> {
        let (source, lower, exhausted, error) =
            match self.nodes.get(&table_inode).map(|node| &node.kind) {
                Some(NodeKind::Table(table)) => (
                    Arc::clone(&table.source),
                    table.lower.clone(),
                    table.exhausted,
                    table.error.clone(),
                ),
                _ => return Err("mount table directory is missing".into()),
            };
        if let Some(error) = error {
            return Err(error);
        }
        if exhausted {
            return Ok(());
        }
        let range = KeyRange::new(lower.clone(), None, ROW_PAGE_LIMIT)
            .map_err(|error| format!("row key range could not be created: {error:?}"))?;
        let scope = source
            .graph
            .open_read_scope()
            .map_err(|error| format!("row key read scope could not be opened: {error:?}"))?;
        let keys = match source.graph.range_row_keys(&range, &scope) {
            Ok(keys) => keys,
            Err(error) => {
                let error = format!("committed row keys could not be read: {error:?}");
                self.record_table_error(table_inode, &error);
                return Err(error);
            }
        };
        if keys.is_empty() {
            if let Some(NodeKind::Table(table)) =
                self.nodes.get_mut(&table_inode).map(|node| &mut node.kind)
            {
                table.exhausted = true;
            }
            return Ok(());
        }
        let page_length = keys.len();
        let next_lower = keys.last().cloned();
        if page_length >= ROW_PAGE_LIMIT && next_lower.as_ref() == lower.as_ref() {
            let error = "row-key pagination did not advance".to_owned();
            self.record_table_error(table_inode, &error);
            return Err(error);
        }
        let skip_duplicate = usize::from(lower.is_some());
        for key in keys.into_iter().skip(skip_duplicate) {
            if let Err(error) = self.add_key_path(table_inode, &source, key) {
                self.record_table_error(table_inode, &error);
                return Err(error);
            }
        }
        if let Some(NodeKind::Table(table)) =
            self.nodes.get_mut(&table_inode).map(|node| &mut node.kind)
        {
            table.lower = next_lower;
            table.exhausted = page_length < ROW_PAGE_LIMIT;
        }
        Ok(())
    }

    fn record_table_error(&mut self, table_inode: u64, error: &str) {
        if let Some(NodeKind::Table(table)) =
            self.nodes.get_mut(&table_inode).map(|node| &mut node.kind)
        {
            if table.error.is_none() {
                table.error = Some(error.to_owned());
                eprintln!("FUSE table row listing failed: {error}");
            }
        }
    }

    fn add_key_path(
        &mut self,
        table_inode: u64,
        source: &Arc<TableSource>,
        key: TypedKey,
    ) -> Result<(), String> {
        let mut components = Vec::new();
        flatten_key_components(&key, &mut components)?;
        let Some((leaf, prefix_components)) = components.split_last() else {
            return Err("committed row key has no path components".into());
        };
        let mut parent = table_inode;
        let mut prefix = Vec::with_capacity(prefix_components.len());
        for component in prefix_components {
            prefix.push(component.clone());
            parent = self.ensure_key_directory(
                parent,
                table_inode,
                &prefix,
                &key_component(component)?,
            )?;
        }
        self.ensure_row_directory(parent, source, key, &key_component(leaf)?)?;
        Ok(())
    }

    fn ensure_key_directory(
        &mut self,
        parent: u64,
        table_inode: u64,
        prefix: &[TypedKey],
        name: &str,
    ) -> Result<u64, String> {
        if let Some(existing) = self
            .directory(parent)
            .and_then(|directory| directory.entry(OsStr::new(name)))
            .map(|entry| entry.inode)
        {
            let matches = self.nodes.get(&existing).is_some_and(|node| {
                matches!(
                    &node.kind,
                    NodeKind::KeyDirectory(directory)
                        if directory.table_inode == table_inode && directory.prefix == prefix
                )
            });
            return if matches {
                Ok(existing)
            } else {
                Err("committed row key path collides with another projected entry".into())
            };
        }
        self.insert_node(
            parent,
            name,
            NodeKind::KeyDirectory(KeyDirectory {
                directory: Directory::new(),
                table_inode,
                prefix: prefix.to_vec(),
            }),
            FileType::Directory,
        )
    }

    fn ensure_row_directory(
        &mut self,
        parent: u64,
        source: &Arc<TableSource>,
        key: TypedKey,
        name: &str,
    ) -> Result<u64, String> {
        if let Some(existing) = self
            .directory(parent)
            .and_then(|directory| directory.entry(OsStr::new(name)))
            .map(|entry| entry.inode)
        {
            let matches = self
                .nodes
                .get(&existing)
                .is_some_and(|node| matches!(&node.kind, NodeKind::Row(row) if row.key == key));
            return if matches {
                Ok(existing)
            } else {
                Err("committed row key path collides with another projected entry".into())
            };
        }
        self.insert_node(
            parent,
            name,
            NodeKind::Row(RowDirectory {
                directory: Directory::new(),
                source: Arc::clone(source),
                key,
                state: RowLoadState::Unloaded,
            }),
            FileType::Directory,
        )
    }

    fn ensure_row_loaded(&mut self, inode: u64) -> Result<(), String> {
        let (source, key, state) = match self.nodes.get(&inode).map(|node| &node.kind) {
            Some(NodeKind::Row(row)) => (
                Arc::clone(&row.source),
                row.key.clone(),
                match &row.state {
                    RowLoadState::Unloaded => None,
                    RowLoadState::Ready => Some(Ok(())),
                    RowLoadState::Failed(error) => Some(Err(error.clone())),
                },
            ),
            _ => return Err("mount row directory is missing".into()),
        };
        if let Some(state) = state {
            return state;
        }
        let result = self.row_file_specs(&source, &key).and_then(|files| {
            let directory = self
                .directory(inode)
                .ok_or_else(|| "mount row directory is missing".to_owned())?;
            let mut names = HashMap::with_capacity(files.len());
            for (name, _) in &files {
                let name = OsString::from(name);
                if directory.entry(name.as_os_str()).is_some() || names.insert(name, ()).is_some() {
                    return Err("projected row file names collide".into());
                }
            }
            for (name, kind) in files {
                self.add_file(inode, &name, kind)?;
            }
            Ok(())
        });
        if let Some(NodeKind::Row(row)) = self.nodes.get_mut(&inode).map(|node| &mut node.kind) {
            row.state = match &result {
                Ok(()) => RowLoadState::Ready,
                Err(error) => RowLoadState::Failed(error.clone()),
            };
        }
        if let Err(error) = &result {
            eprintln!("FUSE row projection failed: {error}");
        }
        result
    }

    fn row_file_specs(
        &self,
        source: &Arc<TableSource>,
        key: &TypedKey,
    ) -> Result<Vec<(String, NodeKind)>, String> {
        let row_scope = source
            .graph
            .open_read_scope()
            .map_err(|error| format!("row read scope could not be opened: {error:?}"))?;
        let row = source
            .graph
            .lookup_row(key, &row_scope)
            .map_err(|error| format!("committed row could not be looked up: {error:?}"))?
            .ok_or_else(|| {
                "indexed committed row disappeared from the pinned snapshot".to_owned()
            })?;
        let decoded = source
            .graph
            .decode_admitted_row_fields(&row, &source.binding, &row_scope)
            .map_err(|error| format!("committed row fields could not be decoded: {error:?}"))?;
        let pinned_fields = decode_canonical_storage_value(decoded.canonical_fields())
            .map_err(|error| format!("committed row values could not be decoded: {error:?}"))?;
        // Runtime rows are joined by current table identity and the exact pinned
        // key. Rekeys have no row-lineage metadata, so they remain pinned here.
        let runtime_row = match (&self.writer, &source.runtime_table) {
            (Some(writer), Some(table)) => {
                let runtime_key = runtime_table_key_bytes(key)?;
                let canonical_key = key
                    .canonical_bytes()
                    .map_err(|error| format!("committed row key cannot be encoded: {error:?}"))?;
                writer
                    .row_baseline(table, &runtime_key)?
                    .filter(|row| {
                        source
                            .schema
                            .descriptor()
                            .validate_stored_row(
                                *source.binding.relation_id(),
                                &canonical_key,
                                row,
                            )
                            .is_ok()
                    })
            }
            _ => None,
        };
        let (canonical_fields, _projection_source) = select_runtime_row_fields(
            runtime_row,
            pinned_fields,
            &source.pinned_field_layout.fields,
            source
                .runtime_field_layout
                .as_ref()
                .map(|layout| layout.fields.as_slice()),
            decoded.fields().iter().map(|field| *field.field_id()),
        );
        let Raw::Array(values) = canonical_fields else {
            return Err("committed row values are not a canonical field array".into());
        };
        if values.len() != decoded.fields().len() {
            return Err("committed row value and field counts differ".into());
        }

        let mut projected_fields = Vec::with_capacity(values.len());
        let mut blob_files = Vec::new();
        let mut blob_fields = Vec::new();
        for (field, value) in decoded.fields().iter().zip(values) {
            let field_name = source
                .field_names
                .get(field.field_id())
                .ok_or_else(|| "committed field has no source identity".to_owned())?;
            let field_type = source
                .pinned_field_layout
                .field_type(field.field_id())
                .ok_or_else(|| "committed field has no schema type".to_owned())?;
            if let Some(metadata) = field.blob_metadata() {
                let value_oid = metadata
                    .descriptor_oid()
                    .ok_or_else(|| "committed Blob field has no descriptor identity".to_owned())?;
                let algorithm = match value_oid.len() {
                    20 => GitHashAlgorithm::Sha1,
                    32 => GitHashAlgorithm::Sha256,
                    _ => return Err("committed Blob descriptor has an invalid OID width".into()),
                };
                let descriptor = NativeOid::new(algorithm, value_oid.as_bytes())
                    .map_err(|error| format!("committed Blob descriptor is invalid: {error:?}"))?;
                let stored = source
                    .graph
                    .read_stored_blob_value(&row, &descriptor, &row_scope)
                    .map_err(|error| {
                        format!("committed Blob reference could not be admitted: {error:?}")
                    })?;
                let annotation = stored.annotation();
                let suffix = annotation.suffix().or(metadata.suffix()).map(str::to_owned);
                let media_type = annotation.media_type().to_owned();
                let file_name = orna_runtime_v1::vfs::project_content_name(
                    field_name,
                    annotation.media_type(),
                    suffix.as_deref(),
                );
                let projected = if is_optional_field(field_type) {
                    VfsStoredField::content_optional(
                        field_name.clone(),
                        annotation.media_type(),
                        suffix.clone(),
                        metadata.length(),
                    )
                } else {
                    VfsStoredField::content(
                        field_name.clone(),
                        annotation.media_type(),
                        suffix.clone(),
                        metadata.length(),
                    )
                };
                projected_fields.push(projected);
                blob_fields.push(BlobField {
                    name: field_name.clone(),
                    file_name: file_name.clone(),
                    media_type,
                    suffix,
                    reference: stored.reference().clone(),
                    replacement_bytes: None,
                });
                blob_files.push((file_name, stored.reference().clone()));
            } else if stored_value_is_null(&value) {
                projected_fields.push(VfsStoredField::null(field_name.clone()));
            } else {
                projected_fields.push(VfsStoredField::document(
                    field_name.clone(),
                    render_stored_source_value(&value)?,
                ));
            }
        }
        let projection = VfsRowProjection::project(
            SnapshotPin::capture(Arc::clone(&source.graph)),
            &projected_fields,
        )
        .map_err(|error| format!("committed row projection is invalid: {error:?}"))?;
        let mut files = Vec::with_capacity(projection.files().len() + 1);
        let document_bytes = projection.document_text().as_bytes().to_vec();
        let document = match &self.writer {
            Some(writer) => NodeKind::Document {
                save: row_save_context(
                    writer,
                    source,
                    key,
                    Arc::clone(&self.table_object_ids),
                    &document_bytes,
                    blob_fields,
                )?,
                bytes: document_bytes,
            },
            None => NodeKind::Bytes(document_bytes),
        };
        files.push(("data.orna".to_owned(), document));
        for file in projection.files() {
            let reference = blob_files
                .iter()
                .find_map(|(name, reference)| (name == file.name()).then_some(reference))
                .ok_or_else(|| "projected Blob has no admitted file reference".to_owned())?;
            files.push((
                file.name().to_owned(),
                NodeKind::Blob {
                    graph: Arc::clone(&source.graph),
                    reference: reference.clone(),
                },
            ));
        }
        Ok(files)
    }

    fn add_file(&mut self, parent: u64, name: &str, kind: NodeKind) -> Result<u64, String> {
        if self
            .directory(parent)
            .is_none_or(|directory| directory.entry(OsStr::new(name)).is_some())
        {
            return Err(format!("mount path contains a duplicate file name: {name}"));
        }
        let file_type = match &kind {
            NodeKind::Directory(_)
            | NodeKind::Table(_)
            | NodeKind::KeyDirectory(_)
            | NodeKind::Row(_) => FileType::Directory,
            NodeKind::Bytes(_)
            | NodeKind::BlobBytes(_)
            | NodeKind::Document { .. }
            | NodeKind::Temporary { .. }
            | NodeKind::Blob { .. } => FileType::RegularFile,
        };
        self.insert_node(parent, name, kind, file_type)
    }

    fn insert_node(
        &mut self,
        parent: u64,
        name: &str,
        kind: NodeKind,
        file_type: FileType,
    ) -> Result<u64, String> {
        let inode = self.next_inode;
        let next_inode = self
            .next_inode
            .checked_add(1)
            .ok_or_else(|| "mount tree has too many entries".to_owned())?;
        let parent_node = self
            .nodes
            .get_mut(&parent)
            .ok_or_else(|| "mount parent directory is missing".to_owned())?;
        let directory = parent_node
            .kind
            .directory_mut()
            .ok_or_else(|| "mount parent is not a directory".to_owned())?;
        let name = OsString::from(name);
        if directory.entry(name.as_os_str()).is_some() {
            return Err("mount directory contains a duplicate entry".into());
        }
        let index = directory.entries.len();
        directory.by_name.insert(name.clone(), index);
        directory.entries.push(DirectoryEntry {
            inode,
            name,
            kind: file_type,
        });
        self.next_inode = next_inode;
        self.nodes.insert(inode, FsNode { parent, kind });
        Ok(inode)
    }
}

struct FsNode {
    parent: u64,
    kind: NodeKind,
}

enum NodeKind {
    Directory(Directory),
    Table(TableDirectory),
    KeyDirectory(KeyDirectory),
    Row(RowDirectory),
    Bytes(Vec<u8>),
    Document {
        bytes: Vec<u8>,
        save: Arc<RowSaveContext>,
    },
    Temporary {
        bytes: Vec<u8>,
        save: Arc<RowSaveContext>,
        target: Arc<ManagedFile<RowMapSnapshot>>,
        scratch: Arc<TemporarySave<RowMapSnapshot>>,
        blob_field: Option<BlobField>,
        baseline: Option<Vec<u8>>,
        identity: Arc<TemporaryScratchIdentity>,
    },
    Blob {
        graph: Arc<NativeGraphContext>,
        reference: AdmittedBlobReference,
    },
    BlobBytes(Arc<[u8]>),
}

impl NodeKind {
    fn directory(&self) -> Option<&Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&table.directory),
            Self::KeyDirectory(directory) => Some(&directory.directory),
            Self::Row(row) => Some(&row.directory),
            Self::Bytes(_)
            | Self::BlobBytes(_)
            | Self::Document { .. }
            | Self::Temporary { .. }
            | Self::Blob { .. } => None,
        }
    }

    fn directory_mut(&mut self) -> Option<&mut Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&mut table.directory),
            Self::KeyDirectory(directory) => Some(&mut directory.directory),
            Self::Row(row) => Some(&mut row.directory),
            Self::Bytes(_)
            | Self::BlobBytes(_)
            | Self::Document { .. }
            | Self::Temporary { .. }
            | Self::Blob { .. } => None,
        }
    }

    fn is_directory(&self) -> bool {
        self.directory().is_some()
    }
}

struct Directory {
    entries: Vec<DirectoryEntry>,
    by_name: HashMap<OsString, usize>,
}

impl Directory {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    fn entry(&self, name: &OsStr) -> Option<&DirectoryEntry> {
        self.by_name
            .get(name)
            .and_then(|index| self.entries.get(*index))
    }
    fn remove_entry(&mut self, name: &OsStr) -> Option<DirectoryEntry> {
        let index = self.by_name.remove(name)?;
        let removed = self.entries.swap_remove(index);
        if let Some(moved) = self.entries.get(index) {
            self.by_name.insert(moved.name.clone(), index);
        }
        Some(removed)
    }
}

struct TableDirectory {
    directory: Directory,
    source: Arc<TableSource>,
    lower: Option<TypedKey>,
    exhausted: bool,
    error: Option<String>,
}

struct KeyDirectory {
    directory: Directory,
    table_inode: u64,
    prefix: Vec<TypedKey>,
}

struct RowDirectory {
    directory: Directory,
    source: Arc<TableSource>,
    key: TypedKey,
    state: RowLoadState,
}

enum RowLoadState {
    Unloaded,
    Ready,
    Failed(String),
}

struct TableSource {
    graph: Arc<NativeGraphContext>,
    rows: Arc<RowMapSnapshot>,
    binding: CommittedTableBinding,
    schema: ResolvedTableSchema,
    value_context: Format3Context,
    field_names: HashMap<[u8; 16], String>,
    pinned_field_layout: StoredFieldLayout,
    runtime_table: Option<RuntimeTableIdentity>,
    runtime_field_layout: Option<StoredFieldLayout>,
}
struct DirectoryEntry {
    inode: u64,
    name: OsString,
    kind: FileType,
}

fn flatten_key_components(key: &TypedKey, components: &mut Vec<TypedKey>) -> Result<(), String> {
    match key {
        TypedKey::Tuple(values) if values.is_empty() => {
            Err("composite row key has no components".into())
        }
        TypedKey::Tuple(values) => {
            for value in values {
                flatten_key_components(value, components)?;
            }
            Ok(())
        }
        _ => {
            components.push(key.clone());
            Ok(())
        }
    }
}

fn field_names_for_table(
    identities: &ObjectIdentityManifest,
    qualified_table: &str,
) -> HashMap<[u8; 16], String> {
    let prefix = format!("{qualified_table}.");
    identities
        .records()
        .iter()
        .filter(|identity| identity.kind() == ObjectKind::Field && !identity.is_retired())
        .filter_map(|identity| {
            identity
                .qualified_name()
                .strip_prefix(&prefix)
                .map(|name| (*identity.id(), name.to_owned()))
        })
        .collect()
}

fn stored_field_layout(descriptor: &SchemaDescriptor) -> Result<StoredFieldLayout, String> {
    let Raw::Map(schema) = descriptor.raw() else {
        return Err("committed table schema is not a canonical map".into());
    };
    let fields_key = Raw::Int(3.into());
    let fields = schema
        .iter()
        .find_map(|(key, value)| (key == &fields_key).then_some(value))
        .ok_or_else(|| "committed table schema has no fields".to_owned())?;
    let Raw::Array(fields) = fields else {
        return Err("committed table schema fields are not an array".into());
    };
    let stored_role = Raw::Int(1.into());
    let mut stored_fields = Vec::with_capacity(fields.len());
    let mut indexes = HashMap::with_capacity(fields.len());
    for field in fields {
        let Raw::Array(parts) = field else {
            return Err("committed table schema field is not an array".into());
        };
        let [Raw::Tag(37, identity), _, field_type, role, _] = parts.as_slice() else {
            return Err("committed table schema field is malformed".into());
        };
        if role != &stored_role {
            continue;
        }
        let Raw::Bytes(identity) = identity.as_ref() else {
            return Err("committed field identity is not a UUID".into());
        };
        let identity: [u8; 16] = identity
            .as_slice()
            .try_into()
            .map_err(|_| "committed field identity has the wrong width")?;
        let index = stored_fields.len();
        if indexes.insert(identity, index).is_some() {
            return Err("committed table schema repeats a stored field".into());
        }
        stored_fields.push(StoredFieldShape {
            field_id: identity,
            field_type: field_type.clone(),
        });
    }
    Ok(StoredFieldLayout {
        fields: stored_fields,
        indexes,
    })
}

fn is_optional_field(field_type: &Raw) -> bool {
    matches!(field_type, Raw::Array(parts) if parts.len() == 2 && parts[0] == Raw::Int(2.into()))
}

fn stored_value_is_null(value: &Raw) -> bool {
    matches!(value, Raw::Null)
        || matches!(value, Raw::Tag(60013, option) if matches!(option.as_ref(), Raw::Array(parts) if matches!(parts.as_slice(), [Raw::Int(index)] if index.to_string() == "0")))
}

fn render_stored_source_value(value: &Raw) -> Result<String, String> {
    match value {
        Raw::Null => Ok("null".into()),
        Raw::Bool(value) => Ok(value.to_string()),
        Raw::Int(value) => Ok(value.to_string()),
        Raw::Float(bits) => {
            let value = f64::from_bits(*bits);
            if !value.is_finite() {
                return Err("committed row contains a non-finite source number".into());
            }
            Ok(format!("{value:?}f"))
        }
        Raw::Text(value) => serde_json::to_string(value)
            .map_err(|error| format!("stored string could not be rendered: {error}")),
        Raw::Bytes(_) => {
            Err("committed row contains a byte value without a source spelling".into())
        }
        Raw::Array(values) => {
            let values = values
                .iter()
                .map(render_stored_source_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[{}]", values.join(", ")))
        }
        Raw::Map(entries) => {
            let fields = entries
                .iter()
                .map(|(key, value)| {
                    let Raw::Text(name) = key else {
                        return Err("stored record has a non-text field name".to_owned());
                    };
                    Ok(format!("{name}: {}", render_stored_source_value(value)?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(format!("{{ {} }}", fields.join(", ")))
        }
        Raw::Tag(60000, decimal) => {
            let Raw::Array(parts) = decimal.as_ref() else {
                return Err("stored decimal has an invalid payload".into());
            };
            let [Raw::Int(coefficient), Raw::Int(exponent)] = parts.as_slice() else {
                return Err("stored decimal has an invalid payload".into());
            };
            Ok(format!("{coefficient}e{exponent}.decimal"))
        }
        Raw::Tag(60001, date) => match date.as_ref() {
            Raw::Text(date) => Ok(date.clone()),
            _ => Err("stored date has an invalid payload".into()),
        },
        Raw::Tag(60002, instant) => render_stored_instant(instant),
        Raw::Tag(60013, option) => {
            let Raw::Array(parts) = option.as_ref() else {
                return Err("stored optional field has an invalid payload".into());
            };
            match parts.as_slice() {
                [Raw::Int(index)] if index.to_string() == "0" => Ok("null".into()),
                [Raw::Int(index), value] if index.to_string() == "1" => {
                    render_stored_source_value(value)
                }
                _ => Err("stored optional field has an invalid value".into()),
            }
        }
        Raw::Tag(60014, _) => Ok("()".into()),
        Raw::Tag(60015, tuple) => {
            let Raw::Array(values) = tuple.as_ref() else {
                return Err("stored tuple has an invalid payload".into());
            };
            let values = values
                .iter()
                .map(render_stored_source_value)
                .collect::<Result<Vec<_>, _>>()?;
            let mut body = values.join(", ");
            if values.len() == 1 {
                body.push(',');
            }
            Ok(format!("({body})"))
        }
        Raw::Tag(60009, record) => {
            let Raw::Array(parts) = record.as_ref() else {
                return Err("stored record has an invalid payload".into());
            };
            let [_, Raw::Array(entries)] = parts.as_slice() else {
                return Err("stored record has an invalid payload".into());
            };
            let fields = entries
                .iter()
                .map(|entry| {
                    let Raw::Array(parts) = entry else {
                        return Err("stored record field is malformed".to_owned());
                    };
                    let [Raw::Text(name), value] = parts.as_slice() else {
                        return Err("stored record field has no source name".to_owned());
                    };
                    Ok(format!("{name}: {}", render_stored_source_value(value)?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(format!("{{ {} }}", fields.join(", ")))
        }
        Raw::Tag(tag, _) => Err(format!(
            "committed row value tag {tag} has no source spelling"
        )),
    }
}

fn render_stored_instant(instant: &Raw) -> Result<String, String> {
    let Raw::Array(parts) = instant else {
        return Err("stored instant has an invalid payload".into());
    };
    let [Raw::Int(seconds), Raw::Int(nanoseconds)] = parts.as_slice() else {
        return Err("stored instant has an invalid payload".into());
    };
    let seconds = seconds
        .to_string()
        .parse::<i64>()
        .map_err(|_| "stored instant seconds are out of range")?;
    let nanoseconds = nanoseconds
        .to_string()
        .parse::<u32>()
        .map_err(|_| "stored instant nanoseconds are out of range")?;
    if nanoseconds >= 1_000_000_000 {
        return Err("stored instant nanoseconds are out of range".into());
    }
    let day = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, date) = civil_date_from_days(day);
    if !(0..=9999).contains(&year) {
        return Err("stored instant is outside the source calendar range".into());
    }
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    let mut rendered = format!("{year:04}-{month:02}-{date:02}T{hour:02}:{minute:02}:{second:02}");
    if nanoseconds != 0 {
        let mut fraction = format!("{nanoseconds:09}");
        while fraction.ends_with('0') {
            fraction.pop();
        }
        rendered.push('.');
        rendered.push_str(&fraction);
    }
    rendered.push('Z');
    Ok(rendered)
}

fn civil_date_from_days(days: i64) -> (i64, i64, i64) {
    let adjusted = days + 719_468;
    let era = if adjusted >= 0 {
        adjusted
    } else {
        adjusted - 146_096
    } / 146_097;
    let day_of_era = adjusted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month, date)
}

fn key_component(key: &TypedKey) -> Result<String, String> {
    let canonical_text = match key {
        TypedKey::Null => "null".to_owned(),
        TypedKey::Bool(value) => value.to_string(),
        TypedKey::UInt(value) => value.to_string(),
        TypedKey::Int(value) => value.to_string(),
        TypedKey::Bytes(bytes) => format!("0x{}", hex_bytes(bytes)),
        TypedKey::Text(value) => value.clone(),
        TypedKey::Tuple(_) => {
            return Err("tuple key must be projected component by component".into());
        }
    };
    Ok(project_component(&canonical_text))
}

/// Removes projected Blob summary fields from a source row before row admission.
/// The selected values are supplied separately by owner-issued Blob bindings.
fn source_without_fields(source: &str, omitted: &[String]) -> Result<String, String> {
    let parsed = parse_row(source);
    if !parsed.diagnostics.is_empty() {
        return Err("mounted row document is not a valid source row".into());
    }
    let Expr::Record { fields, .. } = parsed.value else {
        return Err("mounted row document is not a record".into());
    };
    let mut ranges = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        if !omitted.iter().any(|name| name == &field.name) {
            continue;
        }
        let start = field.span.start;
        let end = field.span.end;
        let next_start = fields
            .get(index + 1)
            .map_or(source.len(), |next| next.span.start);
        if let Some(relative_comma) = source[end..next_start].find(',') {
            ranges.push(start..end + relative_comma + 1);
        } else if index > 0 {
            let previous_end = fields[index - 1].span.end;
            let relative_comma = source[previous_end..start]
                .rfind(',')
                .ok_or_else(|| "mounted row field separator is missing".to_owned())?;
            ranges.push(previous_end + relative_comma..end);
        } else {
            ranges.push(start..end);
        }
    }
    ranges.sort_by_key(|range| range.start);
    let mut output = source.to_owned();
    for range in ranges.into_iter().rev() {
        output.replace_range(range, "");
    }
    Ok(output)
}

fn is_replacement_scratch_name(name: &str) -> bool {
    is_replacement_scratch_for(name, "data.orna")
}

fn is_editor_temporary_name(name: &str) -> bool {
    is_replacement_scratch_name(name)
        || name.strip_prefix("tmp").is_some_and(|suffix| {
            suffix.len() == 6 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn is_replacement_scratch_for(name: &str, file_name: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_prefix(file_name))
        .and_then(|name| name.strip_prefix('.'))
    else {
        return false;
    };
    !suffix.is_empty() && suffix != "." && suffix != ".." && !suffix.contains('/') && suffix != "/"
}
fn hex_bytes(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
fn key_directory(builder: &mut FsBuilder, parent: u64, key: &TypedKey) -> Result<u64, String> {
    let mut components = Vec::new();
    flatten_key_components(key, &mut components)?;
    let mut directory = parent;
    let mut prefix = Vec::new();
    for component in components {
        prefix.push(component.clone());
        directory = builder.ensure_key_directory(
            directory,
            parent,
            &prefix,
            &key_component(&component)?,
        )?;
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{
        Expr, FsBuilder, NodeKind, ROOT_INODE, RowProjectionSource, StoredFieldShape,
        is_editor_temporary_name, is_replacement_scratch_for, is_replacement_scratch_name,
        key_directory, parse_row, runtime_row_baseline_from_rows, runtime_table_key_bytes,
        select_runtime_row_fields, source_without_fields,
    };
    use orna_repository_v1::TypedKey;
    use orna_runtime_v1::{
        RuntimeTableIdentity, RuntimeTableRows, TableObjectId, vfs::project_component,
    };
    use orna_value_v1::{Raw, Value};
    fn encode_fields(fields: Vec<Raw>) -> Vec<u8> {
        Value::new(Raw::Array(fields)).unwrap().encode().unwrap()
    }

    #[test]
    fn renamed_tables_use_current_identity_while_rekeys_keep_the_pinned_boundary() {
        let id = [1; 16];
        let current_table =
            RuntimeTableIdentity::new("Renamed", TableObjectId::new(id)).unwrap();
        let pinned_table =
            RuntimeTableIdentity::new("OldName", TableObjectId::new(id)).unwrap();
        let layout = [StoredFieldShape {
            field_id: id,
            field_type: Raw::Text("Str".into()),
        }];
        let pinned = Raw::Array(vec![Raw::Text("pinned value".into())]);
        let current_bytes = encode_fields(vec![Raw::Text("current value".into())]);
        let renamed_rows: RuntimeTableRows = [(
            "Renamed".to_owned(),
            vec![(b"same-key".to_vec(), current_bytes.clone())],
        )]
        .into();

        let current_row =
            runtime_row_baseline_from_rows(&renamed_rows, &current_table, b"same-key").unwrap();
        assert_eq!(current_row, Some(current_bytes.as_slice()));
        let (projected, source) = select_runtime_row_fields(
            current_row,
            pinned.clone(),
            &layout,
            Some(&layout),
            [id].into_iter(),
        );
        assert_eq!(source, RowProjectionSource::RuntimeSnapshot);
        assert_eq!(
            projected,
            Raw::Array(vec![Raw::Text("current value".into())])
        );
        assert_eq!(
            runtime_row_baseline_from_rows(&renamed_rows, &pinned_table, b"same-key"),
            None
        );

        let rekeyed_rows: RuntimeTableRows = [(
            "Renamed".to_owned(),
            vec![(b"new-key".to_vec(), current_bytes)],
        )]
        .into();
        let current =
            runtime_row_baseline_from_rows(&rekeyed_rows, &current_table, b"old-key").unwrap();
        assert_eq!(current, None);
        let (projected, source) = select_runtime_row_fields(
            current,
            pinned.clone(),
            &layout,
            Some(&layout),
            [id].into_iter(),
        );
        assert_eq!(source, RowProjectionSource::PinnedSnapshot);
        assert_eq!(projected, pinned);
    }

    #[test]
    fn equal_length_field_order_and_schema_changes_reject_runtime_rows() {
        let pinned_layout = [
            StoredFieldShape {
                field_id: [1; 16],
                field_type: Raw::Text("Str".into()),
            },
            StoredFieldShape {
                field_id: [2; 16],
                field_type: Raw::Text("Int".into()),
            },
        ];
        let runtime_bytes = encode_fields(vec![Raw::Text("title".into()), Raw::Int(7.into())]);
        let pinned_fields = Raw::Array(vec![Raw::Text("old title".into()), Raw::Int(6.into())]);
        let reordered_layout = [pinned_layout[1].clone(), pinned_layout[0].clone()];
        let changed_schema_layout = [
            pinned_layout[0].clone(),
            StoredFieldShape {
                field_id: [2; 16],
                field_type: Raw::Text("Bool".into()),
            },
        ];

        for runtime_layout in [&reordered_layout[..], &changed_schema_layout[..]] {
            let (projected, source) = select_runtime_row_fields(
                Some(&runtime_bytes),
                pinned_fields.clone(),
                &pinned_layout,
                Some(runtime_layout),
                [[1; 16], [2; 16]].into_iter(),
            );
            assert_eq!(source, RowProjectionSource::PinnedSnapshot);
            assert_eq!(projected, pinned_fields);
        }
    }

    #[test]
    fn lazy_row_lookups_share_the_captured_runtime_generation() {
        let generation_one: RuntimeTableRows = [(
            "Music".to_owned(),
            vec![
                (b"one".to_vec(), encode_fields(vec![Raw::Text("one-v1".into())])),
                (b"two".to_vec(), encode_fields(vec![Raw::Text("two-v1".into())])),
            ],
        )]
        .into();
        let generation_two: RuntimeTableRows = [(
            "Music".to_owned(),
            vec![
                (b"one".to_vec(), encode_fields(vec![Raw::Text("one-v2".into())])),
                (b"two".to_vec(), encode_fields(vec![Raw::Text("two-v2".into())])),
            ],
        )]
        .into();
        let table = RuntimeTableIdentity::new("Music", TableObjectId::new([3; 16])).unwrap();
        let mount_snapshot = std::sync::OnceLock::new();
        assert!(mount_snapshot.set(generation_one).is_ok());

        let first = runtime_row_baseline_from_rows(mount_snapshot.get().unwrap(), &table, b"one")
            .unwrap()
            .unwrap();
        let second = runtime_row_baseline_from_rows(mount_snapshot.get().unwrap(), &table, b"two")
            .unwrap()
            .unwrap();
        let later_second = runtime_row_baseline_from_rows(&generation_two, &table, b"two")
            .unwrap()
            .unwrap();
        assert_eq!(
            Value::decode(first).unwrap().raw(),
            &Raw::Array(vec![Raw::Text("one-v1".into())]),
        );
        assert_eq!(
            Value::decode(second).unwrap().raw(),
            &Raw::Array(vec![Raw::Text("two-v1".into())]),
        );
        assert_eq!(
            Value::decode(later_second).unwrap().raw(),
            &Raw::Array(vec![Raw::Text("two-v2".into())]),
        );
    }

    #[test]
    fn runtime_table_keys_use_persisted_text_bytes_and_typed_encoding_otherwise() {
        assert_eq!(
            runtime_table_key_bytes(&TypedKey::Text("london".into())).unwrap(),
            b"london"
        );

        let numeric = TypedKey::Int(7);
        assert_eq!(
            runtime_table_key_bytes(&numeric).unwrap(),
            numeric.canonical_bytes().unwrap()
        );
    }
    #[test]
    fn source_row_blob_fields_are_omitted_for_owner_bindings() {
        let source = r#"{ title: "song", content: { path: "./content.mp3", media_type: "audio/mpeg" }, artist: "A", }"#;
        let without_content = source_without_fields(source, &["content".to_owned()]).unwrap();
        let parsed = parse_row(&without_content);
        assert!(parsed.diagnostics.is_empty());
        let Expr::Record { fields, .. } = parsed.value else {
            panic!("row remains a record");
        };
        assert_eq!(
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            ["title", "artist"]
        );
    }

    #[test]
    fn only_replacement_shaped_sibling_names_are_candidates() {
        assert!(is_replacement_scratch_name(".data.orna.tmp"));
        assert!(is_replacement_scratch_name(".data.orna.12345"));
        assert!(!is_replacement_scratch_name("unrelated"));
        assert!(!is_replacement_scratch_name("backup"));
        assert!(!is_replacement_scratch_name(".data.orna."));
        assert!(is_editor_temporary_name("tmpldofdR"));
        assert!(is_editor_temporary_name("tmpBX4D0m"));
        assert!(!is_editor_temporary_name("tmp"));
        assert!(!is_editor_temporary_name("tmpabcde"));
        assert!(!is_editor_temporary_name("unrelated"));
    }

    #[test]
    fn blob_replacement_scratch_names_stay_bound_to_their_target() {
        assert!(is_replacement_scratch_for(
            ".content.mp3.tmp",
            "content.mp3"
        ));
        assert!(is_replacement_scratch_for(
            ".content.mp3.12345",
            "content.mp3"
        ));
        assert!(!is_replacement_scratch_for("unrelated", "content.mp3"));
        assert!(!is_replacement_scratch_for(".content.mp3.", "content.mp3"));
    }

    #[test]
    fn table_and_text_key_paths_use_runtime_components() {
        let mut builder = FsBuilder::new();
        let table = builder.table_directory("Music", "data.orna").unwrap();
        let row = key_directory(&mut builder, table, &TypedKey::Text("".into())).unwrap();

        let NodeKind::Directory(root) = &builder.nodes.get(&ROOT_INODE).unwrap().kind else {
            panic!("root is a directory");
        };
        let module = root.entry(OsStr::new("Music")).unwrap();
        let NodeKind::Directory(tables) = &builder.nodes.get(&module.inode).unwrap().kind else {
            panic!("module path is a directory");
        };
        let table_entry = tables
            .entry(OsStr::new(&project_component("data.orna")))
            .unwrap();
        assert_eq!(table_entry.inode, table);
        let NodeKind::Directory(rows) = &builder.nodes.get(&table).unwrap().kind else {
            panic!("table path is a directory");
        };
        let row_entry = rows.entry(OsStr::new(&project_component(""))).unwrap();
        assert_eq!(row_entry.inode, row);
        assert!(matches!(
            builder.nodes.get(&row).unwrap().kind,
            NodeKind::KeyDirectory(_)
        ));
    }

    #[test]
    fn composite_keys_use_one_projected_path_component_per_part() {
        let mut builder = FsBuilder::new();
        let table = builder.table_directory("Music", "Music").unwrap();
        let row = key_directory(
            &mut builder,
            table,
            &TypedKey::Tuple(vec![TypedKey::Text("live/in".into()), TypedKey::Int(7)]),
        )
        .unwrap();

        let NodeKind::Directory(table_entries) = &builder.nodes.get(&table).unwrap().kind else {
            panic!("table path is a directory");
        };
        let first = table_entries
            .entry(OsStr::new(&project_component("live/in")))
            .unwrap();
        let NodeKind::KeyDirectory(first_entries) = &builder.nodes.get(&first.inode).unwrap().kind
        else {
            panic!("first key component is a directory");
        };
        let second = first_entries
            .directory
            .entry(OsStr::new(&project_component("7")))
            .unwrap();
        assert_eq!(second.inode, row);
    }
}
