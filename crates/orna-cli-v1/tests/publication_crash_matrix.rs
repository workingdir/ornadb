//! Process-kill recovery checks for the real `orna publish` executable.
use std::{
    fs::File,
    ops::{Deref, DerefMut},
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
const REIMPORT_FIXTURE: &str = include_str!("fixtures/media/reimport-image-capture.orna");
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";
const CRASH_AT: &str = "ORNA_TEST_PUBLICATION_CRASH_AT";
const CRASH_DIR: &str = "ORNA_TEST_PUBLICATION_CRASH_DIR";
const MARKER_FILE: &str = "publication-crash-marker";
const CAPTURE_ROOT: &str = "ORNA_TEST_CAPTURE_ROOT";
const CAPTURE_SOURCE: &str = "ORNA_TEST_CAPTURE_SOURCE";
const CAPTURE_RELATION: &str = "ORNA_TEST_CAPTURE_RELATION";
const CAPTURE_SCHEMA_ROOT: &str = "ORNA_TEST_CAPTURE_SCHEMA_ROOT";
const CAPTURE_STORE_ROOT: &str = "ORNA_TEST_CAPTURE_STORE_ROOT";

async fn commit_capture(
    repository: &Repository,
    source_commit: &orna_repository_v1::GitCommitRef,
    state: &RuntimeState,
    writer: WriterLease,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    media_root: &Path,
    insert_only: bool,
    owner_id: [u8; 16],
    request: RequestIdentity,
    relation_id: [u8; 16],
    format_context: &orna_value_v1::Format3Context,
) {
    use orna_project_v1::{ProjectLoader, admit_source_row_with_owner_fields};

    // Format 3 keeps rows only in `.orna/store` (ORNA-REPO-004): the row
    // source is the fixture document, never a committed file.
    let logical_path = "Object/image.orna";
    let row_source = object_source_row(media_root, "image");
    let loader = ProjectLoader::default();
    let project = loader.load_committed_snapshot(repository, source_commit).unwrap();
    let schema = loader
        .resolve_committed_source_table_schema(repository, source_commit, TableObjectId::new(relation_id))
        .unwrap();
    let value = evaluate_expression_ovb2_with_effects(
        expression, &Default::default(), Limits::default(), bindings,
    ).unwrap();
    let blob_binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    let admitted = admit_source_row_with_owner_fields(
        &project,
        logical_path,
        &row_source,
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
    drop(source_bindings);
}

/// A crash child that is SIGKILLed and reaped when dropped, so a failed
/// assertion mid-matrix never leaks a parked publisher or its runtime locks.
struct ChildGuard(Child);

impl Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn cli(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command.current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        command.env_remove(name);
    }
    command.args(arguments).output().expect("CLI process starts")
}

fn launch(directory: &Path, boundary: &str, crash_dir: &Path) -> ChildGuard {
    launch_with_arguments(directory, boundary, crash_dir, &["publish"])
}

fn launch_with_arguments(directory: &Path, boundary: &str, crash_dir: &Path, arguments: &[&str]) -> ChildGuard {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command.current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env(CRASH_AT, boundary)
        .env(CRASH_DIR, crash_dir);
    command.args(arguments);
    ChildGuard(command.spawn().expect("publication child starts"))
}

fn launch_capture_child(
    repository_root: &Path,
    source: &Path,
    relation_id: [u8; 16],
    schema_root: &str,
    store_root: &str,
    boundary: &str,
    crash_dir: &Path,
) -> ChildGuard {
    let mut command = Command::new(std::env::current_exe().expect("test executable path"));
    command
        .current_dir(repository_root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env(CRASH_AT, boundary)
        .env(CAPTURE_ROOT, repository_root)
        .env(CAPTURE_SOURCE, source)
        .env(CAPTURE_RELATION, hex(&relation_id))
        .env(CAPTURE_SCHEMA_ROOT, schema_root)
        .env(CAPTURE_STORE_ROOT, store_root)
        .env(CRASH_DIR, crash_dir);
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        command.env_remove(name);
    }
    command.args(["--exact", "capture_crash_child", "--nocapture"]);
    ChildGuard(command.spawn().expect("capture crash child starts"))
}

#[tokio::test]
async fn capture_crash_child() {
    let Ok(boundary) = std::env::var(CRASH_AT) else { return; };
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
        "capture-read" | "pack-install" => {
            let _candidate = capability
                .capture_blob_candidate(File::open(&source_path).unwrap(), 1 << 20)
                .unwrap();
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
            let fixture = REIMPORT_FIXTURE;
            let root = format!("{:?}", source_root.to_string_lossy().as_ref());
            let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root);
            commit_capture(
                &repository,
                &base,
                &state,
                writer,
                &mut bindings,
                &expression,
                source_root,
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
    let (directory, repository, relation_id, format_context) = object_format3_repository();
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
    let fixture = REIMPORT_FIXTURE;
    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root);
    commit_capture(
        &repository,
        &base,
        &state,
        writer,
        &mut bindings,
        &expression,
        source.path(),
        true,
        [33; 16],
        RequestIdentity { session_id: [21; 16], request_id: [22; 16] },
        relation_id,
        &format_context,
    )
    .await;
    (directory, source, repository, state, base, relation_id, format_context)
}

#[tokio::test]
async fn format3_publication_recovers_when_gc_prunes_candidate_before_publish() {
    let (directory, _source, repository, state, old_head, _relation_id, _format_context) =
        seeded_project().await;
    let root = directory.path();
    let checkpoint = state.latest_checkpoint().await.unwrap().unwrap();
    let freeze = state.freeze([0x44; 16], &checkpoint).await.unwrap();
    let intent_id = freeze.intent_id;
    let expected_index = repository.index_generation().unwrap();
    let mut coordinator = RuntimePublicationCoordinator::prepare_allow_noop(
        &repository,
        &state,
        &old_head,
        expected_index,
        &freeze,
        |_| panic!("Format-3 store-only publication must not request a legacy path"),
        "test format-3 publication",
    )
    .await
    .unwrap()
    .expect("the seeded store-only mutation needs publication");
    let candidate_commit = coordinator.candidate().commit().clone();
    repository
        .write_publication_journal(coordinator.journal())
        .unwrap();

    git(root, &["gc", "--prune=now", "--quiet"]);
    let candidate_status = Command::new("git")
        .args(["cat-file", "-e", candidate_commit.as_str()])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        !candidate_status.status.success(),
        "candidate commit remained reachable after git gc"
    );
    assert!(
        coordinator.publish(&repository).is_err(),
        "publication unexpectedly succeeded with its candidate pruned"
    );
    drop(coordinator);
    drop(state);

    let (identity, digest) = cli_runtime_identity(&repository);
    let recovered = RuntimeState::open(&repository, identity, digest).await.unwrap();
    assert!(
        RuntimePublicationCoordinator::recover(&repository, &recovered)
            .await
            .unwrap()
            .is_some(),
        "recovery must clear the missing prepared candidate"
    );
    assert!(repository.read_publication_journal().unwrap().is_none());
    assert_eq!(repository.head().unwrap(), Some(old_head.clone()));
    assert!(
        recovered.pending_count().await.unwrap() > 0,
        "candidate loss must preserve the frozen runtime mutations"
    );

    let retry_freeze = recovered.publication_freeze(intent_id).await.unwrap();
    let retry_index = repository.index_generation().unwrap();
    let mut retry = RuntimePublicationCoordinator::prepare_allow_noop(
        &repository,
        &recovered,
        &old_head,
        retry_index,
        &retry_freeze,
        |_| panic!("Format-3 store-only retry must not request a legacy path"),
        "retry format-3 publication",
    )
    .await
    .unwrap()
    .expect("the unconsumed store-only mutation must be rebuilt");
    let retry_commit = retry.candidate().commit().clone();
    retry.publish(&repository).unwrap();
    retry.complete(&repository, &recovered, &retry_freeze).await.unwrap();
    assert_eq!(repository.head().unwrap(), Some(retry_commit));
    assert!(repository.read_publication_journal().unwrap().is_none());
    assert_eq!(recovered.pending_count().await.unwrap(), 0);
}

