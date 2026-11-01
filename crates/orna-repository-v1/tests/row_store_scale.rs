use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use orna_foundation_v1::{OvbRaw as Raw, SchemaDescriptor};
use orna_repository_v1::{KeyRange, Repository, RowIndexMutation, TypedKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const SCALE_ROWS: u64 = 1_000_000;
const FIXTURE_SEED: u64 = 0x5eed_cafe;
const BUCKET_COUNT: u64 = 251;
const BUCKET_TO_FIND: u64 = 17;
const ROW_PAGE_LIMIT: usize = 256;
const MIN_PAGE_ENTRIES: usize = 16;
const MAX_PAGE_ENTRIES: usize = 256;
const MAX_REFS: usize = 256;
const NODE_DATA_LIMIT: usize = 65_536;
const PAGE_NODE_OVERHEAD: usize = 16;
const ORP_DOMAIN: &[u8] = b"orna.orp.boundary.v1\0";
const DATABASE: &str = include_str!("fixtures/format-context/database-final.orna");
const MAIN_SOURCE: &str = include_str!("fixtures/git-repository-main.orna");

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static HYDRATION_FETCHES: AtomicU64 = AtomicU64::new(0);
static HYDRATION_NANOS: AtomicU64 = AtomicU64::new(0);
const MILLION_ROWS_ENV: &str = "ORNA_ORP1_MILLION_ROWS";

struct Phase {
    label: &'static str,
    rows: u64,
    started: Instant,
    fetches: u64,
    fetch_nanos: u64,
}

impl Phase {
    fn new(label: &'static str, rows: u64) -> Self {
        Self {
            label,
            rows,
            started: Instant::now(),
            fetches: HYDRATION_FETCHES.load(Ordering::Relaxed),
            fetch_nanos: HYDRATION_NANOS.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        eprintln!(
            "ORP-1 phase label={} rows={} ms={} promisor_fetches={} promisor_fetch_ms={}",
            self.label,
            self.rows,
            self.started.elapsed().as_millis(),
            HYDRATION_FETCHES.load(Ordering::Relaxed) - self.fetches,
            (HYDRATION_NANOS.load(Ordering::Relaxed) - self.fetch_nanos) / 1_000_000,
        );
    }
}

fn timed<T>(label: &'static str, rows: u64, run: impl FnOnce() -> T) -> T {
    let _phase = Phase::new(label, rows);
    run()
}

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        result
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, new_size) };
        if !result.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        result
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Oid([u8; 20]);

impl Oid {
    fn from_hex(value: &str) -> Self {
        assert_eq!(value.len(), 40);
        let mut bytes = [0; 20];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
        }
        Self(bytes)
    }

    fn hex(self) -> String {
        let mut output = String::with_capacity(40);
        for byte in self.0 {
            use std::fmt::Write as _;
            write!(&mut output, "{byte:02x}").expect("write OID hex");
        }
        output
    }
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("invalid lowercase OID hex"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectKind {
    Blob,
    Tree,
    Commit,
    Tag,
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    oid: Oid,
    kind: ObjectKind,
}

struct NodeRecord {
    data_oid: Oid,
    data: Vec<u8>,
    edges: Vec<Edge>,
}

#[derive(Default)]
struct GraphFixture {
    nodes: HashMap<Oid, NodeRecord>,
    blobs: HashMap<Oid, Vec<u8>>,
    index_data_blobs: BTreeMap<Oid, usize>,
    index_tree_oids: BTreeSet<Oid>,
    row_media_descriptor: Option<Oid>,
    row_media_annotation: Option<Vec<u8>>,
    media_payload_oids: BTreeSet<Oid>,
}

struct TreeEntry {
    mode: &'static str,
    name: String,
    oid: Oid,
}

impl GraphFixture {
    fn add_blob(&mut self, bytes: Vec<u8>) -> Oid {
        let oid = git_object_oid("blob", &bytes);
        self.blobs.insert(oid, bytes);
        oid
    }

    fn add_node(&mut self, data: Vec<u8>, mut edges: Vec<Edge>, is_index: bool) -> Oid {
        edges.sort_by_key(|edge| edge.oid);
        edges.dedup_by_key(|edge| edge.oid);

        let data_oid = git_object_oid("blob", &data);
        let mut node_entries = vec![TreeEntry {
            mode: "100644",
            name: "data".to_owned(),
            oid: data_oid,
        }];
        if !edges.is_empty() {
            let refs_entries = edges
                .iter()
                .map(|edge| TreeEntry {
                    mode: match edge.kind {
                        ObjectKind::Blob => "100644",
                        ObjectKind::Tree => "40000",
                        ObjectKind::Commit | ObjectKind::Tag => {
                            panic!("non-tree Git object in directory entry")
                        }
                    },
                    name: edge.oid.hex(),
                    oid: edge.oid,
                })
                .collect();
            let refs_oid = git_tree_oid(refs_entries);
            node_entries.push(TreeEntry {
                mode: "40000",
                name: "refs".to_owned(),
                oid: refs_oid,
            });
        }
        let oid = git_tree_oid(node_entries);
        self.nodes.insert(
            oid,
            NodeRecord {
                data_oid,
                data,
                edges,
            },
        );
        if is_index {
            self.index_data_blobs
                .insert(data_oid, self.nodes[&oid].data.len());
            self.index_tree_oids.insert(oid);
        }
        oid
    }

    fn materialize_node(&self, oid: Oid, path: &Path) {
        let node = self.nodes.get(&oid).expect("node reference is present");
        fs::create_dir_all(path).expect("create native node tree");
        fs::write(path.join("data"), &node.data).expect("write native node data");
        if node.edges.is_empty() {
            return;
        }
        let refs_path = path.join("refs");
        fs::create_dir_all(&refs_path).expect("create native refs tree");
        for edge in &node.edges {
            let child = refs_path.join(edge.oid.hex());
            match edge.kind {
                ObjectKind::Tree => self.materialize_node(edge.oid, &child),
                ObjectKind::Blob => {
                    fs::write(&child, self.blobs.get(&edge.oid).expect("blob bytes"))
                        .expect("write native referenced Blob");
                }
                ObjectKind::Commit | ObjectKind::Tag => {
                    panic!("non-tree Git object in directory entry")
                }
            }
        }
    }
}

fn git_object_oid(kind: &str, bytes: &[u8]) -> Oid {
    let header = format!("{kind} {}\0", bytes.len());
    let mut hasher = <Sha1 as sha1::Digest>::new();
    sha1::Digest::update(&mut hasher, header.as_bytes());
    sha1::Digest::update(&mut hasher, bytes);
    let digest = sha1::Digest::finalize(hasher);
    let mut oid = [0; 20];
    oid.copy_from_slice(&digest);
    Oid(oid)
}

fn git_tree_oid(mut entries: Vec<TreeEntry>) -> Oid {
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let capacity = entries
        .iter()
        .map(|entry| entry.mode.len() + entry.name.len() + 22)
        .sum();
    let mut tree = Vec::with_capacity(capacity);
    for entry in entries {
        tree.extend_from_slice(entry.mode.as_bytes());
        tree.push(b' ');
        tree.extend_from_slice(entry.name.as_bytes());
        tree.push(0);
        tree.extend_from_slice(&entry.oid.0);
    }
    git_object_oid("tree", &tree)
}

fn cbor_head(output: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=255 => output.extend_from_slice(&[prefix | 24, value as u8]),
        256..=65_535 => {
            output.push(prefix | 25);
            output.extend_from_slice(&(value as u16).to_be_bytes());
        }
        65_536..=4_294_967_295 => {
            output.push(prefix | 26);
            output.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            output.push(prefix | 27);
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn cbor_uint(output: &mut Vec<u8>, value: u64) {
    cbor_head(output, 0, value);
}

fn cbor_array(output: &mut Vec<u8>, length: usize) {
    cbor_head(output, 4, length as u64);
}

fn cbor_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    cbor_head(output, 2, bytes.len() as u64);
    output.extend_from_slice(bytes);
}
fn cbor_text(output: &mut Vec<u8>, text: &str) {
    cbor_head(output, 3, text.len() as u64);
    output.extend_from_slice(text.as_bytes());
}

fn cbor_null(output: &mut Vec<u8>) {
    output.push(0xf6);
}

fn encode_uint(value: u64) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(9);
    cbor_uint(&mut encoded, value);
    encoded
}

fn encode_bucket(value: u64) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(3);
    cbor_array(&mut encoded, 1);
    cbor_uint(&mut encoded, value);
    encoded
}

fn encode_rows_domain(relation_id: [u8; 16], schema_digest: [u8; 32]) -> Vec<u8> {
    let mut domain = Vec::with_capacity(55);
    cbor_array(&mut domain, 3);
    cbor_head(&mut domain, 3, 4);
    domain.extend_from_slice(b"rows");
    cbor_bytes(&mut domain, &relation_id);
    cbor_bytes(&mut domain, &schema_digest);
    domain
}

