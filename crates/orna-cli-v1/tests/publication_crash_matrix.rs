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
use orna_storage_v1::RuntimePublicationCoordinator;
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
    logical_path: &str,
    insert_only: bool,
    owner_id: [u8; 16],
    request: RequestIdentity,
    relation_id: [u8; 16],
    format_context: &orna_value_v1::Format3Context,
) {
    use orna_project_v1::{ProjectLoader, admit_source_row_with_owner_fields};

    let loader = ProjectLoader::default();
    let project = loader.load_committed_snapshot(repository, source_commit).unwrap();
    let schema = loader
        .resolve_committed_source_table_schema(repository, source_commit, TableObjectId::new(relation_id))
        .unwrap();
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
        owner_id,
    ).unwrap();
    let (source_mutation, source_bindings, resolved_schema) = admitted.into_mutation_and_bindings();
    assert_eq!(resolved_schema.table_object_id(), TableObjectId::new(relation_id));
    let source_value = source_mutation.value().unwrap().to_vec();
    let mut mutation = if insert_only {
        TableMutation::insert(
            source_mutation.id(),
            source_mutation.table().to_owned(),
            source_mutation.key().to_vec(),
            source_value,
        )
    } else {
        TableMutation::new(
            source_mutation.id(),
            source_mutation.table().to_owned(),
            source_mutation.key().to_vec(),
            Some(source_value),
        )
    }
    .unwrap()
    .with_table_object_id(TableObjectId::new(relation_id));
    for binding in &source_bindings {
        mutation = mutation.with_protected_content_pin(binding.pin()).unwrap();
    }
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
                "Object/image.orna",
                true,
                [33; 16],
                RequestIdentity { session_id: [21; 16], request_id: [22; 16] },
                relation_id,
                &format_context,
            )
            .await;
        }
        _ => panic!("unknown capture crash boundary: {boundary}"),
    }
}

async fn seeded_project() -> (
    TempDir,
    TempDir,
    Repository,
    RuntimeState,
    orna_repository_v1::GitCommitRef,
    [u8; 16],
    orna_value_v1::Format3Context,
) {
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
    commit_capture(
        &repository,
        &base,
        &state,
        writer,
        &mut bindings,
        &expression,
        "Object/image.orna",
        true,
        [33; 16],
        RequestIdentity { session_id: [21; 16], request_id: [22; 16] },
        relation_id,
        &format_context,
    )
    .await;
    (directory, source, repository, state, base, relation_id, format_context)
}

fn head(repository: &Repository) -> String {
    repository.head().unwrap().unwrap().as_str().to_owned()
}

