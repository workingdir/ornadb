//! The `sys.RowRef<T>` of every canonical `sys` relation.
//!
//! Every `sys.XRef` alias of `api/sys.json` is a snapshot-pinned `sys.RowRef<sys.X>`: a
//! reference to one row of the canonical relation `sys.X` that preserves the row's natural
//! key and its resolution context and introduces no synthetic identifier (ORNA-SYS-112).
//! This module is the one place that defines that encoding for the 78 relations, so a
//! provider builds `sys.BranchRef`, `sys.CommitRef` or `sys.ColumnRef` through the same
//! checked path and a reader recognises any of them by its relation identity.
//!
//! # Canonical encoding
//!
//! A reference is the OVB tag-60010 value `[database_id, table_id, key, snapshot]`
//! (`source/29-formats.md`, "Type-preserving values"):
//!
//! | Slot | Content |
//! |---|---|
//! | `database_id` | the attached database's UUID; every snapshot pin names the same database |
//! | `table_id` | the relation's physical identity, [`RelationSpec::table_id`]. The specification names the relations but assigns no sixteen-byte identity, so these are versioned implementation compatibility constants, never per-runtime values |
//! | `key` | the natural key of the row, declared by [`RelationSpec::key`]: the bare component of a one-field key, or the array of the components of a composite key in declaration order |
//! | `snapshot` | the snapshot or runtime pin the reference resolves in |
//!
//! Each key component has the representation of its declared type ([`KeyPartKind`]):
//! UUID-backed identifiers are tag-37 UUIDs, 32-byte identities are byte strings, a
//! `sys.GitOid` is `[algorithm, bytes]` with `"sha1"` and 20 bytes or `"sha256"` and 32
//! bytes (the representation inside tag 60023, without that tag's nominal wrapper, because
//! the relation identity already fixes the key's type), a `Str` is text, an optional
//! component is the component or null, and a reference component is the canonical form of
//! that reference.
//!
//! A reference constructs coordinates only. It never proves that the row exists, is
//! retained or is visible to the caller, and it grants no authority (ORNA-SYS-114): the
//! operation that dereferences it checks all of that.
//!
//! The encodings that predate this registry (`sys.Object`, `sys.Snapshot`, `sys.Run`,
//! `sys.Checkpoint`, `sys.Failure`, ...) are registered bit for bit as their dedicated
//! constructors in the contract module produce them.

use std::fmt;

use num_bigint::{BigInt, Sign};

use crate::contract::{
    ensure_snapshot_database, row_ref_from_raw, row_ref_raw, snapshot_id, uuid as uuid_raw,
};
use crate::relations;
use crate::{
    CanonicalSnapshot, CanonicalValue, ContractError, ENUM_TAG, Error, GitHash,
    OPAQUE_SYSTEM_ID_TAG, ObjectRef, OvbRaw, PATH_TAG, RowRef, SystemReferenceError, UUID_TAG,
    enum_type_id, enum_variant_id,
};

