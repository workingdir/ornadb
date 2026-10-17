//! Process-kill recovery checks for the real `orna publish` executable.
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RequestIdentity, RequestState, RuntimeState, TableMutation, TableObjectId,
    TerminalOutcome, WriterLease,
};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const REIMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/reimport-image-capture.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";
const CRASH_AT: &str = "ORNA_TEST_PUBLICATION_CRASH_AT";
const CRASH_MARKER: &str = "ORNA_TEST_PUBLICATION_CRASH_MARKER";
const CAPTURE_ROOT: &str = "ORNA_TEST_CAPTURE_ROOT";
const CAPTURE_SOURCE: &str = "ORNA_TEST_CAPTURE_SOURCE";
const CAPTURE_RELATION: &str = "ORNA_TEST_CAPTURE_RELATION";
const CAPTURE_SCHEMA_ROOT: &str = "ORNA_TEST_CAPTURE_SCHEMA_ROOT";
const CAPTURE_STORE_ROOT: &str = "ORNA_TEST_CAPTURE_STORE_ROOT";
const CAPTURE_AT: &str = "ORNA_TEST_CAPTURE_CRASH_AT";

async fn commit_capture(
    repository: &Repository,
    source_commit: &orna_repository_v1::GitCommitRef,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    relation_id: [u8; 16],
    format_context: &orna_value_v1::Format3Context,
) {
    use orna_project_v1::{ProjectLoader, admit_source_row_with_owner_fields};

    let loader = ProjectLoader::default();
    let project = loader.load_committed_snapshot(repository, source_commit).unwrap();
    let schema = loader
        .resolve_committed_source_table_schema(repository, source_commit, TableObjectId::new(relation_id))
        .unwrap();
    let logical_path = "Object/image.orna";
    let row_source = repository
        .read_committed_file(source_commit, logical_path, 1 << 20)
        .unwrap();
    let value = evaluate_expression_ovb2_with_effects(
        expression, &Default::default(), Limits::default(), bindings,
    ).unwrap();
    let blob_binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    capture_crash_barrier("pin");
    let admitted = admit_source_row_with_owner_fields(
        &project,
        logical_path,
        std::str::from_utf8(&row_source).unwrap(),
        &schema,
        format_context,
        vec![("payload".to_owned(), blob_binding)],
        vec![],
        [33; 16],
    ).unwrap();
    let (source_mutation, source_bindings, resolved_schema) = admitted.into_mutation_and_bindings();
    assert_eq!(resolved_schema.table_object_id(), TableObjectId::new(relation_id));
    let mut mutation = TableMutation::insert(
        source_mutation.id(), source_mutation.table().to_owned(),
        source_mutation.key().to_vec(), source_mutation.value().unwrap().to_vec(),
    ).unwrap().with_table_object_id(TableObjectId::new(relation_id));
    for binding in &source_bindings {
        mutation = mutation.with_protected_content_pin(binding.pin()).unwrap();
    }
    let request = RequestIdentity { session_id: [21; 16], request_id: [22; 16] };
    let fingerprint = [23; 32];
    let (_, admission) = state.reserve_request_with_admission(request, fingerprint).await.unwrap();
    state.start_request_with_owner_and_admission(
        request, fingerprint, writer, admission.unwrap(),
    ).await.unwrap();
    let context = state.begin_activation().await.unwrap();
    let commit = state.commit_table_request_activation(
        writer, request, fingerprint, &context, &[mutation], [24; 32],
        TerminalOutcome::new(vec![25]).unwrap(), &NoFault,
    ).await.unwrap();
    assert_eq!(commit.request.state, RequestState::Completed);
    capture_crash_barrier("row-commit");
    drop(source_bindings);
}

fn cli(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    command.current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        command.env_remove(name);
    }
    command.args(arguments).output().expect("CLI process starts")
}

fn launch(directory: &Path, boundary: &str, marker: &Path) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    command.current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env(CRASH_AT, boundary)
        .env(CRASH_MARKER, marker);
    command.args(["publish"]);
    command.spawn().expect("publication child starts")
}

fn capture_crash_barrier(boundary: &str) {
    let Ok(selected) = std::env::var(CAPTURE_AT) else { return; };
    if selected != boundary { return; }
    let marker = std::env::var(CRASH_MARKER).expect("capture crash marker path");
    std::fs::write(marker, boundary).expect("write capture crash marker");
    loop { thread::sleep(Duration::from_millis(10)); }
}

struct MidstreamCrashRead<R> {
    inner: R,
    reads: usize,
}

impl<R: Read> Read for MidstreamCrashRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        if count > 0 {
            self.reads += 1;
            if self.reads == 2 {
                capture_crash_barrier("capture");
            }
        }
        Ok(count)
    }
}

