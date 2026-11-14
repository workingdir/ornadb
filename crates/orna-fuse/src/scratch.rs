use super::*;

pub(crate) const VFS_SCRATCH_DIRECTORY: &str = "vfs-editor-scratch-v1";
pub(crate) const VFS_SCRATCH_TEMP_PREFIX: &str = ".scratch-save-";
/// ORNAVFS2 records carry the schema identity captured when the scratch was created (ORNA-VFS-019).
/// ORNAVFS1 records predate that identity. They are never bound to the live schema: at mount open
/// their candidate is kept as a retained draft and the record is removed (`retain_legacy_scratch`).
pub(crate) const VFS_SCRATCH_RECORD_MAGIC: &[u8; 8] = b"ORNAVFS2";
pub(crate) const VFS_SCRATCH_RECORD_MAGIC_V1: &[u8; 8] = b"ORNAVFS1";

#[derive(Clone, Debug)]
pub(crate) struct TemporaryScratchIdentity {
    pub(crate) logical_path: String,
    pub(crate) relation_id: [u8; 16],
    pub(crate) key: Vec<u8>,
    pub(crate) name: String,
    pub(crate) blob_field_name: Option<String>,
}

pub(crate) struct PersistedTemporaryScratch {
    pub(crate) identity: TemporaryScratchIdentity,
    /// `None` for a legacy ORNAVFS1 record, which has no creation identity to keep.
    pub(crate) schema_identity: Option<TableSchemaIdentity>,
    pub(crate) baseline: Option<Vec<u8>>,
    pub(crate) candidate: RecordedCandidate,
}

/// The persisted scratch records read at mount open, and the records that could not be read.
pub(crate) struct ScratchLoad {
    pub(crate) records: Vec<PersistedTemporaryScratch>,
    pub(crate) unreadable: Vec<UnreadableScratch>,
}

/// A scratch record left on disk because it could not be read; the mount still opens.
pub(crate) struct UnreadableScratch {
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

/// Where a record's candidate lies inside its record file. The candidate is
/// copied from here in [`CANDIDATE_CHUNK_BYTES`] slices at restore.
pub(crate) struct RecordedCandidate {
    pub(crate) record: PathBuf,
    pub(crate) offset: u64,
    pub(crate) len: u64,
}

pub(crate) struct VfsScratchStore {
    pub(crate) directory: PathBuf,
    /// Unbound scratch record names this mount created; restored records are never counted.
    pub(crate) unbound_created: Mutex<HashSet<String>>,
}

impl VfsScratchStore {
    pub(crate) fn open(runtime_root: &Path) -> Result<Self, String> {
        let directory = runtime_root.join(VFS_SCRATCH_DIRECTORY);
        let created = {
            use std::os::unix::fs::DirBuilderExt as _;
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(error) => {
                    return Err(format!(
                        "temporary scratch directory could not be created: {error}"
                    ));
                }
            }
        };
        if created {
            Self::sync_directory(runtime_root)?;
        }
        ensure_private_directory(&directory, "temporary scratch directory")?;
        Self::sync_directory(&directory)?;
        Ok(Self {
            directory,
            unbound_created: Mutex::new(HashSet::new()),
        })
    }