/// The representation of one natural-key component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyPartKind {
    /// A UUID-backed identifier (`sys.DatabaseId`, `sys.RunId`, ...): a tag-37 UUID.
    Uuid,
    /// A 32-byte identity (`sys.RevisionId`, or a digest recorded by the evaluator): a byte string.
    Digest,
    /// A `sys.GitOid`: `[algorithm, bytes]`.
    GitOid,
    /// A nonempty `Str`.
    Text,
    /// A `Str?`: a nonempty `Str`, or null when absent.
    OptionalText,
    /// A nonnegative `Int` that fits a 64-bit unsigned integer.
    Int,
    /// A `Path`: the tag-60024 `[flavour, text]`.
    Path,
    /// The reference's own snapshot pin, in the pin's canonical form.
    Pin,
    /// The 32-byte `sys.SnapshotId` of the reference's own snapshot pin.
    PinId,
    /// A reference to a row of the named relation.
    Reference(&'static str),
    /// A reference to a row of the named relation, or null when absent.
    OptionalReference(&'static str),
    /// A payload-free variant of the named closed `sys` enum.
    Enum(&'static str),
    /// A structured supporting value (`sys.ChangeTarget`) held as its canonical value.
    Canonical,
    /// A structured supporting value, or null when absent (`sys.SourceSpan?`).
    OptionalCanonical,
}

/// One field of a relation's natural key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyField {
    /// The field's name in `api/sys.json` (`change.area` for a field of a nested record).
    pub name: &'static str,
    /// The field's declared type in `api/sys.json`.
    pub ty: &'static str,
    /// How the field is held inside the reference's key.
    pub kind: KeyPartKind,
}

/// One canonical `sys` relation and the identity of the references to its rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelationSpec {
    /// The relation, as `api/sys.json` names it (`sys.Branch`).
    pub name: &'static str,
    /// The reference alias of the relation (`sys.BranchRef`).
    pub reference_type: &'static str,
    /// The relation's physical identity: the `table_id` slot of every reference to its rows.
    pub table_id: [u8; 16],
    /// The fields of the natural key, in declaration order.
    pub key: &'static [KeyField],
}

/// The flavour of a `Path` key component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathFlavour {
    /// A safe relative slash-separated repository path.
    Repo,
    /// A POSIX host path.
    Posix,
    /// A Windows host path.
    Windows,
}

impl PathFlavour {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Posix => "posix",
            Self::Windows => "windows",
        }
    }
}

/// The qualified type name a `sys.GitOid` carries in its tag-60023 value.
pub const GIT_OID_TYPE_NAME: &str = "sys.GitOid";

/// A Git object id: its hash algorithm and exactly that algorithm's number of bytes.
///
/// This is the typed `sys.GitOid` (`source/29-formats.md`, "Portable opaque and contextual values"): an opaque
/// identity that compares by the object it names (ORNA-SYS-016), never as a `Str`, a `Uuid` or a digest. Its
/// canonical value is the tag-60023 `["sys.GitOid", [algorithm, bytes]]` ([`GitObjectId::to_opaque_raw`]), and
/// that is the only form [`GitObjectId::from_opaque_raw`] reads back. Ids order by algorithm and then by bytes,
/// which for one algorithm is the order of Git's lowercase hexadecimal spellings.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitObjectId {
    algorithm: GitHash,
    bytes: Vec<u8>,
}

impl GitObjectId {
    /// The object id `bytes` under `algorithm`.
    ///
    /// # Errors
    ///
    /// [`SystemReferenceError::InvalidRelationKey`] when `bytes` is not 20 bytes for SHA-1 or
    /// 32 bytes for SHA-256.
    pub fn new(
        algorithm: GitHash,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self, SystemReferenceError> {
        let bytes = bytes.into();
        if bytes.len() != algorithm.oid_width() {
            return Err(SystemReferenceError::InvalidRelationKey);
        }
        Ok(Self { algorithm, bytes })
    }

    /// The object id Git's lowercase hexadecimal spelling `hex` names: 40 digits are a SHA-1 id and 64 a
    /// SHA-256 id.
    ///
    /// # Errors
    ///
    /// [`SystemReferenceError::InvalidRelationKey`] when `hex` is not exactly 40 or 64 lowercase hexadecimal
    /// digits.
    pub fn from_hex(hex: &str) -> Result<Self, SystemReferenceError> {
        let algorithm = match hex.len() {
            40 => GitHash::Sha1,
            64 => GitHash::Sha256,
            _ => return Err(SystemReferenceError::InvalidRelationKey),
        };
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let bytes = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| Some((digit(pair[0])? << 4) | digit(pair[1])?))
            .collect::<Option<Vec<u8>>>()
            .ok_or(SystemReferenceError::InvalidRelationKey)?;
        Self::new(algorithm, bytes)
    }

    /// The object-hash algorithm.
    #[must_use]
    pub fn algorithm(&self) -> GitHash {
        self.algorithm
    }

