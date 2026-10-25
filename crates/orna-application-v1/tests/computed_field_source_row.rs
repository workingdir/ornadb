//! A committed format-3 table with a computed field: a row admitted against
//! the committed schema is recorded through the staged source-row path, and
//! the computed selector never becomes a stored column (ORNA-FIELD-006,
//! ORNA-FIELD-011).

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use orna_application_v1::ApplicationAuthority;
use orna_evaluator_v1::{Environment, Limits};
use orna_project_v1::{ObjectIdentityManifest, ProjectLoader, admit_format3_row};
use orna_repository_v1::{Repository, TypedKey};
use orna_runtime_v1::TableObjectId;
use orna_semantic_v1::{Catalogue, ModuleInput, analyze};
use orna_value_v1::{
    AvailabilityState, BlobResolver, ContentReference, Error as ValueError, Format3Context,
    Format3Quota, GitHash, NativeOid, OwnerLifetime, Raw, Snapshot, Value,
    decode_canonical_storage_value,
};
use sha2::{Digest, Sha256};

// 07-tables.md "Stored, defaulted and computed fields".
const SOURCE: &str = "pub table Person(id: Str) {
    first: Str,
    last: Str,
    full_name: Str => \"{first} {last}\",
}

pub fn main(): Int = 1;
";
const DATABASE_ID: [u8; 16] = [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 2];
const DATABASE_UUID: &str = "00000000-0000-4000-8000-000000000002";
static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

/// One committed format-3 repository whose store holds the source table's
/// schema node and an empty row map.
struct CommittedFixture {
    root: PathBuf,
    repository: Repository,
    relation_id: [u8; 16],
    schema_oid: String,
    store_root_oid: String,
}

impl Drop for CommittedFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct UnreadableResolver;

impl BlobResolver for UnreadableResolver {
    fn read_range(
        &self,
        _reference: &ContentReference,
        _offset: u64,
        _length: u64,
    ) -> orna_value_v1::Result<Vec<u8>> {
        Err(ValueError::ContentUnavailable)
    }
}

