//! VFS-1 host name projection: reversible component escaping, reserved and
//! colliding names, and MIME-1 suffix selection.
//!
//! A host name is never authority: every component that is served is exactly
//! what the projection produces from the canonical typed text or digest, so a
//! name can never retarget another row, field or blob (ORNA-VFS-004, 016).

use orna_value::{MimeRegistry, Raw, Value as OvbValue};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// VFS-1 host namespace projection (ORNA-VFS-001, 002, 004, 016, 017, 018)
//
// A row directory addresses database/table/typed key. `data.orna` encodes the
// stored non-key fields and each stored Blob becomes an escaped sibling file
// named from its MIME-1 annotation. Nothing here materialises a competing
// authoritative copy: the projection records which stored field owns each host
// name, and it renders the row document from the stored field values the owner
// snapshot already holds.
// ---------------------------------------------------------------------------

/// Encoded component bound from the VFS-1 profile.
pub const VFS_MAX_COMPONENT_BYTES: usize = 200;
/// Repository-relative path bound from the VFS-1 profile.
pub const VFS_MAX_PATH_BYTES: usize = 1024;
/// The reserved projected name of the row document.
pub const VFS_ROW_DOCUMENT: &str = "data.orna";

/// `EAGAIN`: the captured baseline is no longer the accepted row (VFS-011).
pub const VFS_EAGAIN: i32 = 11;
/// `EINVAL`: malformed candidate, name, or document (VFS-001, VFS-016).
pub const VFS_EINVAL: i32 = 22;
/// `ENOENT`: no projected field or row at that address.
pub const VFS_ENOENT: i32 = 2;
/// `EPERM`: authority-refusing path or mutation (VFS-004, VFS-013).
pub const VFS_EPERM: i32 = 1;
/// `EOPNOTSUPP`: a VFS-1 unsupported operation (VFS-014).
pub const VFS_EOPNOTSUPP: i32 = 95;
/// `EIO`: corrupt or unavailable backing data.
pub const VFS_EIO: i32 = 5;

/// Why a projected name, a resolved path, or a candidate document was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VfsPathError {
    /// Absolute paths, traversal spellings, separators inside one component,
    /// and any name outside the field's registered projection (VFS-004).
    OutsideProjection,
    /// The name does not belong to this row/field namespace (VFS-001).
    WrongNamespace,
    /// The encoded component or the whole path exceeds the VFS-1 bound (VFS-016).
    NameTooLong,
    /// Two distinct canonical names project to one host name (VFS-016).
    NameCollision,
    /// The candidate is not a complete stored non-key-field document (VFS-001).
    InvalidDocument,
    /// No stored field or row exists at that address (VFS-001).
    UnknownRow,
    /// The operation is unsupported in VFS-1 (VFS-013, VFS-014).
    Unsupported,
    /// The write baseline no longer matches the accepted row (VFS-011).
    StaleBaseline,
    /// A stored Blob annotation the MIME-1 registry refuses. Stored annotations
    /// are validated when captured, so this is corrupt backing metadata and no
    /// host name is guessed for it (EIO).
    CorruptAnnotation,
}

impl VfsPathError {
    /// The exact `errno` required by the VFS-1 profile error table.
    pub const fn errno(self) -> i32 {
        match self {
            Self::OutsideProjection => VFS_EPERM,
            Self::WrongNamespace | Self::NameTooLong | Self::NameCollision => VFS_EINVAL,
            Self::InvalidDocument => VFS_EINVAL,
            Self::UnknownRow => VFS_ENOENT,
            Self::Unsupported => VFS_EOPNOTSUPP,
            Self::StaleBaseline => VFS_EAGAIN,
            Self::CorruptAnnotation => VFS_EIO,
        }
    }

    /// The stable Orna cause code kept alongside the host `errno`. Unknown
    /// rows report `ENOENT` and corrupt stored metadata reports `EIO`, neither
    /// with an Orna failure code of its own.
    pub const fn failure_code(self) -> Option<&'static str> {
        match self {
            Self::OutsideProjection | Self::Unsupported => Some("sys.vfs.unsupported"),
            Self::WrongNamespace | Self::NameTooLong | Self::InvalidDocument => {
                Some("sys.vfs.invalid_document")
            }
            Self::NameCollision => Some("sys.vfs.name_collision"),
            Self::StaleBaseline => Some("sys.vfs.stale_edit"),
            Self::UnknownRow | Self::CorruptAnnotation => None,
        }
    }
}