/// ORNA-GIT-010: a Prepared journal is a recovery authority for the candidate commit and the base
/// index tree it names. The publisher is SIGKILLed after the journal is durable and before the ref
/// moves, so neither object is reachable from any ref. Coordinated `git gc --prune=now` and
/// `git repack -a -d` run under the Orna coordination lock. Both journal objects must survive, and
/// recovery must complete from the journal without pruning them. HEAD never moved, so recovery
/// discards the pre-ref journal and republishes a fresh candidate; it does not reuse the candidate.
#[tokio::test]
async fn cli_publication_prepared_journal_objects_survive_coordinated_gc() {
    let (directory, _source, repository, state, _base, _relation_id, _format_context) =
        seeded_project().await;
    let root = directory.path();
    std::fs::write(root.join("unrelated-tracked.txt"), b"committed baseline\n").unwrap();
    git(root, &["add", "--", "unrelated-tracked.txt"]);
    git(root, &["commit", "--quiet", "-m", "add unrelated tracked file"]);
    let base = repository.head().unwrap().unwrap();
    // A staged change makes the captured base index tree differ from HEAD's tree, so that tree is
    // referenced only by the journal.
    std::fs::write(root.join("unrelated-staged.txt"), b"staged before crash\n").unwrap();
    git(root, &["add", "--", "unrelated-staged.txt"]);

    let crash = TempDir::new().unwrap();
    let marker = crash.path().join(MARKER_FILE);
    let mut child = launch(root, "prepared", crash.path());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists(), "child never reached prepared; status: {:?}", child.try_wait().unwrap());
    child.kill().expect("SIGKILL publication child");
    let status = child.wait().expect("wait for killed child");
    assert!(!status.success(), "child unexpectedly returned at prepared");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "prepared");
    std::fs::remove_file(&marker).unwrap();
    drop(state);

    let journal = repository
        .read_publication_journal()
        .unwrap()
        .expect("the Prepared journal is durable before the kill");
    let candidate = journal.new_head().as_str().to_owned();
    let base_tree = journal
        .base_index_tree()
        .expect("publication journal names the base index tree")
        .as_str()
        .to_owned();
    assert_eq!(head(&repository), base.as_str(), "ref moved before the kill");
    assert_ne!(candidate, base.as_str());

    repository
        .with_coordination_lock(|| {
            git(root, &["gc", "--prune=now", "--quiet"]);
            git(root, &["repack", "-a", "-d", "--quiet"]);
            Ok::<(), orna_repository_v1::RepositoryError>(())
        })
        .expect("coordination lock is free once the publisher is killed");
    assert!(object_exists(root, &candidate), "gc pruned the journal's candidate commit");
    assert!(object_exists(root, &base_tree), "gc pruned the journal's base index tree");

    let resumed = cli(root, &["--debug", "publish"]);
    assert!(
        resumed.status.success(),
        "recovery after gc failed: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert!(repository.read_publication_journal().unwrap().is_none(), "journal left after recovery");
    assert_ne!(head(&repository), base.as_str(), "batch was not republished after recovery");
    assert!(publication_pin_refs(root).is_empty(), "publication pins remain after recovery");
}