    pub(crate) fn persist(
        &self,
        identity: &TemporaryScratchIdentity,
        schema_identity: &TableSchemaIdentity,
        baseline: Option<&[u8]>,
        candidate: &CandidateSnapshot,
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
        staged
            .write_all(schema_identity.bytes())
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))?;
        Self::write_optional(staged.as_file_mut(), baseline)?;
        Self::write_candidate(staged.as_file_mut(), candidate)?;
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

    /// Loads every persisted record. A record that cannot be read, or whose identity does not
    /// match its name, is returned as unreadable and left on disk untouched so one bad record
    /// cannot stop the mount from opening. Store-level failures still fail the load.
    pub(crate) fn load(&self) -> Result<ScratchLoad, String> {
        let mut records = Vec::new();
        let mut unreadable = Vec::new();
        let mut removed_staging = false;
        for entry in fs::read_dir(&self.directory)
            .map_err(|error| format!("temporary scratch records could not be listed: {error}"))?
        {
            let entry = entry.map_err(|error| {
                format!("temporary scratch record could not be inspected: {error}")
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                format!("temporary scratch record type could not be read: {error}")
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !file_type.is_file() {
                unreadable.push(UnreadableScratch {
                    path,
                    reason: "temporary scratch store contains a non-file entry".into(),
                });
                continue;
            }
            if name.starts_with(VFS_SCRATCH_TEMP_PREFIX) {
                fs::remove_file(&path).map_err(|error| {
                    format!("stale temporary scratch staging file could not be removed: {error}")
                })?;
                removed_staging = true;
                continue;
            }
            if !name.ends_with(".scratch") {
                unreadable.push(UnreadableScratch {
                    path,
                    reason: "temporary scratch store contains an unknown file".into(),
                });
                continue;
            }
            let record = match Self::read_record(&path) {
                Ok(record) => record,
                Err(reason) => {
                    unreadable.push(UnreadableScratch { path, reason });
                    continue;
                }
            };
            if let Err(reason) = Self::validate_identity(&record.identity) {
                unreadable.push(UnreadableScratch { path, reason });
                continue;
            }
            if Self::record_name(&record.identity) != name {
                unreadable.push(UnreadableScratch {
                    path,
                    reason: "temporary scratch record identity does not match its filename".into(),
                });
                continue;
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
        unreadable.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(ScratchLoad {
            records,
            unreadable,
        })
    }

    pub(crate) fn remove(&self, identity: &TemporaryScratchIdentity) -> Result<(), String> {
        Self::validate_identity(identity)?;
        match fs::remove_file(self.record_path(identity)) {
            Ok(()) => Self::sync_directory(&self.directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "temporary scratch record could not be removed: {error}"
            )),
        }
    }

    /// Reserves one of this mount's unbound scratch slots for `identity`. A slot this mount
    /// already holds stays admitted; a new one past `MAX_UNBOUND_SCRATCH` is refused.
    pub(crate) fn reserve_unbound(&self, identity: &TemporaryScratchIdentity) -> bool {
        let name = Self::record_name(identity);
        let mut created = self
            .unbound_created
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if created.contains(&name) {
            return true;
        }
        if created.len() >= MAX_UNBOUND_SCRATCH {
            return false;
        }
        created.insert(name);
        true
    }

    /// Releases the slot of a record this mount removed. Restored records were never reserved,
    /// so releasing them is a no-op.
    pub(crate) fn release_unbound(&self, identity: &TemporaryScratchIdentity) {
        let name = Self::record_name(identity);
        self.unbound_created
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&name);
    }

    pub(crate) fn record_path(&self, identity: &TemporaryScratchIdentity) -> PathBuf {
        self.directory.join(Self::record_name(identity))
    }

    pub(crate) fn record_name(identity: &TemporaryScratchIdentity) -> String {
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

    pub(crate) fn update_digest(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }

    pub(crate) fn validate_identity(identity: &TemporaryScratchIdentity) -> Result<(), String> {
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

    pub(crate) fn write_identity(
        file: &mut File,
        identity: &TemporaryScratchIdentity,
    ) -> Result<(), String> {
        Self::write_bytes(file, identity.logical_path.as_bytes())?;
        file.write_all(&identity.relation_id)
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))?;
        Self::write_bytes(file, &identity.key)?;
        Self::write_bytes(file, identity.name.as_bytes())?;
        Self::write_optional(file, identity.blob_field_name.as_deref().map(str::as_bytes))
    }

    pub(crate) fn write_optional(file: &mut File, value: Option<&[u8]>) -> Result<(), String> {
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

    pub(crate) fn write_bytes(file: &mut File, bytes: &[u8]) -> Result<(), String> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| "temporary scratch record field is too large".to_owned())?;
        file.write_all(&length.to_le_bytes())
            .and_then(|()| file.write_all(bytes))
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))
    }

    /// Writes the candidate as one length-prefixed field, copying it through
    /// [`CANDIDATE_CHUNK_BYTES`]-sized reads so the record never holds the whole
    /// candidate in memory. The layout is identical to [`Self::write_bytes`].
    pub(crate) fn write_candidate(
        file: &mut File,
        candidate: &CandidateSnapshot,
    ) -> Result<(), String> {
        file.write_all(&candidate.len().to_le_bytes())
            .map_err(|error| format!("temporary scratch record could not be written: {error}"))?;
        let length = candidate.len();
        let mut offset = 0_u64;
        while offset < length {
            let wanted = usize::try_from(length - offset)
                .unwrap_or(usize::MAX)
                .min(CANDIDATE_CHUNK_BYTES);
            let chunk = candidate.read_range(offset, wanted).map_err(|error| {
                format!("temporary scratch candidate could not be read: {error}")
            })?;
            if chunk.is_empty() {
                return Err("temporary scratch candidate ended before its recorded length".into());
            }
            file.write_all(&chunk).map_err(|error| {
                format!("temporary scratch record could not be written: {error}")
            })?;
            offset += chunk.len() as u64;
        }
        Ok(())
    }

    pub(crate) fn read_record(path: &Path) -> Result<PersistedTemporaryScratch, String> {
        let mut file = File::open(path)
            .map_err(|error| format!("temporary scratch record could not be opened: {error}"))?;
        let mut magic = [0; 8];
        file.read_exact(&mut magic)
            .map_err(|error| format!("temporary scratch record header is incomplete: {error}"))?;
        let legacy = match &magic {
            VFS_SCRATCH_RECORD_MAGIC => false,
            VFS_SCRATCH_RECORD_MAGIC_V1 => true,
            _ => return Err("temporary scratch record has an unsupported format".into()),
        };
        let identity = Self::read_identity(&mut file)?;
        let schema_identity = if legacy {
            None
        } else {
            let mut schema_bytes = [0; 32];
            file.read_exact(&mut schema_bytes).map_err(|error| {
                format!("temporary scratch record schema identity is incomplete: {error}")
            })?;
            Some(TableSchemaIdentity::new(schema_bytes))
        };
        let baseline = Self::read_optional(&mut file)?;
        let candidate = Self::locate_candidate(&mut file, path)?;
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
            schema_identity,
            baseline,
            candidate,
        })
    }

    /// Skips the candidate field and records where it lies, so restore can stream
    /// it from the record file without holding it in memory.
    pub(crate) fn locate_candidate(
        file: &mut File,
        path: &Path,
    ) -> Result<RecordedCandidate, String> {
        let mut encoded_length = [0; 8];
        file.read_exact(&mut encoded_length).map_err(|error| {
            format!("temporary scratch record field length is incomplete: {error}")
        })?;
        let len = u64::from_le_bytes(encoded_length);
        let offset = file
            .stream_position()
            .map_err(|error| format!("temporary scratch record could not be read: {error}"))?;
        let file_len = file
            .metadata()
            .map_err(|error| format!("temporary scratch record size could not be read: {error}"))?
            .len();
        if offset.checked_add(len).is_none_or(|end| end > file_len) {
            return Err("temporary scratch record field exceeds its file size".into());
        }
        file.seek(SeekFrom::Start(offset + len))
            .map_err(|error| format!("temporary scratch record could not be read: {error}"))?;
        Ok(RecordedCandidate {
            record: path.to_path_buf(),
            offset,
            len,
        })
    }

    pub(crate) fn read_identity(file: &mut File) -> Result<TemporaryScratchIdentity, String> {
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

    pub(crate) fn read_string(file: &mut File) -> Result<String, String> {
        String::from_utf8(Self::read_bytes(file)?)
            .map_err(|error| format!("temporary scratch record string is invalid: {error}"))
    }

    pub(crate) fn read_optional(file: &mut File) -> Result<Option<Vec<u8>>, String> {
        let mut present = [0];
        file.read_exact(&mut present)
            .map_err(|error| format!("temporary scratch record field is incomplete: {error}"))?;
        match present[0] {
            0 => Ok(None),
            1 => Self::read_bytes(file).map(Some),
            _ => Err("temporary scratch record has an invalid field marker".into()),
        }
    }

    pub(crate) fn read_bytes(file: &mut File) -> Result<Vec<u8>, String> {
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

    pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!("temporary scratch directory could not be synchronized: {error}")
            })
    }
}
