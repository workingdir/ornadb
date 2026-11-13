use super::*;

/// A projected Blob's writable identity: the row save that accepts its candidate and the
/// admitted field it replaces (41-vfs.md EDIT-1).
pub(crate) struct BlobCandidate {
    pub(crate) save: Arc<RowSaveContext>,
    pub(crate) field: BlobField,
}

/// Reads a whole admitted Blob's bytes through a fresh read scope.
pub(crate) fn read_blob_bytes(
    graph: &NativeGraphContext,
    reference: &AdmittedBlobReference,
) -> Result<Vec<u8>, String> {
    let scope = graph
        .open_read_scope()
        .map_err(|error| format!("Blob read scope could not be opened: {error:?}"))?;
    let length = usize::try_from(reference.content_identity().length())
        .map_err(|_| "Blob length is not addressable".to_owned())?;
    Ok(graph
        .read_blob_range(reference, 0..length as u64, &scope)
        .map_err(|error| format!("Blob content could not be read: {error:?}"))?
        .bytes()
        .to_vec())
}

/// Reads the window `[offset, offset + size)`, clamped to the Blob's length, through
/// `scope`. Every mount Blob read takes this path, so the scope charges the chunks
/// the window intersects and no more (ORNA-BLOB-012).
pub(crate) fn read_blob_window(
    graph: &NativeGraphContext,
    reference: &AdmittedBlobReference,
    offset: u64,
    size: u32,
    scope: &RepositoryReadScope,
) -> Result<VerifiedBlobRange, GraphError> {
    let length = reference.content_identity().length();
    let start = offset.min(length);
    let end = offset.saturating_add(u64::from(size)).min(length);
    graph.read_blob_range(reference, start..end, scope)
}

pub(crate) struct FsNode {
    pub(crate) parent: u64,
    pub(crate) kind: NodeKind,
}

pub(crate) enum NodeKind {
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
        /// Cached candidate length; the bytes stay in the scratch store and are read by range.
        len: u64,
        save: Arc<RowSaveContext>,
        target: Arc<ManagedFile<RowMapSnapshot>>,
        scratch: Arc<TemporarySave<RowMapSnapshot>>,
        blob_field: Option<BlobField>,
        baseline: Option<Vec<u8>>,
        identity: Arc<TemporaryScratchIdentity>,
        /// The table schema the scratch was built under; its commit is refused once the committed
        /// schema moves (ORNA-VFS-019).
        schema_identity: TableSchemaIdentity,
    },
    Blob {
        graph: Arc<NativeGraphContext>,
        reference: AdmittedBlobReference,
        /// The row's save context and this file's field; `None` on read-only mounts, where
        /// write-intent opens are refused (41-vfs.md EDIT-1 candidates require a writer).
        candidate: Option<BlobCandidate>,
    },
    BlobBytes(Arc<CandidateSnapshot>),
    /// A large Str projected as a sibling text file. Its bytes are captured in the row; a
    /// writable file carries the row save context and its field (profiles/vfs-1.md).
    Text {
        bytes: Arc<CandidateSnapshot>,
        candidate: Option<BlobCandidate>,
    },
    /// A nested record, tuple, list or map value's directory, listed one level at a time.
    NestedDirectory(NestedDirectory),
}

impl NodeKind {
    pub(crate) fn directory(&self) -> Option<&Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&table.directory),
            Self::KeyDirectory(directory) => Some(&directory.directory),
            Self::Row(row) => Some(&row.directory),
            Self::NestedDirectory(nested) => Some(&nested.directory),
            Self::Bytes(_)
            | Self::BlobBytes(_)
            | Self::Text { .. }
            | Self::Document { .. }
            | Self::Temporary { .. }
            | Self::Blob { .. } => None,
        }
    }

    pub(crate) fn directory_mut(&mut self) -> Option<&mut Directory> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Table(table) => Some(&mut table.directory),
            Self::KeyDirectory(directory) => Some(&mut directory.directory),
            Self::Row(row) => Some(&mut row.directory),
            Self::NestedDirectory(nested) => Some(&mut nested.directory),
            Self::Bytes(_)
            | Self::BlobBytes(_)
            | Self::Text { .. }
            | Self::Document { .. }
            | Self::Temporary { .. }
            | Self::Blob { .. } => None,
        }
    }

    pub(crate) fn is_directory(&self) -> bool {
        self.directory().is_some()
    }

    /// The readdir file type of a node of this kind.
    pub(crate) fn file_type(&self) -> FileType {
        match self {
            Self::Directory(_)
            | Self::Table(_)
            | Self::KeyDirectory(_)
            | Self::Row(_)
            | Self::NestedDirectory(_) => FileType::Directory,
            Self::Bytes(_)
            | Self::BlobBytes(_)
            | Self::Text { .. }
            | Self::Document { .. }
            | Self::Temporary { .. }
            | Self::Blob { .. } => FileType::RegularFile,
        }
    }
}