/// Recovery row "Ref still at H" (ORNA-PUB-006): a prepared journal whose
/// candidate `HEAD` never reached names nothing that was published.
/// Coordinator recovery discards it and keeps the frozen batch pending, so
/// the journal cannot fence the next publication.
#[tokio::test]
async fn coordinator_recovery_discards_a_pre_ref_journal_and_keeps_the_batch_pending() {
    let (_directory, _source, repository, state, head, _relation_id, _format_context) =
        seeded_project().await;
    let checkpoint = state.latest_checkpoint().await.unwrap().unwrap();
    let freeze = state.freeze([0x45; 16], &checkpoint).await.unwrap();
    let index = repository.index_generation().unwrap();
    let coordinator = RuntimePublicationCoordinator::prepare_allow_noop(
        &repository,
        &state,
        &head,
        index.clone(),
        &freeze,
        |_| panic!("Format-3 store-only publication must not request a legacy path"),
        "orna: publish runtime data",
    )
    .await
    .unwrap()
    .expect("the seeded store-only mutation needs publication");
    repository
        .write_publication_journal(coordinator.journal())
        .unwrap();
    let pending = state.pending().await.unwrap();

    assert_eq!(
        RuntimePublicationCoordinator::recover(&repository, &state).await,
        Ok(Some(index.clone()))
    );
    assert_eq!(repository.head().unwrap(), Some(head));
    assert_eq!(repository.index_generation().unwrap(), index);
    assert_eq!(state.pending().await.unwrap(), pending);
    assert_eq!(repository.read_publication_journal().unwrap(), None);
}

/// A journal whose entries do not match the candidate it names is not that
/// candidate's evidence. Coordinator recovery refuses it and leaves the ref,
/// the index, the journal and the pending batch as they were.
#[tokio::test]
async fn coordinator_recovery_preserves_a_candidate_byte_binding_failure() {
    let (_directory, _source, repository, state, head, _relation_id, _format_context) =
        seeded_project().await;
    let checkpoint = state.latest_checkpoint().await.unwrap().unwrap();
    let freeze = state.freeze([0x46; 16], &checkpoint).await.unwrap();
    let index = repository.index_generation().unwrap();
    let coordinator = RuntimePublicationCoordinator::prepare_allow_noop(
        &repository,
        &state,
        &head,
        index.clone(),
        &freeze,
        |_| panic!("Format-3 store-only publication must not request a legacy path"),
        "orna: publish runtime data",
    )
    .await
    .unwrap()
    .expect("the seeded store-only mutation needs publication");
    let candidate = coordinator.candidate().commit().clone();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.clone(),
        index.tree().unwrap().clone(),
        freeze.intent_id,
        vec![orna_repository_v1::PublicationJournalEntry::new(
            orna_repository_v1::ManagedPath::new("Object/forged.orna").unwrap(),
            None,
            Some(b"journal row".to_vec()),
        )],
    )
    .unwrap();
    repository.write_publication_journal(&journal).unwrap();
    repository
        .advance_current_ref(&head, coordinator.candidate())
        .unwrap();
    let pending = state.pending().await.unwrap();

    assert_eq!(
        RuntimePublicationCoordinator::recover(&repository, &state).await,
        Err(orna_storage_v1::Error::InvalidPublicationJournal)
    );
    assert_eq!(repository.head().unwrap(), Some(candidate));
    assert_eq!(repository.index_generation().unwrap().tree(), index.tree());
    assert_eq!(state.pending().await.unwrap(), pending);
    assert_eq!(
        repository.read_publication_journal().unwrap(),
        Some(journal)
    );
}

