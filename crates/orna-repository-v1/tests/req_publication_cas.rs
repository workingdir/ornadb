use std::{fs, path::Path, process::Command, sync::Barrier, thread};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
use orna_storage_v1::{Error, LoosePath, RuntimePublicationCoordinator};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "../../orna-cli-v1/tests/support/format3.rs"]
mod format3;
#[path = "../../orna-cli-v1/tests/support/media_commit.rs"]
mod media_commit;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../orna-cli-v1/tests/fixtures/media");
const IMPORT_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../orna-cli-v1/tests/fixtures/media/import-song.orna");

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn import_expression(root: &Path) -> String {
    let source = fs::read_to_string(IMPORT_FIXTURE).expect("read media import fixture");
    let root = format!("{:?}", root.to_string_lossy().as_ref());
    source.trim_end().replace("__MEDIA_ROOT__", &root)
}

#[tokio::test]
async fn publication_cas_race_has_one_winner_and_consistent_state() {
    // ORNA-PUB-009: concurrent publishers that prepared the same expected
    // head/index race the ref; exactly one advances it, the loser is refused
    // with the typed ref conflict (ORNA-PUB-006: no false success), and the
    // repository stays consistent.
    let (repository_dir, repository, relation_id) = format3::empty_format3_repository();
    // The winner's locked publish body runs on a loaded runner; a long bound
    // keeps the loser queued so the race outcome, not a timer, decides it.
    let repository = repository.with_coordination_lock_wait(std::time::Duration::from_secs(120));
    let media = TempDir::new().expect("temporary media directory");
    fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        media.path().join("tone.wav"),
    )
    .expect("copy media fixture");

    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("open runtime state");
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).expect("bounded filesystem provider");
    filesystem.allow_root(media.path()).expect("allow media fixture root");
    let capability = repository
        .capture_capability(relation_id)
        .expect("issue admitted capture capability");
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let key = "publication-cas-a";
    let writer = state
        .acquire_lease(identity.repository_id)
        .await
        .expect("acquire runtime writer");
    media_commit::commit_capture_pending(
        &repository,
        &state,
        writer,
        &mut bindings,
        &import_expression(media.path()),
        key,
        0x71,
        true,
        relation_id,
    )
    .await;
    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("read latest checkpoint")
        .expect("capture leaves a checkpoint");
    let freeze = state
        .freeze([0x81; 16], &checkpoint)
        .await
        .expect("freeze captured row prefix");

    let head = repository.head().expect("read head").expect("fixture has a head");
    let index = repository.index_generation().expect("read index generation");
    let prepared = || async {
        RuntimePublicationCoordinator::prepare(
            &repository,
            &state,
            &head,
            index.clone(),
            &freeze,
            |_| LoosePath::for_key("media", &[key.to_owned()]),
            "orna: publish format-3 row",
        )
        .await
        .expect("storage writer prepares format-3 publication")
    };
    let mut first = prepared().await;
    let mut second = prepared().await;

    // Two real threads release together at a barrier and race the ref.
    let barrier = Barrier::new(2);
    let (first_result, second_result) = thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            first.publish(&repository)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            second.publish(&repository)
        });
        (a.join().unwrap(), b.join().unwrap())
    });

    let (winner_result, loser_result) = match (first_result, second_result) {
        (Ok(index), Err(error)) => ((index, first), error),
        (Err(error), Ok(index)) => ((index, second), error),
        other => panic!("exactly one publisher must win, got {other:?}"),
    };
    let (winner_index, mut winner) = winner_result;
    assert_eq!(loser_result, Error::RefConflict);

    // HEAD, index and the committed store reflect exactly the winner.
    assert_eq!(
        repository.head().expect("read head after race"),
        Some(winner.candidate().commit().clone())
    );
    assert_eq!(
        repository.index_generation().expect("read index after race"),
        winner_index
    );
    let store_paths = git(repository_dir.path(), &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(store_paths.lines().any(|path| path.starts_with(".orna/store/")));
    assert_eq!(
        git(repository_dir.path(), &["status", "--porcelain", "--untracked-files=all"]),
        ""
    );

    winner
        .complete(&repository, &state, &freeze)
        .await
        .expect("complete winning publication");

    // A fresh captured row published from the new head succeeds.
    let key_b = "publication-cas-b";
    let writer = state
        .acquire_lease(identity.repository_id)
        .await
        .expect("acquire runtime writer for second capture");
    media_commit::commit_capture_pending(
        &repository,
        &state,
        writer,
        &mut bindings,
        &import_expression(media.path()),
        key_b,
        0x72,
        true,
        relation_id,
    )
    .await;
    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("read checkpoint for second capture")
        .expect("second capture leaves a checkpoint");
    let freeze_b = state
        .freeze([0x82; 16], &checkpoint)
        .await
        .expect("freeze second captured row prefix");
    let fresh_head = repository.head().expect("read fresh head").expect("head exists");
    let fresh_index = repository.index_generation().expect("read fresh index");
    let mut third = RuntimePublicationCoordinator::prepare(
        &repository,
        &state,
        &fresh_head,
        fresh_index,
        &freeze_b,
        |_| LoosePath::for_key("media", &[key_b.to_owned()]),
        "orna: publish format-3 row",
    )
    .await
    .expect("prepare publication from fresh head");
    third.publish(&repository).expect("fresh-head publication succeeds");
    third
        .complete(&repository, &state, &freeze_b)
        .await
        .expect("complete fresh-head publication");
    assert_eq!(
        repository.head().expect("read head after third"),
        Some(third.candidate().commit().clone())
    );
}