fn committed_fixture(table: &str) -> CommittedFixture {
    let sequence = NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "orna-computed-field-source-row-{}-{timestamp}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    git(
        &root,
        &["config", "user.email", "orna-tests@example.invalid"],
    );
    git(&root, &["config", "user.name", "Orna Tests"]);
    git(&root, &["config", "commit.gpgsign", "false"]);

    let database =
        format!("{{\n    repository_format: 3,\n    database_id: \"{DATABASE_UUID}\",\n}}\n");
    let modules = [ModuleInput::new("main.orna", SOURCE)];
    let analysis = analyze(&modules);
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let identities = ObjectIdentityManifest::reconcile(&modules, None).unwrap();
    let manifest_bytes = identities.encode().unwrap();
    let schema = identities
        .source_table_schema(
            &modules,
            &analysis,
            Some(DATABASE_ID),
            "",
            "main.orna",
            table,
        )
        .expect("a table with a computed field has a schema node");
    let relation_id = *schema.table_object_id();
    let schema_source = schema.descriptor().encode().unwrap();
    // The byte index names its chunk by the plain SHA-256 of the chunk; the
    // schema identity is `orna.schema.v3` over the descriptor (store-format-3 section 5).
    let chunk_digest: [u8; 32] = Sha256::digest(&schema_source).into();
    let schema_digest: [u8; 32] = Sha256::new()
        .chain_update(b"orna.schema.v3\0")
        .chain_update(&schema_source)
        .finalize()
        .into();

    let chunk_oid = git_object(&root, "blob", &schema_source);
    let mut byte_index = vec![0x85, 0x01, 0x04, 0x00];
    cbor_head(&mut byte_index, 0, schema_source.len());
    byte_index.extend_from_slice(&[0x81, 0x83]);
    cbor_head(&mut byte_index, 0, schema_source.len());
    cbor_bytes(&mut byte_index, &oid_bytes(&chunk_oid));
    cbor_bytes(&mut byte_index, &chunk_digest);
    let byte_index_oid = native_node(&root, &byte_index, &[(chunk_oid.as_str(), "blob")]);

    let mut schema_node = vec![0x85, 0x01, 0x06];
    cbor_head(&mut schema_node, 0, schema_source.len());
    cbor_bytes(&mut schema_node, &schema_digest);
    cbor_bytes(&mut schema_node, &oid_bytes(&byte_index_oid));
    let schema_oid = native_node(&root, &schema_node, &[(byte_index_oid.as_str(), "tree")]);

    let mut empty_rows = vec![0x84, 0x01, 0x01, 0x83, 0x64];
    empty_rows.extend_from_slice(b"rows");
    cbor_bytes(&mut empty_rows, &relation_id);
    cbor_bytes(&mut empty_rows, &schema_digest);
    empty_rows.push(0x80);
    let row_root_oid = native_node(&root, &empty_rows, &[]);

    let mut relation_map = vec![0x84, 0x01, 0x01, 0x82, 0x69];
    relation_map.extend_from_slice(b"relations");
    cbor_bytes(&mut relation_map, &DATABASE_ID);
    relation_map.extend_from_slice(&[0x81, 0x82]);
    relation_map.extend_from_slice(
        &TypedKey::Bytes(relation_id.to_vec())
            .canonical_bytes()
            .expect("the relation ID has a canonical typed key"),
    );
    relation_map.push(0x84);
    cbor_bytes(&mut relation_map, &oid_bytes(&schema_oid));
    cbor_bytes(&mut relation_map, &oid_bytes(&row_root_oid));
    relation_map.extend_from_slice(&[0xf6, 0x00]);
    let relation_map_oid = native_node(
        &root,
        &relation_map,
        &[
            (schema_oid.as_str(), "tree"),
            (row_root_oid.as_str(), "tree"),
        ],
    );

    let mut store_root = vec![0x83, 0x01, 0x00];
    cbor_bytes(&mut store_root, &oid_bytes(&relation_map_oid));
    let store_root_oid = native_node(&root, &store_root, &[(relation_map_oid.as_str(), "tree")]);

    let database_oid = git_object(&root, "blob", database.as_bytes());
    let manifest_oid = git_object(&root, "blob", &manifest_bytes);
    let source_oid = git_object(&root, "blob", SOURCE.as_bytes());
    let orna_tree = git_tree(
        &root,
        &format!(
            "100644 blob {database_oid}\tdatabase.orna\n100644 blob {manifest_oid}\tobject-identities.cbor\n040000 tree {store_root_oid}\tstore\n"
        ),
    );
    let root_tree = git_tree(
        &root,
        &format!("040000 tree {orna_tree}\t.orna\n100644 blob {source_oid}\tmain.orna\n"),
    );
    let commit = git_text(
        &root,
        &[
            "commit-tree",
            &root_tree,
            "-m",
            "format-3 computed field fixture",
        ],
    );
    let head_ref = git_text(&root, &["symbolic-ref", "HEAD"]);
    git(&root, &["update-ref", &head_ref, &commit]);
    git(&root, &["reset", "--quiet", "--hard", &commit]);

    let repository = Repository::discover(&root).unwrap();
    CommittedFixture {
        root,
        repository,
        relation_id,
        schema_oid,
        store_root_oid,
    }
}

fn pinned_context(fixture: &CommittedFixture, commit: &str) -> Format3Context {
    let algorithm = GitHash::Sha1;
    Format3Context::from_persisted(
        DATABASE_ID,
        Snapshot::Commit {
            database: DATABASE_ID,
            algorithm,
            oid: oid_bytes(commit),
        },
        algorithm,
        NativeOid::from_bytes(&oid_bytes(&fixture.schema_oid)).unwrap(),
        NativeOid::from_bytes(&oid_bytes(&fixture.store_root_oid)).unwrap(),
        AvailabilityState::Available,
        OwnerLifetime::new([0x66; 16]),
        Format3Quota::new(16, 1 << 20).unwrap(),
        Arc::new(UnreadableResolver),
    )
    .unwrap()
}

