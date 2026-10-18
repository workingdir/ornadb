use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_foundation_v1::SchemaDescriptor;
use orna_repository_v1::{
    GitHashAlgorithm, KeyRange, ManagedPath, NativeObjectKind, NativeOid, ProtectedBlobRowInsert,
    PublicationJournal, PublicationJournalEntry, PublicationJournalStage, Repository,
    RepositoryError, TypedKey, commit_protected_blob_row,
};
use orna_value_v1::Raw;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const CHILD_BOUNDARY: &str = "ORNA_REPOSITORY_CRASH_BOUNDARY";
const CHILD_ROOT: &str = "ORNA_REPOSITORY_CRASH_ROOT";
const CHILD_SOURCE: &str = "ORNA_REPOSITORY_CRASH_SOURCE";
const CHILD_RELATION: &str = "ORNA_REPOSITORY_CRASH_RELATION";
const CHILD_OWNER: &str = "ORNA_REPOSITORY_CRASH_OWNER";
const CHILD_MARKER: &str = "ORNA_REPOSITORY_CRASH_MARKER";
const INITIAL_OWNER: [u8; 16] = [0x44; 16];
const TAKEOVER_OWNER: [u8; 16] = [0x55; 16];
const RELATION_ID: [u8; 16] = [0x32; 16];
const DATABASE_ID: [u8; 16] = [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1];
const DATABASE_UUID: &str = "00000000-0000-4000-8000-000000000001";
const ROW_PATH: &str = "Object/image.orna";
const ROW_KEY: &str = "image";
const RUNTIME_INTENT: [u8; 16] = [0x88; 16];
const PAYLOAD: &[u8] =
    b"captured bytes survive SIGKILL, pack installation, and publication recovery\n";
const MAX_CAPTURE_BYTES: u64 = 1 << 20;
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::{
        sync::Arc,
        task::{Context, Poll, Waker},
    };

    struct NoopWake;
    impl std::task::Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match std::future::Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::yield_now(),
        }
    }
}

struct Fixture {
    directory: TempDir,
    repository: Repository,
    source: PathBuf,
}