#[tokio::test]
async fn cli_publication_reopens_after_process_kill_at_each_reachable_boundary() {
    // The repository observer fires after each ref/index stage is durable;
    // ref-index remains the existing boundary after both stages complete.
    for boundary in ["capture", "row", "candidate", "ref-cas", "index", "ref-index", "tail"] {
        let (directory, _source, repository, state, base, _relation_id, _format_context) =
            seeded_project().await;
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
async fn recovery_consumes_only_frozen_range_and_preserves_unrelated_work() {
    let (directory, source, repository, state, source_commit, relation_id, format_context) =
        seeded_project().await;
    let root = directory.path();
    let initial_payload = std::fs::read(source.path().join("pixel.png")).unwrap();
    std::fs::write(root.join("unrelated-tracked.txt"), b"committed baseline\n").unwrap();
    git(root, &["add", "--", "unrelated-tracked.txt"]);
    git(root, &["commit", "--quiet", "-m", "add unrelated tracked file"]);
    let base = repository.head().unwrap().unwrap();

    let staged_path = root.join("unrelated-staged.txt");
    std::fs::write(&staged_path, b"staged before crash\n").unwrap();
    git(root, &["add", "--", "unrelated-staged.txt"]);
    let unstaged_path = root.join("unrelated-tracked.txt");
    std::fs::write(&unstaged_path, b"unstaged before crash\n").unwrap();
    let staged_before = git_output(root, &["diff", "--cached", "--binary"], None);
    let unstaged_before = git_output(root, &["diff", "--binary"], None);
    assert!(!staged_before.is_empty(), "fixture has staged unrelated work");
    assert!(!unstaged_before.is_empty(), "fixture has unstaged unrelated work");

    let marker = root.join("crash-marker");
    let mut child = launch(root, "candidate", &marker);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "child never reached candidate; status: {:?}",
        child.try_wait().unwrap()
    );
    let tail_payload = std::fs::read(Path::new(MEDIA_FIXTURES).join("pixel2.png")).unwrap();
    assert_ne!(initial_payload, tail_payload, "the later fixture has different image bytes");
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel2.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();

    let (identity, _) = cli_runtime_identity(&repository);
    let source_format_context = format_context;
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let fixture = std::fs::read_to_string(REIMPORT_FIXTURE).unwrap();
    let media_root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &media_root);
    commit_capture(
        &repository,
        &source_commit,
        &state,
        writer,
        &mut bindings,
        &expression,
        "Object/image.orna",
        false,
        [43; 16],
        RequestIdentity { session_id: [41; 16], request_id: [42; 16] },
        relation_id,
        &source_format_context,
    )
    .await;
    drop(bindings);

    child.kill().expect("SIGKILL publication child");
    let status = child.wait().expect("wait for killed publication child");
    assert!(!status.success(), "child unexpectedly returned at candidate");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "candidate");
    std::fs::remove_file(&marker).unwrap();
    drop(state);

    let (identity, digest) = cli_runtime_identity(&repository);
    let recovered = RuntimeState::open(&repository, identity, digest)
        .await
        .unwrap_or_else(|error| panic!("runtime reopen after candidate crash: {error:?}"));
    // PUB-3 steps 1 and 6: freezing retains pending mutations, while the
    // durable publication journal is written only after candidate construction.
    // This child was killed at the earlier candidate hook, so no journal
    // should exist yet and the frozen range must still be pending for recovery.
    assert!(repository.read_publication_journal().unwrap().is_none());
    assert_eq!(head(&repository), base.as_str(), "candidate crash advanced HEAD");
    assert!(recovered.pending_count().await.unwrap() > 0);
    let recovered_index = RuntimePublicationCoordinator::recover(&repository, &recovered)
        .await
        .expect("recover the candidate after process death");
    assert!(recovered_index.is_some(), "prepared candidate was not recovered");
    assert!(repository.read_publication_journal().unwrap().is_none());
    let read_published_payload = || {
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
        assert_eq!(rows.len(), 1, "the image key remains one row");
        let descriptor = rows[0]
            .value()
            .dependencies()
            .iter()
            .find(|dependency| dependency.kind() == orna_repository_v1::NativeObjectKind::Tree)
            .expect("published image retains its blob descriptor")
            .oid()
            .to_owned();
        let blob = graph
            .read_stored_blob_value(&rows[0], &descriptor, &scope)
            .unwrap();
        let reference = blob.reference();
        graph
            .read_blob_range(
                reference,
                0..reference.content_identity().length(),
                &scope,
            )
            .unwrap()
            .bytes()
            .to_vec()
    };
    assert_ne!(head(&repository), base.as_str(), "frozen range was not recovered");
    assert_eq!(
        committed_store_rows(&repository, relation_id).len(),
        1,
        "recovery published only the frozen row, not the newer tail"
    );
    assert_eq!(read_published_payload(), initial_payload, "recovery leaves the frozen blob visible");
    assert!(
        recovered.pending_count().await.unwrap() > 0,
        "newer mutation remains pending after frozen-range recovery"
    );
    assert_eq!(
        git_output(root, &["diff", "--cached", "--binary"], None),
        staged_before,
        "recovery preserves unrelated staged work"
    );
    assert_eq!(
        git_output(root, &["diff", "--binary"], None),
        unstaged_before,
        "recovery preserves unrelated unstaged work"
    );
    assert_eq!(std::fs::read(&staged_path).unwrap(), b"staged before crash\n");
    assert_eq!(std::fs::read(&unstaged_path).unwrap(), b"unstaged before crash\n");
    let recovered_head = head(&repository);
    drop(recovered);

    let tail = cli(root, &["publish"]);
    assert!(tail.status.success(), "later tail publish failed: {}", String::from_utf8_lossy(&tail.stderr));
    assert_ne!(head(&repository), recovered_head, "pending later tail was not publishable");
    assert_eq!(committed_store_rows(&repository, relation_id).len(), 1);
    assert_eq!(read_published_payload(), tail_payload, "later tail replaces the frozen value only after recovery");
    let completed = RuntimeState::open(&repository, identity, digest).await.unwrap();
    assert_eq!(completed.pending_count().await.unwrap(), 0);
    drop(completed);
    assert_eq!(
        git_output(root, &["diff", "--cached", "--binary"], None),
        staged_before,
        "later tail publication preserves unrelated staged work"
    );
    assert_eq!(
        git_output(root, &["diff", "--binary"], None),
        unstaged_before,
        "later tail publication preserves unrelated unstaged work"
    );
    let tail_head = head(&repository);
    let no_op = cli(root, &["publish"]);
    assert!(no_op.status.success(), "repeat publish failed: {}", String::from_utf8_lossy(&no_op.stderr));
    assert_eq!(head(&repository), tail_head);
    assert!(String::from_utf8_lossy(&no_op.stdout).contains("nothing to publish"));
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
