//! `orna push` publishes the ordinary branch ref together with the continuity
//! refs an ordinary push synchronizes, and never exposes a private edit or
//! capture pin (`ORNA-GIT-008`).
//!
//! Every push here is the real `orna-cli-v1` binary against a real bare remote,
//! so this proves the verb's exit status and the refs that actually moved,
//! rather than a library call.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::TempDir;

const ALLOCATOR_REF: &str = "refs/orna/allocator";
const CHECKPOINT_REF: &str = "refs/orna/checkpoints/0123456789abcdef";
const PIN_REF: &str = "refs/orna/pins/0123456789abcdef/scratch/row-1";

/// Runs one Git command in `directory` and returns its trimmed stdout.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(arguments)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git_status(directory: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(arguments)
        .status()
        .expect("git runs")
        .success()
}

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

struct Fixture {
    _root: TempDir,
    worktree: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    /// One worktree whose `HEAD` sits one commit ahead of `origin/main`, with
    /// allocator, checkpoint and private pin refs already present locally. Only
    /// the initial commit is on the remote, so a push has real work to publish.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("worktree");
        let remote = root.path().join("remote.git");
        fs::create_dir(&worktree).unwrap();
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(root.path(), &["init", "-b", "main", worktree.to_str().unwrap()]);
        git(&worktree, &["config", "user.name", "kierandrewett"]);
        git(
            &worktree,
            &["config", "user.email", "kieran@drewett.dev"],
        );
        git(&worktree, &["config", "commit.gpgsign", "false"]);
        git(
            &worktree,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        fs::write(worktree.join("main.orna"), "module main;\n").unwrap();
        git(&worktree, &["add", "main.orna"]);
        git(&worktree, &["commit", "-m", "initial"]);
        let initial = git(&worktree, &["rev-parse", "HEAD"]);
        git(
            &worktree,
            &["update-ref", ALLOCATOR_REF, &initial],
        );
        git(
            &worktree,
            &["update-ref", CHECKPOINT_REF, &initial],
        );
        git(&worktree, &["update-ref", PIN_REF, &initial]);
        git(&worktree, &["push", "origin", "refs/heads/main:refs/heads/main"]);
        git(
            &worktree,
            &[
                "push",
                "origin",
                &format!("{ALLOCATOR_REF}:{ALLOCATOR_REF}"),
                &format!("{CHECKPOINT_REF}:{CHECKPOINT_REF}"),
            ],
        );
        fs::write(worktree.join("main.orna"), "module main;\n\n// next\n").unwrap();
        git(&worktree, &["add", "main.orna"]);
        git(&worktree, &["commit", "-m", "next"]);
        Self {
            _root: root,
            worktree,
            remote,
        }
    }

    fn head(&self) -> String {
        git(&self.worktree, &["rev-parse", "HEAD"])
    }

    fn remote_ref(&self, reference: &str) -> Option<String> {
        git_status(
            &self.remote,
            &["show-ref", "--verify", "--quiet", "--", reference],
        )
        .then(|| git(&self.remote, &["rev-parse", reference]))
    }
}

#[test]
fn push_publishes_the_branch_and_never_exposes_private_pins() {
    let fixture = Fixture::new();
    let head = fixture.head();

    let output = run(&fixture.worktree, &["push"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "push must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "pushed origin -> main"
    );
    assert_eq!(
        fixture.remote_ref("refs/heads/main").as_deref(),
        Some(head.as_str()),
        "the ordinary branch ref must advance on the remote"
    );
    assert_eq!(
        fixture.remote_ref(ALLOCATOR_REF).as_deref(),
        Some(head.as_str()),
        "the allocator continuity ref must be published"
    );
    assert_eq!(
        fixture.remote_ref(CHECKPOINT_REF).as_deref(),
        Some(head.as_str()),
        "the consumer checkpoint continuity ref must be published"
    );
    assert!(
        fixture.remote_ref(PIN_REF).is_none(),
        "an ordinary push must not expose a private edit or capture pin"
    );
}

#[test]
fn push_refuses_a_remote_that_dropped_a_continuity_ref() {
    let fixture = Fixture::new();
    let before = fixture.head();
    let remote_head = fixture.remote_ref("refs/heads/main");
    git(&fixture.remote, &["update-ref", "-d", CHECKPOINT_REF]);

    let output = run(&fixture.worktree, &["push"]);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a missing continuity ref must fail the push: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        fixture.remote_ref("refs/heads/main"),
        remote_head,
        "a refused push must not advance the remote branch"
    );
    assert_eq!(
        fixture.remote_ref(CHECKPOINT_REF),
        None,
        "a refused push must not re-create the refused continuity ref"
    );
    assert_eq!(fixture.head(), before, "a refused push must not move local HEAD");
}

#[test]
fn push_needs_an_explicit_branch_when_head_is_detached() {
    let fixture = Fixture::new();
    git(&fixture.worktree, &["checkout", "--detach", "HEAD"]);

    let output = run(&fixture.worktree, &["push"]);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a detached HEAD must not have its branch guessed"
    );
}
