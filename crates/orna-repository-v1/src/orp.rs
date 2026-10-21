//! ORP-1, the one ordered-map partition of repository format 3.
//!
//! store-format-3 §3 pins where page boundaries fall. Entries accumulate in
//! logical key order. A page closes before an entry that would push its
//! encoded node data past 64 KiB, its entry count past 256 or its distinct
//! native refs past 256, and after an entry whose height-specific anchor is
//! set once the page holds at least 16 entries, or at exactly 256 entries.
//! The final tail is emitted even when short. Each branch level repeats the
//! rule over its children's inclusive maximum keys until one root remains.
//!
//! Every size here is the exact encoded size of the node the page becomes,
//! with its header, domain and entry-count head, and every ref count is the
//! page's set of distinct native refs. The pages are therefore the canonical
//! ones `tools/final_model.py` computes, and one builder serves leaves,
//! branches, the relation map and row maps.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::{
    native_graph::{
        GraphError, MAX_GRAPH_HEIGHT, MAX_REFS, NODE_DATA_LIMIT, NativeOid, NodeData,
        OrderedBranchEntry, OrderedLeafEntry, leaf_value_dependencies,
    },
    row_store::{MAX_PAGE_ENTRIES, MIN_PAGE_ENTRIES, ORP_ANCHOR_BITS, ORP_DOMAIN, TypedKey},
};

/// Whether `token` anchors a page boundary at `height`: the low six bits of
/// `SHA256("orna.orp.boundary.v1" NUL || uint32be(height) || token)` are zero.
pub(crate) fn boundary_anchor(height: u8, token: &[u8]) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(ORP_DOMAIN);
    hasher.update(u32::from(height).to_be_bytes());
    hasher.update(token);
    let digest = hasher.finalize();
    digest[31] & ((1 << ORP_ANCHOR_BITS) - 1) == 0
}

/// Length of the shortest definite CBOR head for `argument`.
pub(crate) const fn cbor_head_len(argument: u64) -> usize {
    if argument < 24 {
        1
    } else if argument <= 0xff {
        2
    } else if argument <= 0xffff {
        3
    } else if argument <= 0xffff_ffff {
        5
    } else {
        9
    }
}

/// Encoded bytes of a leaf node `[1, 1, domain, entries]` before the head of
/// its entry array.
pub(crate) fn leaf_header_len(domain: &[u8]) -> usize {
    3 + domain.len()
}

/// Encoded bytes of a branch node `[1, 2, domain, height, entries]` before
/// the head of its entry array.
pub(crate) fn branch_header_len(domain: &[u8], height: u8) -> usize {
    3 + domain.len() + cbor_head_len(u64::from(height))
}

/// The encoded bytes and the native refs one leaf entry adds to its page:
/// the `[key, value]` pair and the dependencies its value names in `domain`.
pub(crate) fn leaf_entry_cost(
    domain: &[u8],
    entry: &OrderedLeafEntry,
) -> Result<(usize, Vec<NativeOid>), GraphError> {
    let refs = leaf_value_dependencies(domain, &entry.value)?
        .iter()
        .map(|dependency| dependency.oid().clone())
        .collect();
    Ok((1 + entry.key.len() + entry.value.len(), refs))
}

/// The encoded bytes and the native ref one branch entry
/// `[inclusive_max_key, child_oid, row_count]` adds to its page.
pub(crate) fn branch_entry_cost(entry: &OrderedBranchEntry) -> (usize, Vec<NativeOid>) {
    let oid = entry.child.as_bytes().len();
    let size = 1
        + entry.inclusive_max_key.len()
        + cbor_head_len(oid as u64)
        + oid
        + cbor_head_len(entry.row_count);
    (size, vec![entry.child.clone()])
}

/// An entry of one ORP-1 level, which supplies the boundary token.
pub(crate) trait PageEntry {
    /// The canonical logical key the boundary anchor hashes: a row's key, or
    /// a child's inclusive maximum key one level up.
    fn token(&self) -> &[u8];
}

impl PageEntry for OrderedLeafEntry {
    fn token(&self) -> &[u8] {
        &self.key
    }
}

impl PageEntry for OrderedBranchEntry {
    fn token(&self) -> &[u8] {
        &self.inclusive_max_key
    }
}

/// The page of one ORP-1 level under construction.
pub(crate) struct Partition<E> {
    height: u8,
    header: usize,
    entries: Vec<E>,
    entry_bytes: usize,
    refs: BTreeSet<NativeOid>,
}

