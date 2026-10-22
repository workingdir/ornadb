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
    TableObjectId, TerminalOutcome, WriterLease,
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
    repository: &Repository,
    source_commit: &orna_repository_v1::GitCommitRef,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    source_key: &str,
    ordinal: u8,
    insert_only: bool,
    relation_id: [u8; 16],
    format_context: &orna_value_v1::Format3Context,
    key_override: Option<&[u8]>,
) {
    use orna_project_v1::{ProjectLoader, admit_source_row_with_owner_fields};

    let loader = ProjectLoader::default();
    let project = loader
        .load_committed_snapshot(repository, source_commit)
        .expect("load the committed source project");
    let schema = loader
        .resolve_committed_source_table_schema(repository, source_commit, TableObjectId::new(relation_id))
        .expect("resolve the Object table from its committed identity");
    let logical_path = format!("Object/{source_key}.orna");
    let row_source = repository
        .read_committed_file(source_commit, &logical_path, 1 << 20)
        .expect("the committed project contains this source Object row");
    let row_source = std::str::from_utf8(&row_source)
        .expect("the committed source Object row is valid UTF-8");

    let value = evaluate_expression_ovb2_with_effects(
        expression,
        &Default::default(),
        Limits::default(),
        bindings,
    )
    .unwrap();
    let blob_binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let admitted = admit_source_row_with_owner_fields(
        &project,
        &logical_path,
        row_source,
        &schema,
        format_context,
        vec![("payload".to_owned(), blob_binding)],
        vec![],
        [ordinal + 3; 16],
    )
    .expect("admit a complete source-owned row");
    let (source_mutation, source_bindings, resolved_schema) =
        admitted.into_mutation_and_bindings();
    assert_eq!(resolved_schema.table_object_id(), TableObjectId::new(relation_id));
    let key = key_override.unwrap_or_else(|| source_mutation.key()).to_vec();
    let value = source_mutation
        .value()
        .expect("admitted source row has complete fields")
        .to_vec();
    let id = source_mutation.id();
    let table = source_mutation.table().to_owned();
    let mut mutation = if insert_only {
        TableMutation::insert(id, table, key, value)
    } else {
        TableMutation::new(id, table, key, Some(value))
    }
    .expect("construct the admitted source row mutation")
    .with_table_object_id(TableObjectId::new(relation_id));
    for binding in &source_bindings {
        mutation = mutation
            .with_protected_content_pin(binding.pin())
            .expect("retain the admitted Blob pin");
    }

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
    drop(source_bindings);
}