/// Coordinated GC: `git gc --prune=now` may discard only objects that no ref names. Objects named by the
/// protected `refs/orna/pins/*` refs survive; an unreferenced object is removed. This is coordinated GC
/// only and does not claim that an uncoordinated prune is safe (ORNA-GIT-010).
#[tokio::test]
async fn coordinated_gc_prune_keeps_pinned_objects_and_removes_unreferenced_ones() {
    let (directory, _source, _repository, _state, _base, _relation_id, _format_context) =
        seeded_project().await;
    let root = directory.path();
    let pins = git_output(root, &["for-each-ref", "--format=%(objectname)", "refs/orna/pins"], None);
    let pinned = String::from_utf8_lossy(&pins)
        .lines()
        .next()
        .expect("seeded project leaves a protected pin")
        .to_owned();
    let probe = git_output(root, &["hash-object", "-w", "--stdin"], Some(b"orna gc prune probe"));
    let unreferenced = String::from_utf8(probe).unwrap().trim().to_owned();
    assert!(object_exists(root, &pinned), "pinned object missing before gc");
    assert!(object_exists(root, &unreferenced), "probe object missing before gc");

    git(root, &["gc", "--prune=now", "--quiet"]);

    assert!(object_exists(root, &pinned), "gc pruned an object named by a protected pin");
    assert!(!object_exists(root, &unreferenced), "gc kept an unreferenced object");
}

fn object_exists(root: &Path, object: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", object])
        .current_dir(root)
        .status()
        .unwrap()
        .success()
}

fn head(repository: &Repository) -> String {
    repository.head().unwrap().unwrap().as_str().to_owned()
}

/// Every publication pin ref, in `for-each-ref` order (ORNA-GIT-010).
fn publication_pin_refs(root: &Path) -> Vec<String> {
    let listing = git_output(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/orna/publication-indexes/",
            "refs/orna/publication-candidates/",
        ],
        None,
    );
    String::from_utf8_lossy(&listing).lines().map(str::to_owned).collect()
}

/// Pre-Prepared pins belong to this attempt alone: an index pin names its base index tree, and a
/// candidate pin names a commit whose peeled object is itself and whose parent is the base head.
fn assert_attempt_pins(root: &Path, base_head: &str, base_tree: &str, boundary: &str) {
    for reference in publication_pin_refs(root) {
        if let Some(tree) = reference.strip_prefix("refs/orna/publication-indexes/") {
            assert_eq!(tree, base_tree, "index pin {reference} is not this attempt's base tree at {boundary}");
        } else if let Some(commit) = reference.strip_prefix("refs/orna/publication-candidates/") {
            let peeled = git_output(root, &["rev-parse", "--verify", &format!("{commit}^{{commit}}")], None);
            assert_eq!(String::from_utf8_lossy(&peeled).trim(), commit, "candidate pin {reference} is not a commit at {boundary}");
            let parent = git_output(root, &["rev-parse", "--verify", &format!("{commit}^")], None);
            assert_eq!(String::from_utf8_lossy(&parent).trim(), base_head, "candidate pin {reference} does not extend the base head at {boundary}");
        } else {
            panic!("unexpected publication pin {reference} at {boundary}");
        }
    }
}

#[tokio::test]
async fn cli_publication_reopens_after_process_kill_at_each_reachable_boundary() {
    // ORNA-PUB-018: pending pins stay protected through publication/recovery.
    publish_kill_matrix(|_, _| {}).await;
}

/// Gate D (ACCEPTANCE.md): coordinated GC/repack inside the kill window. After the
/// publisher is SIGKILLed at each reachable boundary and before recovery runs,
/// `git gc --prune=now` and `git repack -a -d` execute while holding the Orna
/// coordination lock, the declared maintenance boundary (ORNA-CAP-006). Pinned,
/// staged and unstaged content must survive and recovery must still publish. This
/// is coordinated GC only; it claims nothing for uncoordinated `git prune --expire
/// now` (ORNA-GIT-010).
#[tokio::test]
async fn cli_publication_reopens_after_coordinated_gc_and_repack_in_kill_window() {
    publish_kill_matrix(coordinated_gc_and_repack).await;
}

fn coordinated_gc_and_repack(repository: &Repository, root: &Path) {
    repository
        .with_coordination_lock(|| {
            git(root, &["gc", "--prune=now", "--quiet"]);
            git(root, &["repack", "-a", "-d", "--quiet"]);
            Ok::<(), orna_repository_v1::RepositoryError>(())
        })
        .expect("coordination lock is free once the publisher is killed");
}