    /// The raw object id bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The lowercase hexadecimal spelling Git prints for the object id.
    #[must_use]
    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(self.bytes.len() * 2);
        for byte in &self.bytes {
            text.push(char::from(DIGITS[usize::from(byte >> 4)]));
            text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        text
    }

    /// The canonical tag-60023 value of the object id: `["sys.GitOid", ["sha1", bytes20]]`, or
    /// `["sys.GitOid", ["sha256", bytes32]]`.
    #[must_use]
    pub fn to_opaque_raw(&self) -> OvbRaw {
        OvbRaw::Tag(
            OPAQUE_SYSTEM_ID_TAG,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Text(GIT_OID_TYPE_NAME.to_owned()),
                self.raw(),
            ])),
        )
    }

    /// The validated canonical `sys.GitOid` value of the object id.
    ///
    /// # Errors
    ///
    /// [`Error`] when the value does not validate, which an id built through [`GitObjectId::new`] never
    /// causes.
    pub fn to_canonical(&self) -> Result<CanonicalValue, Error> {
        CanonicalValue::new(self.to_opaque_raw())
    }

    /// The object id a tag-60023 `sys.GitOid` value holds, or `None` when `raw` is anything else: another opaque
    /// identifier (a `sys.RevisionId` is also tag 60023, and is not a Git object id), or a `sys.GitOid` whose
    /// algorithm and byte width disagree.
    #[must_use]
    pub fn from_opaque_raw(raw: &OvbRaw) -> Option<Self> {
        let OvbRaw::Tag(OPAQUE_SYSTEM_ID_TAG, body) = raw else {
            return None;
        };
        let OvbRaw::Array(parts) = body.as_ref() else {
            return None;
        };
        let [OvbRaw::Text(name), representation] = parts.as_slice() else {
            return None;
        };
        if name != GIT_OID_TYPE_NAME {
            return None;
        }
        Self::from_raw(representation)
    }

    /// The `[algorithm, bytes]` representation inside the tag-60023 wrapper, which is also how a `sys.GitOid`
    /// natural key of a reference is held.
    fn raw(&self) -> OvbRaw {
        OvbRaw::Array(vec![
            OvbRaw::Text(algorithm_name(self.algorithm).to_owned()),
            OvbRaw::Bytes(self.bytes.clone()),
        ])
    }

    fn from_raw(raw: &OvbRaw) -> Option<Self> {
        let OvbRaw::Array(parts) = raw else {
            return None;
        };
        let [OvbRaw::Text(algorithm), OvbRaw::Bytes(bytes)] = parts.as_slice() else {
            return None;
        };
        let algorithm = match algorithm.as_str() {
            "sha1" => GitHash::Sha1,
            "sha256" => GitHash::Sha256,
            _ => return None,
        };
        Self::new(algorithm, bytes.clone()).ok()
    }
}

/// The lowercase hexadecimal spelling Git prints.
impl fmt::Display for GitObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

const fn algorithm_name(algorithm: GitHash) -> &'static str {
    match algorithm {
        GitHash::Sha1 => "sha1",
        GitHash::Sha256 => "sha256",
    }
}

/// One natural-key component supplied to [`SysRef::new`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyPart {
    /// For [`KeyPartKind::Uuid`].
    Uuid([u8; 16]),
    /// For [`KeyPartKind::Digest`].
    Digest([u8; 32]),
    /// For [`KeyPartKind::GitOid`].
    GitOid(GitObjectId),
    /// For [`KeyPartKind::Text`] and [`KeyPartKind::OptionalText`].
    Text(String),
    /// For [`KeyPartKind::Int`].
    Int(u64),
    /// For [`KeyPartKind::Path`].
    Path {
        /// The path flavour.
        flavour: PathFlavour,
        /// The path text.
        text: String,
    },
    /// For [`KeyPartKind::Pin`]: the reference's own snapshot pin.
    Pin,
    /// For [`KeyPartKind::PinId`]: the identity of the reference's own snapshot pin.
    PinId,
    /// For [`KeyPartKind::Reference`] and [`KeyPartKind::OptionalReference`].
    Reference(RowRef),
    /// For [`KeyPartKind::Enum`]: the variant's bare name.
    Variant(String),
    /// For [`KeyPartKind::Canonical`] and [`KeyPartKind::OptionalCanonical`].
    Canonical(CanonicalValue),
    /// The absent value of an optional component.
    Null,
}

