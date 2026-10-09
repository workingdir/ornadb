//! `orna publish` end to end for a runtime range with no visible change.
//!
//! The first publication of a committed media row creates exactly one
//! publication commit. Re-importing the identical capture leaves the row
//! byte-identical, so the second publication has nothing to stage: it must
//! consume the frozen runtime range and report that the changes are already
//! committed, without failing the verb and without creating an empty commit
//! (ORNA-PUB-006, ORNA-PUB-008, ORNA-PUB-019).
//!
//! The repository is a real format-3 fixture built by `support/format3.rs`, the
//! rows are committed through the real durable request path, and every
//! publication is the real `orna-cli-v1` binary, so this proves the verb's exit
//! status and output rather than a library call.
use std::path::Path;
use std::process::{Command, Output};

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RequestIdentity, RequestState, RuntimeIdentity, RuntimeState, TableMutation,
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

/// Runs `orna-cli-v1 <arguments>` in `directory` with Git routing isolated, so
/// the developer's own Git configuration cannot decide the outcome.
fn run(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        command.env_remove(name);
    }
    command
        .args(arguments)
        .output()
        .expect("CLI process starts")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn head(directory: &Path) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git runs");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Commits one captured media row durably without publishing it, so the next
/// `orna publish` sees exactly one frozen runtime range.
async fn commit_capture(
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    key: &str,
    ordinal: u8,
    insert_only: bool,
) {
    let request_identity = RequestIdentity {
        session_id: [ordinal; 16],
        request_id: [ordinal + 1; 16],
    };
    let fingerprint = [ordinal + 2; 32];
    let (_, admission) = state
        .reserve_request_with_admission(request_identity, fingerprint)
        .await
        .unwrap();
    state
        .start_request_with_owner_and_admission(
            request_identity,
            fingerprint,
            writer,
            admission.expect("new request returns its admission capability"),
        )
        .await
        .unwrap();
    let context = state.begin_activation().await.unwrap();

    let value = evaluate_expression_ovb2_with_effects(
        expression,
        &Default::default(),
        Limits::default(),
        bindings,
    )
    .unwrap();
    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let (id, key_bytes, row) = ([ordinal + 3; 16], key.as_bytes().to_vec(), Vec::new());
    let mutation = if insert_only {
        TableMutation::insert(id, "media", key_bytes, row)
    } else {
        TableMutation::new(id, "media", key_bytes, Some(row))
    }
    .unwrap()
    .with_orp_blob_binding(binding)
    .unwrap();
    let committed = state
        .commit_table_request_activation(
            writer,
            request_identity,
            fingerprint,
            &context,
            &[mutation],
            [ordinal + 4; 32],
            TerminalOutcome::new(vec![ordinal + 5]).unwrap(),
            &NoFault,
        )
        .await
        .unwrap();
    assert_eq!(committed.request.state, RequestState::Completed);
}

#[tokio::test]
async fn publishing_a_range_with_no_visible_change_consumes_it_without_a_commit() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let root_path = directory.path();
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();

    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x71; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x72; 32])
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let fixture = std::fs::read_to_string(REIMPORT_FIXTURE).unwrap();
    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root);

    // One committed insert, unpublished: the next publish has a real change.
    let writer = state.acquire_lease([0x73; 16]).await.unwrap();
    commit_capture(
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xb0,
        true,
    )
    .await;
    let pending = state.pending_count().await.unwrap();
    drop(state);
    assert!(pending > 0, "the unpublished insert leaves a pending range");

    let first = run(root_path, &["publish"]);
    assert!(
        first.status.success(),
        "publishing a real change succeeds: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_out = stdout(&first);
    assert!(
        first_out.starts_with("published "),
        "the real publication names its commit: {first_out:?}"
    );
    let published_head = head(root_path);
    assert!(
        first_out.contains(&published_head),
        "the named commit is the repository head: {first_out:?}"
    );

    // Re-import the identical capture as a replace. The committed row keeps its
    // bytes, so the frozen range folds to a no-op.
    let state = RuntimeState::open(&repository, runtime_identity, [0x72; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x74; 16]).await.unwrap();
    commit_capture(
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xc0,
        false,
    )
    .await;
    let pending = state.pending_count().await.unwrap();
    drop(state);
    assert!(
        pending > 0,
        "the identical replace still leaves a pending range"
    );

    let second = run(root_path, &["publish"]);
    let second_out = stdout(&second);
    assert!(
        second.status.success(),
        "a range with no visible change does not fail the verb: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        second_out.contains("already committed"),
        "the verb reports that the changes are already committed: {second_out:?}"
    );
    assert_eq!(
        head(root_path),
        published_head,
        "a no-op publication creates no commit"
    );

    // The consumed range left the tail, so the next publish has nothing to do.
    let third = run(root_path, &["publish"]);
    assert!(
        third.status.success(),
        "a second publish with an empty tail succeeds: {}",
        String::from_utf8_lossy(&third.stderr)
    );
    assert!(
        stdout(&third).contains("nothing to publish"),
        "the empty tail is reported, not re-consumed: {:?}",
        stdout(&third)
    );

    drop(directory);
}