fn fixture() -> Fixture {
    let directory = TempDir::new().expect("create disposable publication repository");
    let root = directory.path();
    git(
        root,
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(root, &["config", "user.name", "Orna Test"]);
    git(root, &["config", "user.email", "orna-test@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);

    let schema_descriptor = fixture_schema_descriptor();
    let schema_bytes = schema_descriptor
        .encode()
        .expect("encode canonical test schema");
    let schema_digest: [u8; 32] = Sha256::digest(&schema_bytes).into();
    let schema_chunk = git_object(root, "blob", &schema_bytes);
    let schema_chunk_bytes = decode_hex(&schema_chunk);

    let mut byte_index = Vec::new();
    cbor_array(&mut byte_index, 5);
    cbor_uint(&mut byte_index, 1);
    cbor_uint(&mut byte_index, 4);
    cbor_uint(&mut byte_index, 0);
    cbor_uint(&mut byte_index, schema_bytes.len() as u64);
    cbor_array(&mut byte_index, 1);
    cbor_array(&mut byte_index, 3);
    cbor_uint(&mut byte_index, schema_bytes.len() as u64);
    cbor_bytes(&mut byte_index, &schema_chunk_bytes);
    cbor_bytes(&mut byte_index, &schema_digest);
    let byte_root = native_node(root, &byte_index, &[(schema_chunk.as_str(), "blob")]);

    let mut schema_node = Vec::new();
    cbor_array(&mut schema_node, 5);
    cbor_uint(&mut schema_node, 1);
    cbor_uint(&mut schema_node, 6);
    cbor_uint(&mut schema_node, schema_bytes.len() as u64);
    cbor_bytes(&mut schema_node, &schema_digest);
    cbor_bytes(&mut schema_node, &decode_hex(&byte_root));
    let schema_root = native_node(root, &schema_node, &[(byte_root.as_str(), "tree")]);

    let mut row_domain = Vec::new();
    cbor_array(&mut row_domain, 3);
    cbor_text(&mut row_domain, "rows");
    cbor_bytes(&mut row_domain, &RELATION_ID);
    cbor_bytes(&mut row_domain, &schema_digest);
    let mut empty_rows = Vec::new();
    cbor_array(&mut empty_rows, 4);
    cbor_uint(&mut empty_rows, 1);
    cbor_uint(&mut empty_rows, 1);
    empty_rows.extend_from_slice(&row_domain);
    cbor_array(&mut empty_rows, 0);
    let row_root = native_node(root, &empty_rows, &[]);

    let mut relations_domain = Vec::new();
    cbor_array(&mut relations_domain, 2);
    cbor_text(&mut relations_domain, "relations");
    cbor_bytes(&mut relations_domain, &DATABASE_ID);
    let relation_key = TypedKey::Bytes(RELATION_ID.to_vec())
        .canonical_bytes()
        .expect("encode relation map key");
    let mut relation_map = Vec::new();
    cbor_array(&mut relation_map, 4);
    cbor_uint(&mut relation_map, 1);
    cbor_uint(&mut relation_map, 1);
    relation_map.extend_from_slice(&relations_domain);
    cbor_array(&mut relation_map, 1);
    cbor_array(&mut relation_map, 2);
    relation_map.extend_from_slice(&relation_key);
    cbor_array(&mut relation_map, 4);
    cbor_bytes(&mut relation_map, &decode_hex(&schema_root));
    cbor_bytes(&mut relation_map, &decode_hex(&row_root));
    relation_map.push(0xf6);
    cbor_uint(&mut relation_map, 0);
    let relation_map_root = native_node(
        root,
        &relation_map,
        &[(schema_root.as_str(), "tree"), (row_root.as_str(), "tree")],
    );

    let mut store_node = Vec::new();
    cbor_array(&mut store_node, 3);
    cbor_uint(&mut store_node, 1);
    cbor_uint(&mut store_node, 0);
    cbor_bytes(&mut store_node, &decode_hex(&relation_map_root));
    let store_root = native_node(root, &store_node, &[(relation_map_root.as_str(), "tree")]);

    let database =
        format!("{{\n    repository_format: 3,\n    database_id: \"{DATABASE_UUID}\",\n}}\n");
    let database_oid = git_object(root, "blob", database.as_bytes());
    let source_oid = git_object(root, "blob", b"module main\n");
    let row_oid = git_object(root, "blob", b"original image row\n");
    let staged_oid = git_object(root, "blob", b"staged base\n");
    let unstaged_oid = git_object(root, "blob", b"unstaged base\n");
    let object_tree = git_tree(root, &format!("100644 blob {row_oid}\timage.orna\n"));
    let orna_tree = git_tree(
        root,
        &format!("100644 blob {database_oid}\tdatabase.orna\n040000 tree {store_root}\tstore\n"),
    );
    let root_tree = git_tree(
        root,
        &format!(
            "040000 tree {orna_tree}\t.orna\n040000 tree {object_tree}\tObject\n100644 blob {source_oid}\tmain.orna\n100644 blob {staged_oid}\tunrelated-staged.txt\n100644 blob {unstaged_oid}\tunrelated-unstaged.txt\n"
        ),
    );
    let initial_commit = git_output(
        root,
        &["commit-tree", &root_tree, "-m", "seed publication fixture"],
        None,
    );
    let initial_commit = String::from_utf8(initial_commit.stdout)
        .expect("initial commit id is UTF-8")
        .trim()
        .to_owned();
    git(root, &["update-ref", "refs/heads/main", &initial_commit]);
    git(root, &["reset", "--hard", &initial_commit]);

    let source = root.join("capture.bin");
    fs::write(&source, PAYLOAD).expect("write synthetic capture bytes");
    let repository = Repository::discover(root).expect("discover fixture repository");
    Fixture {
        directory,
        repository,
        source,
    }
}

fn fixture_schema_descriptor() -> SchemaDescriptor {
    let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
    let field = |id: [u8; 16], name: &str, ty: Raw, role: u64| {
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
        (Raw::Int(1.into()), uuid(RELATION_ID)),
        (Raw::Int(2.into()), Raw::Array(vec![uuid([0x11; 16])])),
        (
            Raw::Int(3.into()),
            Raw::Array(vec![
                field(
                    [0x11; 16],
                    "id",
                    Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".to_owned())]),
                    0,
                ),
                field(
                    [0x22; 16],
                    "payload",
                    Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Blob".to_owned())]),
                    1,
                ),
            ]),
        ),
        (Raw::Int(4.into()), Raw::Array(Vec::new())),
    ]))
    .expect("construct format-3 fixture schema")
}

fn git_object(root: &Path, kind: &str, bytes: &[u8]) -> String {
    let output = git_output(
        root,
        &["hash-object", "-w", "-t", kind, "--stdin"],
        Some(bytes),
    );
    assert!(
        output.status.success(),
        "git hash-object failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git object id is UTF-8")
        .trim()
        .to_owned()
}

fn git_tree(root: &Path, entries: &str) -> String {
    let output = git_output(root, &["mktree"], Some(entries.as_bytes()));
    assert!(
        output.status.success(),
        "git mktree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git tree id is UTF-8")
        .trim()
        .to_owned()
}

fn native_node(root: &Path, data: &[u8], references: &[(&str, &str)]) -> String {
    let data_oid = git_object(root, "blob", data);
    let mut entries = String::new();
    {
        let mut sorted_references = references.to_vec();
        sorted_references.sort_by_key(|(oid, _)| *oid);
        for (oid, kind) in sorted_references {
            let mode = if kind == "tree" { "40000" } else { "100644" };
            entries.push_str(&format!("{mode} {kind} {oid}\t{oid}\n"));
        }
    }
    let mut node_entries = format!("100644 blob {data_oid}\tdata\n");
    if !references.is_empty() {
        let refs_oid = git_tree(root, &entries);
        node_entries.push_str(&format!("40000 tree {refs_oid}\trefs\n"));
    }
    git_tree(root, &node_entries)
}

fn git(root: &Path, arguments: &[&str]) {
    let output = git_output(root, arguments, None);
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(root: &Path, arguments: &[&str], input: Option<&[u8]>) -> Output {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(name);
    }
    command.args(arguments);
    if let Some(input) = input {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("start Git plumbing command");
        child
            .stdin
            .take()
            .expect("Git stdin")
            .write_all(input)
            .expect("write Git plumbing input");
        child
            .wait_with_output()
            .expect("wait for Git plumbing command")
    } else {
        command.output().expect("start Git command")
    }
}

fn cbor_head(output: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    if value <= 23 {
        output.push(prefix | value as u8);
    } else if value <= u8::MAX as u64 {
        output.extend_from_slice(&[prefix | 24, value as u8]);
    } else if value <= u16::MAX as u64 {
        output.push(prefix | 25);
        output.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= u32::MAX as u64 {
        output.push(prefix | 26);
        output.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        output.push(prefix | 27);
        output.extend_from_slice(&value.to_be_bytes());
    }
}

fn cbor_array(output: &mut Vec<u8>, length: u64) {
    cbor_head(output, 4, length);
}

fn cbor_uint(output: &mut Vec<u8>, value: u64) {
    cbor_head(output, 0, value);
}

fn cbor_bytes(output: &mut Vec<u8>, value: &[u8]) {
    cbor_head(output, 2, value.len() as u64);
    output.extend_from_slice(value);
}

fn cbor_text(output: &mut Vec<u8>, value: &str) {
    cbor_head(output, 3, value.len() as u64);
    output.extend_from_slice(value.as_bytes());
}

fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).expect("hex high nibble");
            let low = (pair[1] as char).to_digit(16).expect("hex low nibble");
            ((high << 4) | low) as u8
        })
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn parse_owner(value: &str) -> [u8; 16] {
    let bytes = decode_hex(value);
    bytes.try_into().expect("runtime owner ID has 16 bytes")
}