impl<E: PageEntry> Partition<E> {
    /// An empty page for a node of `header` bytes before its entry array.
    pub(crate) fn new(header: usize, height: u8) -> Self {
        Self {
            height,
            header,
            entries: Vec::new(),
            entry_bytes: 0,
            refs: BTreeSet::new(),
        }
    }

    fn node_len(&self, entries: usize, entry_bytes: usize) -> usize {
        self.header
            .saturating_add(cbor_head_len(entries as u64))
            .saturating_add(entry_bytes)
    }

    /// Whether an entry of `size` encoded bytes naming `refs` must start a
    /// new page: the page is nonempty and the entry would push it past 256
    /// entries, 64 KiB of node data or 256 distinct native refs.
    pub(crate) fn closes_before(&self, size: usize, refs: &[NativeOid]) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let entries = self.entries.len() + 1;
        let added = refs.iter().filter(|oid| !self.refs.contains(*oid)).count();
        entries > MAX_PAGE_ENTRIES
            || self.node_len(entries, self.entry_bytes.saturating_add(size)) > NODE_DATA_LIMIT
            || self.refs.len() + added > MAX_REFS
    }

    /// Adds one entry. The page closes first when the entry would overflow
    /// it, and closes after the entry on an anchor at 16 or more entries, or
    /// at exactly 256. Every closed page goes to `emit`, in key order.
    ///
    /// An entry too large for a node of its own is rejected: it must use the
    /// overflow encoding first, never oversize a page (store-format-3 §3).
    pub(crate) fn push(
        &mut self,
        entry: E,
        size: usize,
        refs: Vec<NativeOid>,
        emit: &mut impl FnMut(Vec<E>) -> Result<(), GraphError>,
    ) -> Result<(), GraphError> {
        if self.closes_before(size, &refs) {
            self.close(emit)?;
        }
        self.entry_bytes = self.entry_bytes.saturating_add(size);
        self.refs.extend(refs);
        self.entries.push(entry);
        let node_len = self.node_len(self.entries.len(), self.entry_bytes);
        if node_len > NODE_DATA_LIMIT {
            return Err(GraphError::NodeDataTooLarge(node_len));
        }
        if self.refs.len() > MAX_REFS {
            return Err(GraphError::FanoutExceeded(self.refs.len()));
        }
        let count = self.entries.len();
        let anchored = count >= MIN_PAGE_ENTRIES
            && self
                .entries
                .last()
                .is_some_and(|entry| boundary_anchor(self.height, entry.token()));
        if count == MAX_PAGE_ENTRIES || anchored {
            self.close(emit)?;
        }
        Ok(())
    }

    fn close(
        &mut self,
        emit: &mut impl FnMut(Vec<E>) -> Result<(), GraphError>,
    ) -> Result<(), GraphError> {
        self.entry_bytes = 0;
        self.refs.clear();
        emit(std::mem::take(&mut self.entries))
    }

    /// Emits the final nonempty tail, even when it holds fewer than 16
    /// entries.
    pub(crate) fn finish(
        mut self,
        emit: &mut impl FnMut(Vec<E>) -> Result<(), GraphError>,
    ) -> Result<(), GraphError> {
        if !self.entries.is_empty() {
            self.close(emit)?;
        }
        Ok(())
    }
}

/// Builds the canonical ORP-1 map of `entries`, which must be in strictly
/// ascending logical key order. Every node is written through `write_node`,
/// the leaves first and then each branch level, and the root is returned.
/// An empty map is one empty leaf (store-format-3 §2).
pub(crate) fn build_ordered_map(
    domain: &[u8],
    entries: impl IntoIterator<Item = OrderedLeafEntry>,
    write_node: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
) -> Result<NativeOid, GraphError> {
    let mut children = Vec::new();
    {
        let mut emit = |page: Vec<OrderedLeafEntry>| -> Result<(), GraphError> {
            children.push(write_leaf(domain, page, write_node)?);
            Ok(())
        };
        let mut leaves = Partition::new(leaf_header_len(domain), 0);
        let mut previous: Option<TypedKey> = None;
        for entry in entries {
            let key =
                TypedKey::decode_canonical(&entry.key).map_err(|_| GraphError::NonCanonicalData)?;
            if previous.as_ref().is_some_and(|previous| previous >= &key) {
                return Err(GraphError::NonCanonicalData);
            }
            previous = Some(key);
            let (size, refs) = leaf_entry_cost(domain, &entry)?;
            leaves.push(entry, size, refs, &mut emit)?;
        }
        leaves.finish(&mut emit)?;
    }
    if children.is_empty() {
        return write_node(&NodeData::OrderedLeaf {
            domain: domain.to_vec(),
            entries: Vec::new(),
        });
    }
    build_branch_levels(domain, 1, children, write_node)
}