// The repository observer fires after each ref/index stage is durable;
// ref-index remains the existing boundary after both stages complete.
// index-install parks after the reconciled index is installed but before
// its IndexReconciled journal stage is durable (ORNA-PUB-007).
async fn publish_kill_matrix(between_kill_and_recovery: fn(&Repository, &Path)) {
    for boundary in ["index-lock", "capture", "row", "candidate", "ref-cas", "index-install", "index", "ref-index", "tail"] {
        let (directory, _source, repository, state, _base, _relation_id, _format_context) =
            seeded_project().await;
        let root = directory.path();
        std::fs::write(root.join("unrelated-tracked.txt"), b"committed baseline\n").unwrap();
        git(root, &["add", "--", "unrelated-tracked.txt"]);
        git(root, &["commit", "--quiet", "-m", "add unrelated tracked file"]);
        let base = repository.head().unwrap().unwrap();
        std::fs::write(root.join("unrelated-staged.txt"), b"staged before crash\n").unwrap();
        git(root, &["add", "--", "unrelated-staged.txt"]);
        std::fs::write(root.join("unrelated-tracked.txt"), b"unstaged before crash\n").unwrap();
        let staged_before = git_output(root, &["diff", "--cached", "--binary"], None);
        let unstaged_before = git_output(root, &["diff", "--binary"], None);
        assert!(!staged_before.is_empty() && !unstaged_before.is_empty());
        // Pin refs that exist before the publisher child starts. The fixture's
        // accepted pending pin must survive the kill and recovery (ORNA-PUB-018).
        let seeded_pins = git_output(root, &["for-each-ref", "--format=%(refname)", "refs/orna/pins"], None);
        assert_eq!(String::from_utf8_lossy(&seeded_pins).lines().count(), 1, "fixture pin set changed before {boundary}");
        let seeded_lease = state
            .current_lease()
            .await
            .unwrap()
            .expect("seeded project leaves a writer lease");

        let crash = TempDir::new().unwrap();
        let marker = crash.path().join(MARKER_FILE);
        let base_tree = String::from_utf8_lossy(&git_output(root, &["write-tree"], None)).trim().to_owned();
        let mut child = launch(root, boundary, crash.path());
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
        let pre_journal = repository.read_publication_journal().unwrap().is_some();
        let pre_pins = git_output(root, &["for-each-ref", "--format=%(refname)", "refs/orna/pins"], None);
        assert_eq!(pre_pins, seeded_pins, "killed child changed pin refs at {boundary}");
        assert!(pre_journal == matches!(boundary, "ref-cas" | "index-install" | "index" | "ref-index"), "unexpected journal state at {boundary}");
        // Before the Prepared journal, every publication pin an attempt holds must be its own base
        // index tree or its own candidate commit (parent = base head). Anything else is foreign.
        if matches!(boundary, "index-lock" | "capture" | "row" | "candidate") {
            assert_attempt_pins(root, base.as_str(), &base_tree, boundary);
        }
        let pins_before = publication_pin_refs(root);
        if let Some(journal) = repository.read_publication_journal().unwrap() {
            let mut expected = vec![format!("refs/orna/publication-candidates/{}", journal.new_head().as_str())];
            if let Some(tree) = journal.base_index_tree() {
                expected.push(format!("refs/orna/publication-indexes/{}", tree.as_str()));
            }
            expected.sort();
            assert_eq!(pins_before, expected, "journal pins mismatch at {boundary}");
        }
        // A SIGKILL between adoption and the Prepared write leaves orphan pins with no
        // journal. Recovery sweeps them (asserted after recovery below).
        drop(state);
        between_kill_and_recovery(&repository, root);

        let resumed = cli(root, &["--debug", "publish"]);
        assert!(resumed.status.success(), "recovery at {boundary} failed: {}", String::from_utf8_lossy(&resumed.stderr));
        let once = head(&repository);
        assert_ne!(once, base.as_str(), "candidate not retained after {boundary}");
        assert!(repository.read_publication_journal().unwrap().is_none(), "journal left after {boundary}");
        assert!(
            !root.join(".git/index.lock").exists(),
            "index lock stranded after {boundary}"
        );
        assert_eq!(git_output(root, &["diff", "--cached", "--binary"], None), staged_before, "staged work lost after {boundary}");
        assert_eq!(git_output(root, &["diff", "--binary"], None), unstaged_before, "unstaged work lost after {boundary}");
        let fsck = Command::new("git").args(["fsck", "--no-dangling", "--no-progress"]).current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap();
        assert!(fsck.status.success(), "fsck after {boundary}: {}", String::from_utf8_lossy(&fsck.stderr));
        // ORNA-PUB-018: pending pins stay protected through publication/recovery.
        let post_pins = git_output(root, &["for-each-ref", "--format=%(refname)", "refs/orna/pins"], None);
        assert_eq!(post_pins, seeded_pins, "accepted pending pin not retained through recovery at {boundary}");
        assert!(publication_pin_refs(root).is_empty(), "publication pins remain after recovery at {boundary}");
        let (identity, digest) = cli_runtime_identity(&repository);
        let reopened = RuntimeState::open(&repository, identity, digest)
            .await
            .unwrap_or_else(|error| panic!("runtime reopen after {boundary}: {error:?}"));
        assert_eq!(reopened.pending_count().await.unwrap(), 0, "pending range remains after {boundary}");
        let second = cli(root, &["publish"]);
        assert!(second.status.success(), "second publish after {boundary} failed: {}", String::from_utf8_lossy(&second.stderr));
        assert_eq!(head(&repository), once, "publication repeated after {boundary}");
        assert!(String::from_utf8_lossy(&second.stdout).contains("nothing to publish"));
        assert!(publication_pin_refs(root).is_empty(), "publication pins remain after second publish at {boundary}");
        // Lock and lease-release evidence only. This block does NOT prove Gate D P5 ("no abandoned
        // owner retains write permission"): no token is rejected here, and the publisher never held
        // the seeded writer lease. P5 stays open until production fencing lands (W76 F1; beads
        // .127 and .220). `orna publish` acquires no writer lease, so the seeded lease must be
        // untouched by the killed publish or its recovery, and the publisher's own runtime-owner
        // and coordination locks must be released and acquirable afterwards.
        assert_eq!(
            reopened.current_lease().await.unwrap(),
            Some(seeded_lease),
            "publication changed the writer lease at {boundary}"
        );
        assert!(
            reopened.holds_runtime_owner_lock(),
            "runtime owner lock still held by the killed publisher at {boundary}"
        );
        repository
            .with_coordination_lock(|| Ok::<(), orna_repository_v1::RepositoryError>(()))
            .unwrap_or_else(|error| panic!("coordination lock still held after {boundary}: {error:?}"));
        println!(
            "{boundary}: SIGKILL recovery_publish_exit={}, second_publish_exit={}",
            resumed.status.code().unwrap(),
            second.status.code().unwrap()
        );
    }
}