fn graph_for(
    repository: &Repository,
    owner: [u8; 16],
) -> (
    orna_repository_v1::NativeGraphContext,
    orna_repository_v1::RepositoryReadScope,
) {
    let context = repository
        .open_format_context()
        .expect("open committed format-3 fixture")
        .with_runtime_owner_id(owner);
    let rows = context
        .load_row_map(RELATION_ID)
        .expect("load the fixture table row map");
    let graph = context
        .open_native_graph(&rows)
        .expect("open the fixture native graph");
    let scope = graph.open_read_scope().expect("open graph read scope");
    (graph, scope)
}

fn publish_pause(marker: &Path, fields: &[(&str, String)]) -> ! {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .expect("create crash boundary marker");
    for (name, value) in fields {
        writeln!(file, "{name}={value}").expect("write crash boundary evidence");
    }
    file.sync_all().expect("sync crash boundary marker");
    loop {
        thread::sleep(Duration::from_millis(50));
    }
}

fn kill_child_at(fixture: &Fixture, boundary: &str) -> String {
    let marker = fixture.directory.path().join(format!("{boundary}.marker"));
    let mut child = Command::new(std::env::current_exe().expect("resolve integration test binary"))
        .current_dir(fixture.directory.path())
        .args(["--exact", "crash_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_BOUNDARY, boundary)
        .env(CHILD_ROOT, fixture.directory.path())
        .env(CHILD_SOURCE, &fixture.source)
        .env(CHILD_RELATION, encode_hex(&RELATION_ID))
        .env(CHILD_OWNER, encode_hex(&INITIAL_OWNER))
        .env(CHILD_MARKER, &marker)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn publication crash child");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if marker.exists() {
            break;
        }
        if let Some(status) = child.try_wait().expect("poll crash child") {
            panic!("child exited before {boundary}: {status:?}");
        }
        assert!(Instant::now() < deadline, "child did not reach {boundary}");
        thread::sleep(Duration::from_millis(10));
    }
    let evidence = fs::read_to_string(&marker).expect("read boundary evidence");
    child.kill().expect("SIGKILL child at durable boundary");
    let status = child.wait().expect("wait for killed child");
    assert!(
        !status.success(),
        "crash child survived termination at {boundary}"
    );
    evidence
}

