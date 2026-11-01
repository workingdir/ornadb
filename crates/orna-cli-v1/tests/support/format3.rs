//! Shared format-3 repository and Git fixture builders for CLI integration
//! tests that exercise the capture path. Included by test binaries with
//! `#[path = "support/format3.rs"] mod format3;`.

use std::{
    collections::BTreeSet,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use orna_conformance_v1::DurableTransactionalEvaluator;
use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::{AdmittedRow, KeyRange, Repository, TypedKey};
use orna_runtime_v1::{RuntimeIdentity, RuntimeState, RuntimeTableIdentity, TableObjectId};
use orna_storage_v1::LoosePath;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use std::sync::Arc;

use orna_value_v1::{
    AvailabilityState, BlobResolver, ContentReference, ContextValue, Error as ValueError,
    Format3Context, Format3Quota, GitHash, NativeOid, OwnerLifetime, ROV3_BLOB_TAG,
    ROV3_OVERFLOW_TAG, Raw, Snapshot, ValueFormat,
};

use orna_project_v1::ObjectIdentityManifest;
use orna_semantic_v1::{ModuleInput, analyze};

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// The identity `orna publish` derives for a repository: the metadata's
/// database id, a rotated-and-salted repository id, and both concatenated as
/// the initial digest. This repeats `cli_publish.rs::runtime_identity`, so a
/// harness that opens the runtime must use exactly this identity. A harness
/// that opens the runtime under any other identity leaves the verb unable to
/// see its rows, and the verb refuses the state database as another runtime's.
pub fn cli_runtime_identity(repository: &Repository) -> (RuntimeIdentity, [u8; 32]) {
    let metadata = orna_repository_v1::inspect_metadata(repository)
        .unwrap()
        .expect("the fixture records repository metadata");
    let database_id = *metadata.database_id().as_bytes();
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).expect("bounded rotation");
        let salt = u8::try_from(index).expect("fixed identity length");
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }
    let mut initial_digest = [0; 32];
    initial_digest[..16].copy_from_slice(&database_id);
    initial_digest[16..].copy_from_slice(&repository_id);
    (
        RuntimeIdentity {
            database_id,
            repository_id,
        },
        initial_digest,
    )
}

/// Reads one admitted relation's committed rows at the published `HEAD`, by the
/// relation's committed identity.
///
/// The runtime stores an admitted mutation under
/// `TableMutation::table_storage_key(table, object_id)`, so a name-keyed read
/// misses every row whose mutation carried its admitted table object id. This
/// is the identity-keyed read, not a second key form.
pub async fn committed_admitted_rows(
    state: &RuntimeState,
    table: &str,
    relation_id: [u8; 16],
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let identity = RuntimeTableIdentity::new(table, TableObjectId::new(relation_id))
        .expect("the fixture table admits a committed identity");
    state
        .read_admitted_table_at_head(&identity)
        .await
        .expect("the published HEAD answers the admitted relation")
        .rows()
        .to_vec()
}

/// Reads one admitted relation's committed rows from the published store, the
/// same reader the query and history verbs use: the format context's relation
/// row map, then the native graph's bounded range read.
pub fn committed_store_rows(repository: &Repository, relation_id: [u8; 16]) -> Vec<AdmittedRow> {
    let format = repository
        .open_format_context()
        .expect("open the format context");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load the relation row map");
    let graph = format
        .open_native_graph(&row_map)
        .expect("open the native graph");
    let scope = graph.open_read_scope().expect("open a read scope");
    graph
        .range_rows(&KeyRange::new(None, None, 16).expect("key range"), &scope)
        .expect("read the committed rows")
}

/// The logical key bytes a store row carries: the text a `Str` key spells, or
/// the bytes an opaque key holds.
pub fn logical_key_bytes(key: &TypedKey) -> Vec<u8> {
    match key {
        TypedKey::Text(text) => text.as_bytes().to_vec(),
        TypedKey::Bytes(bytes) => bytes.clone(),
        other => other.canonical_bytes().expect("a canonical logical key"),
    }
}

