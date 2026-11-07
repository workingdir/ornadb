//! Maintenance-lease liveness and publication pinning (ORNA-CAP-006).
//!
//! Capture and prepare hold a shared maintenance lease; a maintenance boundary
//! takes it exclusively. The lease is an advisory `flock`, so the kernel drops
//! it when the holder dies. The SIGKILL tests run a real holder in a child
//! process, kill it while it holds the lease, and check that the lease no
//! longer blocks. The in-process tests check that a prepared publication keeps
//! its lease until publish anchors the candidate, and that publish refuses a
//! candidate object that is no longer present (ORNA-GIT-010).
#![cfg(unix)]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::process::ExitStatusExt,
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
    time::Duration,
};

use orna_evaluator::SysHostBindingRegistry;
use orna_repository::{Repository, RepositoryError};
use orna_runtime::RuntimeState;
use orna_storage::{LoosePath, RuntimePublicationCoordinator};
use orna_sys::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "../../orna-cli/tests/support/format3.rs"]
mod format3;
#[path = "../../orna-cli/tests/support/media_commit.rs"]
mod media_commit;

const MEDIA_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../orna-cli/tests/fixtures/media"
);
const IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../orna-cli/tests/fixtures/media/import-song.orna"
);

/// Environment switches that tell the re-executed test binary which holder to
/// become. Unset means the child test is inert during a normal test run.
const HOLDER_ROLE: &str = "ORNA_MAINTENANCE_LEASE_HOLDER_ROLE";
const HOLDER_ROOT: &str = "ORNA_MAINTENANCE_LEASE_HOLDER_ROOT";
/// Printed by the holder once it owns its lease; the parent waits for it.
const HOLDER_READY: &str = "ORNA_MAINTENANCE_LEASE_HOLDER_READY";
/// Short bound so a busy check fails fast instead of waiting out the default.
const BUSY_WAIT: Duration = Duration::from_millis(200);
const SIGKILL: i32 = 9;

/// A child process that holds a maintenance lease. It is killed on drop, so a
/// failed assertion cannot leave a lease-holding process behind.
struct Holder {
    child: Option<Child>,
    _stdout: BufReader<ChildStdout>,
}

impl Holder {
    fn spawn(root: &Path, role: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("test executable path"))
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env(HOLDER_ROLE, role)
            .env(HOLDER_ROOT, root)
            .args(["--exact", "maintenance_holder_child", "--nocapture"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("maintenance holder child starts");
        let stdout = child.stdout.take().expect("piped holder stdout");
        let mut stdout = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            let read = stdout.read_line(&mut line).expect("read holder stdout");
            assert!(read != 0, "holder exited before it acquired its lease");
            if line.contains(HOLDER_READY) {
                break;
            }
        }
        Self {
            child: Some(child),
            _stdout: stdout,
        }
    }