/// One addressable projected component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VfsComponent {
    /// A component that spells its canonical typed key/field text directly.
    Canonical(String),
    /// A `~key-`/`~field-<digest>` long alias for a canonical name over the
    /// byte bound. Resolving it needs the snapshot-bound long-name index; the
    /// digest is verified there against the full retained name (VFS-016).
    LongAlias { digest: [u8; 32] },
}

/// The projection namespace a canonical component belongs to. Key and field
/// components share one host shape but keep distinct alias prefixes, so a key
/// alias can never resolve as a field name and vice versa (VFS-016).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VfsNameNamespace {
    /// One component of a typed table key.
    Key,
    /// One stored field path inside a row.
    Field,
}

impl VfsNameNamespace {
    const fn alias_prefix(self) -> &'static str {
        match self {
            Self::Key => "~key-",
            Self::Field => "~field-",
        }
    }
}

/// Projects one Str key component (or a Str-named table or namespace) to its
/// host component, escaping every byte outside `[A-Za-z0-9._-]` as uppercase
/// `%HH` and escaping the reserved names, trailing dots and DOS device
/// basenames that would otherwise alias. A name over the component bound
/// becomes the deterministic `~key-` alias of its canonical OVB-1 encoding
/// instead of being dropped or merged (VFS-016).
///
/// # Two surfaces, two documents
///
/// The mount names rows by `profiles/vfs-1.md:35-45`: `%HH` with uppercase
/// hex, a literal `%` and `~` always escaped, and `~empty` for the empty key.
/// A repository *key path* (`tests/path-vectors.json`, ORNA-PATH-001 through
/// 012) names the same rows by `~hh` with lowercase hex and `~ff` for the empty
/// component. Those are separate surfaces and neither is the other's alias: in
/// a mount component `~hh` is a literal `~` followed by hex digits, because the
/// mount escapes every literal `~`, so a key `~2e` is `%7E2e` and a key `~ff`
/// is `%7Eff`. Accepting `~hh` here would give one canonical key two host
/// spellings, the aliasing profiles/vfs-1.md:40-43 forbids for `~empty`.
/// Work ADR 0122 and `crates/orna-cli/tests/vfs_key_escape_spellings.rs`
/// record and exercise the split.
pub fn project_component(text: &str) -> String {
    project_namespaced(text, VfsNameNamespace::Key, || str_component_digest(text))
}

/// Projects one typed key component from its canonical key text and its
/// canonical OVB-1 encoding (for a row key, `TypedKey::canonical_bytes`). The
/// text decides the spelling; a component over the bound becomes the `~key-`
/// alias of the encoding's SHA-256, because VFS-1 digests the canonical typed
/// key component, so the text `1` and the integer 1 never share an alias
/// (VFS-016).
pub fn project_key_component(text: &str, canonical_encoding: &[u8]) -> String {
    project_namespaced(text, VfsNameNamespace::Key, || {
        Sha256::digest(canonical_encoding).into()
    })
}

/// Projects one stored field name to its host component. Field components use
/// the `~field-` alias prefix for names over the byte bound, so the same long
/// text in a key and in a field never lands on one host name (VFS-016).
pub fn project_field_name(text: &str) -> String {
    project_namespaced(text, VfsNameNamespace::Field, || field_name_digest(text))
}

/// Projects a generated table directory whose plain name a source path already
/// occupies. The source path keeps the name; the generated component escapes its
/// first byte, not the schema name (profiles/vfs-1.md:31-33).
pub fn project_colliding_table_component(text: &str) -> String {
    project_namespaced_escaping(text, VfsNameNamespace::Key, true, || {
        str_component_digest(text)
    })
}

fn project_namespaced(
    text: &str,
    namespace: VfsNameNamespace,
    component_digest: impl FnOnce() -> [u8; 32],
) -> String {
    project_namespaced_escaping(
        text,
        namespace,
        is_reserved_component(text),
        component_digest,
    )
}

fn project_namespaced_escaping(
    text: &str,
    namespace: VfsNameNamespace,
    force_first: bool,
    component_digest: impl FnOnce() -> [u8; 32],
) -> String {
    if text.is_empty() {
        return "~empty".to_owned();
    }
    // Every byte outside `[A-Za-z0-9._-]` is escaped, `~` and `%` included, so a
    // spelling under the bound can never claim the alias namespace and always
    // stays an exact, reversible `%HH` encoding of the canonical text.
    let out = escape_component_bytes(text.bytes().enumerate(), force_first);
    if out.len() > VFS_MAX_COMPONENT_BYTES {
        long_alias(namespace, &component_digest())
    } else {
        out
    }
}