/// Publishes one committed media row through the explicit publication
/// boundary, the same path `orna` uses. The runtime only stages durable
/// mutations; this freezes the pending tail at its checkpoint and projects the
/// frozen prefix into the format-3 store as one Git commit, so the row becomes
/// reachable from the published snapshot.
///
/// `ordinal` names the publication intent; it must not repeat within one
/// runtime, because a frozen intent is durable.
pub async fn publish_media_row(
    repository: &Repository,
    state: &RuntimeState,
    key: &str,
    ordinal: u8,
) {
    let checkpoint = state
        .latest_checkpoint()
        .await
        .unwrap()
        .expect("a committed row leaves a checkpoint");
    let logical_key = key.to_owned();
    DurableTransactionalEvaluator::default()
        .publish_pending(
            repository,
            state,
            [ordinal; 16],
            &checkpoint,
            |mutation| LoosePath::for_key(mutation.table(), &[logical_key.clone()]),
            &format!("orna: publish {key}"),
        )
        .await
        .expect("the explicit publication boundary succeeds");
}
/// Publishes every source row by its canonical mutation key and committed path.
pub async fn publish_source_rows(
    repository: &Repository,
    state: &RuntimeState,
    paths_by_key: &std::collections::BTreeMap<Vec<u8>, LoosePath>,
    publish_id: [u8; 16],
) {
    let checkpoint = state
        .latest_checkpoint()
        .await
        .unwrap()
        .expect("source rows leave a committed checkpoint");
    DurableTransactionalEvaluator::default()
        .publish_pending(
            repository,
            state,
            publish_id,
            &checkpoint,
            |mutation| {
                Ok(paths_by_key
                    .get(mutation.key())
                    .expect("each source mutation has a committed path")
                    .clone())
            },
            "orna: publish source rows",
        )
        .await
        .expect("the explicit source publication boundary succeeds");
}

pub fn capture_failure(
    bindings: &mut SysHostBindingRegistry,
    root: &str,
    path: &str,
    max_bytes: u64,
) -> String {
    let source = format!("sys.blob.capture_file({root:?}, {path:?}, {max_bytes})");
    evaluate_expression_ovb2_with_effects(&source, &Default::default(), Limits::default(), bindings)
        .unwrap_err()
        .code()
        .to_owned()
}

pub fn empty_format3_repository() -> (TempDir, Repository, [u8; 16]) {
    format3_repository(include_str!("../fixtures/media/empty-media-main.orna"))
}

/// Creates an Object project, its canonical identity manifest, and matching
/// native descriptor roots in one committed repository. The Object rows are
/// not committed: format 3 keeps rows only in `.orna/store`
/// (ORNA-REPO-004), so a caller admits [`object_source_row`] documents into
/// the store.
pub fn object_format3_repository() -> (TempDir, Repository, [u8; 16], Format3Context) {
    format3_repository_with_context(
        include_str!("../fixtures/media/offline-query-main.orna"),
        "Object",
    )
}

/// The source document of one fixture Object row (`song`, `image` or
/// `overflow`), naming its media under `media_root`.
pub fn object_source_row(media_root: &Path, key: &str) -> String {
    let fixture = match key {
        "song" => include_str!("../fixtures/media/source-song.orna"),
        "image" => include_str!("../fixtures/media/source-image.orna"),
        "overflow" => include_str!("../fixtures/media/overflow.orna"),
        other => panic!("no fixture Object row {other:?}"),
    };
    fixture.replace(
        "__MEDIA_ROOT__",
        &format!("{:?}", media_root.to_string_lossy().as_ref()),
    )
}