pub(crate) struct Directory {
    pub(crate) entries: Vec<DirectoryEntry>,
    pub(crate) by_name: HashMap<OsString, usize>,
    /// Cookie the next pushed entry takes. Cookies only grow, so removing or replacing a sibling
    /// never changes an existing entry's readdir offset.
    pub(crate) next_cookie: u64,
}

impl Directory {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            by_name: HashMap::new(),
            next_cookie: 0,
        }
    }

    /// Appends `name` in readdir order under a fresh cookie.
    pub(crate) fn push(&mut self, name: OsString, inode: u64, kind: FileType) {
        let cookie = self.next_cookie;
        self.next_cookie += 1;
        self.by_name.insert(name.clone(), self.entries.len());
        self.entries.push(DirectoryEntry {
            inode,
            name,
            kind,
            cookie,
        });
    }

    /// Index of the first entry whose readdir offset is past `start`, the offset the kernel
    /// hands back for the last entry it received. Offsets are cookies, so removing an entry
    /// before a held cursor shifts indexes but not offsets, and the listing resumes after it.
    pub(crate) fn resume_index(&self, start: u64) -> usize {
        self.entries
            .partition_point(|entry| entry.offset() <= start)
    }

    pub(crate) fn entry(&self, name: &OsStr) -> Option<&DirectoryEntry> {
        self.by_name
            .get(name)
            .and_then(|index| self.entries.get(*index))
    }
    pub(crate) fn remove_entry(&mut self, name: &OsStr) -> Option<DirectoryEntry> {
        let index = self.by_name.remove(name)?;
        // Order-preserving: siblings keep their readdir order and cookies, so offsets hold.
        let removed = self.entries.remove(index);
        for (position, entry) in self.entries.iter().enumerate().skip(index) {
            self.by_name.insert(entry.name.clone(), position);
        }
        Some(removed)
    }
    #[allow(clippy::assigning_clones)]
    /// Renames `name` in place, so every entry keeps its readdir position (ORNA-VFS-017).
    /// Returns false, leaving the directory unchanged, when `name` is absent or `newname` is taken.
    pub(crate) fn rename_entry(&mut self, name: &OsStr, newname: &OsStr) -> bool {
        if self.by_name.contains_key(newname) {
            return false;
        }
        let Some(index) = self.by_name.remove(name) else {
            return false;
        };
        self.entries[index].name = newname.to_owned();
        self.by_name.insert(newname.to_owned(), index);
        true
    }

    /// Drops every entry `keep` rejects; kept entries stay in place.
    pub(crate) fn retain_entries(&mut self, keep: impl Fn(&DirectoryEntry) -> bool) {
        self.entries.retain(|entry| keep(entry));
        self.by_name = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.name.clone(), index))
            .collect();
    }
}

/// A nested record, tuple, list or map value's directory (profiles/vfs-1.md:71-75). It is created
/// empty and lists its children from the pinned `root` one page at a time, on lookup or readdir,
/// so nothing below a directory the user has not entered is built.
pub(crate) struct NestedDirectory {
    pub(crate) directory: Directory,
    pub(crate) root: Arc<Raw>,
    /// Child positions from `root` down to this directory's value.
    pub(crate) path: Vec<usize>,
    /// Index of the next child to list; children before it are in `directory`.
    pub(crate) next_index: usize,
    /// Set once every child has been listed.
    pub(crate) exhausted: bool,
}

impl NestedDirectory {
    pub(crate) fn new(root: Arc<Raw>, path: Vec<usize>) -> Self {
        Self {
            directory: Directory::new(),
            root,
            path,
            next_index: 0,
            exhausted: false,
        }
    }
}

pub(crate) struct TableDirectory {
    pub(crate) directory: Directory,
    pub(crate) source: Arc<TableSource>,
    pub(crate) lower: Option<TypedKey>,
    pub(crate) exhausted: bool,
    pub(crate) error: Option<String>,
    pub(crate) indexed_rows: HashMap<OsString, u64>,
}

pub(crate) struct KeyDirectory {
    pub(crate) directory: Directory,
    pub(crate) table_inode: u64,
    pub(crate) prefix: Vec<TypedKey>,
}

pub(crate) struct RowDirectory {
    pub(crate) directory: Directory,
    pub(crate) source: Arc<TableSource>,
    pub(crate) key: TypedKey,
    pub(crate) state: RowLoadState,
}

pub(crate) enum RowLoadState {
    Unloaded,
    Ready,
    Failed(String),
}

