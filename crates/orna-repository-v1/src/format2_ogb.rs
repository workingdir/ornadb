//! OGB-1 content-root verification for format-2 Blob storage (read-only).
//!
//! Sole authority: the draft compatibility material in
//! `reference/Orna-1.1.0/compatibility/previous-1.1.0-draft.zip`, entry
//! `Orna-1.1.0/Orna-1.1.0.md`, section "Binary descriptors" (clauses
//! ORNA-OGB-004 full verification and ORNA-OGB-005 range reads). The byte
//! layouts follow that text and the draft reference model
//! `Orna-1.1.0/tools/blob_model.py` (`metadata`, `node`, `verify`).
//!
//! This module never writes, never fetches over the network and never resolves
//! `.orna/blobs/` itself. The caller supplies an [`OgbObjects`] view of the Git
//! object database. A missing object is reported as [`OgbError::Missing`],
//! which is distinct from structural or integrity failures.

// `sha2` 0.11 implements `digest` 0.11 while `sha1` 0.10 implements `digest`
// 0.10; name each trait through its own crate so both hashers resolve.
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Digest as Sha2Digest, Sha256};

/// Largest admitted length: 2^63 - 1 bytes.
pub const OGB_MAX_LENGTH: u64 = (1 << 63) - 1;
/// Largest admitted leaf chunk: 1 MiB.
pub const OGB_MAX_CHUNK: u64 = 1 << 20;
/// Maximum entries per index node.
pub const OGB_FANOUT: usize = 256;
/// Maximum tree height, enforced before recursion.
pub const OGB_MAX_HEIGHT: u64 = 8;

const MODE_BLOB: &str = "100644";
const MODE_TREE: &str = "40000";

/// One Git tree entry as presented by the caller's object view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OgbTreeEntry {
    pub name: String,
    pub mode: String,
    pub oid: Vec<u8>,
}

/// Read-only view of the Git object database needed for verification.
pub trait OgbObjects {
    /// Entries of the tree with this OID, or `None` if the object is absent.
    fn tree(&self, oid: &[u8]) -> Option<Vec<OgbTreeEntry>>;
    /// Raw bytes of the blob with this OID, or `None` if the object is absent.
    fn blob(&self, oid: &[u8]) -> Option<Vec<u8>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OgbError {
    /// A referenced Git object is not available. Not evidence of corruption.
    Missing { oid_hex: String },
    /// Structure, canonical CBOR, modes, names, widths or ranges are invalid.
    Malformed(&'static str),
    /// Structure is valid but content bytes do not match a recorded digest or length.
    Integrity(&'static str),
}

impl std::fmt::Display for OgbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { oid_hex } => write!(f, "OGB-1 object {oid_hex} is missing"),
            Self::Malformed(why) => write!(f, "OGB-1 structure is malformed: {why}"),
            Self::Integrity(why) => write!(f, "OGB-1 integrity check failed: {why}"),
        }
    }
}

impl std::error::Error for OgbError {}

/// A fully verified content root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OgbVerified {
    pub content_sha256: [u8; 32],
    pub total_length: u64,
}