fn marker_fields(evidence: &str) -> std::collections::BTreeMap<&str, &str> {
    evidence
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect()
}

fn owner_refs(root: &Path, owner: [u8; 16]) -> Vec<String> {
    let prefix = format!("refs/orna/pins/{}/", encode_hex(&owner));
    let output = git_output(
        root,
        &["for-each-ref", "--format=%(refname)", &prefix],
        None,
    );
    assert!(output.status.success(), "list owner pin refs");
    String::from_utf8(output.stdout)
        .expect("ref names are UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn take_over(repository: &Repository) -> orna_repository_v1::RuntimeOwnerLock {
    let owner = repository
        .acquire_runtime_owner_lock(TAKEOVER_OWNER)
        .expect("take over the killed publication owner");
    assert_eq!(owner.previous_owner(), Some(INITIAL_OWNER));
    owner
}

fn assert_empty_committed_table(repository: &Repository) {
    let context = repository
        .open_format_context()
        .expect("open published format-3 snapshot");
    let rows = context
        .load_row_map(RELATION_ID)
        .expect("load published row map");
    let graph = context
        .open_native_graph(&rows)
        .expect("open published row graph");
    let scope = graph.open_read_scope().expect("open graph read scope");
    assert!(
        graph
            .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
            .unwrap()
            .is_empty(),
        "a killed pre-commit capture must not acknowledge a row"
    );
}

fn published_payload(repository: &Repository) -> Vec<u8> {
    let context = repository
        .open_format_context()
        .expect("open recovered format-3 snapshot");
    let rows = context
        .load_row_map(RELATION_ID)
        .expect("load recovered row map");
    let graph = context
        .open_native_graph(&rows)
        .expect("open recovered native graph");
    let scope = graph.open_read_scope().expect("open graph read scope");
    let rows = graph
        .range_rows(&KeyRange::new(None, None, 16).unwrap(), &scope)
        .expect("read recovered rows");
    assert_eq!(rows.len(), 1, "one acknowledged blob row is published");
    let descriptor = rows[0]
        .value()
        .dependencies()
        .iter()
        .find(|dependency| dependency.kind() == NativeObjectKind::Tree)
        .expect("the row names its Blob descriptor")
        .oid()
        .to_owned();
    let blob = graph
        .read_stored_blob_value(&rows[0], &descriptor, &scope)
        .expect("load protected Blob metadata");
    let reference = blob.reference();
    graph
        .read_blob_range(reference, 0..reference.content_identity().length(), &scope)
        .expect("read recovered Blob bytes")
        .bytes()
        .to_vec()
}

fn stage_unrelated_work(fixture: &Fixture) -> (Vec<u8>, Vec<u8>) {
    let root = fixture.directory.path();
    fs::write(root.join("unrelated-staged.txt"), b"staged human edit\n")
        .expect("write unrelated staged edit");
    git(root, &["add", "--", "unrelated-staged.txt"]);
    fs::write(
        root.join("unrelated-unstaged.txt"),
        b"unstaged human edit\n",
    )
    .expect("write unrelated unstaged edit");
    let staged = git_output(root, &["diff", "--cached", "--binary"], None).stdout;
    let unstaged = git_output(root, &["diff", "--binary"], None).stdout;
    assert!(!staged.is_empty());
    assert!(!unstaged.is_empty());
    (staged, unstaged)
}

fn assert_unrelated_work(fixture: &Fixture, expected: &(Vec<u8>, Vec<u8>)) {
    let root = fixture.directory.path();
    assert_eq!(
        git_output(root, &["diff", "--cached", "--binary"], None).stdout,
        expected.0
    );
    assert_eq!(
        git_output(root, &["diff", "--binary"], None).stdout,
        expected.1
    );
    assert_eq!(
        fs::read(root.join("unrelated-staged.txt")).unwrap(),
        b"staged human edit\n"
    );
    assert_eq!(
        fs::read(root.join("unrelated-unstaged.txt")).unwrap(),
        b"unstaged human edit\n"
    );
}

fn git_gc(root: &Path) {
    let output = git_output(root, &["gc", "--prune=now", "--quiet"], None);
    assert!(
        output.status.success(),
        "git gc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn crash_child() {
    let Ok(boundary) = std::env::var(CHILD_BOUNDARY) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(CHILD_ROOT).expect("child repository path"));
    let source = PathBuf::from(std::env::var_os(CHILD_SOURCE).expect("child source path"));
    let relation = decode_hex(&std::env::var(CHILD_RELATION).expect("child relation id"));
    assert_eq!(relation.as_slice(), RELATION_ID);
    let owner = parse_owner(&std::env::var(CHILD_OWNER).expect("child runtime owner"));
    let marker = PathBuf::from(std::env::var_os(CHILD_MARKER).expect("child crash marker"));
    let repository = Repository::discover(&root).expect("open child repository");
    let _owner_lock = repository
        .acquire_runtime_owner_lock(owner)
        .expect("acquire child runtime owner lock");
    let (graph, scope) = graph_for(&repository, owner);

    match boundary.as_str() {
        "capture" => {
            let reader = MidstreamRead {
                inner: File::open(&source).expect("open authorized capture source"),
                reads: 0,
                marker: marker.clone(),
            };
            let _candidate = graph
                .capture_blob_candidate(reader, MAX_CAPTURE_BYTES, &scope)
                .expect("capture bytes before crash barrier");
            panic!("midstream capture returned without reaching its crash barrier");
        }
        "pack-install" => {
            let candidate = graph
                .capture_blob_candidate(File::open(&source).unwrap(), MAX_CAPTURE_BYTES, &scope)
                .expect("capture full OGB-2 closure");
            let repack = git_output(&root, &["repack", "-ad", "--quiet"], None);
            assert!(
                repack.status.success(),
                "git repack failed: {}",
                String::from_utf8_lossy(&repack.stderr)
            );
            publish_pause(
                &marker,
                &[("length", candidate.content_identity().length().to_string())],
            );
        }
        "pin" => {
            let candidate = graph
                .capture_blob_candidate(File::open(&source).unwrap(), MAX_CAPTURE_BYTES, &scope)
                .expect("capture full OGB-2 closure");
            let pin = graph
                .protect_captured_blob(candidate, &scope)
                .expect("install durable owner-scoped pin");
            let transfer = pin.transfer_record();
            publish_pause(
                &marker,
                &[
                    ("pin", encode_hex(transfer.pin_id())),
                    ("descriptor", transfer.descriptor_oid().to_hex()),
                    ("owner", encode_hex(transfer.owner_id())),
                ],
            );
        }
        "candidate" | "row-commit" | "ref-cas" | "index-reconcile" | "tail-cleanup" => {
            let captured = graph
                .capture_blob_candidate(File::open(&source).unwrap(), MAX_CAPTURE_BYTES, &scope)
                .expect("capture full OGB-2 closure");
            let pin = graph
                .protect_captured_blob(captured, &scope)
                .expect("install durable owner-scoped pin");
            let repository_for_callback = repository.clone();
            let root_for_callback = root.clone();
            let marker_for_callback = marker.clone();
            let boundary_for_callback = boundary.clone();
            let graph_for_callback = &graph;
            let old_head = repository.head().unwrap().unwrap();
            let index = repository.index_generation().unwrap();
            let old_bytes = fs::read(root.join(ROW_PATH)).expect("read managed base row");

            let _ = block_on(commit_protected_blob_row(
                &graph,
                pin,
                "application/octet-stream",
                None,
                ProtectedBlobRowInsert::new(TypedKey::Text(ROW_KEY.to_owned())),
                move |row_candidate| {
                    let store_root = row_candidate.store_root().clone();
                    let candidate = repository_for_callback
                        .build_private_commit_with_store_root(
                            &old_head,
                            graph_for_callback,
                            &store_root,
                            "orna: publish captured row",
                        )
                        .expect("build private row candidate");
                    let path = ManagedPath::new(ROW_PATH).unwrap();
                    let mut journal = PublicationJournal::new_with_runtime_intent(
                        old_head,
                        candidate.commit().clone(),
                        index.tree().unwrap().clone(),
                        RUNTIME_INTENT,
                        vec![PublicationJournalEntry::new(
                            path,
                            Some(old_bytes.clone()),
                            Some(old_bytes),
                        )],
                    )
                    .expect("journal frozen row publication")
                    .with_store_root(&store_root)
                    .expect("record frozen store root");
                    let transfer = row_candidate.protected_content_transfer().clone();
                    let boundary = boundary_for_callback.clone();
                    let repository = repository_for_callback.clone();
                    let root = root_for_callback.clone();
                    let marker = marker_for_callback.clone();
                    let index = index.clone();
                    async move {
                        let marker_fields = [
                            ("commit", candidate.commit().as_str().to_owned()),
                            ("store", store_root.to_hex()),
                            ("pin", encode_hex(transfer.pin_id())),
                            ("descriptor", transfer.descriptor_oid().to_hex()),
                        ];
                        match boundary.as_str() {
                            "candidate" => publish_pause(&marker, &marker_fields),
                            "row-commit" => {
                                repository
                                    .write_publication_journal(&journal)
                                    .expect("durably record accepted row and frozen intent");
                                publish_pause(&marker, &marker_fields);
                            }
                            "ref-cas" | "index-reconcile" => {
                                repository
                                    .write_publication_journal(&journal)
                                    .expect("durably record accepted row and frozen intent");
                                let wanted = if boundary == "ref-cas" {
                                    PublicationJournalStage::RefAdvanced
                                } else {
                                    PublicationJournalStage::IndexReconciled
                                };
                                repository
                                    .publish_candidate_with_stage_hook(
                                        &index,
                                        &candidate,
                                        &mut journal,
                                        |stage| {
                                            if stage == wanted {
                                                publish_pause(&marker, &marker_fields);
                                            }
                                        },
                                    )
                                    .expect("publish through selected durable boundary");
                            }
                            "tail-cleanup" => {
                                repository
                                    .publish_candidate(&index, &candidate, &mut journal)
                                    .expect("publish complete frozen row");
                                repository
                                    .mark_runtime_complete(RUNTIME_INTENT, &mut journal)
                                    .expect("consume frozen tail and clear publication journal");
                                let later_tail = root.join("Object/later-image.orna");
                                fs::write(&later_tail, b"newer unpublished tail\n")
                                    .expect("append newer local tail after frozen range");
                                publish_pause(&marker, &marker_fields);
                            }
                            _ => panic!("unknown publication boundary {boundary}"),
                        }
                        Ok::<(), RepositoryError>(())
                    }
                },
            ));
            panic!("publication callback returned before the {boundary} kill");
        }
        _ => panic!("unknown crash boundary {boundary}"),
    }
}

struct MidstreamRead<R> {
    inner: R,
    reads: usize,
    marker: PathBuf,
}

impl<R: Read> Read for MidstreamRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let limit = buffer.len().min(8);
        let count = self.inner.read(&mut buffer[..limit])?;
        if count > 0 {
            self.reads += 1;
            if self.reads == 2 {
                publish_pause(&self.marker, &[("phase", "source-stream".to_owned())]);
            }
        }
        Ok(count)
    }
}