fn encode_relations_domain(database_id: [u8; 16]) -> Vec<u8> {
    let mut domain = Vec::with_capacity(30);
    cbor_array(&mut domain, 2);
    cbor_head(&mut domain, 3, 9);
    domain.extend_from_slice(b"relations");
    cbor_bytes(&mut domain, &database_id);
    domain
}

fn add_synthetic_media(graph: &mut GraphFixture) {
    let media = (0..32_768usize)
        .map(|index| ((index * 37 + 11) & 0xff) as u8)
        .collect::<Vec<_>>();
    let raw_digest: [u8; 32] = Sha256::digest(&media).into();
    let media_oid = graph.add_blob(media.clone());
    graph.media_payload_oids.insert(media_oid);

    let mut byte_index = Vec::with_capacity(96);
    cbor_array(&mut byte_index, 5);
    cbor_uint(&mut byte_index, 1);
    cbor_uint(&mut byte_index, 4);
    cbor_uint(&mut byte_index, 0);
    cbor_uint(&mut byte_index, media.len() as u64);
    cbor_array(&mut byte_index, 1);
    cbor_array(&mut byte_index, 3);
    cbor_uint(&mut byte_index, media.len() as u64);
    cbor_bytes(&mut byte_index, &media_oid.0);
    cbor_bytes(&mut byte_index, &raw_digest);
    let byte_root = graph.add_node(
        byte_index,
        vec![Edge {
            oid: media_oid,
            kind: ObjectKind::Blob,
        }],
        false,
    );

    let mut descriptor = Vec::with_capacity(64);
    cbor_array(&mut descriptor, 5);
    cbor_uint(&mut descriptor, 1);
    cbor_uint(&mut descriptor, 5);
    cbor_uint(&mut descriptor, media.len() as u64);
    cbor_bytes(&mut descriptor, &raw_digest);
    cbor_bytes(&mut descriptor, &byte_root.0);
    let descriptor_oid = graph.add_node(
        descriptor,
        vec![Edge {
            oid: byte_root,
            kind: ObjectKind::Tree,
        }],
        false,
    );

    let mut annotation = Vec::with_capacity(96);
    cbor_head(&mut annotation, 6, 60111);
    cbor_array(&mut annotation, 5);
    cbor_uint(&mut annotation, media.len() as u64);
    cbor_bytes(&mut annotation, &raw_digest);
    cbor_text(&mut annotation, "application/octet-stream");
    cbor_null(&mut annotation);
    cbor_bytes(&mut annotation, &descriptor_oid.0);
    graph.row_media_descriptor = Some(descriptor_oid);
    graph.row_media_annotation = Some(annotation);
}

fn encode_leaf(domain: &[u8], rows: &[(u64, u64)], media_annotation: Option<&[u8]>) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + domain.len() + rows.len() * 14);
    cbor_array(&mut data, 4);
    cbor_uint(&mut data, 1);
    cbor_uint(&mut data, 1);
    data.extend_from_slice(domain);
    cbor_array(&mut data, rows.len());
    for (key, value) in rows {
        cbor_array(&mut data, 2);
        data.extend_from_slice(&encode_uint(*key));
        if *key == 0 {
            if let Some(annotation) = media_annotation {
                cbor_array(&mut data, 2);
                cbor_uint(&mut data, *value);
                data.extend_from_slice(annotation);
                continue;
            }
        }
        data.extend_from_slice(&encode_bucket(*value));
    }
    data
}

fn boundary_anchor(height: u8, token: &[u8]) -> bool {
    let mut hasher = <Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut hasher, ORP_DOMAIN);
    sha2::Digest::update(&mut hasher, u32::from(height).to_be_bytes());
    sha2::Digest::update(&mut hasher, token);
    let digest = sha2::Digest::finalize(hasher);
    digest[31] & 0x3f == 0
}

#[derive(Clone)]
struct BranchCandidate {
    max_key: Vec<u8>,
    oid: Oid,
    row_count: u64,
}

fn make_leaf(graph: &mut GraphFixture, domain: &[u8], rows: &[(u64, u64)]) -> BranchCandidate {
    assert!(!rows.is_empty());
    let max_key = encode_uint(rows.last().expect("leaf tail").0);
    let media_annotation = if rows.iter().any(|(key, _)| *key == 0) {
        graph.row_media_annotation.as_deref()
    } else {
        None
    };
    let edges = if media_annotation.is_some() {
        vec![Edge {
            oid: graph.row_media_descriptor.expect("Blob descriptor exists"),
            kind: ObjectKind::Tree,
        }]
    } else {
        Vec::new()
    };
    let oid = graph.add_node(encode_leaf(domain, rows, media_annotation), edges, true);
    BranchCandidate {
        max_key,
        oid,
        row_count: rows.len() as u64,
    }
}

fn leaf_pages(
    graph: &mut GraphFixture,
    domain: &[u8],
    row_count: u64,
) -> (Vec<BranchCandidate>, usize) {
    let mut pages = Vec::new();
    let mut page = Vec::with_capacity(MAX_PAGE_ENTRIES);
    let mut encoded_size = PAGE_NODE_OVERHEAD;
    for key in 0..row_count {
        let value = (key + FIXTURE_SEED) % BUCKET_COUNT;
        let key_bytes = encode_uint(key);
        let value_bytes = encode_bucket(value);
        let entry_size = key_bytes.len() + value_bytes.len() + 8;
        if !page.is_empty()
            && (page.len() == MAX_PAGE_ENTRIES || encoded_size + entry_size > NODE_DATA_LIMIT)
        {
            pages.push(make_leaf(graph, domain, &page));
            page.clear();
            encoded_size = PAGE_NODE_OVERHEAD;
        }
        encoded_size += entry_size;
        page.push((key, value));
        if page.len() >= MIN_PAGE_ENTRIES
            && (page.len() == MAX_PAGE_ENTRIES || boundary_anchor(0, &key_bytes))
        {
            pages.push(make_leaf(graph, domain, &page));
            page.clear();
            encoded_size = PAGE_NODE_OVERHEAD;
        }
    }
    if !page.is_empty() {
        pages.push(make_leaf(graph, domain, &page));
    }
    let count = pages.len();
    (pages, count)
}

fn cbor_unsigned_len(value: u64) -> usize {
    match value {
        0..=23 => 1,
        24..=255 => 2,
        256..=65_535 => 3,
        65_536..=4_294_967_295 => 5,
        _ => 9,
    }
}

fn branch_entry_size(candidate: &BranchCandidate) -> usize {
    1 + 1 + candidate.max_key.len() + 1 + 20 + cbor_unsigned_len(candidate.row_count)
}

fn branch_groups(
    candidates: &[BranchCandidate],
    domain: &[u8],
    height: u8,
) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    let mut encoded_size = 16 + domain.len();
    let mut refs = 0usize;
    for (index, candidate) in candidates.iter().enumerate() {
        let entry_size = branch_entry_size(candidate);
        assert!(entry_size + 16 + domain.len() <= NODE_DATA_LIMIT);
        let held = index - start;
        if held != 0
            && (held == MAX_PAGE_ENTRIES
                || refs + 1 > MAX_REFS
                || encoded_size + entry_size > NODE_DATA_LIMIT)
        {
            groups.push(start..index);
            start = index;
            encoded_size = 16 + domain.len();
            refs = 0;
        }
        encoded_size += entry_size;
        refs += 1;
        let held = index - start + 1;
        if held >= MIN_PAGE_ENTRIES
            && (held == MAX_PAGE_ENTRIES || boundary_anchor(height, &candidate.max_key))
        {
            groups.push(start..index + 1);
            start = index + 1;
            encoded_size = 16 + domain.len();
            refs = 0;
        }
    }
    if start < candidates.len() {
        groups.push(start..candidates.len());
    }
    groups
}

fn encode_branch(domain: &[u8], height: u8, candidates: &[BranchCandidate]) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + domain.len() + candidates.len() * 40);
    cbor_array(&mut data, 5);
    cbor_uint(&mut data, 1);
    cbor_uint(&mut data, 2);
    data.extend_from_slice(domain);
    cbor_uint(&mut data, u64::from(height));
    cbor_array(&mut data, candidates.len());
    for candidate in candidates {
        cbor_array(&mut data, 3);
        data.extend_from_slice(&candidate.max_key);
        cbor_bytes(&mut data, &candidate.oid.0);
        cbor_uint(&mut data, candidate.row_count);
    }
    data
}