    /// Kills the holder with SIGKILL and reaps it, so the kernel releases its
    /// `flock` before the caller checks the lease.
    fn kill_with_sigkill(&mut self) {
        let child = self.child.as_mut().expect("holder is still running");
        child.kill().expect("SIGKILL the holder");
        let status = child.wait().expect("reap the killed holder");
        assert_eq!(
            status.signal(),
            Some(SIGKILL),
            "the holder must die by SIGKILL, got {status}"
        );
        self.child = None;
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Child half of the SIGKILL tests. Inert unless a parent test spawned it with
/// `HOLDER_ROLE` set; then it takes the named lease and blocks until killed.
#[test]
fn maintenance_holder_child() {
    let Ok(role) = std::env::var(HOLDER_ROLE) else {
        return;
    };
    let root = std::env::var(HOLDER_ROOT).expect("holder repository root");
    let repository = Repository::discover(&root).expect("discover holder repository");
    match role.as_str() {
        "shared" => {
            let _lease = repository
                .hold_maintenance_lease()
                .expect("holder takes the shared lease");
            announce_and_block();
        }
        "exclusive" => {
            repository
                .with_maintenance_boundary(|| -> Result<(), RepositoryError> {
                    announce_and_block();
                    Ok(())
                })
                .expect("holder takes the exclusive boundary");
        }
        other => panic!("unknown holder role {other}"),
    }
}

fn announce_and_block() {
    println!("{HOLDER_READY}");
    std::io::stdout().flush().expect("flush holder readiness");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

#[test]
fn shared_lease_holder_killed_releases_boundary() {
    // A shared holder (capture or prepare) blocks the exclusive boundary; once
    // it is SIGKILLed the kernel drops its lease and the boundary proceeds.
    let (root, repository, _relation_id) = format3::empty_format3_repository();
    let repository = repository.with_coordination_lock_wait(BUSY_WAIT);
    let boundary = || repository.with_maintenance_boundary(|| Ok::<(), RepositoryError>(()));
    let mut holder = Holder::spawn(root.path(), "shared");
    assert!(
        matches!(boundary(), Err(RepositoryError::RepositoryBusy)),
        "a live shared holder must block the boundary"
    );
    holder.kill_with_sigkill();
    boundary().expect("a killed shared holder no longer blocks the boundary");
}

#[test]
fn exclusive_boundary_holder_killed_releases_lease() {
    // An exclusive boundary blocks every lease; SIGKILL during the boundary must
    // not leave the repository stuck in the busy state.
    let (root, repository, _relation_id) = format3::empty_format3_repository();
    let repository = repository.with_coordination_lock_wait(BUSY_WAIT);
    let mut holder = Holder::spawn(root.path(), "exclusive");
    assert!(
        matches!(
            repository.hold_maintenance_lease(),
            Err(RepositoryError::RepositoryBusy)
        ),
        "a live exclusive boundary must block a shared lease"
    );
    holder.kill_with_sigkill();
    repository
        .hold_maintenance_lease()
        .expect("a killed boundary holder no longer blocks a shared lease");
}

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

/// Captures one media row and prepares its format-3 publication. The returned
/// coordinator is the one a real publisher holds between prepare and publish.
async fn prepare_publication(
    repository: &Repository,
    relation_id: [u8; 16],
) -> RuntimePublicationCoordinator {
    let media = TempDir::new().expect("temporary media directory");
    fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        media.path().join("tone.wav"),
    )
    .expect("copy media fixture");
    let (identity, initial_digest) = format3::cli_runtime_identity(repository);
    let state = RuntimeState::open(repository, identity, initial_digest)
        .await
        .expect("open runtime state");
    let mut filesystem =
        FilesystemProvider::with_limits(1 << 20, 16).expect("bounded filesystem provider");
    filesystem
        .allow_root(media.path())
        .expect("allow media fixture root");
    let capability = repository
        .capture_capability(relation_id)
        .expect("issue admitted capture capability");
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let key = "maintenance-lease-a";
    let writer = state
        .acquire_lease(identity.repository_id)
        .await
        .expect("acquire runtime writer");
    media_commit::commit_capture_pending(
        repository,
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
        .freeze([0x82; 16], &checkpoint)
        .await
        .expect("freeze captured row prefix");
    let head = repository
        .head()
        .expect("read head")
        .expect("fixture has a head");
    let index = repository
        .index_generation()
        .expect("read index generation");
    RuntimePublicationCoordinator::prepare(
        repository,
        &state,
        &head,
        index,
        &freeze,
        |_| LoosePath::for_key("media", &[key.to_owned()]),
        "orna: publish format-3 row",
    )
    .await
    .expect("storage writer prepares format-3 publication")
}

#[tokio::test]
async fn prepared_publication_holds_shared_lease_until_publish() {
    // ORNA-CAP-006: the shared lease taken by prepare is kept on the coordinator
    // until publish anchors the candidate, so a boundary cannot prune it in
    // between. Publish then releases the lease.
    let (_root, repository, relation_id) = format3::empty_format3_repository();
    let repository = repository.with_coordination_lock_wait(BUSY_WAIT);
    let mut coordinator = prepare_publication(&repository, relation_id).await;
    assert!(
        matches!(
            repository.with_maintenance_boundary(|| Ok::<(), RepositoryError>(())),
            Err(RepositoryError::RepositoryBusy)
        ),
        "a prepared publication must keep the boundary blocked until publish"
    );
    coordinator
        .publish(&repository)
        .expect("publish prepared candidate");
    repository
        .with_maintenance_boundary(|| Ok::<(), RepositoryError>(()))
        .expect("a published candidate releases the maintenance lease");
}

#[tokio::test]
async fn publish_fails_closed_on_candidate_pruned_before_publish() {
    // ORNA-GIT-010 and ORNA-CAP-006: maintenance is supported only through the
    // boundary, so a bypassing `git prune --expire=now` is unsupported. The
    // defence that remains: publish must fail closed on an absent candidate and
    // must not move the head.
    let (root, repository, relation_id) = format3::empty_format3_repository();
    let repository = repository.with_coordination_lock_wait(BUSY_WAIT);
    let mut coordinator = prepare_publication(&repository, relation_id).await;
    let candidate = coordinator.candidate().commit().as_str().to_owned();
    let head_before = repository
        .head()
        .expect("read head")
        .map(|head| head.as_str().to_owned());

    git(root.path(), &["prune", "--expire=now"]);
    let present = Command::new("git")
        .current_dir(root.path())
        .args(["cat-file", "-e", candidate.as_str()])
        .status()
        .expect("run git cat-file")
        .success();
    assert!(
        !present,
        "prune must remove the unreferenced candidate, or the case proves nothing"
    );

    // A format-3 candidate carries a store tree, so publication reads it before
    // the pin step and fails closed there. The pin policy's own refusal is
    // covered by pin_policy_refuses_absent_object_only_for_publish in orna-repository.
    let published = coordinator.publish(&repository);
    assert!(
        published.is_err(),
        "publish must fail closed on a candidate object that is no longer present"
    );
    let head_after = repository
        .head()
        .expect("read head")
        .map(|head| head.as_str().to_owned());
    assert_eq!(
        head_after, head_before,
        "a refused publish must leave the head unchanged"
    );
}