/// Adds one admitted source row to the native Format-3 graph and commits a
/// canonical graph snapshot. History reads the immutable native graph.
pub fn commit_source_row_to_native_graph(
    root: &Path,
    repository: &Repository,
    relation_id: [u8; 16],
    context: &Format3Context,
    key: &str,
) -> String {
    let head = repository.head().unwrap().unwrap();
    let format = repository
        .open_pinned_format_context(head.as_str())
        .unwrap();
    let rows = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&rows).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let stored_row = graph
        .range_rows(&KeyRange::new(None, None, 16).expect("key range"), &scope)
        .expect("read the committed source rows")
        .into_iter()
        .find(|row| logical_key_bytes(row.key()).as_slice() == key.as_bytes())
        .expect("the source row is present in the committed row map");
    let fields = stored_row
        .value()
        .encoded_fields()
        .expect("the fixture row fields fit inline");
    let schema_oid = rows.version().schema().schema_oid();
    let schema_digest = *rows.version().schema().schema_digest();
    let database_id = format.require_database_id().unwrap();

    let raw_fields = ContextValue::decode_in_context(fields, ValueFormat::Rov3, context).unwrap();
    let mut dependency_oids = BTreeSet::new();
    collect_row_dependencies(raw_fields.raw(), &mut dependency_oids);
    let dependencies = dependency_oids
        .iter()
        .map(|oid| (oid.as_str(), "tree"))
        .collect::<Vec<_>>();

    let mut row_domain = vec![0x83, 0x64];
    row_domain.extend_from_slice(b"rows");
    append_cbor_bytes(&mut row_domain, &relation_id);
    append_cbor_bytes(&mut row_domain, &schema_digest);
    let key_bytes = TypedKey::Text(key.to_owned()).canonical_bytes().unwrap();
    let mut row = vec![0x84, 0x01, 0x01];
    row.extend_from_slice(&row_domain);
    row.extend_from_slice(&[0x81, 0x82]);
    row.extend_from_slice(&key_bytes);
    row.extend_from_slice(fields);
    let row_root = native_fixture_node(root, &row, &dependencies);
    let schema_oid_hex = schema_oid.to_hex();

    let mut relation_domain = vec![0x82, 0x69];
    relation_domain.extend_from_slice(b"relations");
    append_cbor_bytes(&mut relation_domain, database_id.as_bytes());
    let mut relation_map = vec![0x84, 0x01, 0x01];
    relation_map.extend_from_slice(&relation_domain);
    relation_map.extend_from_slice(&[0x81, 0x82]);
    append_cbor_bytes(&mut relation_map, &relation_id);
    relation_map.push(0x84);
    append_cbor_bytes(&mut relation_map, schema_oid.as_bytes());
    append_cbor_bytes(&mut relation_map, &git_oid_bytes(&row_root));
    relation_map.extend_from_slice(&[0xf6, 0x01]);
    let relation_map = native_fixture_node(
        root,
        &relation_map,
        &[
            (schema_oid_hex.as_str(), "tree"),
            (row_root.as_str(), "tree"),
        ],
    );
    let mut store_root = vec![0x83, 0x01, 0x00];
    append_cbor_bytes(&mut store_root, &git_oid_bytes(&relation_map));
    let store_root = native_fixture_node(root, &store_root, &[(relation_map.as_str(), "tree")]);

    // The index equals `HEAD`, so its tree carries the committed source.
    let staged_tree = String::from_utf8(git_output(root, &["write-tree"], None))
        .unwrap()
        .trim()
        .to_owned();
    let entries = String::from_utf8(git_output(root, &["ls-tree", &staged_tree], None)).unwrap();
    let old_orna_tree = String::from_utf8(git_output(
        root,
        &["rev-parse", &format!("{staged_tree}:.orna")],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    let old_orna_entries =
        String::from_utf8(git_output(root, &["ls-tree", &old_orna_tree], None)).unwrap();
    let mut orna_entries = format!("040000 tree {store_root}\tstore\n");
    for entry in old_orna_entries
        .lines()
        .filter(|entry| !entry.ends_with("\tstore"))
    {
        orna_entries.push_str(entry);
        orna_entries.push('\n');
    }
    let orna_tree = git_tree(root, &orna_entries);
    let mut root_entries = format!("040000 tree {orna_tree}\t.orna\n");
    for entry in entries.lines().filter(|entry| !entry.ends_with("\t.orna")) {
        root_entries.push_str(entry);
        root_entries.push('\n');
    }
    let root_tree = git_tree(root, &root_entries);
    let commit = String::from_utf8(git_output(
        root,
        &[
            "commit-tree",
            &root_tree,
            "-p",
            head.as_str(),
            "-m",
            "add native Object row",
        ],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    let head_ref = String::from_utf8(git_output(root, &["symbolic-ref", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    git(root, &["update-ref", &head_ref, &commit]);
    git(root, &["reset", "--quiet", "--hard", &commit]);
    commit
}

fn collect_row_dependencies(raw: &Raw, dependencies: &mut BTreeSet<String>) {
    match raw {
        Raw::Tag(ROV3_BLOB_TAG, payload) => {
            if let Raw::Array(fields) = payload.as_ref()
                && let Some(Raw::Bytes(oid)) = fields.get(4)
            {
                dependencies.insert(hex(oid));
            }
        }
        Raw::Tag(ROV3_OVERFLOW_TAG, payload) => {
            if let Raw::Array(fields) = payload.as_ref()
                && let Some(Raw::Bytes(oid)) = fields.first()
            {
                dependencies.insert(hex(oid));
            }
        }
        Raw::Array(values) => {
            for value in values {
                collect_row_dependencies(value, dependencies);
            }
        }
        Raw::Map(entries) => {
            for (key, value) in entries {
                collect_row_dependencies(key, dependencies);
                collect_row_dependencies(value, dependencies);
            }
        }
        Raw::Tag(_, value) => collect_row_dependencies(value, dependencies),
        Raw::Null | Raw::Bool(_) | Raw::Int(_) | Raw::Float(_) | Raw::Bytes(_) | Raw::Text(_) => {}
    }
}

pub fn format3_repository(media_source: &str) -> (TempDir, Repository, [u8; 16]) {
    format3_repository_for_table(media_source, "media")
}

pub fn format3_repository_for_table(
    media_source: &str,
    table_name: &str,
) -> (TempDir, Repository, [u8; 16]) {
    let (directory, repository, relations, _) =
        format3_repository_parts(media_source, &[table_name], &[]);
    (directory, repository, relations[0].relation_id)
}
/// Creates an empty format-3 table and returns a context pinned to that
/// exact committed snapshot. Rows enter the store through admission, never as
/// committed files (ORNA-REPO-004).
pub fn format3_repository_with_context(
    media_source: &str,
    table_name: &str,
) -> (TempDir, Repository, [u8; 16], Format3Context) {
    format3_repository_with_context_and_modules(media_source, table_name, &[])
}

/// Like [`format3_repository_with_context`], with extra `(logical path, source)`
/// modules committed beside `main.orna`. `main.orna` must `use` a module to make
/// it reachable. Logical paths may nest (`lib/inner/deep.orna`); each parent directory is created.
pub fn format3_repository_with_context_and_modules(
    media_source: &str,
    table_name: &str,
    extra_modules: &[(&str, &str)],
) -> (TempDir, Repository, [u8; 16], Format3Context) {
    let (directory, repository, relations, store_root) =
        format3_repository_parts(media_source, &[table_name], extra_modules);
    let relation = relations.into_iter().next().unwrap();
    let commit = repository.head().unwrap().unwrap();
    let context =
        persisted_format3_context(&repository, commit.as_str(), &relation.schema_oid, &store_root);
    (directory, repository, relation.relation_id, context)
}

/// Creates a format-3 repository whose `main.orna` declares every name in
/// `table_names`, with one committed relation per table and no rows.
/// `extra_modules` are committed beside `main.orna` as in
/// [`format3_repository_with_context_and_modules`].
pub fn format3_repository_with_tables(
    media_source: &str,
    table_names: &[&str],
    extra_modules: &[(&str, &str)],
) -> (TempDir, Repository) {
    let (directory, repository, _, _) =
        format3_repository_parts(media_source, table_names, extra_modules);
    (directory, repository)
}

/// One relation the fixture commits: its table object identity and the object
/// ID of its schema node.
struct FixtureRelation {
    relation_id: [u8; 16],
    schema_oid: String,
}

/// Commits a format-3 repository whose `main.orna` declares each name in
/// `declaration_names`, with one relation per declared table and no rows.
/// Returns the relations in declaration order and the store root's object ID.
fn format3_repository_parts(
    media_source: &str,
    declaration_names: &[&str],
    extra_modules: &[(&str, &str)],
) -> (TempDir, Repository, Vec<FixtureRelation>, String) {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.email", "kieran@drewett.dev"]);
    git(root, &["config", "user.name", "kierandrewett"]);
    git(root, &["config", "commit.gpgsign", "false"]);

    let database_id = [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1];
    let database_uuid = "00000000-0000-4000-8000-000000000001";
    let database =
        format!("{{\n    repository_format: 3,\n    database_id: \"{database_uuid}\",\n}}\n");
    let mut modules = vec![ModuleInput::new("main.orna", media_source)];
    modules.extend(extra_modules.iter().map(|&(path, source)| ModuleInput::new(path, source)));
    let analysis = analyze(&modules);
    let identities = ObjectIdentityManifest::reconcile(&modules, None).unwrap();
    let manifest_bytes = identities.encode().unwrap();
    std::fs::create_dir_all(root.join(".orna/store")).unwrap();
    std::fs::write(root.join(".orna/database.orna"), &database).unwrap();
    std::fs::write(root.join(".orna/object-identities.cbor"), &manifest_bytes).unwrap();
    std::fs::write(root.join(".orna/store/data"), b"store marker").unwrap();
    std::fs::write(root.join("main.orna"), media_source).unwrap();
    for &(path, source) in extra_modules {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, source).unwrap();
    }
    git(root, &["add", "--all"]);
    git(
        root,
        &["commit", "--quiet", "-m", "format3 capture fixture"],
    );

    // The byte index names its chunk by the chunk's SHA-256; the kind-6 node
    // and the rows domain carry the format-3 schema identity: SHA-256 of
    // `orna.schema.v3`, NUL and the descriptor bytes.
    let mut relations = Vec::new();
    let mut relation_entries = Vec::new();
    let mut relation_children: Vec<(String, &str)> = Vec::new();
    for &declaration_name in declaration_names {
        let table = identities
            .source_table_schema(
                &modules,
                &analysis,
                Some(database_id),
                "",
                "main.orna",
                declaration_name,
            )
            .unwrap();
        let relation_id = *table.table_object_id();
        let schema_source = table.descriptor().encode().unwrap();
        let chunk_digest: [u8; 32] = Sha256::digest(&schema_source).into();
        let schema_digest: [u8; 32] = Sha256::new()
            .chain_update(b"orna.schema.v3\0")
            .chain_update(&schema_source)
            .finalize()
            .into();
        let chunk_oid = git_object(root, "blob", &schema_source);
        let mut byte_index = vec![0x85, 0x01, 0x04, 0x00];
        append_cbor_head(&mut byte_index, 0, schema_source.len());
        byte_index.push(0x81);
        byte_index.push(0x83);
        append_cbor_head(&mut byte_index, 0, schema_source.len());
        append_cbor_bytes(&mut byte_index, &git_oid_bytes(&chunk_oid));
        append_cbor_bytes(&mut byte_index, &chunk_digest);
        let byte_index_oid =
            native_fixture_node(root, &byte_index, &[(chunk_oid.as_str(), "blob")]);

        let mut schema = vec![0x85, 0x01, 0x06];
        append_cbor_head(&mut schema, 0, schema_source.len());
        append_cbor_bytes(&mut schema, &schema_digest);
        append_cbor_bytes(&mut schema, &git_oid_bytes(&byte_index_oid));
        let schema_oid = native_fixture_node(root, &schema, &[(byte_index_oid.as_str(), "tree")]);

        let mut row_domain = vec![0x83, 0x64];
        row_domain.extend_from_slice(b"rows");
        append_cbor_bytes(&mut row_domain, &relation_id);
        append_cbor_bytes(&mut row_domain, &schema_digest);
        let mut empty_rows = vec![0x84, 0x01, 0x01];
        empty_rows.extend_from_slice(&row_domain);
        empty_rows.push(0x80);
        let row_root_oid = native_fixture_node(root, &empty_rows, &[]);

        let key = TypedKey::Bytes(relation_id.to_vec())
            .canonical_bytes()
            .expect("the relation ID has a canonical typed key");
        let mut value = vec![0x84];
        append_cbor_bytes(&mut value, &git_oid_bytes(&schema_oid));
        append_cbor_bytes(&mut value, &git_oid_bytes(&row_root_oid));
        value.push(0xf6);
        value.push(0x00);
        relation_entries.push((key, value));
        relation_children.push((schema_oid.clone(), "tree"));
        relation_children.push((row_root_oid, "tree"));
        relations.push(FixtureRelation {
            relation_id,
            schema_oid,
        });
    }
    relation_entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut relation_domain = vec![0x82, 0x69];
    relation_domain.extend_from_slice(b"relations");
    append_cbor_bytes(&mut relation_domain, &database_id);
    let mut relation_map = vec![0x84, 0x01, 0x01];
    relation_map.extend_from_slice(&relation_domain);
    append_cbor_head(&mut relation_map, 4, relation_entries.len());
    for (key, value) in &relation_entries {
        relation_map.push(0x82);
        relation_map.extend_from_slice(key);
        relation_map.extend_from_slice(value);
    }
    let relation_children: Vec<(&str, &str)> = relation_children
        .iter()
        .map(|(oid, kind)| (oid.as_str(), *kind))
        .collect();
    let relation_map_oid = native_fixture_node(root, &relation_map, &relation_children);

    let mut store_root = vec![0x83, 0x01, 0x00];
    append_cbor_bytes(&mut store_root, &git_oid_bytes(&relation_map_oid));
    let store_root_oid =
        native_fixture_node(root, &store_root, &[(relation_map_oid.as_str(), "tree")]);
    let database_oid = git_object(root, "blob", database.as_bytes());
    let manifest_oid = git_object(root, "blob", &manifest_bytes);
    let source_oid = git_object(root, "blob", media_source.as_bytes());
    let orna_tree = git_tree(
        root,
        &format!(
            "100644 blob {database_oid}\tdatabase.orna\n100644 blob {manifest_oid}\tobject-identities.cbor\n040000 tree {store_root_oid}\tstore\n"
        ),
    );
    let mut module_root = ModuleDirectory::default();
    for &(path, source) in extra_modules {
        let oid = git_object(root, "blob", source.as_bytes());
        module_root.insert(path, oid);
    }
    let root_entries = format!(
        "040000 tree {orna_tree}\t.orna\n100644 blob {source_oid}\tmain.orna\n{}",
        module_root.entries(root)
    );
    let root_tree = git_tree(root, &root_entries);
    let parent = String::from_utf8(git_output(root, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    let commit = String::from_utf8(git_output(
        root,
        &[
            "commit-tree",
            &root_tree,
            "-p",
            &parent,
            "-m",
            "format3 graph fixture",
        ],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    let head_ref = String::from_utf8(git_output(root, &["symbolic-ref", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    git(root, &["update-ref", &head_ref, &commit]);
    git(root, &["reset", "--quiet", "--hard", &commit]);

    let repository = Repository::discover(root).unwrap();
    (directory, repository, relations, store_root_oid)
}

struct UnreadableFixtureResolver;

impl BlobResolver for UnreadableFixtureResolver {
    fn read_range(
        &self,
        _reference: &ContentReference,
        _offset: u64,
        _length: u64,
    ) -> orna_value_v1::Result<Vec<u8>> {
        Err(ValueError::ContentUnavailable)
    }
}

pub fn persisted_format3_context(
    repository: &Repository,
    commit: &str,
    schema_root: &str,
    store_root: &str,
) -> Format3Context {
    let database_id = *repository
        .open_format_context()
        .unwrap()
        .require_database_id()
        .unwrap()
        .as_bytes();
    let algorithm = GitHash::Sha1;
    Format3Context::from_persisted(
        database_id,
        Snapshot::Commit {
            database: database_id,
            algorithm,
            oid: git_oid_bytes(commit),
        },
        algorithm,
        NativeOid::from_bytes(&git_oid_bytes(schema_root)).unwrap(),
        NativeOid::from_bytes(&git_oid_bytes(store_root)).unwrap(),
        AvailabilityState::Available,
        OwnerLifetime::new([0x66; 16]),
        Format3Quota::new(16, 1 << 20).unwrap(),
        Arc::new(UnreadableFixtureResolver),
    )
    .unwrap()
}

/// Creates a payload-unreadable context pinned to an arbitrary repository commit.
pub fn format3_context_for_snapshot(repository: &Repository, commit: &str) -> Format3Context {
    let database_id = *repository
        .open_format_context()
        .unwrap()
        .require_database_id()
        .unwrap()
        .as_bytes();
    let algorithm = GitHash::Sha1;
    let root = NativeOid::from_bytes(&[0x55; 20]).unwrap();
    Format3Context::from_persisted(
        database_id,
        Snapshot::Commit {
            database: database_id,
            algorithm,
            oid: git_oid_bytes(commit),
        },
        algorithm,
        root.clone(),
        root,
        AvailabilityState::Available,
        OwnerLifetime::new([0x66; 16]),
        Format3Quota::new(16, 1 << 20).unwrap(),
        Arc::new(UnreadableFixtureResolver),
    )
    .unwrap()
}

pub fn git(path: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap()
            .success()
    );
}

pub fn git_output(path: &Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub fn git_object(path: &Path, kind: &str, bytes: &[u8]) -> String {
    String::from_utf8(git_output(
        path,
        &["hash-object", "-w", "-t", kind, "--stdin"],
        Some(bytes),
    ))
    .unwrap()
    .trim()
    .to_owned()
}

/// Source modules at logical paths, written as nested git trees. `git mktree`
/// rejects a slash in an entry name, so each parent directory is its own tree.
#[derive(Default)]
struct ModuleDirectory {
    files: std::collections::BTreeMap<String, String>,
    directories: std::collections::BTreeMap<String, ModuleDirectory>,
}

impl ModuleDirectory {
    fn insert(&mut self, path: &str, oid: String) {
        match path.split_once('/') {
            Some((directory, rest)) => self
                .directories
                .entry(directory.to_owned())
                .or_default()
                .insert(rest, oid),
            None => {
                self.files.insert(path.to_owned(), oid);
            }
        }
    }

    fn entries(&self, path: &Path) -> String {
        let mut entries = String::new();
        for (name, oid) in &self.files {
            entries.push_str(&format!("100644 blob {oid}\t{name}\n"));
        }
        for (name, directory) in &self.directories {
            let tree = git_tree(path, &directory.entries(path));
            entries.push_str(&format!("040000 tree {tree}\t{name}\n"));
        }
        entries
    }
}

pub fn git_tree(path: &Path, entries: &str) -> String {
    String::from_utf8(git_output(path, &["mktree"], Some(entries.as_bytes())))
        .unwrap()
        .trim()
        .to_owned()
}

pub fn native_fixture_node(path: &Path, data: &[u8], dependencies: &[(&str, &str)]) -> String {
    let data_oid = git_object(path, "blob", data);
    let mut dependencies = dependencies.to_vec();
    dependencies.sort_by_key(|(oid, _)| *oid);
    let refs_oid = if dependencies.is_empty() {
        None
    } else {
        let refs = dependencies
            .iter()
            .map(|(oid, kind)| {
                let mode = if *kind == "tree" { "40000" } else { "100644" };
                format!("{mode} {kind} {oid}\t{oid}\n")
            })
            .collect::<String>();
        Some(git_tree(path, &refs))
    };
    let mut envelope = format!("100644 blob {data_oid}\tdata\n");
    if let Some(refs_oid) = refs_oid {
        envelope.push_str(&format!("40000 tree {refs_oid}\trefs\n"));
    }
    git_tree(path, &envelope)
}

pub fn append_cbor_head(output: &mut Vec<u8>, major: u8, value: usize) {
    let prefix = major << 5;
    match value {
        0..=23 => output.push(prefix | value as u8),
        24..=255 => output.extend_from_slice(&[prefix | 24, value as u8]),
        _ => panic!("fixture CBOR value is unexpectedly large"),
    }
}

pub fn append_cbor_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    append_cbor_head(output, 2, bytes.len());
    output.extend_from_slice(bytes);
}

pub fn git_oid_bytes(oid: &str) -> Vec<u8> {
    oid.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hex = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(hex, 16).unwrap()
        })
        .collect()
}