/// Escapes one canonical component's bytes: every byte outside
/// `[A-Za-z0-9._-]` becomes uppercase `%HH`, a reserved first byte is escaped,
/// and trailing dots are escaped so no host spelling ends in `.`.
fn escape_component_bytes<I>(bytes: I, force_first: bool) -> String
where
    I: Iterator<Item = (usize, u8)>,
{
    let mut out = String::new();
    for (index, byte) in bytes {
        let allowed = byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        if allowed && !(force_first && index == 0) {
            out.push(byte as char);
        } else {
            out.push_str(&percent_byte(byte));
        }
    }
    let trailing = out.len() - out.trim_end_matches('.').len();
    if trailing > 0 {
        let stem = out.len() - trailing;
        out.truncate(stem);
        for _ in 0..trailing {
            out.push_str(&percent_byte(b'.'));
        }
    }
    out
}

fn percent_byte(byte: u8) -> String {
    format!("%{byte:02X}")
}

/// True for the names the profile reserves: `.`, `..`, `.orna`, the row document `data.orna`
/// (profiles/vfs-1.md:44-50), every Git metadata spelling, and Windows device basenames in any
/// case with any extension.
pub(super) fn is_reserved_component(text: &str) -> bool {
    if matches!(text, "." | ".." | ".orna" | VFS_ROW_DOCUMENT)
        || orna_foundation::is_git_metadata_component(text.as_bytes())
    {
        return true;
    }
    let stem = text
        .trim_end_matches([' ', '.'])
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4
        && matches!(stem.get(..3), Some("COM" | "LPT"))
        && matches!(bytes[3], b'1'..=b'9')
}

/// `~key-<64 hex SHA-256 of the canonical typed key component>` for keys and
/// `~field-<the same digest form>` for stored field names (VFS-1, "Row/key
/// directory mapping"). The digest names the component's canonical OVB-1
/// encoding, never its host spelling or raw UTF-8 bytes, so one derivation
/// serves projection and verification and a digest can never name a
/// component it did not come from (VFS-016).
fn long_alias(namespace: VfsNameNamespace, digest: &[u8; 32]) -> String {
    let mut alias = String::with_capacity(namespace.alias_prefix().len() + 64);
    alias.push_str(namespace.alias_prefix());
    for byte in digest {
        alias.push_str(&format!("{byte:02x}"));
    }
    alias
}

/// The SHA-256 of one Str component's canonical OVB-1 encoding: the typed
/// component a long key or field alias names.
fn str_component_digest(text: &str) -> [u8; 32] {
    let encoding = OvbValue::new(Raw::Text(text.to_owned()))
        .and_then(|value| value.encode())
        .expect("every Str has a canonical OVB-1 encoding");
    Sha256::digest(encoding).into()
}

/// The digest bound into a `~field-` alias for one stored field name: the
/// SHA-256 of the name's canonical OVB-1 Str encoding. The snapshot-bound
/// long-name index re-derives it from the full retained name, so a colliding
/// digest is detected instead of merged (VFS-016).
pub fn field_name_digest(field: &str) -> [u8; 32] {
    str_component_digest(field)
}

/// Projects one stored Blob field name to its sibling content-file name:
/// escaped field name plus the MIME-1 selected suffix. A field that would spell
/// the reserved row document keeps its own distinct name (VFS-016).
///
/// Stored annotations are canonical MIME-1, so a pair the registry refuses is
/// corrupt backing metadata. It is reported as `CorruptAnnotation` (EIO)
/// instead of being projected under a guessed `bin` name (W08-09).
pub fn project_content_name(
    field: &str,
    media_type: &str,
    suffix: Option<&str>,
) -> Result<String, VfsPathError> {
    let annotation = MimeRegistry::mime1()
        .canonical_annotation(media_type, suffix)
        .map_err(|_| VfsPathError::CorruptAnnotation)?;
    let selected = annotation.selected_suffix();
    // The suffix is part of the projected component, so VFS-016's component
    // bound covers the whole `field + "." + suffix` spelling rather than the
    // escaped field base alone. A base that leaves no room for the suffix takes
    // the same `~field-` digest alias an over-bound field name takes: the alias
    // is derived from the stored field name, so the full name is still
    // re-derivable and verified by the long-name index (VFS-016).
    if project_field_name(field).len() + 1 + selected.len() > VFS_MAX_COMPONENT_BYTES {
        return Ok(format!(
            "{}.{}",
            long_alias(VfsNameNamespace::Field, &field_name_digest(field)),
            selected
        ));
    }
    let mut name = project_field_name(field);
    name.push('.');
    name.push_str(selected);
    if name == VFS_ROW_DOCUMENT {
        name.replace_range(..1, &format!("%{:02X}", b'd'));
    }
    Ok(name)
}