/// A checked `sys.XRef` of any canonical relation.
///
/// The relation is fixed by the reference's `table_id`, and the natural key has the shape
/// that relation declares, so a `SysRef` is exactly the reference its relation's rows expose
/// as `reference`. It states coordinates only; it does not prove that the row exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysRef {
    relation: &'static RelationSpec,
    row: RowRef,
}

impl SysRef {
    /// The reference to the row of `relation` whose natural key is `key`, pinned to `snapshot`.
    ///
    /// `key` holds one part per key field, in declaration order. A part must have the
    /// representation its field declares, and a reference part must name a row of the field's
    /// relation in the same database.
    ///
    /// # Errors
    ///
    /// [`SystemReferenceError::DatabaseMismatch`] when `snapshot` belongs to another database,
    /// and [`SystemReferenceError::InvalidRelationKey`] when `key` is not a valid natural key
    /// of `relation`.
    pub fn new(
        relation: &'static RelationSpec,
        database_id: [u8; 16],
        snapshot: CanonicalSnapshot,
        key: Vec<KeyPart>,
    ) -> Result<Self, SystemReferenceError> {
        ensure_snapshot_database(database_id, &snapshot)?;
        let key = encode_key(relation, key, &snapshot)?;
        validate_key(relation, &key, database_id, &snapshot)?;
        let row = RowRef::new(database_id, relation.table_id, key, snapshot)
            .map_err(|_| SystemReferenceError::InvalidRelationKey)?;
        Ok(Self { relation, row })
    }

    /// The checked reference that `row` is.
    ///
    /// # Errors
    ///
    /// [`SystemReferenceError::UnknownRelation`] when `row` does not address a canonical
    /// relation, and [`SystemReferenceError::InvalidRelationKey`] when its key is not a valid
    /// natural key of that relation.
    pub fn from_row_ref(row: RowRef) -> Result<Self, SystemReferenceError> {
        let relation =
            relations::by_table_id(row.table_id).ok_or(SystemReferenceError::UnknownRelation)?;
        validate_key(relation, &row.key, row.database_id, &row.snapshot)?;
        Ok(Self { relation, row })
    }