#[test]
fn capture_kill_midstream_does_not_acknowledge_a_row() {
    let fixture = fixture();
    let base = fixture.repository.head().unwrap().unwrap();
    kill_child_at(&fixture, "capture");

    assert_eq!(fixture.repository.head().unwrap(), Some(base.clone()));
    assert!(
        fixture
            .repository
            .read_publication_journal()
            .unwrap()
            .is_none()
    );
    assert_empty_committed_table(&fixture.repository);
    let owner = take_over(&fixture.repository);
    assert_eq!(
        owner
            .reclaim_previous_owner_pins(&Default::default())
            .unwrap(),
        0
    );
    assert!(owner_refs(fixture.directory.path(), INITIAL_OWNER).is_empty());
    git_gc(fixture.directory.path());

    let (graph, scope) = graph_for(&fixture.repository, TAKEOVER_OWNER);
    let retried = graph
        .capture_blob_candidate(
            File::open(&fixture.source).unwrap(),
            MAX_CAPTURE_BYTES,
            &scope,
        )
        .expect("a replacement owner can capture the source after recovery");
    assert_eq!(retried.content_identity().length(), PAYLOAD.len() as u64);
}

#[test]
fn pack_install_kill_recovers_only_the_unaccepted_object_root() {
    let fixture = fixture();
    let base = fixture.repository.head().unwrap().unwrap();
    kill_child_at(&fixture, "pack-install");

    let root = fixture.directory.path();
    assert_eq!(fixture.repository.head().unwrap(), Some(base.clone()));
    assert!(
        fixture
            .repository
            .read_publication_journal()
            .unwrap()
            .is_none()
    );
    assert_empty_committed_table(&fixture.repository);
    let refs = owner_refs(root, INITIAL_OWNER);
    assert_eq!(
        refs.len(),
        1,
        "captured graph is protected before pin transfer"
    );
    assert!(refs[0].contains("/scratch/"));
    assert!(
        git_output(root, &["fsck", "--full", "--no-reflogs"], None)
            .status
            .success()
    );

    let owner = take_over(&fixture.repository);
    assert_eq!(
        owner
            .reclaim_previous_owner_pins(&Default::default())
            .unwrap(),
        1
    );
    assert!(owner_refs(root, INITIAL_OWNER).is_empty());
    git_gc(root);
    assert!(
        git_output(root, &["fsck", "--full", "--no-reflogs"], None)
            .status
            .success()
    );
    assert_eq!(fixture.repository.head().unwrap(), Some(base.clone()));
}