/// Why a row's projection failed. A transient failure (the table's committed schema history is
/// still being scanned, or a recent scan failed) leaves the row unloaded so a later access
/// retries it; a fatal failure is sticky for the row.
#[derive(Debug)]
pub(crate) enum RowLoadError {
    Transient(String),
    Fatal(String),
}

impl From<String> for RowLoadError {
    fn from(message: String) -> Self {
        Self::Fatal(message)
    }
}

impl From<&str> for RowLoadError {
    fn from(message: &str) -> Self {
        Self::Fatal(message.to_owned())
    }
}

impl From<RowLoadError> for String {
    fn from(error: RowLoadError) -> Self {
        match error {
            RowLoadError::Transient(message) | RowLoadError::Fatal(message) => message,
        }
    }
}

pub(crate) struct TableSource {
    pub(crate) graph: Arc<NativeGraphContext>,
    pub(crate) rows: Arc<RowMapSnapshot>,
    pub(crate) binding: CommittedTableBinding,
    pub(crate) schema: ResolvedTableSchema,
    pub(crate) value_context: Format3Context,
    pub(crate) field_names: HashMap<[u8; 16], String>,
    pub(crate) pinned_field_layout: StoredFieldLayout,
    pub(crate) runtime_table: Option<RuntimeTableIdentity>,
    pub(crate) runtime_field_layout: Option<StoredFieldLayout>,
}

/// Readdir offsets of entries begin after `.` (1) and `..` (2).
pub(crate) const READDIR_ENTRY_BASE: u64 = 3;

pub(crate) struct DirectoryEntry {
    pub(crate) inode: u64,
    pub(crate) name: OsString,
    pub(crate) kind: FileType,
    /// Assigned when the entry is pushed and never reused within its directory.
    pub(crate) cookie: u64,
}

impl DirectoryEntry {
    pub(crate) fn offset(&self) -> u64 {
        self.cookie.saturating_add(READDIR_ENTRY_BASE)
    }
}