/// Builds the branch levels above `level`, the entries naming the nodes one
/// height below `height` in key order, until one node remains; that node is
/// the root, so a root with one child collapses to the child.
pub(crate) fn build_branch_levels(
    domain: &[u8],
    mut height: u8,
    mut level: Vec<OrderedBranchEntry>,
    write_node: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
) -> Result<NativeOid, GraphError> {
    while level.len() > 1 {
        if height > MAX_GRAPH_HEIGHT {
            return Err(GraphError::HeightExceeded(height));
        }
        let children = level.len();
        let mut next = Vec::new();
        {
            let mut emit = |page: Vec<OrderedBranchEntry>| -> Result<(), GraphError> {
                next.push(write_branch(domain, height, page, write_node)?);
                Ok(())
            };
            let mut branches = Partition::new(branch_header_len(domain, height), height);
            for child in level {
                let (size, refs) = branch_entry_cost(&child);
                branches.push(child, size, refs, &mut emit)?;
            }
            branches.finish(&mut emit)?;
        }
        // Children too large to share a node never converge on a root.
        if next.len() >= children {
            return Err(GraphError::FanoutExceeded(children));
        }
        level = next;
        height += 1;
    }
    level
        .pop()
        .map(|entry| entry.child)
        .ok_or(GraphError::InvalidCount(0))
}

/// Writes one leaf page and returns the entry its parent names it by.
pub(crate) fn write_leaf(
    domain: &[u8],
    page: Vec<OrderedLeafEntry>,
    write_node: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
) -> Result<OrderedBranchEntry, GraphError> {
    let inclusive_max_key = page
        .last()
        .map(|entry| entry.key.clone())
        .ok_or(GraphError::InvalidCount(0))?;
    let row_count = page.len() as u64;
    let child = write_node(&NodeData::OrderedLeaf {
        domain: domain.to_vec(),
        entries: page,
    })?;
    Ok(OrderedBranchEntry {
        inclusive_max_key,
        child,
        row_count,
    })
}