#[test]
fn pin_kill_retains_only_manifest_accepted_content_across_repack() {
    let fixture = fixture();
    let base = fixture.repository.head().unwrap().unwrap();
    let evidence = kill_child_at(&fixture, "pin");
    let fields = marker_fields(&evidence);
    let pin_id: [u8; 16] = decode_hex(fields["pin"])
        .try_into()
        .expect("accepted pin id");
    let descriptor = NativeOid::from_hex(GitHashAlgorithm::Sha1, fields["descriptor"])
        .expect("accepted descriptor ID");
    let owner = take_over(&fixture.repository);
    let accepted = std::collections::BTreeMap::from([(pin_id, descriptor.clone())]);
    assert_eq!(owner.reclaim_previous_owner_pins(&accepted).unwrap(), 0);
    let refs = owner_refs(fixture.directory.path(), INITIAL_OWNER);
    assert_eq!(refs.len(), 1);
    assert!(refs[0].contains("/pending/"));
    let repack = git_output(
        fixture.directory.path(),
        &["repack", "-ad", "--quiet"],
        None,
    );
    assert!(repack.status.success(), "repack accepted pinned objects");
    assert!(
        git_output(
            fixture.directory.path(),
            &[
                "cat-file",
                "-e",
                &format!("{}^{{tree}}", descriptor.to_hex())
            ],
            None,
        )
        .status
        .success(),
        "the pending pin protects the descriptor through repack"
    );
    assert_eq!(fixture.repository.head().unwrap(), Some(base.clone()));
    assert_empty_committed_table(&fixture.repository);

    assert_eq!(
        owner
            .reclaim_previous_owner_pins(&Default::default())
            .unwrap(),
        1
    );
    assert!(owner_refs(fixture.directory.path(), INITIAL_OWNER).is_empty());
    git_gc(fixture.directory.path());
}