pub(crate) fn flatten_key_components(
    key: &TypedKey,
    components: &mut Vec<TypedKey>,
) -> Result<(), String> {
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

pub(crate) fn field_names_for_table(
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

pub(crate) fn stored_field_layout(
    descriptor: &SchemaDescriptor,
) -> Result<StoredFieldLayout, String> {
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

/// Commits `bytes` as the row's whole document through the CAS boundary a draft save uses.
/// `baseline` is the row `bytes` were derived from, captured with them (41-vfs.md
/// Temporary-save replacement); the commit refuses as stale when the live row has moved on.
pub(crate) fn commit_row_document(
    save: &Arc<RowSaveContext>,
    bytes: &[u8],
    baseline: Option<Vec<u8>>,
    schema_identity: TableSchemaIdentity,
) -> Result<(), DocumentCommitError> {
    let scratch = save.writer.runtime.block_on(
        save.target
            .begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES),
    );
    save.writer
        .runtime
        .block_on(scratch.write_at(0, bytes))
        .map_err(|error| {
            DocumentCommitError::Failed(format!("row document could not be staged: {error:?}"))
        })?;
    save.commit_temporary(&scratch, baseline, schema_identity, true)
        .map(|_| ())
}

/// Swaps the admitted Blob field `field_name` for `replacement` (`None` removes it) and commits
/// the rewritten `data.orna` against `baseline`, the row `document` was derived from. A refused
/// commit restores the admitted list.
pub(crate) fn commit_blob_descriptor_change(
    save: &Arc<RowSaveContext>,
    field_name: &str,
    replacement: Option<BlobField>,
    document: &[u8],
    baseline: Option<Vec<u8>>,
    schema_identity: TableSchemaIdentity,
) -> Result<(), DocumentCommitError> {
    let removed = replacement.is_none();
    let mut fields = save
        .blob_fields
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let index = fields
        .iter()
        .position(|field| field.name == field_name)
        .ok_or_else(|| {
            DocumentCommitError::Failed(format!("Blob field {field_name} is not admitted"))
        })?;
    let previous = match replacement {
        Some(replacement) => std::mem::replace(&mut fields[index], replacement),
        None => fields.remove(index),
    };
    drop(fields);
    let committed = commit_row_document(save, document, baseline, schema_identity);
    if committed.is_err() {
        let mut fields = save
            .blob_fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if removed {
            let index = index.min(fields.len());
            fields.insert(index, previous);
        } else if let Some(slot) = fields.get_mut(index) {
            *slot = previous;
        }
    }
    committed
}

#[allow(clippy::option_option)]
/// The suffix hint for renaming Blob field `field_name` of `media_type` to the sibling
/// `newname`, when `newname` is that field's projected name under a compatible suffix (VFS-012).
pub(crate) fn blob_rename_hint(
    media_type: &str,
    field_name: &str,
    newname: &str,
) -> Option<Option<String>> {
    newname.match_indices('.').find_map(|(index, _)| {
        let hint =
            orna_vfs::classify_content_suffix(media_type, Some(&newname[index + 1..]))
                .ok()?;
        let projected =
            orna_vfs::project_content_name(field_name, media_type, hint.as_deref())
                .ok()?;
        (projected == newname).then_some(hint)
    })
}

/// The row's `data.orna` inode and its current bytes.
pub(crate) fn row_document(state: &mut FsBuilder, row: u64) -> Result<(u64, Vec<u8>), Errno> {
    let inode = state
        .lookup_child(row, OsStr::new("data.orna"))
        .map_err(|_| Errno::EIO)?
        .ok_or(Errno::EIO)?;
    match state.nodes.get(&inode).map(|node| &node.kind) {
        Some(NodeKind::Document { bytes, .. }) => Ok((inode, bytes.clone())),
        _ => Err(Errno::EIO),
    }
}

/// The save context of a row's `data.orna` document. Blob commits take it from the document, so a
/// refresh after a stale refusal (41-vfs.md EDIT-1) reaches the next rename or unlink.
pub(crate) fn document_save(
    state: &FsBuilder,
    document_inode: u64,
) -> Result<Arc<RowSaveContext>, Errno> {
    match state.nodes.get(&document_inode).map(|node| &node.kind) {
        Some(NodeKind::Document { save, .. }) => Ok(Arc::clone(save)),
        _ => Err(Errno::EIO),
    }
}

/// The canonical descriptor line prefix of Blob field `name` whose sibling file is `file`.
pub(crate) fn descriptor_prefix(name: &str, file: &str) -> String {
    format!("    {name}: {{ path: \"./{file}\", media_type: \"")
}

/// Repoints Blob field `name`'s descriptor from sibling `old` to sibling `new`. The canonical
/// descriptor must appear exactly once, otherwise the document is refused.
pub(crate) fn replace_descriptor_path(
    document: &str,
    name: &str,
    old: &str,
    new: &str,
) -> Result<String, String> {
    let from = descriptor_prefix(name, old);
    if document.matches(&from).count() != 1 {
        return Err(format!(
            "Blob field {name} descriptor is not in its canonical form"
        ));
    }
    Ok(document.replacen(&from, &descriptor_prefix(name, new), 1))
}

/// Sets Blob field `name`'s canonical descriptor for sibling `file` to `null`. The canonical
/// descriptor must appear exactly once, otherwise the document is refused.
pub(crate) fn null_descriptor(document: &str, name: &str, file: &str) -> Result<String, String> {
    let from = descriptor_prefix(name, file);
    if document.matches(&from).count() != 1 {
        return Err(format!(
            "Blob field {name} descriptor is not in its canonical form"
        ));
    }
    let start = document
        .find(&from)
        .ok_or_else(|| format!("Blob field {name} descriptor is missing"))?;
    let close = "\" },\n";
    let rest = &document[start..];
    let end = rest
        .find(close)
        .ok_or_else(|| format!("Blob field {name} descriptor is not closed"))?
        + close.len();
    Ok(format!(
        "{}    {name}: null,\n{}",
        &document[..start],
        &rest[end..]
    ))
}

/// Sets the large-Str field `field` to `null` in `document`, replacing its `{ text, encoding }`
/// entry. The entry must be the projected descriptor for the field's sibling file.
pub(crate) fn null_text_descriptor(document: &str, field: &BlobField) -> Result<String, String> {
    let parsed = parse_row(document);
    if !parsed.diagnostics.is_empty() {
        return Err("mounted row document is not a valid source row".into());
    }
    let Expr::Record {
        fields: entries, ..
    } = &parsed.value
    else {
        return Err("mounted row document is not a record".into());
    };
    let name = &field.name;
    let entry = entries
        .iter()
        .find(|entry| &entry.name == name)
        .ok_or_else(|| {
            format!("Str field {name} is missing; data.orna must keep its descriptor")
        })?;
    validate_text_descriptor(&entry.value, field)?;
    let mut output = document.to_owned();
    output.replace_range(entry.span.start..entry.span.end, &format!("{name}: null"));
    Ok(output)
}

pub(crate) fn is_optional_field(field_type: &Raw) -> bool {
    matches!(field_type, Raw::Array(parts) if parts.len() == 2 && parts[0] == Raw::Int(2.into()))
}

pub(crate) fn stored_value_is_null(value: &Raw) -> bool {
    matches!(value, Raw::Null)
        || matches!(value, Raw::Tag(60013, option) if matches!(option.as_ref(), Raw::Array(parts) if matches!(parts.as_slice(), [Raw::Int(index)] if index.to_string() == "0")))
}

/// The Blob payload a row field value selects: a present optional's payload, or the bare Blob.
pub(crate) fn selected_blob_payload(value: &Raw) -> &Raw {
    match value {
        Raw::Tag(60013, option) => match option.as_ref() {
            Raw::Array(parts) => match parts.as_slice() {
                [Raw::Int(index), payload] if index.to_string() == "1" => payload,
                _ => value,
            },
            _ => value,
        },
        _ => value,
    }
}

/// Whether a selected row value shows a Blob: a bare Blob or a present optional's Blob.
pub(crate) fn selects_blob(value: &Raw) -> bool {
    matches!(
        selected_blob_payload(value),
        Raw::Tag(orna_value::ROV3_BLOB_TAG, _)
    )
}

/// Bytes above which a Str projects as a sibling file rather than staying inline (profiles/vfs-1.md:76).
pub(crate) const LARGE_TEXT_BOUND_BYTES: usize = 64 * 1024;

/// The text a stored Str holds when it exceeds 64 KiB and therefore projects as a sibling file.
pub(crate) fn large_text_payload(value: &Raw) -> Option<&str> {
    match selected_blob_payload(value) {
        Raw::Text(text) if text.len() > LARGE_TEXT_BOUND_BYTES => Some(text.as_str()),
        _ => None,
    }
}

/// Admits the accepted pin that holds the selected replacement content for a descriptor the
/// pinned row does not name. Every pin matching the selected length and SHA-256 is tried in
/// turn; when none is admitted the Blob is refused rather than served from the committed row.
pub(crate) fn admit_accepted_replacement(
    writer: Option<&MountWriter>,
    graph: &NativeGraphContext,
    descriptor: &NativeOid,
    length: u64,
    sha256: [u8; 32],
    scope: &RepositoryReadScope,
) -> Result<AdmittedBlobReference, String> {
    let writer = writer.ok_or_else(|| {
        "committed Blob content is absent from the row and no CWD runtime holds it".to_owned()
    })?;
    let pins = writer
        .runtime
        .block_on(
            writer
                .state
                .accepted_content_pins_for_descriptor(descriptor),
        )
        .map_err(|error| format!("accepted content pins could not be listed: {error:?}"))?;
    let identity = ContentIdentity::new(length, sha256)
        .map_err(|error| format!("committed Blob identity is invalid: {error:?}"))?;
    let mut refusal = "no accepted pin holds the selected Blob content".to_owned();
    for pin in pins
        .iter()
        .filter(|pin| pin.content_length() == length && *pin.content_sha256() == sha256)
    {
        match graph.admit_accepted_blob_pin(pin.pin_id(), descriptor, identity, scope) {
            Ok(reference) => return Ok(reference),
            Err(error) => refusal = format!("accepted Blob pin could not be admitted: {error:?}"),
        }
    }
    Err(refusal)
}

#[allow(clippy::option_option)]
/// A stored optional (`Tag(60013, [index, payload])`, index 1 = present): `Some(Some(payload))`
/// when present, `Some(None)` when absent, `None` when the value is not an optional.
pub(crate) fn optional_state(value: &Raw) -> Option<Option<&Raw>> {
    let Raw::Tag(60013, option) = value else {
        return None;
    };
    let Raw::Array(parts) = option.as_ref() else {
        return None;
    };
    match parts.as_slice() {
        [Raw::Int(index), payload] => Some((index.to_string() == "1").then_some(payload)),
        _ => None,
    }
}

/// The stored children of a nested container, borrowed from the value (profiles/vfs-1.md:71-75).
/// Record fields and map keys are escaped names, tuple and list elements are decimal positions.
/// A present optional projects its payload; an absent one has no children.
#[derive(Clone, Copy)]
pub(crate) enum NestedEntries<'a> {
    /// Tuple or list elements, listed by decimal position.
    Positional(&'a [Raw]),
    /// Stored record fields: `[field, value]` pairs in encoded-key order.
    Record(&'a [Raw]),
    /// Text-keyed map entries, listed by escaped key.
    Map(&'a [(Raw, Raw)]),
}

impl NestedEntries<'_> {
    pub(crate) fn child_count(self) -> usize {
        match self {
            Self::Positional(items) | Self::Record(items) => items.len(),
            Self::Map(entries) => entries.len(),
        }
    }
}

/// The container head of a nested value. `None` for an absent optional and for non-container
/// values, which stay inside `data.orna` as values rather than directories.
pub(crate) fn nested_entries(value: &Raw) -> Option<NestedEntries<'_>> {
    let value = match optional_state(value) {
        Some(Some(payload)) => payload,
        Some(None) => return None,
        None => value,
    };
    match value {
        // Stored records are `Tag(RECORD, [type-or-null, [[field, value], ...]])` (typed_row.rs).
        Raw::Tag(60009, body) => match body.as_ref() {
            Raw::Array(parts) => match parts.as_slice() {
                [_, Raw::Array(entries)] => Some(NestedEntries::Record(entries)),
                _ => None,
            },
            _ => None,
        },
        Raw::Tag(60015, body) => match body.as_ref() {
            Raw::Array(items) => Some(NestedEntries::Positional(items)),
            _ => None,
        },
        Raw::Array(items) => Some(NestedEntries::Positional(items)),
        Raw::Map(entries) => Some(NestedEntries::Map(entries)),
        _ => None,
    }
}

/// How a child is named before projection; the projected name is built only when inserted.
pub(crate) enum NestedName<'a> {
    Position,
    Field(&'a str),
    Key(&'a str),
}

impl NestedName<'_> {
    pub(crate) fn project(self, index: usize) -> String {
        match self {
            Self::Position => index.to_string(),
            Self::Field(name) => orna_vfs::project_field_name(name),
            Self::Key(name) => project_component(name),
        }
    }
}

/// Child `index` of a nested container: its unprojected name and its value. Builds no vector;
/// `None` for an out-of-range index, a malformed entry, or a non-container value.
pub(crate) fn nested_entry(value: &Raw, index: usize) -> Option<(NestedName<'_>, &Raw)> {
    match nested_entries(value)? {
        NestedEntries::Positional(items) => {
            items.get(index).map(|child| (NestedName::Position, child))
        }
        NestedEntries::Record(entries) => match entries.get(index)? {
            Raw::Array(pair) => match pair.as_slice() {
                [Raw::Text(name), child] => Some((NestedName::Field(name.as_str()), child)),
                _ => None,
            },
            _ => None,
        },
        NestedEntries::Map(entries) => match entries.get(index)? {
            (Raw::Text(name), child) => Some((NestedName::Key(name.as_str()), child)),
            _ => None,
        },
    }
}

/// Whether a value is a directory rather than a leaf: its stored shape holds children and every
/// entry is well formed. Names nothing, so classifying each level costs no allocation.
pub(crate) fn has_nested_children(value: &Raw) -> bool {
    match nested_entries(value) {
        None => false,
        Some(NestedEntries::Positional(_)) => true,
        Some(NestedEntries::Record(entries)) => entries.iter().all(|entry| {
            matches!(entry, Raw::Array(pair) if matches!(pair.as_slice(), [Raw::Text(_), _]))
        }),
        Some(NestedEntries::Map(entries)) => {
            entries.iter().all(|(key, _)| matches!(key, Raw::Text(_)))
        }
    }
}

/// The value at `path` below `root`; each step is a child position from `nested_entry`.
pub(crate) fn nested_value_at<'a>(root: &'a Raw, path: &[usize]) -> Option<&'a Raw> {
    path.iter().try_fold(root, |value, index| {
        nested_entry(value, *index).map(|(_, child)| child)
    })
}