    /// The relation the reference addresses.
    #[must_use]
    pub fn relation(&self) -> &'static RelationSpec {
        self.relation
    }

    /// Whether the reference addresses `relation`.
    #[must_use]
    pub fn is_of(&self, relation: &RelationSpec) -> bool {
        self.relation.table_id == relation.table_id
    }

    /// The reference as its generic row reference.
    #[must_use]
    pub fn as_row_ref(&self) -> &RowRef {
        &self.row
    }

    /// The reference as its generic row reference.
    #[must_use]
    pub fn into_row_ref(self) -> RowRef {
        self.row
    }

    /// The canonical tag-60010 value of the reference.
    ///
    /// # Errors
    ///
    /// [`ContractError`] when the reference does not encode.
    pub fn to_canonical(&self) -> Result<CanonicalValue, ContractError> {
        self.row.to_canonical()
    }

    /// The natural key of a one-field `Str` key (`name` of `sys.Branch`, `sys.Tag`, ...), or
    /// `None` when the key is not a single text component.
    #[must_use]
    pub fn key_text(&self) -> Option<&str> {
        match &self.row.key {
            OvbRaw::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The natural key of a one-field `sys.GitOid` key (`oid` of `sys.Commit` and
    /// `sys.GitObject`), or `None` when the key is not a single object id.
    #[must_use]
    pub fn key_oid(&self) -> Option<GitObjectId> {
        GitObjectId::from_raw(&self.row.key)
    }
}

/// The reference to the row `name` of the relation `relation`, whose natural key is one `Str`.
fn named_reference(
    relation: &'static RelationSpec,
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    name: impl Into<String>,
) -> Result<SysRef, SystemReferenceError> {
    SysRef::new(
        relation,
        database_id,
        snapshot,
        vec![KeyPart::Text(name.into())],
    )
}

/// The reference to the row `oid` of the relation `relation`, whose natural key is one `sys.GitOid`.
fn oid_reference(
    relation: &'static RelationSpec,
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    oid: GitObjectId,
) -> Result<SysRef, SystemReferenceError> {
    SysRef::new(relation, database_id, snapshot, vec![KeyPart::GitOid(oid)])
}

/// The `sys.BranchRef` of branch `name`.
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn branch_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    name: impl Into<String>,
) -> Result<SysRef, SystemReferenceError> {
    named_reference(&relations::BRANCH, database_id, snapshot, name)
}

/// The `sys.CommitRef` of the commit `oid`.
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn commit_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    oid: GitObjectId,
) -> Result<SysRef, SystemReferenceError> {
    oid_reference(&relations::COMMIT, database_id, snapshot, oid)
}

/// The `sys.GitObjectRef` of the Git object `oid`.
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn git_object_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    oid: GitObjectId,
) -> Result<SysRef, SystemReferenceError> {
    oid_reference(&relations::GIT_OBJECT, database_id, snapshot, oid)
}

/// The `sys.RefRef` of the Git ref `name` (the full ref name).
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn git_ref_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    name: impl Into<String>,
) -> Result<SysRef, SystemReferenceError> {
    named_reference(&relations::REF, database_id, snapshot, name)
}

/// The `sys.TagRef` of tag `name`.
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn tag_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    name: impl Into<String>,
) -> Result<SysRef, SystemReferenceError> {
    named_reference(&relations::TAG, database_id, snapshot, name)
}

/// The `sys.RemoteRef` of remote `name`.
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn remote_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    name: impl Into<String>,
) -> Result<SysRef, SystemReferenceError> {
    named_reference(&relations::REMOTE, database_id, snapshot, name)
}

/// The `sys.StashRef` of the stash at `index`.
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn stash_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    index: u64,
) -> Result<SysRef, SystemReferenceError> {
    SysRef::new(
        &relations::STASH,
        database_id,
        snapshot,
        vec![KeyPart::Int(index)],
    )
}

/// The `sys.DatabaseRef` of the database `database_id`, pinned to `snapshot`.
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn database_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
) -> Result<SysRef, SystemReferenceError> {
    SysRef::new(
        &relations::DATABASE,
        database_id,
        snapshot,
        vec![KeyPart::Uuid(database_id)],
    )
}

/// The `sys.GitRepositoryRef` of the repository whose `database` row is `database`.
///
/// # Errors
///
/// As [`SysRef::new`].
pub fn git_repository_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    database: RowRef,
) -> Result<SysRef, SystemReferenceError> {
    SysRef::new(
        &relations::GIT_REPOSITORY,
        database_id,
        snapshot,
        vec![KeyPart::Reference(database)],
    )
}

/// The `sys.WorktreeEntryRef` of the worktree entry at the repository-relative `path`.
///
/// # Errors
///
/// As [`SysRef::new`]; `path` must be a safe repository path.
pub fn worktree_entry_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    path: &str,
) -> Result<SysRef, SystemReferenceError> {
    SysRef::new(
        &relations::WORKTREE_ENTRY,
        database_id,
        snapshot,
        vec![KeyPart::Path {
            flavour: PathFlavour::Repo,
            text: path.to_owned(),
        }],
    )
}