#[test]
fn row_commit_kill_keeps_the_frozen_row_and_pin_pending() {
    let fixture = fixture();
    let base = fixture.repository.head().unwrap().unwrap();
    let evidence = kill_child_at(&fixture, "row-commit");

    assert_eq!(fixture.repository.head().unwrap(), Some(base.clone()));
    let journal = fixture
        .repository
        .read_publication_journal()
        .unwrap()
        .expect("row commit has a durable pending publication journal");
    assert_eq!(journal.stage(), PublicationJournalStage::Prepared);
    assert!(
        matches!(
            fixture.repository.recover_publication().unwrap_err(),
            RepositoryError::PublicationPending
        ),
        "pre-ref recovery preserves the pending batch rather than reporting success"
    );
    let owner = take_over(&fixture.repository);
    assert!(
        matches!(
            owner
                .reclaim_previous_owner_pins(&Default::default())
                .unwrap_err(),
            RepositoryError::PublicationPending
        ),
        "the publication journal remains authoritative over accepted pin cleanup"
    );
    let fields = marker_fields(&evidence);
    let descriptor = NativeOid::from_hex(GitHashAlgorithm::Sha1, fields["descriptor"])
        .expect("pending descriptor ID");
    assert!(
        git_output(
            fixture.directory.path(),
            &[
                "cat-file",
                "-e",
                &format!("{}^{{tree}}", descriptor.to_hex())
            ],
            None,
        )
        .status
        .success(),
        "pending accepted content remains reachable after recovery"
    );
    assert!(
        fixture
            .repository
            .read_publication_journal()
            .unwrap()
            .is_some()
    );
    assert_eq!(fixture.repository.head().unwrap(), Some(base.clone()));
    assert_empty_committed_table(&fixture.repository);
}

#[test]
fn private_candidate_kill_before_journaling_does_not_publish_or_acknowledge() {
    let fixture = fixture();
    let base = fixture.repository.head().unwrap().unwrap();
    let evidence = kill_child_at(&fixture, "candidate");
    let fields = marker_fields(&evidence);
    let candidate = fields["commit"].to_owned();

    assert!(fixture.repository.recover_publication().unwrap().is_none());
    assert!(
        fixture
            .repository
            .read_publication_journal()
            .unwrap()
            .is_none()
    );
    assert_eq!(fixture.repository.head().unwrap(), Some(base.clone()));
    assert_empty_committed_table(&fixture.repository);
    let owner = take_over(&fixture.repository);
    assert!(
        owner
            .reclaim_previous_owner_pins(&Default::default())
            .unwrap()
            > 0
    );
    git_gc(fixture.directory.path());
    assert!(
        !git_output(
            fixture.directory.path(),
            &["cat-file", "-e", &format!("{candidate}^{{commit}}")],
            None,
        )
        .status
        .success(),
        "an unjournaled private candidate may be collected"
    );
    assert_eq!(fixture.repository.head().unwrap(), Some(base.clone()));
}