/// Verifies every node and recomputes the raw-content SHA-256 (ORNA-OGB-004).
///
/// `root_entries` are the entries of the content-root tree, which must contain
/// exactly `meta` and `data`.
pub fn verify_ogb_root<S: OgbObjects>(
    store: &S,
    root_entries: &[OgbTreeEntry],
) -> Result<OgbVerified, OgbError> {
    exact_names(root_entries, &["data", "meta"])?;
    let meta_entry = find(root_entries, "meta")?;
    let data_entry = find(root_entries, "data")?;
    if meta_entry.mode != MODE_BLOB || data_entry.mode != MODE_TREE {
        return Err(OgbError::Malformed("root entry modes"));
    }

    let meta_bytes = blob(store, &meta_entry.oid)?;
    let meta = decode_exact(&meta_bytes)?;
    let Item::Array(meta) = meta else {
        return Err(OgbError::Malformed("meta shape"));
    };
    let [version, digest, total, data_oid] =
        <[Item; 4]>::try_from(meta).map_err(|_| OgbError::Malformed("meta arity"))?;
    if version != Item::Uint(1) {
        return Err(OgbError::Malformed("meta version"));
    }
    let Item::Bytes(digest) = digest else {
        return Err(OgbError::Malformed("meta digest"));
    };
    let digest = <[u8; 32]>::try_from(digest.as_slice())
        .map_err(|_| OgbError::Malformed("meta digest width"))?;
    let Item::Uint(total) = total else {
        return Err(OgbError::Malformed("meta total"));
    };
    if total > OGB_MAX_LENGTH {
        return Err(OgbError::Malformed("meta total range"));
    }
    let Item::Bytes(data_oid) = data_oid else {
        return Err(OgbError::Malformed("meta data oid"));
    };
    if data_oid != data_entry.oid {
        return Err(OgbError::Malformed("meta/tree mismatch"));
    }
    let width = oid_width(&data_oid)?;

    let mut hasher = Sha256::new();
    let mut streamed = 0u64;
    verify_node(
        store,
        &data_oid,
        width,
        Some(total),
        None,
        &mut hasher,
        &mut streamed,
    )?;
    let actual: [u8; 32] = hasher.finalize().into();
    if streamed != total {
        return Err(OgbError::Integrity("total length"));
    }
    if actual != digest {
        return Err(OgbError::Integrity("whole-content digest"));
    }
    Ok(OgbVerified {
        content_sha256: digest,
        total_length: total,
    })
}

/// Recursively verifies one data node. Chunk bytes are fed to `hasher` in order.
fn verify_node<S: OgbObjects>(
    store: &S,
    oid: &[u8],
    width: usize,
    expected_len: Option<u64>,
    expected_height: Option<u64>,
    hasher: &mut Sha256,
    streamed: &mut u64,
) -> Result<(), OgbError> {
    let entries = tree(store, oid)?;
    let index_entry = find(&entries, "index")?;
    if index_entry.mode != MODE_BLOB {
        return Err(OgbError::Malformed("index mode"));
    }
    let index_bytes = blob(store, &index_entry.oid)?;
    let Item::Array(index) = decode_exact(&index_bytes)? else {
        return Err(OgbError::Malformed("node shape"));
    };
    let [version, height, total, children] =
        <[Item; 4]>::try_from(index).map_err(|_| OgbError::Malformed("node arity"))?;
    if version != Item::Uint(1) {
        return Err(OgbError::Malformed("node version"));
    }
    let Item::Uint(height) = height else {
        return Err(OgbError::Malformed("node height"));
    };
    if height > OGB_MAX_HEIGHT {
        return Err(OgbError::Malformed("node height range"));
    }
    let Item::Uint(total) = total else {
        return Err(OgbError::Malformed("node total"));
    };
    if total > OGB_MAX_LENGTH {
        return Err(OgbError::Malformed("node total range"));
    }
    if expected_len.is_some_and(|len| len != total) {
        return Err(OgbError::Malformed("child length"));
    }
    if expected_height.is_some_and(|h| h != height) {
        return Err(OgbError::Malformed("child height"));
    }
    let Item::Array(children) = children else {
        return Err(OgbError::Malformed("node entries"));
    };
    if children.len() > OGB_FANOUT || (children.is_empty() && (height != 0 || total != 0)) {
        return Err(OgbError::Malformed("node fanout"));
    }

    let child_names: Vec<String> = (0..children.len()).map(|i| format!("{i:03x}")).collect();
    let mut expected_names: Vec<&str> = vec!["index"];
    expected_names.extend(child_names.iter().map(String::as_str));
    exact_names(&entries, &expected_names)?;

    let mut end = 0u64;
    for (i, child) in children.into_iter().enumerate() {
        let Item::Array(fields) = child else {
            return Err(OgbError::Malformed("entry shape"));
        };
        let [stop, target, digest] =
            <[Item; 3]>::try_from(fields).map_err(|_| OgbError::Malformed("entry arity"))?;
        let Item::Uint(stop) = stop else {
            return Err(OgbError::Malformed("entry interval"));
        };
        if !(end < stop && stop <= OGB_MAX_LENGTH) {
            return Err(OgbError::Malformed("entry interval range"));
        }
        let Item::Bytes(target) = target else {
            return Err(OgbError::Malformed("entry target"));
        };
        if target.len() != width {
            return Err(OgbError::Malformed("entry oid width"));
        }
        let child_entry = find(&entries, &child_names[i])?;
        let want_mode = if height == 0 { MODE_BLOB } else { MODE_TREE };
        if child_entry.mode != want_mode || child_entry.oid != target {
            return Err(OgbError::Malformed("entry target"));
        }
        let length = stop - end;
        if height == 0 {
            let Item::Bytes(chunk_digest) = digest else {
                return Err(OgbError::Malformed("chunk digest"));
            };
            let chunk_digest = <[u8; 32]>::try_from(chunk_digest.as_slice())
                .map_err(|_| OgbError::Malformed("chunk digest width"))?;
            if length > OGB_MAX_CHUNK {
                return Err(OgbError::Malformed("chunk size"));
            }
            let data = blob(store, &target)?;
            if data.len() as u64 != length {
                return Err(OgbError::Integrity("chunk length"));
            }
            let got: [u8; 32] = Sha256::digest(&data).into();
            if got != chunk_digest {
                return Err(OgbError::Integrity("chunk digest"));
            }
            hasher.update(&data);
            *streamed += length;
        } else {
            if digest != Item::Null {
                return Err(OgbError::Malformed("internal digest"));
            }
            verify_node(
                store,
                &target,
                width,
                Some(length),
                Some(height - 1),
                hasher,
                streamed,
            )?;
        }
        end = stop;
    }
    if end != total {
        return Err(OgbError::Malformed("node total"));
    }
    Ok(())
}