fn launch_capture_child(
    repository_root: &Path,
    source: &Path,
    relation_id: [u8; 16],
    schema_root: &str,
    store_root: &str,
    boundary: &str,
    marker: &Path,
) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("test executable path"));
    command
        .current_dir(repository_root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env(CAPTURE_AT, boundary)
        .env(CAPTURE_ROOT, repository_root)
        .env(CAPTURE_SOURCE, source)
        .env(CAPTURE_RELATION, hex(&relation_id))
        .env(CAPTURE_SCHEMA_ROOT, schema_root)
        .env(CAPTURE_STORE_ROOT, store_root)
        .env(CRASH_MARKER, marker);
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        command.env_remove(name);
    }
    command.args(["--exact", "capture_crash_child", "--nocapture"]);
    command.spawn().expect("capture crash child starts")
}

#[tokio::test]
async fn capture_crash_child() {
    let Ok(boundary) = std::env::var(CAPTURE_AT) else { return; };
    let repository_root = std::env::var(CAPTURE_ROOT).expect("repository root");
    let source_path = std::env::var(CAPTURE_SOURCE).expect("capture source path");
    let relation = std::env::var(CAPTURE_RELATION).expect("capture relation id");
    assert_eq!(relation.len(), 32);
    let relation_id: [u8; 16] = std::array::from_fn(|index| {
        u8::from_str_radix(&relation[index * 2..index * 2 + 2], 16).unwrap()
    });
    let repository = Repository::discover(&repository_root).unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    match boundary.as_str() {
        "capture" => {
            let reader = MidstreamCrashRead {
                inner: File::open(&source_path).unwrap(),
                reads: 0,
            };
            let _candidate = capability.capture_blob_candidate(reader, 1 << 20).unwrap();
        }
        "pack-install" => {
            let candidate = capability
                .capture_blob_candidate(File::open(&source_path).unwrap(), 1 << 20)
                .unwrap();
            capture_crash_barrier("pack-install");
            drop(candidate);
        }
        "pin" | "row-commit" => {
            let base = repository.head().unwrap().unwrap();
            let schema_root = std::env::var(CAPTURE_SCHEMA_ROOT).unwrap();
            let store_root = std::env::var(CAPTURE_STORE_ROOT).unwrap();
            let format_context = persisted_format3_context(
                &repository,
                base.as_str(),
                &schema_root,
                &store_root,
            );
            let (identity, digest) = cli_runtime_identity(&repository);
            let state = RuntimeState::open(&repository, identity, digest).await.unwrap();
            let writer = state.acquire_lease(identity.repository_id).await.unwrap();
            let source_root = Path::new(&source_path).parent().unwrap();
            let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
            filesystem.allow_root(source_root).unwrap();
            let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
                .with_filesystem_provider(filesystem)
                .with_repository_capture_capability(capability);
            let fixture = std::fs::read_to_string(REIMPORT_FIXTURE).unwrap();
            let root = format!("{:?}", source_root.to_string_lossy().as_ref());
            let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root);
            commit_capture(
                &repository,
                &base,
                &state,
                writer,
                &mut bindings,
                &expression,
                relation_id,
                &format_context,
            )
            .await;
        }
        _ => panic!("unknown capture crash boundary: {boundary}"),
    }
}

async fn seeded_project() -> (TempDir, TempDir, Repository, RuntimeState, orna_repository_v1::GitCommitRef) {
    let source = TempDir::new().unwrap();
    std::fs::copy(Path::new(MEDIA_FIXTURES).join("pixel.png"), source.path().join("pixel.png")).unwrap();
    let (directory, repository, relation_id, format_context) = object_format3_repository(source.path());
    let base = repository.head().unwrap().unwrap();
    let (identity, digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, digest).await.unwrap();
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let fixture = std::fs::read_to_string(REIMPORT_FIXTURE).unwrap();
    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root);
    commit_capture(&repository, &base, &state, writer, &mut bindings, &expression, relation_id, &format_context).await;
    (directory, source, repository, state, base)
}

fn head(repository: &Repository) -> String {
    repository.head().unwrap().unwrap().as_str().to_owned()
}