#[test]
fn committed_computed_field_row_is_recorded_without_a_stored_column() {
    let fixture = committed_fixture("Person");
    let repository = &fixture.repository;
    let commit = repository.head().unwrap().expect("the fixture commit");
    let loader = ProjectLoader::default();
    let schema = loader
        .resolve_committed_source_table_schema(
            repository,
            &commit,
            TableObjectId::new(fixture.relation_id),
        )
        .expect("the committed computed-field table resolves its schema");
    let Raw::Map(entries) = schema.descriptor().raw() else {
        panic!("the schema descriptor is a map");
    };
    let Some((_, Raw::Array(fields))) = entries.iter().find(|(key, _)| *key == Raw::Int(3.into()))
    else {
        panic!("the schema descriptor lists its fields");
    };
    let full_name = fields
        .iter()
        .find_map(|field| match field {
            Raw::Array(parts) if parts.get(1) == Some(&Raw::Text("full_name".into())) => {
                parts.get(3)
            }
            _ => None,
        })
        .expect("the computed field is declared");
    assert_eq!(full_name, &Raw::Int(2.into()));

    let project = loader
        .load_committed_snapshot(repository, &commit)
        .expect("the committed project loads");
    let context = pinned_context(&fixture, commit.as_str());
    let key = Value::new(Raw::Text("ada".into()))
        .and_then(|key| key.encode())
        .unwrap();
    let row = Raw::Map(vec![
        (Raw::Text("first".into()), Raw::Text("Ada".into())),
        (Raw::Text("id".into()), Raw::Text("ada".into())),
        (Raw::Text("last".into()), Raw::Text("Lovelace".into())),
    ]);
    let admitted = admit_format3_row(
        &project,
        &key,
        &row,
        &schema,
        &context,
        Vec::new(),
        [0x51; 16],
    )
    .expect("a row omitting its computed field is admitted");

    let authority = ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
    let application = authority
        .admit_module("main.orna", SOURCE, "main")
        .expect("the computed-field module is admitted");
    let staged = authority
        .evaluate_staged_with_source_rows(
            &application,
            &Environment::new(),
            vec![admitted],
            &context,
        )
        .expect("the admitted row is recorded; its computed field is skipped");

    let [mutation] = staged.mutations() else {
        panic!("exactly one recorded source row");
    };
    assert_eq!(mutation.table(), "Person");
    assert!(mutation.table_object_id() == Some(TableObjectId::new(fixture.relation_id)));
    assert_eq!(mutation.key(), b"ada");
    let stored = decode_canonical_storage_value(mutation.value().expect("an inserted row"))
        .expect("canonical stored fields");
    let Raw::Array(values) = stored else {
        panic!("stored fields are an array");
    };
    assert_eq!(values.len(), 2, "only first and last are stored columns");
    assert!(values.contains(&Raw::Text("Ada".into())));
    assert!(values.contains(&Raw::Text("Lovelace".into())));
}

fn git(path: &Path, args: &[&str]) {
    git_output(path, args, None);
}

fn git_text(path: &Path, args: &[&str]) -> String {
    String::from_utf8(git_output(path, args, None))
        .unwrap()
        .trim()
        .to_owned()
}

fn git_output(path: &Path, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
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

fn git_object(path: &Path, kind: &str, bytes: &[u8]) -> String {
    String::from_utf8(git_output(
        path,
        &["hash-object", "-w", "-t", kind, "--stdin"],
        Some(bytes),
    ))
    .unwrap()
    .trim()
    .to_owned()
}

fn git_tree(path: &Path, entries: &str) -> String {
    String::from_utf8(git_output(path, &["mktree"], Some(entries.as_bytes())))
        .unwrap()
        .trim()
        .to_owned()
}

/// One OGS-1 node tree: `data` plus a `refs` tree of its native children.
fn native_node(path: &Path, data: &[u8], dependencies: &[(&str, &str)]) -> String {
    let data_oid = git_object(path, "blob", data);
    let mut dependencies = dependencies.to_vec();
    dependencies.sort_by_key(|(oid, _)| *oid);
    let mut envelope = format!("100644 blob {data_oid}\tdata\n");
    if !dependencies.is_empty() {
        let refs = dependencies
            .iter()
            .map(|(oid, kind)| {
                let mode = if *kind == "tree" { "40000" } else { "100644" };
                format!("{mode} {kind} {oid}\t{oid}\n")
            })
            .collect::<String>();
        envelope.push_str(&format!("40000 tree {}\trefs\n", git_tree(path, &refs)));
    }
    git_tree(path, &envelope)
}

fn cbor_head(output: &mut Vec<u8>, major: u8, value: usize) {
    let prefix = major << 5;
    if let Ok(small) = u8::try_from(value) {
        if small < 24 {
            output.push(prefix | small);
        } else {
            output.extend_from_slice(&[prefix | 24, small]);
        }
    } else {
        let value = u16::try_from(value).expect("fixture CBOR length fits two bytes");
        output.push(prefix | 25);
        output.extend_from_slice(&value.to_be_bytes());
    }
}

fn cbor_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    cbor_head(output, 2, bytes.len());
    output.extend_from_slice(bytes);
}

fn oid_bytes(oid: &str) -> Vec<u8> {
    oid.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