/// Family B: an external edit to a tracked worktree file lands inside the kill window at the
/// `index` boundary, after the publisher is SIGKILLed and before recovery runs. Recovery must
/// leave that edit intact and publication must still succeed.
#[tokio::test]
async fn cli_publication_reopens_after_external_edit_in_kill_window() {
    let (directory, _source, repository, state, _base, _relation_id, _format_context) =
        seeded_project().await;
    let root = directory.path();
    std::fs::write(root.join("unrelated-tracked.txt"), b"committed baseline\n").unwrap();
    git(root, &["add", "--", "unrelated-tracked.txt"]);
    git(
        root,
        &["commit", "--quiet", "-m", "add unrelated tracked file"],
    );
    let base = repository.head().unwrap().unwrap();

    let crash = TempDir::new().unwrap();
    let marker = crash.path().join(MARKER_FILE);
    let mut child = launch(root, "index", crash.path());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "child never reached index; status: {:?}",
        child.try_wait().unwrap()
    );
    child.kill().expect("SIGKILL publication child");
    let status = child.wait().expect("wait for killed child");
    assert!(!status.success(), "child unexpectedly returned at index");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "index");
    std::fs::remove_file(&marker).unwrap();
    drop(state);

    let external = b"external edit during kill window\n";
    std::fs::write(root.join("unrelated-tracked.txt"), external).unwrap();

    let resumed = cli(root, &["--debug", "publish"]);
    assert!(
        resumed.status.success(),
        "recovery after external edit failed (exit {:?}): {}",
        resumed.status.code(),
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_ne!(
        head(&repository),
        base.as_str(),
        "candidate not published after external edit"
    );
    assert_eq!(
        std::fs::read(root.join("unrelated-tracked.txt")).unwrap(),
        external,
        "external worktree edit lost during recovery"
    );
    assert!(
        repository.read_publication_journal().unwrap().is_none(),
        "journal left after external edit"
    );
    assert!(
        !root.join(".git/index.lock").exists(),
        "index lock stranded after external edit"
    );
    let fsck = Command::new("git")
        .args(["fsck", "--no-dangling", "--no-progress"])
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        fsck.status.success(),
        "fsck after external edit: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
    let second = cli(root, &["publish"]);
    assert!(
        second.status.success(),
        "second publish after external edit failed (exit {:?}): {}",
        second.status.code(),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("nothing to publish"));
}