fn recover_after_ref_or_index_boundary(fixture: &Fixture, boundary: &str) {
    let expected_work = stage_unrelated_work(fixture);
    let base = fixture.repository.head().unwrap().unwrap();
    kill_child_at(fixture, boundary);

    let journal = fixture
        .repository
        .read_publication_journal()
        .unwrap()
        .expect("a ref/index crash retains its recovery journal");
    assert!(matches!(
        journal.stage(),
        PublicationJournalStage::RefAdvanced | PublicationJournalStage::IndexReconciled
    ));
    assert_ne!(fixture.repository.head().unwrap(), Some(base));
    assert!(
        matches!(
            fixture.repository.recover_publication().unwrap_err(),
            RepositoryError::RuntimeCompletionRequired
        ),
        "recovery finishes index/worktree reconciliation and waits for the runtime boundary"
    );
    let mut recovered = fixture
        .repository
        .read_publication_journal()
        .unwrap()
        .expect("runtime completion remains journalled after repository recovery");
    assert_eq!(
        recovered.stage(),
        PublicationJournalStage::WorktreeReconciled
    );
    assert_eq!(published_payload(&fixture.repository), PAYLOAD);
    assert_unrelated_work(fixture, &expected_work);
    let recovered_head = fixture.repository.head().unwrap().unwrap();

    fixture
        .repository
        .mark_runtime_complete(RUNTIME_INTENT, &mut recovered)
        .expect("complete the recovered frozen runtime intent");
    assert!(
        fixture
            .repository
            .read_publication_journal()
            .unwrap()
            .is_none()
    );
    assert!(fixture.repository.recover_publication().unwrap().is_none());
    assert_eq!(
        fixture.repository.head().unwrap(),
        Some(recovered_head.clone())
    );
    assert_eq!(published_payload(&fixture.repository), PAYLOAD);
    assert_unrelated_work(fixture, &expected_work);

    let owner = take_over(&fixture.repository);
    assert!(
        owner
            .reclaim_previous_owner_pins(&Default::default())
            .unwrap()
            > 0
    );
    git_gc(fixture.directory.path());
    assert_eq!(published_payload(&fixture.repository), PAYLOAD);
    assert_eq!(fixture.repository.head().unwrap(), Some(recovered_head));
}

#[test]
fn ref_cas_kill_recovers_index_worktree_and_blob_once() {
    let fixture = fixture();
    recover_after_ref_or_index_boundary(&fixture, "ref-cas");
}

#[test]
fn index_reconcile_kill_finishes_worktree_recovery_once() {
    let fixture = fixture();
    recover_after_ref_or_index_boundary(&fixture, "index-reconcile");
}

#[test]
fn tail_cleanup_kill_does_not_republish_or_consume_later_work() {
    let fixture = fixture();
    let expected_work = stage_unrelated_work(&fixture);
    let base = fixture.repository.head().unwrap().unwrap();
    let later_tail = fixture.directory.path().join("Object/later-image.orna");
    kill_child_at(&fixture, "tail-cleanup");

    assert!(
        fixture
            .repository
            .read_publication_journal()
            .unwrap()
            .is_none()
    );
    assert!(fixture.repository.recover_publication().unwrap().is_none());
    let recovered_head = fixture.repository.head().unwrap().unwrap();
    assert_ne!(recovered_head, base);
    assert_eq!(published_payload(&fixture.repository), PAYLOAD);
    assert_eq!(fs::read(&later_tail).unwrap(), b"newer unpublished tail\n");
    assert!(
        !git_output(
            fixture.directory.path(),
            &["cat-file", "-e", "HEAD:Object/later-image.orna"],
            None,
        )
        .status
        .success(),
        "the later tail is not in the frozen publication"
    );
    assert_unrelated_work(&fixture, &expected_work);
    assert!(fixture.repository.recover_publication().unwrap().is_none());
    assert_eq!(
        fixture.repository.head().unwrap(),
        Some(recovered_head.clone())
    );
    assert_eq!(published_payload(&fixture.repository), PAYLOAD);

    let owner = take_over(&fixture.repository);
    assert!(
        owner
            .reclaim_previous_owner_pins(&Default::default())
            .unwrap()
            > 0
    );
    git_gc(fixture.directory.path());
    assert_eq!(published_payload(&fixture.repository), PAYLOAD);
    assert_eq!(fixture.repository.head().unwrap(), Some(recovered_head));
    assert_eq!(fs::read(later_tail).unwrap(), b"newer unpublished tail\n");
}