fn build_index(
    graph: &mut GraphFixture,
    relation_id: [u8; 16],
    schema_digest: [u8; 32],
    row_count: u64,
    fault: Fault,
) -> (Oid, usize, usize) {
    let mut domain_digest = schema_digest;
    if fault == Fault::WrongSchemaDomain {
        domain_digest[0] ^= 0x80;
    }
    let domain = encode_rows_domain(relation_id, domain_digest);

    if fault == Fault::OverlappingRanges {
        let left = make_leaf(
            graph,
            &domain,
            &(0..5).map(|key| (key, key)).collect::<Vec<_>>(),
        );
        let right = make_leaf(
            graph,
            &domain,
            &(5..10).map(|key| (key, key)).collect::<Vec<_>>(),
        );
        let branch = vec![
            BranchCandidate {
                max_key: encode_uint(4),
                ..left
            },
            BranchCandidate {
                max_key: encode_uint(4),
                ..right
            },
        ];
        let root = graph.add_node(
            encode_branch(&domain, 1, &branch),
            tree_edges(&branch),
            true,
        );
        return (root, 2, 1);
    }

    if fault == Fault::GiantFanout {
        let leaves: Vec<_> = (0..257)
            .map(|key| make_leaf(graph, &domain, &[(key, key)]))
            .collect();
        let root = graph.add_node(
            encode_branch(&domain, 1, &leaves),
            tree_edges(&leaves),
            true,
        );
        return (root, leaves.len(), 1);
    }

    let (mut level, leaf_count) = leaf_pages(graph, &domain, row_count);
    let mut height = 1u8;
    while level.len() > 1 {
        let groups = branch_groups(&level, &domain, height);
        assert!(groups.len() < level.len(), "branch level reduces fanout");
        let mut next = Vec::with_capacity(groups.len());
        for group in groups {
            let children = &level[group.clone()];
            let row_count = children.iter().map(|child| child.row_count).sum();
            let candidate = BranchCandidate {
                max_key: children
                    .last()
                    .expect("nonempty branch group")
                    .max_key
                    .clone(),
                oid: graph.add_node(
                    encode_branch(&domain, height, children),
                    tree_edges(children),
                    true,
                ),
                row_count,
            };
            next.push(candidate);
        }
        level = next;
        height += 1;
    }
    let root = level.pop().expect("nonempty row index").oid;
    let branch_count = graph.index_tree_oids.len() - leaf_count;
    (root, leaf_count, branch_count)
}

fn tree_edges(candidates: &[BranchCandidate]) -> Vec<Edge> {
    candidates
        .iter()
        .map(|candidate| Edge {
            oid: candidate.oid,
            kind: ObjectKind::Tree,
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fault {
    None,
    OverlappingRanges,
    BrokenCounts,
    GiantFanout,
    WrongSchemaDomain,
}

fn schema_descriptor(relation_id: [u8; 16]) -> SchemaDescriptor {
    let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
    let field = |id: [u8; 16], name: &str, ty: Raw, role: u8| {
        Raw::Array(vec![
            uuid(id),
            Raw::Text(name.to_owned()),
            ty,
            Raw::Int(role.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ])
    };
    SchemaDescriptor::new(Raw::Map(vec![
        (Raw::Int(0.into()), Raw::Int(1.into())),
        (Raw::Int(1.into()), uuid(relation_id)),
        (Raw::Int(2.into()), Raw::Array(vec![uuid([0x11; 16])])),
        (
            Raw::Int(3.into()),
            Raw::Array(vec![
                field(
                    [0x11; 16],
                    "id",
                    Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]),
                    0,
                ),
                field(
                    [0x22; 16],
                    "bucket",
                    Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]),
                    1,
                ),
            ]),
        ),
        (Raw::Int(4.into()), Raw::Array(Vec::new())),
    ]))
    .expect("valid deterministic format-3 table schema")
}

fn encode_store_root(relation_map: Oid) -> Vec<u8> {
    let mut data = Vec::with_capacity(25);
    cbor_array(&mut data, 3);
    cbor_uint(&mut data, 1);
    cbor_uint(&mut data, 0);
    cbor_bytes(&mut data, &relation_map.0);
    data
}

fn encode_relation_map(
    database_id: [u8; 16],
    relation_id: [u8; 16],
    schema_oid: Oid,
    row_root: Oid,
    row_count: u64,
) -> Vec<u8> {
    let domain = encode_relations_domain(database_id);
    let key = TypedKey::Bytes(relation_id.to_vec())
        .canonical_bytes()
        .expect("canonical relation UUID key");
    let mut value = Vec::with_capacity(64);
    cbor_array(&mut value, 4);
    cbor_bytes(&mut value, &schema_oid.0);
    cbor_bytes(&mut value, &row_root.0);
    cbor_null(&mut value);
    cbor_uint(&mut value, row_count);

    let mut data = Vec::with_capacity(domain.len() + key.len() + value.len() + 12);
    cbor_array(&mut data, 4);
    cbor_uint(&mut data, 1);
    cbor_uint(&mut data, 1);
    data.extend_from_slice(&domain);
    cbor_array(&mut data, 1);
    cbor_array(&mut data, 2);
    data.extend_from_slice(&key);
    data.extend_from_slice(&value);
    data
}

fn setup_git_repository(root: &Path) {
    fs::create_dir_all(root.join(".orna/store")).expect("create initial store path");
    fs::write(root.join("main.orna"), MAIN_SOURCE).expect("write source fixture");
    fs::write(root.join(".orna/database.orna"), DATABASE).expect("write database fixture");
    fs::write(root.join(".orna/store/data"), b"initial store marker")
        .expect("write initial store marker");
    git(
        root,
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(root, &["config", "user.name", "Orna scale fixture"]);
    git(
        root,
        &["config", "user.email", "orna-scale@example.invalid"],
    );
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "seed ORP scale fixture"]);
}

struct Fixture {
    root: TempDir,
    relation_id: [u8; 16],
    row_count: u64,
    index_data_blobs: BTreeMap<Oid, usize>,
    index_tree_oids: BTreeSet<Oid>,
    bootstrap_blob_oids: BTreeSet<Oid>,
    media_annotation: Option<Vec<u8>>,
    media_payload_oids: BTreeSet<Oid>,
    leaf_pages: usize,
    branch_pages: usize,
    root_data_oid: Oid,
}

fn build_fixture(row_count: u64, fault: Fault) -> Fixture {
    let root = TempDir::new().expect("create deterministic scale repository");
    setup_git_repository(root.path());

    let repository = Repository::discover(root.path()).expect("discover base repository");
    let context = repository
        .open_format_context()
        .expect("admit initial format-3 repository");
    let database_id = *context
        .require_database_id()
        .expect("database UUID")
        .as_bytes();
    let relation_id = [0x6d; 16];
    let schema_bytes = schema_descriptor(relation_id)
        .encode()
        .expect("encode canonical table schema");
    let chunk_digest: [u8; 32] = <Sha256 as sha2::Digest>::digest(&schema_bytes).into();
    // The kind-6 node and every rows domain carry the format-3 schema
    // identity: SHA-256 of `orna.schema.v3`, NUL and the descriptor bytes.
    let schema_digest: [u8; 32] = <Sha256 as sha2::Digest>::new()
        .chain_update(b"orna.schema.v3\0")
        .chain_update(&schema_bytes)
        .finalize()
        .into();

    let mut graph = GraphFixture::default();
    if fault == Fault::None {
        add_synthetic_media(&mut graph);
    }
    let schema_chunk = graph.add_blob(schema_bytes.clone());
    let mut byte_index_data = Vec::with_capacity(schema_bytes.len() + 40);
    cbor_array(&mut byte_index_data, 5);
    cbor_uint(&mut byte_index_data, 1);
    cbor_uint(&mut byte_index_data, 4);
    cbor_uint(&mut byte_index_data, 0);
    cbor_uint(&mut byte_index_data, schema_bytes.len() as u64);
    cbor_array(&mut byte_index_data, 1);
    cbor_array(&mut byte_index_data, 3);
    cbor_uint(&mut byte_index_data, schema_bytes.len() as u64);
    cbor_bytes(&mut byte_index_data, &schema_chunk.0);
    cbor_bytes(&mut byte_index_data, &chunk_digest);
    let byte_index = graph.add_node(
        byte_index_data,
        vec![Edge {
            oid: schema_chunk,
            kind: ObjectKind::Blob,
        }],
        false,
    );

    let mut schema_node_data = Vec::with_capacity(64);
    cbor_array(&mut schema_node_data, 5);
    cbor_uint(&mut schema_node_data, 1);
    cbor_uint(&mut schema_node_data, 6);
    cbor_uint(&mut schema_node_data, schema_bytes.len() as u64);
    cbor_bytes(&mut schema_node_data, &schema_digest);
    cbor_bytes(&mut schema_node_data, &byte_index.0);
    let schema_node = graph.add_node(
        schema_node_data,
        vec![Edge {
            oid: byte_index,
            kind: ObjectKind::Tree,
        }],
        false,
    );

    let (row_root, leaf_pages, branch_pages) = {
        let _phase = Phase::new("build_index", row_count);
        build_index(&mut graph, relation_id, schema_digest, row_count, fault)
    };
    let declared_count = if fault == Fault::BrokenCounts {
        row_count + 1
    } else {
        row_count
    };
    let relation_map_data = encode_relation_map(
        database_id,
        relation_id,
        schema_node,
        row_root,
        declared_count,
    );
    let relation_map = graph.add_node(
        relation_map_data,
        vec![
            Edge {
                oid: schema_node,
                kind: ObjectKind::Tree,
            },
            Edge {
                oid: row_root,
                kind: ObjectKind::Tree,
            },
        ],
        false,
    );
    let store_root = graph.add_node(
        encode_store_root(relation_map),
        vec![Edge {
            oid: relation_map,
            kind: ObjectKind::Tree,
        }],
        false,
    );
    let root_data_oid = graph.nodes[&store_root].data_oid;
    let bootstrap_blob_oids = graph
        .nodes
        .values()
        .filter_map(|node| {
            (!graph.index_data_blobs.contains_key(&node.data_oid)).then_some(node.data_oid)
        })
        .chain(
            graph
                .blobs
                .keys()
                .filter(|oid| !graph.media_payload_oids.contains(*oid))
                .copied(),
        )
        .chain([root_data_oid, graph.nodes[&row_root].data_oid])
        .collect::<BTreeSet<_>>();

    let store_path = root.path().join(".orna/store");
    fs::remove_dir_all(&store_path).expect("replace initial store marker");
    graph.materialize_node(store_root, &store_path);
    git(root.path(), &["add", "-A"]);
    git(
        root.path(),
        &["commit", "--quiet", "-m", "publish ORP scale fixture"],
    );
    let committed_store = git_text(root.path(), &["rev-parse", "HEAD:.orna/store"]);
    assert_eq!(
        committed_store,
        store_root.hex(),
        "Git tree hash matches fixture graph"
    );
    git(root.path(), &["config", "uploadpack.allowFilter", "true"]);
    git(
        root.path(),
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );

    Fixture {
        root,
        relation_id,
        row_count,
        index_data_blobs: graph.index_data_blobs,
        index_tree_oids: graph.index_tree_oids,
        bootstrap_blob_oids,
        media_annotation: graph.row_media_annotation.clone(),
        media_payload_oids: graph.media_payload_oids,
        leaf_pages,
        branch_pages,
        root_data_oid,
    }
}