/// The projected sibling name of a stored Str projected as text: the escaped
/// field name plus `.txt`. A field whose name cannot fit the component bound
/// takes the same digest alias a Blob takes.
pub fn project_text_name(field: &str) -> Result<String, VfsPathError> {
    const TEXT_SUFFIX: &str = "txt";
    if project_field_name(field).len() + 1 + TEXT_SUFFIX.len() > VFS_MAX_COMPONENT_BYTES {
        return Ok(format!(
            "{}.{}",
            long_alias(VfsNameNamespace::Field, &field_name_digest(field)),
            TEXT_SUFFIX
        ));
    }
    let mut name = project_field_name(field);
    name.push('.');
    name.push_str(TEXT_SUFFIX);
    Ok(name)
}

/// Whether `suffix` is a hint MIME-1 admits for this media type: a compatible
/// suffix of a registered type, or any safe suffix for octet-stream and
/// unregistered types (MIME-1, VFS-012).
pub fn content_suffix_is_compatible(media_type: &str, suffix: &str) -> bool {
    classify_content_suffix(media_type, Some(suffix)).is_ok()
}

/// The canonical stored hint for an editor-selected suffix (MIME-1,
/// ORNA-VFS-012).
///
/// `selected` is the suffix the sibling file now spells; `None` means the
/// projected preferred suffix. The result is the hint to record, or `None` when
/// the selection is the field's preferred suffix, which the profile stores as
/// an absent hint rather than a redundant one. The registry's hint rules
/// decide: a registered type takes only a compatible suffix, while
/// octet-stream and unregistered types take any safe suffix.
///
/// A rename changes only this hint: it never transcodes the stored bytes,
/// renames a column, or rekeys the row, and a refused suffix is a malformed
/// candidate. The caller applies the result through the normal CAS/activation
/// boundary.
pub fn classify_content_suffix(
    media_type: &str,
    selected: Option<&str>,
) -> Result<Option<String>, VfsPathError> {
    let Some(selected) = selected else {
        return Ok(None);
    };
    MimeRegistry::mime1()
        .canonical_annotation(media_type, Some(selected))
        .map(|annotation| annotation.suffix().map(str::to_owned))
        .map_err(|_| VfsPathError::InvalidDocument)
}

/// Decodes one projected component back to its canonical text, refusing any
/// spelling that is not exactly what the projection would produce. Aliases
/// therefore cannot target a different row or field.
///
/// `~empty` is the mount's one empty-key spelling; the repository key path's
/// `~ff` and every other `~hh` spelling are refused (see
/// [`project_component`] and work ADR 0122).
pub fn unproject_component(component: &str) -> Result<VfsComponent, VfsPathError> {
    if let Some(hex) = component.strip_prefix("~key-") {
        if hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            let mut digest = [0u8; 32];
            for (index, slot) in digest.iter_mut().enumerate() {
                *slot = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                    .map_err(|_| VfsPathError::WrongNamespace)?;
            }
            return Ok(VfsComponent::LongAlias { digest });
        }
        return Err(VfsPathError::WrongNamespace);
    }
    if component == "~empty" {
        return Ok(VfsComponent::Canonical(String::new()));
    }
    if component.contains('~') {
        return Err(VfsPathError::WrongNamespace);
    }
    if component.len() > VFS_MAX_COMPONENT_BYTES {
        return Err(VfsPathError::NameTooLong);
    }
    let mut text = Vec::with_capacity(component.len());
    let bytes = component.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = component
                .get(at + 1..at + 3)
                .ok_or(VfsPathError::WrongNamespace)?;
            if !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
            {
                return Err(VfsPathError::WrongNamespace);
            }
            text.push(u8::from_str_radix(hex, 16).map_err(|_| VfsPathError::WrongNamespace)?);
            at += 3;
            continue;
        }
        let byte = bytes[at];
        if !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')) {
            return Err(VfsPathError::WrongNamespace);
        }
        text.push(byte);
        at += 1;
    }
    let text = String::from_utf8(text).map_err(|_| VfsPathError::WrongNamespace)?;
    if project_component(&text) != component {
        return Err(VfsPathError::WrongNamespace);
    }
    Ok(VfsComponent::Canonical(text))
}