#[tokio::test]
async fn publish_rejects_noncanonical_source_key_before_candidate_changes() {
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    let (directory, repository, relation_id, format_context) =
        object_format3_repository(source.path());
    let root_path = directory.path();
    let source_commit = repository.head().unwrap().unwrap();
    let (runtime_identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let fixture = std::fs::read_to_string(REIMPORT_FIXTURE).unwrap();
    let media_root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture
        .trim_end()
        .replace(MEDIA_ROOT_PLACEHOLDER, &media_root);
    commit_capture(
        &repository,
        &source_commit,
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xe0,
        true,
        relation_id,
        &format_context,
        Some(b"\xff"),
    )
    .await;
    let pending_before = state.pending_count().await.unwrap();
    assert!(pending_before > 0);
    let head_before = head(root_path);
    let refs_before = git_output(root_path, &["show-ref", "--head"], None);
    let index_before = std::fs::read(root_path.join(".git/index")).unwrap();
    let status_before = git_output(
        root_path,
        &["status", "--short", "--untracked-files=all"],
        None,
    );
    let staged_before = git_output(root_path, &["diff", "--cached", "--binary"], None);
    let unstaged_before = git_output(root_path, &["diff", "--binary"], None);
    drop(state);

    let published = run(root_path, &["publish"]);
    assert!(!published.status.success());
    assert!(stdout(&published).is_empty());
    let error = String::from_utf8_lossy(&published.stderr);
    assert!(error.contains("E2300"), "typed CLI error: {error}");
    assert!(
        error.contains("source row key is not canonical"),
        "source failure: {error}"
    );
    assert_eq!(head(root_path), head_before);
    assert_eq!(git_output(root_path, &["show-ref", "--head"], None), refs_before);
    assert_eq!(
        std::fs::read(root_path.join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        git_output(
            root_path,
            &["status", "--short", "--untracked-files=all"],
            None
        ),
        status_before
    );
    assert_eq!(
        git_output(root_path, &["diff", "--cached", "--binary"], None),
        staged_before
    );
    assert_eq!(
        git_output(root_path, &["diff", "--binary"], None),
        unstaged_before
    );
    assert!(repository.read_publication_journal().unwrap().is_none());
    let preserved = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    assert_eq!(preserved.pending_count().await.unwrap(), pending_before);
}

#[tokio::test]
async fn publish_stores_a_new_format3_row_from_its_typed_key_without_source_lookup() {
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    let (directory, repository, relation_id, format_context) =
        object_format3_repository(source.path());
    let root_path = directory.path();
    let source_commit = repository.head().unwrap().unwrap();
    let (runtime_identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let fixture = std::fs::read_to_string(REIMPORT_FIXTURE).unwrap();
    let media_root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture
        .trim_end()
        .replace(MEDIA_ROOT_PLACEHOLDER, &media_root);
    let logical_key = "runtime/../only";
    let key = logical_key.as_bytes().to_vec();
    let projection_path =
        orna_storage_v1::LoosePath::for_key("Object", &[logical_key.to_owned()]).unwrap();
    let projection = root_path.join(projection_path.as_managed_path().as_path());
    assert!(!projection.exists());
    commit_capture(
        &repository,
        &source_commit,
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xe1,
        true,
        relation_id,
        &format_context,
        Some(&key),
    )
    .await;
    drop(state);

    let published = run(root_path, &["publish"]);
    assert!(
        published.status.success(),
        "valid typed key needs no pre-existing source path: {}",
        String::from_utf8_lossy(&published.stderr)
    );
    assert_ne!(head(root_path), source_commit.as_str());
    assert!(
        !projection.exists(),
        "format-3 publication must not create a loose row projection"
    );
    assert!(
        committed_store_rows(&repository, relation_id)
            .iter()
            .any(|row| {
                row.key() == &orna_repository_v1::TypedKey::Text(logical_key.to_owned())
            })
    );
}

#[tokio::test]
async fn publishing_a_range_with_no_visible_change_consumes_it_without_a_commit() {
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    let (directory, repository, relation_id, format_context) =
        object_format3_repository(source.path());
    let root_path = directory.path();
    let source_commit = repository.head().unwrap().unwrap();
    // `orna publish` opens the runtime through the repository's own identity,
    // so the harness must commit under exactly that identity or the verb
    // reports an unavailable runtime for a repository it can see.
    let (runtime_identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
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
    // The durable writer lease is a singleton keyed by owner, and `orna publish`
    // takes it as the repository-derived id (see `invocation_owner_id`), so a
    // later CLI process resumes this same local owner instead of failing.
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    commit_capture(
        &repository,
        &source_commit,
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xb0,
        true,
        relation_id,
        &format_context,
        None,
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
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    commit_capture(
        &repository,
        &source_commit,
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xc0,
        false,
        relation_id,
        &format_context,
        None,
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

#[tokio::test]
async fn publish_recovers_ref_advanced_journal_before_selecting_pending_tail() {
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();
    let (directory, repository, relation_id, format_context) =
        object_format3_repository(source.path());
    let root_path = directory.path();
    let source_commit = repository.head().unwrap().unwrap();
    let deleted_path = root_path.join("delete-me.orna");
    std::fs::write(&deleted_path, b"tracked before publication\n").unwrap();
    let rename_path = root_path.join("rename-source/original.orna");
    std::fs::create_dir_all(rename_path.parent().unwrap()).unwrap();
    std::fs::write(&rename_path, b"renamed into the publication directory\n").unwrap();
    git(root_path, &["read-tree", "HEAD"]);
    git(
        root_path,
        &["add", "--", "delete-me.orna", "rename-source/original.orna"],
    );
    git(
        root_path,
        &["commit", "--quiet", "-m", "index reconciliation fixtures"],
    );
    git(root_path, &["rm", "--quiet", "delete-me.orna"]);
    let (runtime_identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
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
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    commit_capture(
        &repository,
        &source_commit,
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xd0,
        true,
        relation_id,
        &format_context,
        None,
    )
    .await;

    let source_path = root_path.join("unrelated.orna");
    std::fs::write(
        &source_path,
        include_bytes!("fixtures/project-core-main.orna"),
    )
    .unwrap();
    git(root_path, &["add", "unrelated.orna"]);
    std::fs::write(
        &source_path,
        include_bytes!("fixtures/project-run-zero.orna"),
    )
    .unwrap();
    let unstaged_source = git_output(
        root_path,
        &["diff", "--binary", "--", "unrelated.orna"],
        None,
    );
    let staged_unrelated = git_output(
        root_path,
        &["diff", "--cached", "--binary", "--", "unrelated.orna"],
        None,
    );

    let checkpoint = state.latest_checkpoint().await.unwrap().unwrap();
    let freeze = state.freeze([0xd1; 16], &checkpoint).await.unwrap();
    let base_head = repository.head().unwrap().unwrap();
    let index = repository.index_generation().unwrap();
    let prepared = orna_storage_v1::RuntimePublicationCoordinator::prepare(
        &repository,
        &state,
        &base_head,
        index,
        &freeze,
        |_mutation| {
            orna_storage_v1::LoosePath::for_key("Object", &["image".to_owned()])
        },
        "orna: interrupted publication",
    )
    .await
    .unwrap();
    repository
        .write_publication_journal(prepared.journal())
        .unwrap();
    assert!(repository.read_publication_journal().unwrap().is_some());
    repository
        .advance_current_ref(&base_head, prepared.candidate())
        .unwrap();
    // This tracked-file rename creates a staged file at `media`, where the
    // publication candidate needs the directory `media/image.orna`.
    git(root_path, &["mv", "rename-source/original.orna", "media"]);

    let concurrent_path = root_path.join("concurrent.orna");
    std::fs::write(
        &concurrent_path,
        include_bytes!("fixtures/project-core-main.orna"),
    )
    .unwrap();
    git(root_path, &["add", "concurrent.orna"]);
    std::fs::write(
        &concurrent_path,
        include_bytes!("fixtures/project-run-zero.orna"),
    )
    .unwrap();
    let staged_before = git_output(root_path, &["diff", "--cached", "--binary"], None);
    let unstaged_before = git_output(root_path, &["diff", "--binary"], None);
    let published_head = head(root_path);
    assert_ne!(published_head, base_head.as_str());
    assert!(state.pending_count().await.unwrap() > 0);
    drop(state);

    let conflict = run(root_path, &["publish"]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("E2300"));
    assert_eq!(head(root_path), published_head);
    assert!(repository.read_publication_journal().unwrap().is_some());
    let (runtime_identity, initial_digest) = cli_runtime_identity(&repository);
    let preserved = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    assert!(preserved.pending_count().await.unwrap() > 0);
    drop(preserved);
    assert_eq!(
        git_output(root_path, &["diff", "--cached", "--binary"], None),
        staged_before
    );
    assert_eq!(
        git_output(root_path, &["diff", "--binary"], None),
        unstaged_before
    );

    git(
        root_path,
        &["update-index", "--force-remove", "--", "concurrent.orna"],
    );
    let staged_without_untracked_conflict =
        git_output(root_path, &["diff", "--cached", "--binary"], None);
    let rename_conflict = run(root_path, &["publish"]);
    assert!(!rename_conflict.status.success());
    assert!(String::from_utf8_lossy(&rename_conflict.stderr).contains("E2300"));
    assert_eq!(head(root_path), published_head);
    assert!(repository.read_publication_journal().unwrap().is_some());
    let still_pending = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    assert!(still_pending.pending_count().await.unwrap() > 0);
    drop(still_pending);
    assert_eq!(
        git_output(root_path, &["diff", "--cached", "--binary"], None),
        staged_without_untracked_conflict,
        "the D/F rename conflict preserves the staged index"
    );

    // Revert only the rename that occupies the candidate's directory path.
    // The unrelated staged deletion must survive both the conflict and retry.
    git(
        root_path,
        &[
            "restore",
            "--source",
            base_head.as_str(),
            "--staged",
            "--worktree",
            "--",
            "media",
            "rename-source/original.orna",
        ],
    );
    let output = run(root_path, &["publish"]);
    assert!(
        output.status.success(),
        "recovery succeeds after resolving the D/F rename conflict: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout(&output).contains("nothing to publish"));
    assert_eq!(head(root_path), published_head);
    assert_eq!(repository.read_publication_journal().unwrap(), None);
    let staged_status = String::from_utf8(git_output(
        root_path,
        &["diff", "--cached", "--name-status"],
        None,
    ))
    .unwrap();
    assert!(
        staged_status.contains("D\tdelete-me.orna"),
        "staged deletion survives: {staged_status:?}"
    );
    assert!(
        !staged_status.contains("media/image.orna"),
        "published row is not staged as a reversal: {staged_status:?}"
    );
    assert!(
        !staged_status.contains("rename-source/original.orna"),
        "resolved rename does not leak into the index: {staged_status:?}"
    );
    assert_eq!(
        git_output(
            root_path,
            &["diff", "--cached", "--binary", "--", "unrelated.orna"],
            None
        ),
        staged_unrelated,
        "unrelated staged source remains unchanged"
    );
    assert_eq!(
        git_output(
            root_path,
            &["diff", "--binary", "--", "unrelated.orna"],
            None
        ),
        unstaged_source
    );
    assert_eq!(
        std::fs::read(&concurrent_path).unwrap(),
        include_bytes!("fixtures/project-run-zero.orna")
    );
    assert!(
        String::from_utf8_lossy(&git_output(root_path, &["status", "--short"], None))
            .contains("?? concurrent.orna")
    );
    let recovered = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    assert_eq!(recovered.pending_count().await.unwrap(), 0);
}