/// The reference to the row of an object-keyed `relation` (`sys.Table`, `sys.Column`,
/// `sys.Namespace`, ...) whose `object` is `object`, pinned to the object's own snapshot.
///
/// # Errors
///
/// As [`SysRef::new`]; the key of `relation` must be exactly one `sys.ObjectRef`.
pub fn object_keyed_reference(
    relation: &'static RelationSpec,
    object: ObjectRef,
) -> Result<SysRef, SystemReferenceError> {
    let object = object.into_row_ref();
    SysRef::new(
        relation,
        object.database_id,
        object.snapshot.clone(),
        vec![KeyPart::Reference(object)],
    )
}

/// The key of a reference to a row of `relation`, as the parts map onto its key fields.
fn encode_key(
    relation: &RelationSpec,
    parts: Vec<KeyPart>,
    snapshot: &CanonicalSnapshot,
) -> Result<OvbRaw, SystemReferenceError> {
    if parts.len() != relation.key.len() {
        return Err(SystemReferenceError::InvalidRelationKey);
    }
    let mut components = relation
        .key
        .iter()
        .zip(parts)
        .map(|(field, part)| encode_part(field.kind, part, snapshot))
        .collect::<Result<Vec<_>, _>>()?;
    if components.len() == 1 {
        Ok(components.remove(0))
    } else {
        Ok(OvbRaw::Array(components))
    }
}

/// The canonical form of `part` as a component of representation `kind`.
fn encode_part(
    kind: KeyPartKind,
    part: KeyPart,
    snapshot: &CanonicalSnapshot,
) -> Result<OvbRaw, SystemReferenceError> {
    use KeyPartKind as Kind;
    Ok(match (kind, part) {
        (Kind::Uuid, KeyPart::Uuid(id)) => uuid_raw(id),
        (Kind::Digest, KeyPart::Digest(digest)) => OvbRaw::Bytes(digest.to_vec()),
        (Kind::GitOid, KeyPart::GitOid(oid)) => oid.raw(),
        (Kind::Text | Kind::OptionalText, KeyPart::Text(text)) => OvbRaw::Text(text),
        (Kind::Int, KeyPart::Int(number)) => OvbRaw::Int(BigInt::from(number)),
        (Kind::Path, KeyPart::Path { flavour, text }) => OvbRaw::Tag(
            PATH_TAG,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Text(flavour.as_str().to_owned()),
                OvbRaw::Text(text),
            ])),
        ),
        (Kind::Pin, KeyPart::Pin) => snapshot.raw(),
        (Kind::PinId, KeyPart::PinId) => OvbRaw::Bytes(snapshot_id(snapshot)?.to_vec()),
        (Kind::Reference(_) | Kind::OptionalReference(_), KeyPart::Reference(row)) => {
            row_ref_raw(&row)
        }
        (Kind::Enum(name), KeyPart::Variant(variant)) => OvbRaw::Tag(
            ENUM_TAG,
            Box::new(OvbRaw::Array(vec![
                uuid_raw(enum_type_id(name)),
                uuid_raw(enum_variant_id(name, &variant)),
                OvbRaw::Null,
            ])),
        ),
        (Kind::Canonical | Kind::OptionalCanonical, KeyPart::Canonical(value)) => {
            value.raw().clone()
        }
        (
            Kind::OptionalText | Kind::OptionalReference(_) | Kind::OptionalCanonical,
            KeyPart::Null,
        ) => OvbRaw::Null,
        _ => return Err(SystemReferenceError::InvalidRelationKey),
    })
}

/// Checks that `key` is a natural key of `relation` in `database_id` at `snapshot`.
fn validate_key(
    relation: &RelationSpec,
    key: &OvbRaw,
    database_id: [u8; 16],
    snapshot: &CanonicalSnapshot,
) -> Result<(), SystemReferenceError> {
    match relation.key {
        [field] => validate_part(field.kind, key, database_id, snapshot),
        fields => {
            let OvbRaw::Array(parts) = key else {
                return Err(SystemReferenceError::InvalidRelationKey);
            };
            if parts.len() != fields.len() {
                return Err(SystemReferenceError::InvalidRelationKey);
            }
            fields.iter().zip(parts).try_for_each(|(field, part)| {
                validate_part(field.kind, part, database_id, snapshot)
            })
        }
    }
}