/// W14-01 / ORNA-PUB-016: a `commit` killed after the Orna index lock is taken
/// must not strand Git's index lock. The next commit reclaims the dead lock,
/// recovers, and then succeeds. The index-install (ref advanced, IndexReconciled
/// not durable) commit kill is NOT covered here; see the residual risk notes.
#[tokio::test]
async fn cli_commit_reopens_after_process_kill_at_index_lock() {
    for boundary in ["index-lock"] {
        let (directory, _source, repository, state, _base, _relation_id, _format_context) =
            seeded_project().await;
        let root = directory.path();
        std::fs::write(root.join("unrelated-tracked.txt"), b"committed baseline\n").unwrap();
        git(root, &["add", "--", "unrelated-tracked.txt"]);
        git(root, &["commit", "--quiet", "-m", "add unrelated tracked file"]);
        let base = repository.head().unwrap().unwrap();
        std::fs::write(root.join("unrelated-staged.txt"), b"staged before crash\n").unwrap();
        git(root, &["add", "--", "unrelated-staged.txt"]);
        std::fs::write(root.join("unrelated-tracked.txt"), b"unstaged before crash\n").unwrap();
        let staged_before = git_output(root, &["diff", "--cached", "--binary"], None);
        assert!(!staged_before.is_empty());
        let unstaged_before = git_output(root, &["diff", "--binary"], None);
        assert!(!unstaged_before.is_empty());
        let seeded_pins = git_output(root, &["for-each-ref", "--format=%(refname)", "refs/orna/pins"], None);
        drop(state);

        let crash = TempDir::new().unwrap();
        let marker = crash.path().join(MARKER_FILE);
        let mut child = launch_with_arguments(root, boundary, crash.path(), &["commit", "-m", "crashed commit"]);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists(), "commit child never reached {boundary}; status: {:?}", child.try_wait().unwrap());
        child.kill().expect("SIGKILL commit child");
        let status = child.wait().expect("wait for killed commit child");
        assert!(!status.success(), "commit child unexpectedly returned at {boundary}");
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), boundary);
        std::fs::remove_file(&marker).unwrap();
        assert!(root.join(".git/index.lock").exists(), "{boundary} must strand the Orna index lock for recovery to reclaim");
        assert_eq!(git_output(root, &["for-each-ref", "--format=%(refname)", "refs/orna/pins"], None), seeded_pins);

        // Git stays blocked by the stranded Orna lock until Orna recovers it.
        // A stranded Orna index lock is recovered by the next commit.
        let resumed = cli(root, &["commit", "-m", "recovered commit"]);
        assert!(
            resumed.status.success(),
            "commit after {boundary} failed: {}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        assert!(!root.join(".git/index.lock").exists(), "index lock left after {boundary}");
        assert_ne!(head(&repository), base.as_str(), "no commit advanced after {boundary}");
        if boundary == "index-lock" {
            let subject = git_output(root, &["log", "-1", "--format=%s"], None);
            assert_eq!(String::from_utf8_lossy(&subject).trim(), "recovered commit", "commit subject after {boundary}");
        }
        assert!(repository.read_publication_journal().unwrap().is_none(), "journal left after {boundary}");
        assert_eq!(git_output(root, &["diff", "--binary"], None), unstaged_before, "unstaged work lost after {boundary}");
        // `commit` consumes the staged index, so the staged path is now in HEAD
        // rather than still staged; the staged content must survive into HEAD.
        assert_eq!(git_output(root, &["show", "HEAD:unrelated-staged.txt"], None), b"staged before crash\n", "staged work lost after {boundary}");
        // Raw Git works again once Orna has recovered the lock.
        std::fs::write(root.join("after-crash.txt"), b"after crash\n").unwrap();
        git(root, &["add", "--", "after-crash.txt"]);
        let fsck = Command::new("git").args(["fsck", "--no-dangling", "--no-progress"]).current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap();
        assert!(fsck.status.success(), "fsck after {boundary}: {}", String::from_utf8_lossy(&fsck.stderr));
    }
}