fn clone_partial(source: &Path) -> (TempDir, PathBuf, Duration) {
    let holder = TempDir::new().expect("create partial clone parent");
    let destination = holder.path().join("clone");
    let source_url = format!("file://{}", source.canonicalize().unwrap().display());
    let started = Instant::now();
    git_outside(&[
        "clone",
        "--quiet",
        "--filter=blob:none",
        "--no-checkout",
        "--no-local",
        &source_url,
        destination.to_str().expect("UTF-8 temp clone path"),
    ]);
    (holder, destination, started.elapsed())
}

fn bootstrap_partial_clone(fixture: &Fixture, repository_path: &Path) {
    // Local-only admission needs metadata, schema source, store path, and root page.
    git(
        repository_path,
        &["cat-file", "blob", "HEAD:.orna/database.orna"],
    );
    git(repository_path, &["cat-file", "blob", "HEAD:main.orna"]);
    for oid in &fixture.bootstrap_blob_oids {
        let object = oid.hex();
        git(repository_path, &["cat-file", "blob", &object]);
    }
}

fn query_with_hydration<T>(
    repository_path: &Path,
    graph: &orna_repository_v1::NativeGraphContext,
    operation: &str,
    mut query: impl FnMut(
        &orna_repository_v1::RepositoryReadScope,
    ) -> Result<T, orna_repository_v1::GraphError>,
) -> T {
    loop {
        let scope = graph
            .open_read_scope()
            .unwrap_or_else(|error| panic!("{operation} scope: {error:?}"));
        match query(&scope) {
            Ok(result) => return result,
            Err(orna_repository_v1::GraphError::PromisedObjectNotMaterialized { object_id }) => {
                // Use Git's promisor fetch here; the object is already proven by this graph read.
                let started = Instant::now();
                git(repository_path, &["cat-file", "blob", &object_id]);
                HYDRATION_FETCHES.fetch_add(1, Ordering::Relaxed);
                HYDRATION_NANOS.fetch_add(
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            }
            Err(error) => panic!("{operation}: {error:?}"),
        }
    }
}

fn exercise_media_metadata(fixture: &Fixture) {
    assert!(fixture.row_count >= 256);
    let (_clone_holder, clone_path, clone_elapsed) = clone_partial(fixture.root.path());
    bootstrap_partial_clone(fixture, &clone_path);
    let before_cold = object_inventory(&clone_path);
    assert!(
        fixture
            .media_payload_oids
            .iter()
            .all(|oid| !before_cold.objects.contains_key(oid)),
        "raw media starts absent from metadata clone"
    );

    let repository = Repository::discover(&clone_path).expect("discover media metadata clone");
    let context = repository
        .open_format_context()
        .expect("admit media metadata clone");
    let snapshot = context
        .load_row_map(fixture.relation_id)
        .expect("load media row snapshot");
    let graph = context
        .open_native_graph(&snapshot)
        .expect("open media metadata graph");

    let before_cold_allocations = allocations();
    let cold_started = Instant::now();
    let (cold_metadata_found, cold_payload_bytes_read) =
        query_with_hydration(&clone_path, &graph, "cold Blob metadata row", |scope| {
            graph.lookup_row(&TypedKey::UInt(0), scope).map(|row| {
                (
                    row.expect("media row zero is present")
                        .blob_metadata(1)
                        .expect("decode media descriptor")
                        .is_some(),
                    scope.payload_bytes_read(),
                )
            })
        });
    let cold_elapsed = cold_started.elapsed();
    let cold_allocations = allocation_delta(before_cold_allocations, allocations());
    assert!(cold_metadata_found, "Blob field metadata must be visible");
    assert_eq!(
        cold_payload_bytes_read, 0,
        "metadata lookup reads no media bytes"
    );
    let after_cold = object_inventory(&clone_path);
    let cold_index_transfer =
        index_data_delta(&fixture.index_data_blobs, &before_cold, &after_cold);
    assert!(
        cold_index_transfer.0 > 0,
        "cold metadata lookup hydrates its row page"
    );
    let cold_media_transfer =
        media_payload_delta(&fixture.media_payload_oids, &before_cold, &after_cold);
    assert_eq!(
        cold_media_transfer,
        (0, 0),
        "cold metadata lookup transfers no media"
    );

    let before_warm_allocations = allocations();
    let warm_started = Instant::now();
    let (warm_metadata_found, warm_payload_bytes_read) =
        query_with_hydration(&clone_path, &graph, "warm Blob metadata row", |scope| {
            graph.lookup_row(&TypedKey::UInt(0), scope).map(|row| {
                (
                    row.expect("warm media row zero is present")
                        .blob_metadata(1)
                        .expect("decode warm media descriptor")
                        .is_some(),
                    scope.payload_bytes_read(),
                )
            })
        });
    let warm_elapsed = warm_started.elapsed();
    let warm_allocations = allocation_delta(before_warm_allocations, allocations());
    let after_warm = object_inventory(&clone_path);
    assert!(warm_metadata_found);
    assert_eq!(warm_payload_bytes_read, 0);
    assert_eq!(after_cold.objects, after_warm.objects);
    assert_eq!(
        media_payload_delta(&fixture.media_payload_oids, &after_cold, &after_warm),
        (0, 0)
    );

    eprintln!(
        "ORP-1 blob_metadata rows={} clone_ms={} cold_us={} cold_index_blobs={} cold_index_bytes={} cold_payload_bytes_read={} cold_media_blobs={} cold_media_bytes={} cold_allocs={} cold_alloc_bytes={} warm_us={} warm_payload_bytes_read={} warm_allocs={} warm_alloc_bytes={}",
        fixture.row_count,
        millis(clone_elapsed),
        micros(cold_elapsed),
        cold_index_transfer.0,
        cold_index_transfer.1,
        cold_payload_bytes_read,
        cold_media_transfer.0,
        cold_media_transfer.1,
        cold_allocations.0,
        cold_allocations.1,
        micros(warm_elapsed),
        warm_payload_bytes_read,
        warm_allocations.0,
        warm_allocations.1,
    );
}

fn single_concurrent_writer(
    repository_path: PathBuf,
    relation_id: [u8; 16],
    key: u64,
    expected_count: u64,
) -> (Duration, usize, usize, usize, u64) {
    let repository = Repository::discover(&repository_path).expect("discover write clone");
    let context = repository
        .open_format_context()
        .expect("admit concurrent-write clone");
    let snapshot = context
        .load_row_map(relation_id)
        .expect("load concurrent-write row map");
    assert_eq!(snapshot.version().row_count(), Some(expected_count));
    let graph = context
        .open_native_graph(&snapshot)
        .expect("open concurrent-write graph");
    let key_bytes = encode_uint(key);
    let mutations = [RowIndexMutation::insert_canonical(&key_bytes, &[0x80])
        .expect("construct canonical concurrent insert")];
    let started = Instant::now();
    let (maintenance, payload_bytes_read) = query_with_hydration(
        &repository_path,
        &graph,
        "concurrent index writer",
        |scope| {
            graph
                .maintain_row_index(&mutations, scope)
                .map(|maintenance| (maintenance, scope.payload_bytes_read()))
        },
    );
    assert_eq!(maintenance.row_count(), expected_count + 1);
    (
        started.elapsed(),
        maintenance.index_objects_created().len(),
        maintenance.index_objects_reused().len(),
        maintenance.payload_objects_reused().len(),
        payload_bytes_read,
    )
}

fn concurrent_write_pair(
    repository_path: &Path,
    relation_id: [u8; 16],
    row_count: u64,
) -> (Duration, Duration, usize, usize, usize, u64, (u64, u64)) {
    let before_allocations = allocations();
    let started = Instant::now();
    let left_path = repository_path.to_owned();
    let right_path = repository_path.to_owned();
    let left = std::thread::spawn(move || {
        single_concurrent_writer(left_path, relation_id, row_count + 1, row_count)
    });
    let right = std::thread::spawn(move || {
        single_concurrent_writer(right_path, relation_id, row_count + 2, row_count)
    });
    let left = left.join().expect("left concurrent writer did not panic");
    let right = right.join().expect("right concurrent writer did not panic");
    let pair_elapsed = started.elapsed();
    let pair_allocations = allocation_delta(before_allocations, allocations());
    (
        pair_elapsed,
        left.0.max(right.0),
        left.1 + right.1,
        left.2 + right.2,
        left.3 + right.3,
        left.4 + right.4,
        pair_allocations,
    )
}

fn exercise_concurrent_writes(fixture: &Fixture) {
    assert!(
        fixture.leaf_pages > 1,
        "cold-write fixture needs multiple pages"
    );
    let (_clone_holder, clone_path, _) = clone_partial(fixture.root.path());
    bootstrap_partial_clone(fixture, &clone_path);

    let before_cold = object_inventory(&clone_path);
    let cold = concurrent_write_pair(&clone_path, fixture.relation_id, fixture.row_count);
    let after_cold = object_inventory(&clone_path);
    let cold_page_delta = index_data_delta(&fixture.index_data_blobs, &before_cold, &after_cold);
    assert!(
        cold_page_delta.0 > 0,
        "cold writes must fetch missing row pages"
    );
    // Maintenance is incremental (store-format-3 §3): an append reads the
    // path to the last page, never the table.
    assert!(
        cold_page_delta.0 * 100 < fixture.index_data_blobs.len(),
        "cold writes fetched {} of {} source row pages",
        cold_page_delta.0,
        fixture.index_data_blobs.len()
    );

    let cold_media_delta =
        media_payload_delta(&fixture.media_payload_oids, &before_cold, &after_cold);
    assert_eq!(
        cold_media_delta,
        (0, 0),
        "concurrent index writes transfer no media"
    );
    let before_warm = object_inventory(&clone_path);
    let warm = concurrent_write_pair(&clone_path, fixture.relation_id, fixture.row_count);
    let after_warm = object_inventory(&clone_path);
    let warm_page_delta = index_data_delta(&fixture.index_data_blobs, &before_warm, &after_warm);
    assert_eq!(
        warm_page_delta,
        (0, 0),
        "warm writes fetch no source row pages"
    );

    let warm_media_delta =
        media_payload_delta(&fixture.media_payload_oids, &before_warm, &after_warm);
    assert_eq!(
        warm_media_delta,
        (0, 0),
        "warm concurrent writes transfer no media"
    );
    eprintln!(
        "ORP-1 concurrent_writes rows={} cold_pair_ms={} cold_writer_max_ms={} cold_index_created={} cold_index_reused={} cold_payload_reused={} cold_payload_bytes_read={} cold_media_blobs={} cold_media_bytes={} cold_page_blobs={} cold_page_bytes={} cold_allocs={} cold_alloc_bytes={} warm_pair_ms={} warm_writer_max_ms={} warm_index_created={} warm_index_reused={} warm_payload_reused={} warm_payload_bytes_read={} warm_media_blobs={} warm_media_bytes={} warm_page_blobs={} warm_page_bytes={} warm_allocs={} warm_alloc_bytes={}",
        fixture.row_count,
        millis(cold.0),
        millis(cold.1),
        cold.2,
        cold.3,
        cold.4,
        cold.5,
        cold_media_delta.0,
        cold_media_delta.1,
        cold_page_delta.0,
        cold_page_delta.1,
        cold.6.0,
        cold.6.1,
        millis(warm.0),
        millis(warm.1),
        warm.2,
        warm.3,
        warm.4,
        warm.5,
        warm_media_delta.0,
        warm_media_delta.1,
        warm_page_delta.0,
        warm_page_delta.1,
        warm.6.0,
        warm.6.1,
    );
}

struct IndexChurnMetrics {
    elapsed: Duration,
    index_created: usize,
    index_reused: usize,
    payload_reused: usize,
    payload_bytes_read: u64,
    allocations: (u64, u64),
    source_page_transfer: (usize, usize),
    new_tree_objects: usize,
    media_transfer: (usize, usize),
}

/// Runs one maintenance pass. `media_page` says whether the pass rewrites the
/// page holding the Blob row, or adds a row naming its descriptor: only then
/// do the rewritten pages name the descriptor again.
fn measure_index_churn(
    fixture: &Fixture,
    repository_path: &Path,
    operation: &str,
    mutation: RowIndexMutation,
    expected_rows: u64,
    media_page: bool,
) -> IndexChurnMetrics {
    let repository = Repository::discover(repository_path).expect("discover churn clone");
    let context = repository.open_format_context().expect("admit churn clone");
    let snapshot = context
        .load_row_map(fixture.relation_id)
        .expect("load churn snapshot");
    assert_eq!(snapshot.version().row_count(), Some(fixture.row_count));
    let graph = context
        .open_native_graph(&snapshot)
        .expect("open churn graph");
    let before_objects = object_inventory(repository_path);
    let before_allocations = allocations();
    let started = Instant::now();
    let (maintenance, payload_bytes_read) =
        query_with_hydration(repository_path, &graph, operation, |scope| {
            graph
                .maintain_row_index(std::slice::from_ref(&mutation), scope)
                .map(|maintenance| (maintenance, scope.payload_bytes_read()))
        });
    let elapsed = started.elapsed();
    let allocations = allocation_delta(before_allocations, allocations());
    assert_eq!(maintenance.row_count(), expected_rows);
    assert_eq!(
        payload_bytes_read, 0,
        "index maintenance must not read media bytes"
    );
    assert!(
        !maintenance.index_objects_created().is_empty(),
        "{operation} must create index objects"
    );
    // Untouched pages keep their object IDs unread (store-format-3 §3); a
    // rewritten page names the stored Blob descriptor without rewriting it.
    assert_eq!(
        !maintenance.payload_objects_reused().is_empty(),
        media_page,
        "{operation}: the rewritten pages name the existing Blob descriptor"
    );
    if fixture.leaf_pages > 1 {
        assert!(
            !maintenance.index_objects_reused().is_empty(),
            "{operation} keeps the untouched pages by object ID"
        );
    }
    let after_objects = object_inventory(repository_path);
    let source_page_transfer =
        index_data_delta(&fixture.index_data_blobs, &before_objects, &after_objects);
    let new_tree_objects = tree_object_delta(&before_objects, &after_objects);
    let media_transfer =
        media_payload_delta(&fixture.media_payload_oids, &before_objects, &after_objects);
    assert_eq!(
        media_transfer,
        (0, 0),
        "{operation} must not fetch media bytes"
    );

    IndexChurnMetrics {
        elapsed,
        index_created: maintenance.index_objects_created().len(),
        index_reused: maintenance.index_objects_reused().len(),
        payload_reused: maintenance.payload_objects_reused().len(),
        payload_bytes_read,
        allocations,
        source_page_transfer,
        new_tree_objects,
        media_transfer,
    }
}

fn report_index_churn(rows: u64, operation: &str, metrics: IndexChurnMetrics) {
    eprintln!(
        "ORP-1 index_churn rows={} operation={} elapsed_ms={} index_created={} index_reused={} payload_objects_reused={} payload_bytes_read={} source_page_blobs={} source_page_bytes={} new_tree_objects={} media_blobs={} media_bytes={} allocs={} alloc_bytes={}",
        rows,
        operation,
        millis(metrics.elapsed),
        metrics.index_created,
        metrics.index_reused,
        metrics.payload_reused,
        metrics.payload_bytes_read,
        metrics.source_page_transfer.0,
        metrics.source_page_transfer.1,
        metrics.new_tree_objects,
        metrics.media_transfer.0,
        metrics.media_transfer.1,
        metrics.allocations.0,
        metrics.allocations.1,
    );
}

fn exercise_index_churn(fixture: &Fixture) {
    let (_clone_holder, clone_path, _) = clone_partial(fixture.root.path());
    bootstrap_partial_clone(fixture, &clone_path);

    let prepend = RowIndexMutation::insert_canonical(&[0x20], &[0x81, 0x01])
        .expect("construct negative prepend row");
    report_index_churn(
        fixture.row_count,
        "prepend",
        measure_index_churn(
            fixture,
            &clone_path,
            "prepend row",
            prepend,
            fixture.row_count + 1,
            true,
        ),
    );

    report_index_churn(
        fixture.row_count,
        "delete",
        measure_index_churn(
            fixture,
            &clone_path,
            "delete row",
            RowIndexMutation::Delete {
                key: TypedKey::UInt(fixture.row_count / 2),
            },
            fixture.row_count - 1,
            fixture.leaf_pages == 1,
        ),
    );

    report_index_churn(
        fixture.row_count,
        "rekey",
        measure_index_churn(
            fixture,
            &clone_path,
            "rekey row",
            RowIndexMutation::Rekey {
                from: TypedKey::UInt(fixture.row_count / 2),
                to: TypedKey::UInt(fixture.row_count + 20),
            },
            fixture.row_count,
            fixture.leaf_pages == 1,
        ),
    );

    let mut large_row = Vec::with_capacity(8_192);
    cbor_array(&mut large_row, 3);
    cbor_uint(&mut large_row, bucket_for(fixture.row_count + 30));
    let large_text = "x".repeat(7_700);
    cbor_text(&mut large_row, &large_text);
    large_row.extend_from_slice(
        fixture
            .media_annotation
            .as_deref()
            .expect("valid fixture has a media annotation"),
    );
    assert!(large_row.len() > 7_500 && large_row.len() <= 8_192);
    let large_key = encode_uint(fixture.row_count + 30);
    let large_row = RowIndexMutation::insert_canonical(&large_key, &large_row)
        .expect("construct large inline row with shared media reference");
    report_index_churn(
        fixture.row_count,
        "large_row",
        measure_index_churn(
            fixture,
            &clone_path,
            "insert large row",
            large_row,
            fixture.row_count + 1,
            true,
        ),
    );
    let after_churn = object_inventory(&clone_path);
    assert!(
        fixture
            .media_payload_oids
            .iter()
            .all(|oid| !after_churn.objects.contains_key(oid)),
        "index churn never fetches raw media"
    );
}

fn run_case(fixture: &Fixture) {
    let (_clone_holder, clone_path, clone_elapsed) = clone_partial(fixture.root.path());
    assert_eq!(
        git_text(&clone_path, &["config", "--get", "remote.origin.promisor"]),
        "true",
        "clone must retain promisor remote authority"
    );
    assert_eq!(
        git_text(
            &clone_path,
            &["config", "--get", "remote.origin.partialclonefilter"]
        ),
        "blob:none",
        "clone must be a blob-filtered partial clone"
    );

    let source_inventory = object_inventory(fixture.root.path());
    let clone_inventory = object_inventory(&clone_path);
    assert!(
        clone_inventory.blob_count() < source_inventory.blob_count(),
        "partial clone must omit row-node data blobs"
    );
    assert!(fixture.index_tree_oids.iter().all(|oid| {
        clone_inventory
            .objects
            .get(oid)
            .is_some_and(|object| object.kind == ObjectKind::Tree)
    }));
    assert!(
        fixture
            .index_data_blobs
            .keys()
            .all(|oid| !clone_inventory.objects.contains_key(oid)),
        "blob:none clone starts without ORP row-page data"
    );
    assert!(
        fixture
            .media_payload_oids
            .iter()
            .all(|oid| !clone_inventory.objects.contains_key(oid)),
        "blob:none clone starts without raw media payload"
    );
    bootstrap_partial_clone(fixture, &clone_path);

    let repository = Repository::discover(&clone_path).expect("discover partial repository");
    let context = repository
        .open_format_context()
        .expect("admit partial format-3 snapshot");
    let load_started = Instant::now();
    let snapshot = context
        .load_row_map(fixture.relation_id)
        .expect("load lazy ORP row-map identity");
    assert_eq!(snapshot.version().row_count(), Some(fixture.row_count));
    let graph = context
        .open_native_graph(&snapshot)
        .expect("open graph for exact row snapshot");
    let load_elapsed = load_started.elapsed();
    let after_load = object_inventory(&clone_path);
    assert!(
        fixture
            .bootstrap_blob_oids
            .iter()
            .all(|oid| after_load.objects.contains_key(oid))
    );
    assert!(after_load.objects.contains_key(&fixture.root_data_oid));
    assert!(
        fixture
            .media_payload_oids
            .iter()
            .all(|oid| !after_load.objects.contains_key(oid)),
        "row-map admission must not fetch raw media payload"
    );
    assert!(
        fixture
            .index_data_blobs
            .keys()
            .filter(|oid| !fixture.bootstrap_blob_oids.contains(oid))
            .all(|oid| !after_load.objects.contains_key(oid)),
        "row-map admission must not fetch non-root index pages"
    );

    let point_key = fixture.row_count * 3 / 4;
    let before_point = allocations();
    let point_started = Instant::now();
    let point = query_with_hydration(&clone_path, &graph, "cold point query", |scope| {
        graph
            .lookup_row(&TypedKey::UInt(point_key), scope)
            .map(|row| row.expect("existing point key"))
    });
    let point_elapsed = point_started.elapsed();
    let point_allocations = allocation_delta(before_point, allocations());
    assert_eq!(point.key(), &TypedKey::UInt(point_key));
    assert_eq!(
        decode_single_uint_array(point.value().encoded_fields().expect("inline bucket field")),
        bucket_for(point_key)
    );
    let after_point = object_inventory(&clone_path);
    let point_metadata = index_data_delta(&fixture.index_data_blobs, &after_load, &after_point);
    let point_tree_delta = tree_object_delta(&after_load, &after_point);

    let before_warm_point = allocations();
    let warm_point_started = Instant::now();
    let warm_point = query_with_hydration(&clone_path, &graph, "warm point query", |scope| {
        graph
            .lookup_row(&TypedKey::UInt(point_key), scope)
            .map(|row| row.expect("warm point key"))
    });
    let warm_point_elapsed = warm_point_started.elapsed();
    let warm_point_allocations = allocation_delta(before_warm_point, allocations());
    assert_eq!(warm_point.key(), &TypedKey::UInt(point_key));
    let after_warm_point = object_inventory(&clone_path);
    assert_eq!(
        after_point.objects, after_warm_point.objects,
        "warm point query transfers no objects"
    );

    let range_start = fixture.row_count / 3;
    let range_count = 20.min(fixture.row_count - range_start) as usize;
    let range_end = range_start + range_count as u64;
    let range = KeyRange::new(
        Some(TypedKey::UInt(range_start)),
        Some(TypedKey::UInt(range_end)),
        range_count,
    )
    .expect("valid indexed range");
    let before_range = allocations();
    let range_started = Instant::now();
    let range_rows = query_with_hydration(&clone_path, &graph, "cold range query", |scope| {
        graph.range_rows(&range, scope)
    });
    let range_elapsed = range_started.elapsed();
    let range_allocations = allocation_delta(before_range, allocations());
    assert_eq!(range_rows.len(), range_count);
    for (offset, row) in range_rows.iter().enumerate() {
        let key = range_start + offset as u64;
        assert_eq!(row.key(), &TypedKey::UInt(key));
        assert_eq!(
            decode_single_uint_array(row.value().encoded_fields().expect("inline bucket field")),
            bucket_for(key)
        );
    }
    let after_range = object_inventory(&clone_path);
    let range_metadata =
        index_data_delta(&fixture.index_data_blobs, &after_warm_point, &after_range);
    let range_tree_delta = tree_object_delta(&after_warm_point, &after_range);

    let before_warm_range = allocations();
    let warm_range_started = Instant::now();
    let warm_range_rows = query_with_hydration(&clone_path, &graph, "warm range query", |scope| {
        graph.range_rows(&range, scope)
    });
    assert_eq!(warm_range_rows.len(), range_count);
    let warm_range_elapsed = warm_range_started.elapsed();
    let warm_range_allocations = allocation_delta(before_warm_range, allocations());
    let after_warm_range = object_inventory(&clone_path);
    assert_eq!(
        after_range.objects, after_warm_range.objects,
        "warm range query transfers no objects"
    );

    let missing_key = fixture.row_count + 7;
    let before_missing = allocations();
    let missing_started = Instant::now();
    assert!(
        query_with_hydration(&clone_path, &graph, "cold missing-key query", |scope| graph
            .lookup_row(&TypedKey::UInt(missing_key), scope),)
        .is_none()
    );
    let missing_elapsed = missing_started.elapsed();
    let missing_allocations = allocation_delta(before_missing, allocations());
    let after_missing = object_inventory(&clone_path);
    let missing_metadata =
        index_data_delta(&fixture.index_data_blobs, &after_range, &after_missing);
    let missing_tree_delta = tree_object_delta(&after_range, &after_missing);

    let before_warm_missing = allocations();
    let warm_missing_started = Instant::now();
    assert!(
        query_with_hydration(&clone_path, &graph, "warm missing-key query", |scope| graph
            .lookup_row(&TypedKey::UInt(missing_key), scope),)
        .is_none()
    );
    let warm_missing_elapsed = warm_missing_started.elapsed();
    let warm_missing_allocations = allocation_delta(before_warm_missing, allocations());
    let after_warm_missing = object_inventory(&clone_path);
    assert_eq!(
        after_missing.objects, after_warm_missing.objects,
        "warm missing-key query transfers no objects"
    );

    let before_scan = allocations();
    let scan_started = Instant::now();
    let (scanned, matches) = {
        let _phase = Phase::new("cold_unindexed_scan", fixture.row_count);
        scan_unindexed_bucket(&clone_path, &graph, fixture.row_count)
    };
    let scan_elapsed = scan_started.elapsed();
    let scan_allocations = allocation_delta(before_scan, allocations());
    assert_eq!(scanned, fixture.row_count);
    assert_eq!(matches, expected_bucket_count(fixture.row_count));
    let after_scan = object_inventory(&clone_path);
    assert!(
        fixture
            .index_data_blobs
            .keys()
            .all(|oid| after_scan.objects.contains_key(oid))
    );
    let scan_metadata =
        index_data_delta(&fixture.index_data_blobs, &after_warm_missing, &after_scan);
    let scan_tree_delta = tree_object_delta(&after_warm_missing, &after_scan);

    let before_warm_scan = allocations();
    let warm_scan_started = Instant::now();
    assert_eq!(
        scan_unindexed_bucket(&clone_path, &graph, fixture.row_count),
        (fixture.row_count, expected_bucket_count(fixture.row_count))
    );
    let warm_scan_elapsed = warm_scan_started.elapsed();
    let warm_scan_allocations = allocation_delta(before_warm_scan, allocations());
    let after_warm_scan = object_inventory(&clone_path);
    assert_eq!(
        after_scan.objects, after_warm_scan.objects,
        "warm unindexed scan transfers no objects"
    );
    let payload_traffic =
        media_payload_delta(&fixture.media_payload_oids, &clone_inventory, &after_scan);
    assert_eq!(
        payload_traffic,
        (0, 0),
        "index-only queries must not transfer media bytes"
    );
    let payload_blobs = payload_traffic.0;
    let payload_bytes = payload_traffic.1;
    let tree_bytes = clone_inventory
        .objects
        .values()
        .filter(|object| object.kind == ObjectKind::Tree)
        .map(|object| object.size)
        .sum::<usize>();

    eprintln!(
        "ORP-1 rows={} leaves={} branches={} index_churn={} clone_trees={} clone_tree_bytes={} load_ms={} cold_point_us={} cold_point_index_blobs={} cold_point_index_bytes={} cold_point_tree_delta={} warm_point_us={} warm_point_allocs={} range_us={} range_index_blobs={} range_index_bytes={} range_tree_delta={} warm_range_us={} warm_range_allocs={} missing_us={} missing_index_blobs={} missing_index_bytes={} missing_tree_delta={} missing_allocs={} warm_missing_us={} warm_missing_allocs={} unindexed_scan_ms={} scanned={} matches={} scan_index_blobs={} scan_index_bytes={} scan_tree_delta={} warm_scan_ms={} warm_scan_allocs={} payload_blobs={} payload_bytes={} allocs_point={} alloc_bytes_point={} allocs_range={} alloc_bytes_range={} allocs_scan={} alloc_bytes_scan={} clone_ms={}",
        fixture.row_count,
        fixture.leaf_pages,
        fixture.branch_pages,
        fixture.index_tree_oids.len(),
        clone_inventory.tree_count(),
        tree_bytes,
        millis(load_elapsed),
        micros(point_elapsed),
        point_metadata.0,
        point_metadata.1,
        point_tree_delta,
        micros(warm_point_elapsed),
        warm_point_allocations.0,
        micros(range_elapsed),
        range_metadata.0,
        range_metadata.1,
        range_tree_delta,
        micros(warm_range_elapsed),
        warm_range_allocations.0,
        micros(missing_elapsed),
        missing_metadata.0,
        missing_metadata.1,
        missing_tree_delta,
        missing_allocations.0,
        micros(warm_missing_elapsed),
        warm_missing_allocations.0,
        millis(scan_elapsed),
        scanned,
        matches,
        scan_metadata.0,
        scan_metadata.1,
        scan_tree_delta,
        millis(warm_scan_elapsed),
        warm_scan_allocations.0,
        payload_blobs,
        payload_bytes,
        point_allocations.0,
        point_allocations.1,
        range_allocations.0,
        range_allocations.1,
        scan_allocations.0,
        scan_allocations.1,
        millis(clone_elapsed),
    );
}

fn scan_unindexed_bucket(
    repository_path: &Path,
    graph: &orna_repository_v1::NativeGraphContext,
    row_count: u64,
) -> (u64, u64) {
    let mut next_key = 0u64;
    let mut scanned = 0u64;
    let mut matches = 0u64;
    while next_key < row_count {
        let range = KeyRange::new(
            Some(TypedKey::UInt(next_key)),
            Some(TypedKey::UInt(row_count)),
            ROW_PAGE_LIMIT,
        )
        .expect("valid sequential scan page");
        let rows = query_with_hydration(repository_path, graph, "unindexed scan page", |scope| {
            graph.range_rows(&range, scope)
        });
        assert!(!rows.is_empty(), "sequential scan cannot skip a key");
        for row in rows {
            let key = match row.key() {
                TypedKey::UInt(key) => *key,
                other => panic!("fixture row has unexpected key {other:?}"),
            };
            assert_eq!(key, next_key, "full scan returns every key in order");
            if decode_single_uint_array(row.value().encoded_fields().expect("bucket field"))
                == BUCKET_TO_FIND
            {
                matches += 1;
            }
            next_key += 1;
            scanned += 1;
        }
    }
    (scanned, matches)
}

fn bucket_for(key: u64) -> u64 {
    (key + FIXTURE_SEED) % BUCKET_COUNT
}

fn expected_bucket_count(row_count: u64) -> u64 {
    let first = (BUCKET_TO_FIND + BUCKET_COUNT - (FIXTURE_SEED % BUCKET_COUNT)) % BUCKET_COUNT;
    if first >= row_count {
        0
    } else {
        1 + (row_count - 1 - first) / BUCKET_COUNT
    }
}

fn decode_single_uint_array(encoded: &[u8]) -> u64 {
    let has_blob_field = match encoded.first() {
        Some(&0x81) => false,
        Some(&0x82) => true,
        _ => panic!("fixture value must be a one- or two-field array"),
    };
    let value = *encoded.get(1).expect("integer CBOR head");
    match value {
        0..=23 => {
            if has_blob_field {
                assert!(encoded.len() > 2);
            } else {
                assert_eq!(encoded.len(), 2);
            }
            u64::from(value)
        }
        0x18 => {
            if has_blob_field {
                assert!(encoded.len() > 3);
            } else {
                assert_eq!(encoded.len(), 3);
            }
            u64::from(encoded[2])
        }
        _ => panic!("fixture bucket is not a canonical small unsigned integer"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObjectInfo {
    kind: ObjectKind,
    size: usize,
}

#[derive(Default)]
struct ObjectInventory {
    objects: BTreeMap<Oid, ObjectInfo>,
}

impl ObjectInventory {
    fn blob_count(&self) -> usize {
        self.objects
            .values()
            .filter(|object| object.kind == ObjectKind::Blob)
            .count()
    }

    fn tree_count(&self) -> usize {
        self.objects
            .values()
            .filter(|object| object.kind == ObjectKind::Tree)
            .count()
    }
}

fn object_inventory(repository: &Path) -> ObjectInventory {
    let output = git_output(
        repository,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ],
        None,
    );
    let text = std::str::from_utf8(&output).expect("Git inventory is UTF-8");
    let mut objects = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let oid = Oid::from_hex(fields.next().expect("object ID"));
        let kind = match fields.next().expect("object type") {
            "blob" => ObjectKind::Blob,
            "tree" => ObjectKind::Tree,
            "commit" => ObjectKind::Commit,
            "tag" => ObjectKind::Tag,
            other => panic!("unexpected fixture object type {other}"),
        };
        let size = fields
            .next()
            .expect("object size")
            .parse()
            .expect("numeric object size");
        objects.insert(oid, ObjectInfo { kind, size });
    }
    ObjectInventory { objects }
}

fn index_data_delta(
    index_data_blobs: &BTreeMap<Oid, usize>,
    before: &ObjectInventory,
    after: &ObjectInventory,
) -> (usize, usize) {
    index_data_blobs
        .iter()
        .filter(|(oid, _)| !before.objects.contains_key(oid) && after.objects.contains_key(oid))
        .fold((0, 0), |(count, bytes), (_, size)| {
            (count + 1, bytes + size)
        })
}

fn media_payload_delta(
    media_payload_oids: &BTreeSet<Oid>,
    before: &ObjectInventory,
    after: &ObjectInventory,
) -> (usize, usize) {
    media_payload_oids
        .iter()
        .filter(|oid| !before.objects.contains_key(oid))
        .filter_map(|oid| after.objects.get(oid))
        .fold((0, 0), |(count, bytes), object| {
            assert_eq!(object.kind, ObjectKind::Blob);
            (count + 1, bytes + object.size)
        })
}

fn tree_object_delta(before: &ObjectInventory, after: &ObjectInventory) -> usize {
    after
        .objects
        .iter()
        .filter(|(oid, object)| {
            object.kind == ObjectKind::Tree && !before.objects.contains_key(oid)
        })
        .count()
}

fn assert_rejected(
    fault: Fault,
    rows: u64,
    expected: fn(&orna_repository_v1::FormatContextError) -> bool,
) {
    let control = build_fixture(rows, Fault::None);
    let control_repository =
        Repository::discover(control.root.path()).expect("discover control fixture");
    let control_context = control_repository
        .open_format_context()
        .expect("admit control graph snapshot header");
    control_context
        .load_row_map(control.relation_id)
        .expect("control graph of identical shape must load");

    let fixture = build_fixture(rows, fault);
    let repository = Repository::discover(fixture.root.path()).expect("discover malformed fixture");
    let context = repository
        .open_format_context()
        .expect("admit malformed graph snapshot header");
    let error = context
        .load_row_map(fixture.relation_id)
        .err()
        .expect("production row-map loader must reject a malformed graph");
    assert!(
        expected(&error),
        "{fault:?} must report its specific diagnostic, got {error:?}"
    );
}

fn git(repository: &Path, arguments: &[&str]) {
    let _ = git_output(repository, arguments, None);
}

fn git_text(repository: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_output(repository, arguments, None))
        .expect("Git output is UTF-8")
        .trim()
        .to_owned()
}

fn git_output(repository: &Path, arguments: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    run_git(Some(repository), arguments, input)
}

fn git_outside(arguments: &[&str]) -> Vec<u8> {
    run_git(None, arguments, None)
}

fn run_git(repository: Option<&Path>, arguments: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut command = Command::new("git");
    if let Some(repository) = repository {
        command.current_dir(repository);
    }
    command
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().expect("spawn bounded Git command");
    if let Some(input) = input {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .expect("Git stdin")
            .write_all(input)
            .expect("write Git input");
    }
    let output = child
        .wait_with_output()
        .expect("wait for bounded Git command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn allocations() -> (u64, u64) {
    (
        ALLOCATIONS.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed),
    )
}

fn allocation_delta(before: (u64, u64), after: (u64, u64)) -> (u64, u64) {
    (after.0 - before.0, after.1 - before.1)
}

fn millis(duration: Duration) -> u128 {
    duration.as_millis()
}

fn micros(duration: Duration) -> u128 {
    duration.as_micros()
}

#[test]
fn orp1_row_store_ten_rows() {
    let small = timed("build_fixture", 10, || build_fixture(10, Fault::None));
    timed("run_case", 10, || run_case(&small));
    timed("index_churn", 10, || exercise_index_churn(&small));
}

#[path = "../../orna-cli-v1/tests/support/format3.rs"]
mod format3;
#[path = "../../orna-cli-v1/tests/support/media_commit.rs"]
mod media_commit;

/// Admits ten rows through a runtime request commit and publishes them with
/// the production writer (`prepare_allow_noop` -> `publish` -> `complete`),
/// the same sequence `orna publish` runs. The relation is seeded by the test
/// fixture with an empty ORP-1 row root (`format3::empty_format3_repository`);
/// the ten rows and the graph nodes that publication adds on top of that seed
/// are written by the production writer.
#[tokio::test]
async fn orp1_production_writer_ten_rows() {
    use orna_evaluator_v1::SysHostBindingRegistry;
    use orna_runtime_v1::RuntimeState;
    use orna_storage_v1::RuntimePublicationCoordinator;
    use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};

    let media_fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../orna-cli-v1/tests/fixtures/media");
    let (_repository_dir, repository, relation_id) = format3::empty_format3_repository();
    let media = TempDir::new().expect("temporary media directory");
    fs::copy(
        Path::new(media_fixtures).join("tone.wav"),
        media.path().join("tone.wav"),
    )
    .expect("copy media fixture");
    let import_source = fs::read_to_string(
        Path::new(media_fixtures).join("import-song.orna"),
    )
    .expect("read media import fixture");
    let root = format!("{:?}", media.path().to_string_lossy().as_ref());
    let expression = import_source.trim_end().replace("__MEDIA_ROOT__", &root);

    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("open runtime state");
    let writer = state
        .acquire_lease(identity.repository_id)
        .await
        .expect("acquire runtime writer");
    let capability = repository
        .capture_capability(relation_id)
        .expect("issue admitted capture capability");
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).expect("bounded filesystem provider");
    filesystem.allow_root(media.path()).expect("allow media fixture root");
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let head_before = repository
        .head()
        .expect("read repository head")
        .expect("format-3 fixture has a head");
    for index in 0..10_u64 {
        // Each request gets its own ordinal band so request, mutation and
        // digest identities never collide across the ten rows. Ordinal 0 is
        // excluded: an all-zero session id is an invalid runtime identity.
        let ordinal = u8::try_from(index * 10 + 1).expect("ordinal fits a byte");
        let key = format!("production-writer-{index:02}");
        media_commit::commit_capture_pending(
            &repository,
            &state,
            writer,
            &mut bindings,
            &expression,
            &key,
            ordinal,
            true,
            relation_id,
        )
        .await;
    }

    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("read latest checkpoint")
        .expect("admitted rows leave a checkpoint");
    let freeze = state
        .freeze([0x91; 16], &checkpoint)
        .await
        .expect("freeze admitted row prefix");
    let frozen = state
        .pending_table_mutations_through(&freeze)
        .await
        .expect("read frozen runtime range");
    assert_eq!(frozen.len(), 10, "every admitted row is in the frozen range");
    let head = repository
        .head()
        .expect("read repository head")
        .expect("format-3 fixture has a head");
    let index = repository
        .index_generation()
        .expect("read repository index generation");
    let mut coordinator = RuntimePublicationCoordinator::prepare_allow_noop(
        &repository,
        &state,
        &head,
        index,
        &freeze,
        |_| Err(orna_storage_v1::Error::InvalidKey),
        "orna: publish production writer rows",
    )
    .await
    .expect("storage writer prepares production publication")
    .expect("ten admitted rows are a visible change");
    coordinator
        .publish(&repository)
        .expect("storage writer publishes production rows");
    coordinator
        .complete(&repository, &state, &freeze)
        .await
        .expect("complete production publication");

    let head_after = repository
        .head()
        .expect("read published head")
        .expect("publication leaves a head");
    assert_ne!(
        head_before.as_str(),
        head_after.as_str(),
        "publication advances HEAD"
    );

    let format = repository
        .open_format_context()
        .expect("open the format context");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load the relation row map");
    assert_eq!(row_map.version().row_count(), Some(10));
    let rows = format3::committed_store_rows(&repository, relation_id);
    assert_eq!(rows.len(), 10, "ten committed rows are readable from the store");

    let graph = format
        .open_native_graph(&row_map)
        .expect("open the native graph");
    let scope = graph.open_read_scope().expect("open a read scope");
    let stats = graph.object_stats(&scope).expect("count reachable store nodes");
    let kind_count = |name: &str| -> u64 {
        stats
            .nodes()
            .iter()
            .filter(|(kind, _)| format!("{kind:?}") == name)
            .map(|(_, count)| *count)
            .sum()
    };
    let leaves = kind_count("OrderedLeaf");
    let store_roots = kind_count("StoreRoot");
    assert!(leaves >= 1, "the ordered tree has at least one leaf");
    assert_eq!(store_roots, 1, "exactly one store root is reachable");
    eprintln!(
        "ORP-1 production_writer rows={} leaves={} store_roots={} blob_references={} head={}",
        rows.len(),
        leaves,
        store_roots,
        stats.blob_references(),
        head_after.as_str(),
    );
}