fn validate_part(
    kind: KeyPartKind,
    part: &OvbRaw,
    database_id: [u8; 16],
    snapshot: &CanonicalSnapshot,
) -> Result<(), SystemReferenceError> {
    use KeyPartKind as Kind;
    let valid = match kind {
        Kind::Uuid => matches!(part, OvbRaw::Tag(UUID_TAG, body)
            if matches!(body.as_ref(), OvbRaw::Bytes(bytes) if bytes.len() == 16)),
        Kind::Digest => matches!(part, OvbRaw::Bytes(bytes) if bytes.len() == 32),
        Kind::GitOid => GitObjectId::from_raw(part).is_some(),
        Kind::Text => matches!(part, OvbRaw::Text(text) if !text.is_empty()),
        Kind::OptionalText => {
            matches!(part, OvbRaw::Null) || matches!(part, OvbRaw::Text(text) if !text.is_empty())
        }
        Kind::Int => matches!(part, OvbRaw::Int(number)
            if number.sign() != Sign::Minus && *number <= BigInt::from(u64::MAX)),
        Kind::Path => is_path(part),
        Kind::Pin => *part == snapshot.raw(),
        Kind::PinId => {
            matches!(part, OvbRaw::Bytes(id) if id.as_slice() == snapshot_id(snapshot)?.as_slice())
        }
        Kind::Reference(target) => return validate_reference(target, part, database_id),
        Kind::OptionalReference(target) => {
            return if matches!(part, OvbRaw::Null) {
                Ok(())
            } else {
                validate_reference(target, part, database_id)
            };
        }
        Kind::Enum(name) => is_payload_free_variant(name, part),
        Kind::Canonical | Kind::OptionalCanonical => true,
    };
    if valid {
        Ok(())
    } else {
        Err(SystemReferenceError::InvalidRelationKey)
    }
}

/// Checks that `part` is a reference to a valid row of the relation `target`, in `database_id`.
fn validate_reference(
    target: &str,
    part: &OvbRaw,
    database_id: [u8; 16],
) -> Result<(), SystemReferenceError> {
    let nested = row_ref_from_raw(part).map_err(|_| SystemReferenceError::InvalidRelationKey)?;
    let relation = relations::by_name(target).ok_or(SystemReferenceError::InvalidRelationKey)?;
    if nested.table_id != relation.table_id || nested.database_id != database_id {
        return Err(SystemReferenceError::InvalidRelationKey);
    }
    validate_key(relation, &nested.key, nested.database_id, &nested.snapshot)
}

fn is_path(part: &OvbRaw) -> bool {
    let OvbRaw::Tag(PATH_TAG, body) = part else {
        return false;
    };
    let OvbRaw::Array(fields) = body.as_ref() else {
        return false;
    };
    matches!(
        fields.as_slice(),
        [OvbRaw::Text(flavour), OvbRaw::Text(text)]
            if matches!(flavour.as_str(), "repo" | "posix" | "windows") && !text.is_empty()
    )
}

fn is_payload_free_variant(enumeration: &str, part: &OvbRaw) -> bool {
    let OvbRaw::Tag(ENUM_TAG, body) = part else {
        return false;
    };
    let OvbRaw::Array(fields) = body.as_ref() else {
        return false;
    };
    let [type_id, variant_id, OvbRaw::Null] = fields.as_slice() else {
        return false;
    };
    *type_id == uuid_raw(enum_type_id(enumeration))
        && matches!(variant_id, OvbRaw::Tag(UUID_TAG, id)
            if matches!(id.as_ref(), OvbRaw::Bytes(bytes) if bytes.len() == 16))
}