fn oid_width(oid: &[u8]) -> Result<usize, OgbError> {
    match oid.len() {
        20 | 32 => Ok(oid.len()),
        _ => Err(OgbError::Malformed("OID width")),
    }
}

fn find<'a>(entries: &'a [OgbTreeEntry], name: &str) -> Result<&'a OgbTreeEntry, OgbError> {
    entries
        .iter()
        .find(|e| e.name == name)
        .ok_or(OgbError::Malformed("missing entry"))
}

/// Requires exactly the given names, each present once, and no others.
fn exact_names(entries: &[OgbTreeEntry], expected: &[&str]) -> Result<(), OgbError> {
    let mut seen: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    seen.sort_unstable();
    let mut want: Vec<&str> = expected.to_vec();
    want.sort_unstable();
    if seen.windows(2).any(|w| w[0] == w[1]) {
        return Err(OgbError::Malformed("duplicate names"));
    }
    if seen != want {
        return Err(OgbError::Malformed("unexpected names"));
    }
    Ok(())
}

fn tree<S: OgbObjects>(store: &S, oid: &[u8]) -> Result<Vec<OgbTreeEntry>, OgbError> {
    store.tree(oid).ok_or_else(|| missing(oid))
}

fn blob<S: OgbObjects>(store: &S, oid: &[u8]) -> Result<Vec<u8>, OgbError> {
    store.blob(oid).ok_or_else(|| missing(oid))
}

