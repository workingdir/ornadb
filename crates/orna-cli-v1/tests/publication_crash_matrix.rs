//! Process-kill recovery checks for the real `orna publish` executable.
use std::{
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
    // Capture and candidate are before publication-side durable changes; the
    // ref-index boundary is deliberately combined by the repository API.
    for boundary in ["capture", "row", "candidate", "ref-index", "tail"] {
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