#[tokio::test]
async fn cli_publication_reopens_after_process_kill_at_each_reachable_boundary() {
    // The repository observer fires after each ref/index stage is durable;
    // ref-index remains the existing boundary after both stages complete.
    for boundary in ["capture", "row", "candidate", "ref-cas", "index", "ref-index", "tail"] {
        let (directory, _source, repository, state, base) = seeded_project().await;
        let root = directory.path();
        let marker = root.join("crash-marker");
        let mut child = launch(root, boundary, &marker);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists(), "child never reached {boundary}; status: {:?}", child.try_wait().unwrap());
        child.kill().expect("SIGKILL publication child");
        let status = child.wait().expect("wait for killed child");
        assert!(!status.success(), "child unexpectedly returned at {boundary}");
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), boundary);
        std::fs::remove_file(&marker).unwrap();
        drop(state);

        let resumed = cli(root, &["publish"]);
        assert!(resumed.status.success(), "recovery at {boundary} failed: {}", String::from_utf8_lossy(&resumed.stderr));
        let once = head(&repository);
        if boundary == "capture" || boundary == "candidate" {
            assert_ne!(once, base.as_str(), "pending range not published after {boundary}");
        } else {
            assert_ne!(once, base.as_str(), "candidate not retained after {boundary}");
        }
        let (identity, digest) = cli_runtime_identity(&repository);
        RuntimeState::open(&repository, identity, digest)
            .await
            .unwrap_or_else(|error| panic!("runtime reopen after {boundary}: {error:?}"));
        let second = cli(root, &["publish"]);
        assert!(second.status.success(), "second publish after {boundary} failed: {}", String::from_utf8_lossy(&second.stderr));
        assert_eq!(head(&repository), once, "publication repeated after {boundary}");
        assert!(String::from_utf8_lossy(&second.stdout).contains("nothing to publish"));
        println!(
            "{boundary}: SIGKILL recovery_publish_exit={}, second_publish_exit={}",
            resumed.status.code().unwrap(),
            second.status.code().unwrap()
        );
    }
}

#[tokio::test]
async fn capture_and_row_commit_recover_after_process_kill() {
    for boundary in ["capture", "pack-install", "pin", "row-commit"] {
        let source = TempDir::new().unwrap();
        std::fs::copy(Path::new(MEDIA_FIXTURES).join("pixel.png"), source.path().join("pixel.png")).unwrap();
        let payload = vec![0x6d; 128 * 1024];
        let source_path = source.path().join("capture.bin");
        std::fs::write(&source_path, &payload).unwrap();
        let (directory, repository, relation_id, format_context) = object_format3_repository(source.path());
        let base = head(&repository);
        let schema_root = hex(format_context.schema_root().as_bytes());
        let store_root = hex(format_context.store_root().as_bytes());
        let marker = directory.path().join("capture-crash-marker");
        let mut child = launch_capture_child(
            directory.path(),
            &source_path,
            relation_id,
            &schema_root,
            &store_root,
            boundary,
            &marker,
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists(), "capture child never reached {boundary}; status: {:?}", child.try_wait().unwrap());
        child.kill().expect("SIGKILL capture child");
        let status = child.wait().expect("wait for killed capture child");
        assert!(!status.success(), "capture child unexpectedly returned at {boundary}");
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), boundary);
        std::fs::remove_file(&marker).unwrap();

        let resumed = cli(directory.path(), &["publish"]);
        assert!(resumed.status.success(), "recovery at {boundary} failed: {}", String::from_utf8_lossy(&resumed.stderr));
        let once = head(&repository);
        if boundary == "row-commit" {
            assert_ne!(once, base, "committed row was not published after {boundary}");
            let format = repository.open_format_context().unwrap();
            let row_map = format.load_row_map(relation_id).unwrap();
            let graph = format.open_native_graph(&row_map).unwrap();
            let scope = graph.open_read_scope().unwrap();
            let rows = graph
                .range_rows(
                    &orna_repository_v1::KeyRange::new(None, None, 16).unwrap(),
                    &scope,
                )
                .unwrap();
            assert_eq!(rows.len(), 1, "recovered row count after {boundary}");
            let descriptor = rows[0]
                .value()
                .dependencies()
                .iter()
                .find(|dependency| dependency.kind() == orna_repository_v1::NativeObjectKind::Tree)
                .expect("recovered row retains its blob descriptor")
                .oid().to_owned();
            let blob = graph.read_stored_blob_value(&rows[0], &descriptor, &scope).unwrap();
            let reference = blob.reference();
            let bytes = graph
                .read_blob_range(
                    reference,
                    0..reference.content_identity().length(),
                    &scope,
                )
                .unwrap();
            assert_eq!(bytes.bytes(), std::fs::read(source.path().join("pixel.png")).unwrap());
        } else {
            assert_eq!(once, base, "uncommitted capture became published after {boundary}");
        }
        let second = cli(directory.path(), &["publish"]);
        assert!(second.status.success(), "second publish after {boundary} failed: {}", String::from_utf8_lossy(&second.stderr));
        assert_eq!(head(&repository), once, "publication repeated after {boundary}");
        assert!(String::from_utf8_lossy(&second.stdout).contains("nothing to publish"));
        println!("{boundary}: recovery_publish_exit={}, second_publish_exit={}", resumed.status.code().unwrap(), second.status.code().unwrap());
    }
}