fn missing(oid: &[u8]) -> OgbError {
    OgbError::Missing {
        oid_hex: oid.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

/// Deterministic CBOR subset used by OGB descriptors: definite arrays,
/// unsigned shortest integers, byte strings and null. No tags, maps or floats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Uint(u64),
    Bytes(Vec<u8>),
    Null,
    Array(Vec<Item>),
}

/// Decodes exactly one item occupying all of `bytes`.
pub fn decode_exact(bytes: &[u8]) -> Result<Item, OgbError> {
    let mut pos = 0usize;
    let item = decode_item(bytes, &mut pos, 0)?;
    if pos != bytes.len() {
        return Err(OgbError::Malformed("trailing CBOR bytes"));
    }
    Ok(item)
}

fn decode_item(bytes: &[u8], pos: &mut usize, depth: usize) -> Result<Item, OgbError> {
    // A descriptor nests at most three levels (index -> entries -> entry).
    if depth > 4 {
        return Err(OgbError::Malformed("CBOR depth"));
    }
    let initial = *bytes
        .get(*pos)
        .ok_or(OgbError::Malformed("CBOR truncated"))?;
    *pos += 1;
    let major = initial >> 5;
    let info = initial & 0x1f;
    if info == 0x16 && major == 7 {
        return Ok(Item::Null);
    }
    let arg = read_argument(bytes, pos, info)?;
    match major {
        0 => Ok(Item::Uint(arg)),
        2 => {
            let len = usize::try_from(arg).map_err(|_| OgbError::Malformed("CBOR length"))?;
            let end = pos
                .checked_add(len)
                .filter(|end| *end <= bytes.len())
                .ok_or(OgbError::Malformed("CBOR truncated"))?;
            let out = bytes[*pos..end].to_vec();
            *pos = end;
            Ok(Item::Bytes(out))
        }
        4 => {
            let count = usize::try_from(arg).map_err(|_| OgbError::Malformed("CBOR length"))?;
            if count > bytes.len().saturating_sub(*pos) {
                return Err(OgbError::Malformed("CBOR truncated"));
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(decode_item(bytes, pos, depth + 1)?);
            }
            Ok(Item::Array(items))
        }
        _ => Err(OgbError::Malformed("CBOR item not admitted")),
    }
}

/// Reads an additional-information argument in shortest form only.
fn read_argument(bytes: &[u8], pos: &mut usize, info: u8) -> Result<u64, OgbError> {
    let (width, min) = match info {
        0..=23 => return Ok(u64::from(info)),
        24 => (1usize, 24u64),
        25 => (2, 1 << 8),
        26 => (4, 1 << 16),
        27 => (8, 1 << 32),
        _ => return Err(OgbError::Malformed("CBOR argument not admitted")),
    };
    let end = pos
        .checked_add(width)
        .filter(|end| *end <= bytes.len())
        .ok_or(OgbError::Malformed("CBOR truncated"))?;
    let mut value = 0u64;
    for b in &bytes[*pos..end] {
        value = (value << 8) | u64::from(*b);
    }
    *pos = end;
    if value < min {
        return Err(OgbError::Malformed("CBOR non-shortest form"));
    }
    Ok(value)
}

/// Git object id of `data` of the given kind (SHA-1 repository).
pub fn git_sha1_oid(kind: &str, data: &[u8]) -> Vec<u8> {
    let mut hasher = sha1::Sha1::new();
    hasher.update(format!("{kind} {}\0", data.len()).as_bytes());
    hasher.update(data);
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    enum Obj {
        Blob(Vec<u8>),
        Tree(Vec<OgbTreeEntry>),
    }

    #[derive(Default)]
    struct MemStore {
        objects: HashMap<Vec<u8>, Obj>,
    }

    impl MemStore {
        fn put_blob(&mut self, data: &[u8]) -> Vec<u8> {
            let oid = git_sha1_oid("blob", data);
            self.objects.insert(oid.clone(), Obj::Blob(data.to_vec()));
            oid
        }
        fn put_tree(&mut self, mut entries: Vec<OgbTreeEntry>) -> Vec<u8> {
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            let mut raw = Vec::new();
            for e in &entries {
                raw.extend_from_slice(e.mode.as_bytes());
                raw.push(b' ');
                raw.extend_from_slice(e.name.as_bytes());
                raw.push(0);
                raw.extend_from_slice(&e.oid);
            }
            let oid = git_sha1_oid("tree", &raw);
            self.objects.insert(oid.clone(), Obj::Tree(entries));
            oid
        }
    }

    impl OgbObjects for MemStore {
        fn tree(&self, oid: &[u8]) -> Option<Vec<OgbTreeEntry>> {
            match self.objects.get(oid)? {
                Obj::Tree(entries) => Some(entries.clone()),
                Obj::Blob(_) => None,
            }
        }
        fn blob(&self, oid: &[u8]) -> Option<Vec<u8>> {
            match self.objects.get(oid)? {
                Obj::Blob(data) => Some(data.clone()),
                Obj::Tree(_) => None,
            }
        }
    }

    fn entry(name: &str, mode: &str, oid: &[u8]) -> OgbTreeEntry {
        OgbTreeEntry {
            name: name.to_owned(),
            mode: mode.to_owned(),
            oid: oid.to_vec(),
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn cbor_head(major: u8, n: u64, out: &mut Vec<u8>) {
        let m = major << 5;
        if n < 24 {
            out.push(m | n as u8);
        } else if n < 256 {
            out.push(m | 24);
            out.push(n as u8);
        } else {
            out.push(m | 25);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
    }

    fn cbor_bytes(data: &[u8], out: &mut Vec<u8>) {
        cbor_head(2, data.len() as u64, out);
        out.extend_from_slice(data);
    }

    /// Builds the one-chunk root described by the zip vector "abc" and returns
    /// `(store, root_entries)`. Descriptor bytes follow ORNA-OGB-004 layout.
    fn abc_root(chunk: &[u8]) -> (MemStore, Vec<OgbTreeEntry>) {
        let mut store = MemStore::default();
        let digest: [u8; 32] = Sha256::digest(b"abc").into();
        let chunk_oid = store.put_blob(chunk);

        let mut index = vec![0x84, 0x01, 0x00, 0x03, 0x81, 0x83, 0x03];
        cbor_bytes(&chunk_oid, &mut index);
        cbor_bytes(&digest, &mut index);
        let index_oid = store.put_blob(&index);

        let data_oid = store.put_tree(vec![
            entry("000", MODE_BLOB, &chunk_oid),
            entry("index", MODE_BLOB, &index_oid),
        ]);

        let mut meta = vec![0x84, 0x01];
        cbor_bytes(&digest, &mut meta);
        meta.push(0x03);
        cbor_bytes(&data_oid, &mut meta);
        let meta_oid = store.put_blob(&meta);

        let root = vec![
            entry("data", MODE_TREE, &data_oid),
            entry("meta", MODE_BLOB, &meta_oid),
        ];
        (store, root)
    }

    #[test]
    fn zip_abc_vector_known_answers_match() {
        // Vector "abc" from Orna-1.1.0/tests/blob-vectors.json.
        let (store, root) = abc_root(b"abc");
        let verified = verify_ogb_root(&store, &root).unwrap();
        assert_eq!(verified.total_length, 3);
        assert_eq!(
            hex(&verified.content_sha256),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&git_sha1_oid("blob", b"abc")),
            "f2ba8f84ab5c1bce84a7b441cb1959cfc7093b7f"
        );
    }

    #[test]
    fn rejects_tampered_chunk_bytes_as_integrity_failure() {
        let (mut store, root) = abc_root(b"abc");
        let oid = git_sha1_oid("blob", b"abc");
        store.objects.insert(oid, Obj::Blob(b"abd".to_vec()));
        assert!(matches!(
            verify_ogb_root(&store, &root),
            Err(OgbError::Integrity(_))
        ));
    }

    #[test]
    fn missing_chunk_is_distinct_from_corruption() {
        let (mut store, root) = abc_root(b"abc");
        store.objects.remove(&git_sha1_oid("blob", b"abc"));
        assert!(matches!(
            verify_ogb_root(&store, &root),
            Err(OgbError::Missing { .. })
        ));
    }

    #[test]
    fn rejects_extra_entry_in_data_tree() {
        let (mut store, root) = abc_root(b"abc");
        let data_oid = root[0].oid.clone();
        let Some(Obj::Tree(mut entries)) = store.objects.remove(&data_oid) else {
            panic!("data tree missing");
        };
        entries.push(entry("zzz", MODE_BLOB, &git_sha1_oid("blob", b"abc")));
        store.objects.insert(data_oid, Obj::Tree(entries));
        assert!(matches!(
            verify_ogb_root(&store, &root),
            Err(OgbError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_non_shortest_cbor_and_trailing_bytes() {
        // 0x18 0x05 encodes 5 with a one-byte argument: not shortest form.
        assert_eq!(
            decode_exact(&[0x18, 0x05]),
            Err(OgbError::Malformed("CBOR non-shortest form"))
        );
        assert_eq!(
            decode_exact(&[0x01, 0x00]),
            Err(OgbError::Malformed("trailing CBOR bytes"))
        );
    }

    #[test]
    fn rejects_height_above_maximum() {
        let (mut store, root) = abc_root(b"abc");
        let data_oid = root[0].oid.clone();
        let Some(Obj::Tree(entries)) = store.objects.get(&data_oid) else {
            panic!("data tree missing");
        };
        let index_oid = entries
            .iter()
            .find(|e| e.name == "index")
            .unwrap()
            .oid
            .clone();
        // [version=1, height=OGB_MAX_HEIGHT+1, total=3, children=[]]
        let bad = vec![0x84, 0x01, (OGB_MAX_HEIGHT as u8 + 1), 0x03, 0x80];
        store.objects.insert(index_oid, Obj::Blob(bad));
        assert_eq!(
            verify_ogb_root(&store, &root).unwrap_err(),
            OgbError::Malformed("node height range")
        );
    }
}