/// ORNA-PUB-007: recovery after ref advancement completes index/worktree reconciliation and Turso cleanup.
/// ORNA-PUB-016: recovery retains distinct staged and unstaged changes across the crash boundary.
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

    let crash = TempDir::new().unwrap();
    let marker = crash.path().join(MARKER_FILE);
    let mut child = launch(root, "ref-cas", crash.path());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "child never reached ref-cas; status: {:?}",
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
    let fixture = REIMPORT_FIXTURE;
    let media_root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &media_root);
    commit_capture(
        &repository,
        &source_commit,
        &state,
        writer,
        &mut bindings,
        &expression,
        source.path(),
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
    assert!(!status.success(), "child unexpectedly returned at ref-cas");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "ref-cas");
    std::fs::remove_file(&marker).unwrap();
    drop(state);

    let (identity, digest) = cli_runtime_identity(&repository);
    let recovered = RuntimeState::open(&repository, identity, digest)
        .await
        .unwrap_or_else(|error| panic!("runtime reopen after ref-cas crash: {error:?}"));
    // PUB-3 steps 6 and 9 journal before advancing the ref; ORNA-PUB-007,
    // ORNA-PUB-008, and ORNA-PUB-016 require recovery to finish reconciliation,
    // consume only the frozen range, and preserve unrelated index/worktree edits.
    // The ref-cas hook is the first crash boundary with a durable journal.
    assert!(repository.read_publication_journal().unwrap().is_some());
    assert_ne!(head(&repository), base.as_str(), "ref-cas crash did not advance HEAD");
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
    let store_path = root.join(".orna/store/data");
    let mut edited_store = std::fs::read(&store_path).expect("recovered store data exists");
    assert!(!edited_store.is_empty(), "the recovered store data has bytes to edit");
    edited_store[0] ^= 0x01;
    std::fs::write(&store_path, &edited_store).unwrap();
    let store_status_before_publish =
        git_output(root, &["status", "--short", "--", ".orna/store/data"], None);

    let attempted_tail = cli(root, &["publish"]);
    let store_status_after_publish =
        git_output(root, &["status", "--short", "--", ".orna/store/data"], None);
    assert!(
        String::from_utf8_lossy(&store_status_before_publish).contains(".orna/store/data")
            || String::from_utf8_lossy(&store_status_after_publish).contains(".orna/store/data"),
        "a user edit under .orna/store remains visible to Git after recovery and publish"
    );
    assert_eq!(
        std::fs::read(&store_path).unwrap(),
        edited_store,
        "later publish replaced a user edit under .orna/store"
    );
    if !attempted_tail.status.success() {
        assert_eq!(
            head(&repository),
            recovered_head,
            "a detected .orna/store edit must not advance HEAD"
        );
    }
    let indexed_store = git_output(root, &["show", ":.orna/store/data"], None);
    std::fs::write(&store_path, indexed_store).unwrap();

    drop(attempted_tail);
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
    for boundary in ["capture-read", "pack-install", "pin", "row-commit"] {
        let source = TempDir::new().unwrap();
        std::fs::copy(Path::new(MEDIA_FIXTURES).join("pixel.png"), source.path().join("pixel.png")).unwrap();
        let payload = vec![0x6d; 128 * 1024];
        let source_path = source.path().join("capture.bin");
        std::fs::write(&source_path, &payload).unwrap();
        let (directory, repository, relation_id, format_context) = object_format3_repository();
        let root = directory.path();
        std::fs::write(root.join("unrelated-tracked.txt"), b"committed baseline\n").unwrap();
        git(root, &["add", "--", "unrelated-tracked.txt"]);
        git(root, &["commit", "--quiet", "-m", "add unrelated tracked file"]);
        let base = head(&repository);
        std::fs::write(root.join("unrelated-staged.txt"), b"staged before crash\n").unwrap();
        git(root, &["add", "--", "unrelated-staged.txt"]);
        std::fs::write(root.join("unrelated-tracked.txt"), b"unstaged before crash\n").unwrap();
        let staged_before = git_output(root, &["diff", "--cached", "--binary"], None);
        let unstaged_before = git_output(root, &["diff", "--binary"], None);
        assert!(!staged_before.is_empty(), "fixture has staged unrelated work");
        assert!(!unstaged_before.is_empty(), "fixture has unstaged unrelated work");
        let schema_root = hex(format_context.schema_root().as_bytes());
        let store_root = hex(format_context.store_root().as_bytes());
        // The killed publisher runs under this owner; the fence at the end takes over this exact lease.
        let (identity, digest) = cli_runtime_identity(&repository);
        let state = RuntimeState::open(&repository, identity, digest).await.unwrap();
        state.acquire_lease(identity.repository_id).await.unwrap();
        let abandoned = state
            .current_lease()
            .await
            .unwrap()
            .expect("the fixture leaves a writer lease for the abandoned owner");
        assert_eq!(abandoned.owner_id, identity.repository_id);
        drop(state);
        let crash = TempDir::new().unwrap();
        let marker = crash.path().join(MARKER_FILE);
        let mut child = launch_capture_child(
            directory.path(),
            &source_path,
            relation_id,
            &schema_root,
            &store_root,
            boundary,
            crash.path(),
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
        assert_eq!(head(&repository), base, "killed capture child moved HEAD at {boundary}");
        assert!(committed_store_rows(&repository, relation_id).is_empty(), "killed capture child published a row at {boundary}");
        assert_eq!(git_output(root, &["diff", "--cached", "--binary"], None), staged_before, "staged work changed by kill at {boundary}");
        assert_eq!(git_output(root, &["diff", "--binary"], None), unstaged_before, "unstaged work changed by kill at {boundary}");

        let resumed = cli(directory.path(), &["publish"]);
        assert!(resumed.status.success(), "recovery at {boundary} failed: {}", String::from_utf8_lossy(&resumed.stderr));
        let once = head(&repository);
        assert!(repository.read_publication_journal().unwrap().is_none(), "journal left after {boundary}");
        assert_eq!(git_output(root, &["diff", "--cached", "--binary"], None), staged_before, "staged work lost after {boundary}");
        assert_eq!(git_output(root, &["diff", "--binary"], None), unstaged_before, "unstaged work lost after {boundary}");
        git_output(root, &["fsck", "--no-dangling", "--no-progress"], None);
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
            assert!(committed_store_rows(&repository, relation_id).is_empty(), "uncommitted capture published a row after {boundary}");
        }
        let second = cli(directory.path(), &["publish"]);
        assert!(second.status.success(), "second publish after {boundary} failed: {}", String::from_utf8_lossy(&second.stderr));
        assert_eq!(head(&repository), once, "publication repeated after {boundary}");
        assert!(String::from_utf8_lossy(&second.stdout).contains("nothing to publish"));
        let (identity, digest) = cli_runtime_identity(&repository);
        let reopened = RuntimeState::open(&repository, identity, digest)
            .await
            .unwrap_or_else(|error| panic!("runtime reopen after {boundary}: {error:?}"));
        assert_eq!(reopened.pending_count().await.unwrap(), 0, "pending range remains after {boundary}");
        // Takeover runs last: it mutates the lease and may set takeover_recovery_pending.
        let fresh = [0x5a; 16];
        let replacement = reopened.takeover_lease(abandoned, fresh).await.expect("takeover of abandoned lease");
        assert_eq!(replacement.owner_id, fresh);
        assert_eq!(replacement.epoch, abandoned.epoch + 1);
        assert!(matches!(
            reopened.takeover_lease(abandoned, [0x5b; 16]).await,
            Err(orna_runtime_v1::RuntimeError::OwnerLost)
        ), "abandoned lease took over twice after {boundary}");
        assert!(reopened.acquire_lease(abandoned.owner_id).await.is_err(), "abandoned owner regained writer lease after {boundary}");
        println!("{boundary}: recovery_publish_exit={}, second_publish_exit={}", resumed.status.code().unwrap(), second.status.code().unwrap());
    }
}
