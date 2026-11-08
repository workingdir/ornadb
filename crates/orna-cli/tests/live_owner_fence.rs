//! Gate D live-writer coexistence (ORNA-TURSO-004/005, D2) against the real `orna` executable.
//!
//! A live process that holds a worktree's runtime owner lock stays the only writer
//! of the runtime lease, even while it is paused (SIGSTOP). Publication is not a
//! lease operation (22-publication PUB-3 step 7 excludes cooperating mutators with
//! the publication lock), so a second `orna publish` started during that window
//! admits the durable tail and leaves the paused owner's lock intact.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

use orna_repository::Repository;
use orna_runtime::RuntimeState;
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::{cli_runtime_identity, format3_repository_for_table};

const SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn seed() { music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" }); }\n\
pub fn main() {}\n";
const CHILD_ROOT: &str = "ORNA_LIVE_OWNER_ROOT";
const CHILD_DIR: &str = "ORNA_LIVE_OWNER_DIR";
const READY_FILE: &str = "owner-ready";
const RELEASE_FILE: &str = "owner-release";
const WAIT_LIMIT: Duration = Duration::from_mins(1);

/// Child role: holds the runtime owner lock until the parent writes the release file.
/// Inert when run by the normal test harness (the environment selects the role).
#[tokio::test]
async fn live_owner_child_holds_runtime() {
    let Ok(root) = std::env::var(CHILD_ROOT) else {
        return;
    };
    let directory = PathBuf::from(std::env::var(CHILD_DIR).expect("owner directory"));
    let repository = Repository::discover(&root).expect("child repository");
    let (identity, digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, digest)
        .await
        .expect("child opens the runtime");
    assert!(
        state.holds_runtime_owner_lock(),
        "child must own the runtime"
    );
    fs::write(directory.join(READY_FILE), b"ready").expect("ready marker");
    wait_for(&directory.join(RELEASE_FILE));
    assert!(
        state.holds_runtime_owner_lock(),
        "owner lock must be held until release"
    );
}

/// A paused owner keeps its runtime owner lock; an outside `orna publish` still
/// admits the durable tail through the publication lock, and the owner is
/// unaffected: it still holds the lock after it resumes and exits cleanly.
#[test]
fn paused_live_owner_admits_outside_publish() {
    let (directory, repository, _relation_id) = format3_repository_for_table(SOURCE, "music");
    let root = directory.path().to_path_buf();

    // Leave a durable runtime change for the outside publish to admit.
    let seed = orna(&root, &["run", "seed"]);
    assert_eq!(
        seed.status.code(),
        Some(0),
        "seed run failed: {}",
        combined(&seed)
    );
    let head_before = head(&repository);

    let owner_directory = TempDir::new().expect("owner directory");
    let mut owner = ChildGuard(spawn_owner(&root, owner_directory.path()));
    wait_for(&owner_directory.path().join(READY_FILE));

    signal(&owner, "STOP");
    let published = orna(
        &root,
        &["publish", "--message", "Publish behind a paused owner"],
    );
    assert!(
        published.status.success(),
        "publish behind a paused live owner must admit the tail: {}",
        combined(&published)
    );
    assert_ne!(
        head(&repository),
        head_before,
        "publish behind a paused live owner must commit the durable tail"
    );
    assert!(
        repository
            .read_publication_journal()
            .expect("journal readable")
            .is_none(),
        "completed publish left a publication journal"
    );

    signal(&owner, "CONT");
    fs::write(owner_directory.path().join(RELEASE_FILE), b"release").expect("release marker");
    let status = owner.0.wait().expect("owner exits");
    assert!(
        status.success(),
        "owner must still hold the runtime after the outside publish: {status}"
    );
}

fn head(repository: &Repository) -> String {
    repository
        .head()
        .expect("head readable")
        .expect("committed head")
        .as_str()
        .to_owned()
}

fn spawn_owner(root: &Path, owner_directory: &Path) -> Child {
    let mut command = Command::new(std::env::current_exe().expect("test executable path"));
    command
        .current_dir(root)
        .env(CHILD_ROOT, root)
        .env(CHILD_DIR, owner_directory);
    command.args(["--exact", "live_owner_child_holds_runtime", "--nocapture"]);
    command.spawn().expect("owner child starts")
}

fn orna(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", root.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("orna CLI process")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn signal(owner: &ChildGuard, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(owner.0.id().to_string())
        .status()
        .expect("kill starts");
    assert!(status.success(), "kill -{name} failed");
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + WAIT_LIMIT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// Kills and reaps the owner if the test fails before it exits on its own.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