/// Writes one branch page and returns the entry its parent names it by.
pub(crate) fn write_branch(
    domain: &[u8],
    height: u8,
    page: Vec<OrderedBranchEntry>,
    write_node: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
) -> Result<OrderedBranchEntry, GraphError> {
    let inclusive_max_key = page
        .last()
        .map(|entry| entry.inclusive_max_key.clone())
        .ok_or(GraphError::InvalidCount(0))?;
    let row_count = page.iter().try_fold(0u64, |sum, entry| {
        sum.checked_add(entry.row_count)
            .ok_or(GraphError::InvalidCount(usize::MAX))
    })?;
    let child = write_node(&NodeData::OrderedBranch {
        domain: domain.to_vec(),
        height,
        entries: page,
    })?;
    Ok(OrderedBranchEntry {
        inclusive_max_key,
        child,
        row_count,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value as Json;

    use super::*;
    use crate::native_graph::{
        ByteIndexEntry, CborValue, GitHashAlgorithm, NativeNodeEnvelope, NativeObjectKind,
        canonical_value_bytes,
    };

    /// Roots the reference model builds; see the fixture's README.
    const ORP1_VECTORS: &str = include_str!("../tests/fixtures/orp1-vectors/orp1-vectors.json");
    /// The release's golden native objects (Orna 1.1.0 tests/final-vectors.json).
    const RELEASE_VECTORS: &str =
        include_str!("../../orna-value-v1/src/fixtures/reference/tests/final-vectors.json");
    const BLOB_MEDIA_TYPE: &str = "application/octet-stream";

    fn sha256(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The Git object ID of `bytes` stored as `kind`, computed in process.
    fn object_id(algorithm: GitHashAlgorithm, kind: &str, bytes: &[u8]) -> NativeOid {
        let header = format!("{kind} {}\0", bytes.len());
        let digest = match algorithm {
            GitHashAlgorithm::Sha1 => {
                let mut hasher = <sha1::Sha1 as sha1::Digest>::new();
                sha1::Digest::update(&mut hasher, header.as_bytes());
                sha1::Digest::update(&mut hasher, bytes);
                sha1::Digest::finalize(hasher).to_vec()
            }
            GitHashAlgorithm::Sha256 => {
                let mut hasher = Sha256::new();
                hasher.update(header.as_bytes());
                hasher.update(bytes);
                hasher.finalize().to_vec()
            }
        };
        NativeOid::new(algorithm, digest).expect("a digest of the native width")
    }

    fn tree_entry(mode: &str, name: &str, oid: &NativeOid) -> Vec<u8> {
        let mut entry = Vec::with_capacity(mode.len() + name.len() + 2 + oid.as_bytes().len());
        entry.extend_from_slice(mode.as_bytes());
        entry.push(b' ');
        entry.extend_from_slice(name.as_bytes());
        entry.push(0);
        entry.extend_from_slice(oid.as_bytes());
        entry
    }

    /// Every object of one node's OGS-1 envelope: its `data` blob, its `refs`
    /// tree when it names dependencies, and the envelope tree, whose ID comes
    /// first. The tree layout is store-format-3 §1.
    fn envelope_objects(
        algorithm: GitHashAlgorithm,
        node: &NodeData,
    ) -> (NativeOid, Vec<(NativeOid, &'static str, Vec<u8>)>) {
        let envelope = NativeNodeEnvelope::from_node(node).expect("a canonical node");
        let data = object_id(algorithm, "blob", &envelope.data);
        let mut tree = tree_entry("100644", "data", &data);
        let mut objects = vec![(data, "blob", envelope.data.clone())];
        if !envelope.refs.is_empty() {
            let mut refs = envelope.refs.clone();
            refs.sort_by_key(|reference| reference.oid().to_hex());
            let mut refs_tree = Vec::new();
            for reference in &refs {
                let mode = match reference.kind() {
                    NativeObjectKind::Tree => "40000",
                    NativeObjectKind::Blob => "100644",
                };
                refs_tree.extend(tree_entry(mode, &reference.oid().to_hex(), reference.oid()));
            }
            let refs_oid = object_id(algorithm, "tree", &refs_tree);
            tree.extend(tree_entry("40000", "refs", &refs_oid));
            objects.push((refs_oid, "tree", refs_tree));
        }
        let root = object_id(algorithm, "tree", &tree);
        objects.push((root.clone(), "tree", tree));
        (root, objects)
    }

    /// The in-process hasher and envelope layout these tests rely on
    /// reproduce the release's own golden objects for one Blob graph.
    #[test]
    fn release_blob_graph_vectors_match_byte_for_byte() {
        let vectors: Json = serde_json::from_str(RELEASE_VECTORS).expect("release vectors");
        let graph = &vectors["native_graph"];
        assert_eq!(graph["algorithm"], "sha1");
        let algorithm = GitHashAlgorithm::Sha1;
        let payload = b"abc";
        let chunk = object_id(algorithm, "blob", payload);
        let chunk_sha256 = sha256(payload);
        let (byte_index, mut objects) = envelope_objects(
            algorithm,
            &NodeData::ByteIndex {
                height: 0,
                total_length: payload.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: payload.len() as u64,
                    target: chunk.clone(),
                    chunk_sha256: Some(chunk_sha256),
                }],
            },
        );
        let (descriptor, descriptor_objects) = envelope_objects(
            algorithm,
            &NodeData::BlobDescriptor {
                length: payload.len() as u64,
                raw_sha256: chunk_sha256,
                byte_root: Some(byte_index),
            },
        );
        objects.extend(descriptor_objects);
        objects.push((chunk, "blob", payload.to_vec()));
        assert_eq!(graph["blob_sha256"], hex(&chunk_sha256));
        assert_eq!(graph["descriptor_oid"], descriptor.to_hex());

        let mut actual: Vec<(String, String, String)> = objects
            .into_iter()
            .map(|(oid, kind, bytes)| (oid.to_hex(), kind.to_owned(), hex(&bytes)))
            .collect();
        actual.sort();
        let mut expected: Vec<(String, String, String)> = graph["objects"]
            .as_array()
            .expect("release object list")
            .iter()
            .map(|object| {
                (
                    object["oid"].as_str().expect("oid").to_owned(),
                    object["kind"].as_str().expect("kind").to_owned(),
                    object["bytes_hex"].as_str().expect("bytes").to_owned(),
                )
            })
            .collect();
        expected.sort();
        assert_eq!(actual, expected);
    }

    fn uint(value: u64) -> CborValue {
        CborValue::Unsigned(value)
    }

    fn text(value: String) -> CborValue {
        CborValue::Text(value)
    }

    fn entry(key: CborValue, value: CborValue) -> OrderedLeafEntry {
        OrderedLeafEntry {
            key: canonical_value_bytes(&key),
            value: canonical_value_bytes(&value),
        }
    }

    fn json_u64(value: &Json) -> u64 {
        value.as_u64().expect("an unsigned vector parameter")
    }

    fn json_hex(value: &Json) -> Vec<u8> {
        let text = value.as_str().expect("a hex vector parameter");
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
            .collect()
    }

    /// Rebuilds one vector recipe exactly as `scripts/generate_orp1_vectors.py`
    /// spells it for the reference model: the domain, then entries in key
    /// order.
    fn recipe(
        vectors: &Json,
        case: &Json,
        algorithm: GitHashAlgorithm,
    ) -> (Vec<u8>, Vec<OrderedLeafEntry>) {
        let width = algorithm.width();
        let rows_domain = canonical_value_bytes(&CborValue::Array(vec![
            text("rows".to_owned()),
            CborValue::Bytes(json_hex(&vectors["rows_domain"]["relation_hex"])),
            CborValue::Bytes(json_hex(&vectors["rows_domain"]["schema_digest_hex"])),
        ]));
        let letter = |index: u64| char::from(b'a' + (index % 26) as u8).to_string();
        match case["recipe"].as_str().expect("recipe") {
            "int-rows" => (
                rows_domain,
                (0..json_u64(&case["rows"]))
                    .map(|index| {
                        entry(
                            uint(index),
                            CborValue::Array(vec![uint(index % 251), text(format!("row {index}"))]),
                        )
                    })
                    .collect(),
            ),
            "text-lengths" => (
                rows_domain,
                case["text_lengths"]
                    .as_array()
                    .expect("text lengths")
                    .iter()
                    .enumerate()
                    .map(|(index, length)| {
                        let index = index as u64;
                        entry(
                            uint(index),
                            CborValue::Array(vec![text(
                                letter(index).repeat(json_u64(length) as usize),
                            )]),
                        )
                    })
                    .collect(),
            ),
            "shared-blobs" => {
                let pool = json_u64(&case["pool"]);
                let per_row = json_u64(&case["refs_per_row"]);
                let stride = json_u64(&case["stride"]);
                let blob = |member: u64| {
                    let descriptor =
                        sha256(format!("orna orp1 vector descriptor {member}").as_bytes());
                    CborValue::Tag(
                        60_111,
                        Box::new(CborValue::Array(vec![
                            uint(member + 1),
                            CborValue::Bytes(
                                sha256(format!("orna orp1 vector payload {member}").as_bytes())
                                    .to_vec(),
                            ),
                            text(BLOB_MEDIA_TYPE.to_owned()),
                            CborValue::Null,
                            CborValue::Bytes(descriptor[..width].to_vec()),
                        ])),
                    )
                };
                (
                    rows_domain,
                    (0..json_u64(&case["rows"]))
                        .map(|index| {
                            entry(
                                uint(index),
                                CborValue::Array(
                                    (0..per_row)
                                        .map(|k| blob((index * stride + k) % pool))
                                        .collect(),
                                ),
                            )
                        })
                        .collect(),
                )
            }
            "long-text-keys" => {
                let key_length = json_u64(&case["key_length"]) as usize;
                (
                    rows_domain,
                    (0..json_u64(&case["rows"]))
                        .map(|index| {
                            entry(
                                text(format!("{index:05}") + &"k".repeat(key_length - 5)),
                                CborValue::Array(vec![uint(index)]),
                            )
                        })
                        .collect(),
                )
            }
            "relations" => {
                let domain = canonical_value_bytes(&CborValue::Array(vec![
                    text("relations".to_owned()),
                    CborValue::Bytes(json_hex(&vectors["relations_database_hex"])),
                ]));
                let mut entries: Vec<OrderedLeafEntry> = (0..json_u64(&case["rows"]))
                    .map(|index| {
                        let digest = |label: &str| {
                            sha256(format!("orna orp1 vector {label} {index}").as_bytes())
                        };
                        entry(
                            CborValue::Bytes(digest("relation")[..16].to_vec()),
                            CborValue::Array(vec![
                                CborValue::Bytes(digest("schema")[..width].to_vec()),
                                CborValue::Bytes(digest("primary")[..width].to_vec()),
                                CborValue::Null,
                                uint(index),
                            ]),
                        )
                    })
                    .collect();
                entries.sort_by_key(|entry| {
                    TypedKey::decode_canonical(&entry.key).expect("a relation key")
                });
                (domain, entries)
            }
            other => panic!("unknown vector recipe {other}"),
        }
    }

    /// store-format-3 §3 and ORNA-ROW-003: the complete build is the
    /// canonical partition. Each recipe's root and every node at every height
    /// must equal what `tools/final_model.py` builds, so size-bound cuts
    /// (including a node of exactly 65,536 bytes), distinct-ref cuts over
    /// shared Blobs, branch size cuts and both Git hashes are byte-identical
    /// to the model (A07-storage-rows-06).
    #[test]
    fn complete_build_matches_the_reference_model() {
        let vectors: Json = serde_json::from_str(ORP1_VECTORS).expect("ORP-1 vectors");
        let cases = vectors["cases"].as_array().expect("vector cases");
        assert_eq!(cases.len(), 9);
        for case in cases {
            let id = case["id"].as_str().expect("case id");
            let algorithm = match case["algorithm"].as_str().expect("algorithm") {
                "sha1" => GitHashAlgorithm::Sha1,
                "sha256" => GitHashAlgorithm::Sha256,
                other => panic!("{id}: unknown algorithm {other}"),
            };
            let (domain, entries) = recipe(&vectors, case, algorithm);
            let mut levels: BTreeMap<u8, Vec<String>> = BTreeMap::new();
            let root = build_ordered_map(&domain, entries, &mut |node| {
                let height = match node {
                    NodeData::OrderedLeaf { .. } => 0,
                    NodeData::OrderedBranch { height, .. } => *height,
                    other => panic!("{id}: the builder wrote a {:?} node", other.kind()),
                };
                let encoded = node.encode_canonical()?;
                assert!(encoded.len() <= NODE_DATA_LIMIT, "{id}: oversized node");
                let (oid, _) = envelope_objects(algorithm, node);
                levels.entry(height).or_default().push(oid.to_hex());
                Ok(oid)
            })
            .unwrap_or_else(|error| panic!("{id}: {error:?}"));
            assert_eq!(case["root"], root.to_hex(), "{id}: root");
            let expected: Vec<Vec<String>> = case["levels"]
                .as_array()
                .expect("levels")
                .iter()
                .map(|level| {
                    level
                        .as_array()
                        .expect("level")
                        .iter()
                        .map(|oid| oid.as_str().expect("oid").to_owned())
                        .collect()
                })
                .collect();
            assert_eq!(
                levels.into_values().collect::<Vec<_>>(),
                expected,
                "{id}: nodes by height"
            );
        }
    }

    fn rows_domain() -> Vec<u8> {
        canonical_value_bytes(&CborValue::Array(vec![
            text("rows".to_owned()),
            CborValue::Bytes(vec![0x11; 16]),
            CborValue::Bytes(vec![0x22; 32]),
        ]))
    }

    /// An entry that cannot fit a node of its own is refused instead of
    /// written as an oversized page; it needs the overflow encoding first.
    /// The value itself decodes (it is under 64 KiB); the node it would make,
    /// with the domain and headers, is over.
    #[test]
    fn an_entry_too_large_for_a_node_is_refused() {
        let oversized = entry(
            uint(1),
            CborValue::Array(vec![text("x".repeat(65_500))]),
        );
        let mut writes = 0;
        let result = build_ordered_map(&rows_domain(), [oversized], &mut |_| {
            writes += 1;
            Err(GraphError::NonCanonicalData)
        });
        assert!(matches!(result, Err(GraphError::NodeDataTooLarge(_))));
        assert_eq!(writes, 0);
    }

    /// The build requires strictly ascending logical keys.
    #[test]
    fn unordered_or_duplicate_keys_are_refused() {
        for keys in [[2u64, 1], [1, 1]] {
            let entries = keys.map(|key| entry(uint(key), CborValue::Array(Vec::new())));
            assert_eq!(
                build_ordered_map(&rows_domain(), entries, &mut |_| unreachable!()),
                Err(GraphError::NonCanonicalData)
            );
        }
    }
}