#[test]
fn orp1_malformed_graphs_report_specific_diagnostics() {
    use orna_repository_v1::{FormatContextError, GraphError};
    assert_rejected(Fault::OverlappingRanges, 10, |error| {
        matches!(
            error,
            FormatContextError::GraphMalformed(GraphError::OverlappingChildRange)
        )
    });
    assert_rejected(Fault::BrokenCounts, 10, |error| {
        matches!(
            error,
            FormatContextError::GraphMalformed(GraphError::InvalidCount(_))
        )
    });
    assert_rejected(Fault::GiantFanout, 257, |error| {
        matches!(
            error,
            FormatContextError::GraphMalformed(GraphError::FanoutExceeded(_))
        )
    });
    assert_rejected(Fault::WrongSchemaDomain, 10, |error| {
        matches!(
            error,
            FormatContextError::GraphMalformed(GraphError::InvalidDomain)
        )
    });
}

#[test]
#[ignore = "requires ORNA_ORP1_MILLION_ROWS=1 and --ignored; a run without the variable must fail, not pass"]
fn orp1_row_store_one_million_rows() {
    assert!(
        std::env::var_os(MILLION_ROWS_ENV).is_some(),
        "ORP-1 one-million-row case needs {MILLION_ROWS_ENV}=1; refusing to report a skipped run as passed"
    );
    let million = timed("build_fixture", SCALE_ROWS, || {
        build_fixture(SCALE_ROWS, Fault::None)
    });
    assert!(million.leaf_pages > 1);
    assert!(million.branch_pages > 0);
    timed("run_case", SCALE_ROWS, || run_case(&million));
    timed("media_metadata", SCALE_ROWS, || {
        exercise_media_metadata(&million)
    });
    timed("concurrent_writes", SCALE_ROWS, || {
        exercise_concurrent_writes(&million)
    });
    timed("index_churn", SCALE_ROWS, || exercise_index_churn(&million));
}