/// The text of a leaf child: a string unquoted, other scalars in their source spelling, a
/// present optional as its payload, an absent one as `null`.
pub(crate) fn nested_leaf_text(value: &Raw) -> Result<String, String> {
    match optional_state(value) {
        Some(Some(payload)) => nested_leaf_text(payload),
        Some(None) => Ok("null".to_owned()),
        None => match value {
            Raw::Text(text) => Ok(text.clone()),
            other => render_stored_source_value(other),
        },
    }
}

pub(crate) fn render_stored_source_value(value: &Raw) -> Result<String, String> {
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
        Raw::Text(value) => Ok(orna_str_literal(value)),
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

pub(crate) fn render_stored_instant(instant: &Raw) -> Result<String, String> {
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

pub(crate) fn civil_date_from_days(days: i64) -> (i64, i64, i64) {
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

pub(crate) fn key_component_text(key: &TypedKey) -> Result<KeyComponentText, String> {
    if let TypedKey::Tuple(_) = key {
        return Err("tuple key must be projected component by component".into());
    }
    let text = key
        .component_text()
        .ok_or_else(|| "key type needs its schema to project".to_owned())?;
    // A long key's `~key-` alias digests the typed component's canonical
    // OVB-1 encoding, not its text (VFS-1, "Row/key directory mapping").
    let canonical_encoding = key
        .canonical_bytes()
        .map_err(|_| "key component has no canonical encoding".to_owned())?;
    Ok(KeyComponentText {
        text,
        canonical_encoding,
    })
}

pub(crate) fn key_component(key: &TypedKey) -> Result<String, String> {
    let component = key_component_text(key)?;
    Ok(project_key_component(
        &component.text,
        &component.canonical_encoding,
    ))
}

/// Checks every captured Blob field of a candidate `data.orna` before its
/// descriptors are replaced by owner bindings (A15-04, ORNA-VFS-004,
/// ORNA-VFS-012). `data.orna` is a complete document, so a missing Blob field
/// fails like any missing stored field. An optional Blob written as an explicit
/// `null` is absent instead (profiles/vfs-1.md:82,110; ORNA-VFS-013). The
/// descriptor must stay exactly as projected: its path names the field's own
/// sibling file and its media type describes the captured content. Content
/// changes by renaming a file over the sibling, so any other descriptor is
/// refused instead of acknowledged and then dropped. Returns the names of the
/// optional Blob fields set to `null`.
pub(crate) fn validate_blob_descriptors(
    source: &str,
    fields: &[BlobField],
) -> Result<Vec<String>, String> {
    if fields.is_empty() {
        return Ok(Vec::new());
    }
    let parsed = parse_row(source);
    if !parsed.diagnostics.is_empty() {
        return Err("mounted row document is not a valid source row".into());
    }
    let Expr::Record {
        fields: entries, ..
    } = &parsed.value
    else {
        return Err("mounted row document is not a record".into());
    };
    let mut absent = Vec::new();
    for field in fields {
        let name = &field.name;
        let Some(entry) = entries.iter().find(|entry| &entry.name == name) else {
            return Err(format!(
                "Blob field {name} is missing; data.orna must keep its descriptor"
            ));
        };
        if !field.required
            && matches!(
                entry.value,
                Expr::Literal {
                    kind: LiteralKind::Null,
                    ..
                }
            )
        {
            absent.push(name.clone());
            continue;
        }
        let Expr::Record {
            fields: descriptor, ..
        } = &entry.value
        else {
            return Err(format!(
                "Blob field {name} must keep its {{ path, media_type }} descriptor"
            ));
        };
        let mut path = None;
        let mut media_type = None;
        for part in descriptor {
            let value = plain_string_literal(&part.value).ok_or_else(|| {
                format!("Blob field {name} descriptor entries must be plain strings")
            })?;
            match part.name.as_str() {
                "path" if path.is_none() => path = Some(value),
                "media_type" if media_type.is_none() => media_type = Some(value),
                other => {
                    return Err(format!(
                        "Blob field {name} descriptor has an unsupported entry {other}"
                    ));
                }
            }
        }
        let expected_path = format!("./{}", field.file_name);
        if path != Some(expected_path.as_str()) {
            return Err(format!(
                "Blob field {name} must keep path \"{expected_path}\"; replace its content by renaming a file over {}",
                field.file_name
            ));
        }
        if media_type != Some(field.media_type.as_str()) {
            return Err(format!(
                "Blob field {name} must keep media_type \"{}\" of its captured content",
                field.media_type
            ));
        }
    }
    Ok(absent)
}

/// The value of a string literal written without escapes, the only spelling a
/// projected descriptor uses.
pub(crate) fn plain_string_literal(value: &Expr) -> Option<&str> {
    match value {
        Expr::Literal {
            text,
            kind: LiteralKind::String,
            ..
        } => text
            .strip_prefix('"')?
            .strip_suffix('"')
            .filter(|body| !body.contains('\\')),
        _ => None,
    }
}

/// The body of any string literal, escapes included, as written in a row document.
pub(crate) fn inline_string_literal(value: &Expr) -> Option<&str> {
    match value {
        Expr::Literal {
            text,
            kind: LiteralKind::String,
            ..
        } => text.strip_prefix('"')?.strip_suffix('"'),
        _ => None,
    }
}

/// Replaces each captured text Str descriptor in a candidate `data.orna` with the Str literal of
/// its bytes (profiles/vfs-1.md, Large Str values). The descriptor must keep the field's own
/// sibling path and `utf-8` encoding. The replacement, when named, supplies new content for that
/// field; otherwise the captured bytes stand. An optional field may stay `null`.
/// An inline string literal within the bound stands as the field's new value; that is how a
/// shrink returns a Str to the row document.
pub(crate) fn substitute_text_fields(
    source: &str,
    fields: &[BlobField],
    replacement: Option<(&str, &[u8])>,
) -> Result<String, String> {
    if let Some((name, _)) = replacement
        && !fields.iter().any(|field| field.name == name)
    {
        return Err(format!(
            "replacement names {name}, which is not a text field"
        ));
    }
    if fields.is_empty() {
        return Ok(source.to_owned());
    }
    let parsed = parse_row(source);
    if !parsed.diagnostics.is_empty() {
        return Err("mounted row document is not a valid source row".into());
    }
    let Expr::Record {
        fields: entries, ..
    } = &parsed.value
    else {
        return Err("mounted row document is not a record".into());
    };
    let mut edits = Vec::with_capacity(fields.len());
    for field in fields {
        let name = &field.name;
        let entry = entries
            .iter()
            .find(|entry| &entry.name == name)
            .ok_or_else(|| {
                format!("Str field {name} is missing; data.orna must keep its descriptor")
            })?;
        if !field.required
            && matches!(
                entry.value,
                Expr::Literal {
                    kind: LiteralKind::Null,
                    ..
                }
            )
        {
            continue;
        }
        let replaced = replacement.is_some_and(|(replaced, _)| replaced == name);
        if !replaced && let Some(body) = inline_string_literal(&entry.value) {
            if body.len() > LARGE_TEXT_BOUND_BYTES {
                return Err(format!(
                    "Str field {name} is over 64 KiB and must keep its {{ text, encoding }} descriptor"
                ));
            }
            continue;
        }
        validate_text_descriptor(&entry.value, field)?;
        let owned;
        let bytes: &[u8] = match replacement {
            Some((replaced, bytes)) if replaced == name => bytes,
            _ => {
                owned = field
                    .replacement_bytes
                    .as_ref()
                    .ok_or_else(|| format!("Str field {name} has no captured text"))?
                    .to_arc()
                    .map_err(|error| format!("Str field {name} text could not be read: {error}"))?;
                &owned[..]
            }
        };
        edits.push((
            entry.span.start..entry.span.end,
            format!("{name}: {}", orna_string_literal(bytes)?),
        ));
    }
    edits.sort_by_key(|(range, _): &(std::ops::Range<usize>, String)| range.start);
    let mut output = source.to_owned();
    for (range, text) in edits.into_iter().rev() {
        output.replace_range(range, &text);
    }
    Ok(output)
}

/// Checks that a text Str's descriptor is exactly the projected `{ text, encoding }` pair for its
/// sibling file. Content changes by renaming a file over the sibling, never by editing the row.
pub(crate) fn validate_text_descriptor(value: &Expr, field: &BlobField) -> Result<(), String> {
    let name = &field.name;
    let Expr::Record {
        fields: descriptor, ..
    } = value
    else {
        return Err(format!(
            "Str field {name} must keep its {{ text, encoding }} descriptor"
        ));
    };
    let mut text = None;
    let mut encoding = None;
    for part in descriptor {
        let value = plain_string_literal(&part.value)
            .ok_or_else(|| format!("Str field {name} descriptor entries must be plain strings"))?;
        match part.name.as_str() {
            "text" if text.is_none() => text = Some(value),
            "encoding" if encoding.is_none() => encoding = Some(value),
            other => {
                return Err(format!(
                    "Str field {name} descriptor has an unsupported entry {other}"
                ));
            }
        }
    }
    let expected_text = format!("./{}", field.file_name);
    if text != Some(expected_text.as_str()) {
        return Err(format!(
            "Str field {name} must keep text \"{expected_text}\"; replace its content by renaming a file over {}",
            field.file_name
        ));
    }
    if encoding != Some("utf-8") {
        return Err(format!("Str field {name} must keep encoding \"utf-8\""));
    }
    Ok(())
}

/// The Orna string literal whose value is `text`. `{` is escaped so the
/// literal never opens an interpolation, and other control characters use `\u{..}`.
pub(crate) fn orna_str_literal(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for character in text.chars() {
        match character {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            '\0' => literal.push_str("\\0"),
            '{' => literal.push_str("\\u{7b}"),
            control if control.is_control() => {
                let _ = write!(literal, "\\u{{{:x}}}", control as u32);
            }
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
}

/// The Orna string literal whose value is `bytes`, which must be UTF-8.
pub(crate) fn orna_string_literal(bytes: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "text field content is not valid UTF-8".to_owned())?;
    Ok(orna_str_literal(text))
}

/// Removes projected Blob summary fields from a source row before row admission.
/// The selected values are supplied separately by owner-issued Blob bindings.
pub(crate) fn source_without_fields(source: &str, omitted: &[String]) -> Result<String, String> {
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

pub(crate) fn is_replacement_scratch_for(name: &str, file_name: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_prefix(file_name))
        .and_then(|name| name.strip_prefix('.'))
    else {
        return false;
    };
    !suffix.is_empty() && suffix != "." && suffix != ".." && !suffix.contains('/') && suffix != "/"
}
