use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use orna_repository_v1::{
    FetchError, FetchRequest, GitObjectKind, GitObjectState, NativeObjectId, OrnaInternalRef,
    PushRequest, RemoteContinuity, Repository, RequestedRef, RequiredInternalRef,
};
use tempfile::TempDir;

const INTERNAL_REF: &str = "refs/orna/ids/0123456789abcdef";
const ALLOCATOR_REF: &str = "refs/orna/allocator";
const OTHER_INTERNAL_REF: &str = "refs/orna/checkpoints/0123456789abcdef";
const STALE_INTERNAL_REF: &str = "refs/orna/runs/0123456789abcdef";
const FETCH_CHILD_LOCAL: &str = "ORNA_FETCH_ROUTING_LOCAL";
const FETCH_CHILD_OTHER: &str = "ORNA_FETCH_ROUTING_OTHER";
const CLONE_CHILD_REMOTE: &str = "ORNA_CLONE_ROUTING_REMOTE";
const CLONE_CHILD_DESTINATION: &str = "ORNA_CLONE_ROUTING_DESTINATION";

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git_no_lazy(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn promised_inventory(directory: &Path) -> Vec<String> {
    let mut inventory = git_no_lazy(
        directory,
        &[
            "rev-list",
            "--objects",
            "--missing=print",
            "--no-object-names",
            "--all",
        ],
    )
    .lines()
    .filter_map(|line| line.strip_prefix('?'))
    .map(str::to_owned)
    .collect::<Vec<_>>();
    inventory.sort();
    inventory
}

fn git_path_bytes(directory: &Path, name: &str) -> Option<Vec<u8>> {
    let path = PathBuf::from(git(directory, &["rev-parse", "--git-path", name]));
    let path = if path.is_absolute() {
        path
    } else {
        directory.join(path)
    };
    fs::read(path).ok()
}

fn git_status(directory: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .status()
        .unwrap()
        .success()
}

struct Fixture {
    root: TempDir,
    source: PathBuf,
    local: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let local = root.path().join("local");
        let remote = root.path().join("remote.git");
        fs::create_dir(&source).unwrap();

        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(&source, &["init", "-b", "main"]);
        git(
            &source,
            &["config", "user.email", "transport@example.invalid"],
        );
        git(&source, &["config", "user.name", "Transport test"]);
        git(&source, &["config", "commit.gpgsign", "false"]);
        fs::write(source.join("main.orna"), "module main;\n").unwrap();
        git(&source, &["add", "main.orna"]);
        git(&source, &["commit", "-m", "initial"]);
        git(&source, &["tag", "v1"]);
        let initial = git(&source, &["rev-parse", "HEAD"]);
        git(&source, &["update-ref", INTERNAL_REF, &initial]);
        let internal_refspec = format!("{INTERNAL_REF}:{INTERNAL_REF}");
        git(
            &source,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(
            &source,
            &[
                "push",
                "origin",
                "refs/heads/main:refs/heads/main",
                "refs/tags/v1:refs/tags/v1",
                &internal_refspec,
            ],
        );
        git(
            root.path(),
            &["clone", remote.to_str().unwrap(), local.to_str().unwrap()],
        );
        git(
            &local,
            &["config", "user.email", "transport@example.invalid"],
        );
        git(&local, &["config", "user.name", "Transport test"]);
        git(&local, &["config", "commit.gpgsign", "false"]);

        Self {
            root,
            source,
            local,
            remote,
        }
    }

    fn repository(&self) -> Repository {
        Repository::discover(&self.local).unwrap()
    }

    fn initial_head(&self) -> String {
        git(&self.source, &["rev-parse", "HEAD"])
    }

    fn advance_branch_and_internal(&self) -> String {
        fs::write(self.source.join("main.orna"), "module main;\n\n// next\n").unwrap();
        git(&self.source, &["add", "main.orna"]);
        git(&self.source, &["commit", "-m", "next"]);
        let next = git(&self.source, &["rev-parse", "HEAD"]);
        git(&self.source, &["update-ref", INTERNAL_REF, &next]);
        let internal_refspec = format!("{INTERNAL_REF}:{INTERNAL_REF}");
        git(
            &self.source,
            &[
                "push",
                "origin",
                "refs/heads/main:refs/heads/main",
                &internal_refspec,
            ],
        );
        next
    }

    fn relocate_source_origin(&self) -> PathBuf {
        let relocated = self.root.path().join("relocated.git");
        git(
            self.root.path(),
            &["init", "--bare", relocated.to_str().unwrap()],
        );
        git(
            &self.source,
            &["remote", "set-url", "origin", relocated.to_str().unwrap()],
        );
        let internal_refspec = format!("{INTERNAL_REF}:{INTERNAL_REF}");
        git(
            &self.source,
            &[
                "push",
                "origin",
                "refs/heads/main:refs/heads/main",
                "refs/tags/v1:refs/tags/v1",
                &internal_refspec,
            ],
        );
        relocated
    }

    fn advance_branch_only(&self) -> String {
        fs::write(self.source.join("main.orna"), "module main;\n\n// branch\n").unwrap();
        git(&self.source, &["add", "main.orna"]);
        git(&self.source, &["commit", "-m", "branch only"]);
        let next = git(&self.source, &["rev-parse", "HEAD"]);
        git(
            &self.source,
            &["push", "origin", "refs/heads/main:refs/heads/main"],
        );
        next
    }

    fn install_local_internal(&self, object_id: &str) {
        git(&self.local, &["update-ref", INTERNAL_REF, object_id]);
    }

    fn configure_fetch_race(&self, raced_object_id: &str) {
        let script = self.root.path().join("race-upload-pack");
        let state = self.root.path().join("race-upload-pack.count");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\ncount=0\nif [ -f '{state}' ]; then count=$(cat '{state}'); fi\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > '{state}'\nif [ \"$count\" -eq 2 ]; then git --git-dir='{remote}' update-ref refs/heads/main {raced_object_id}; fi\nexec git-upload-pack \"$@\"\n",
            state = state.display(),
            remote = self.remote.display(),
        );
        fs::write(&script, script_contents).unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        git(
            &self.local,
            &[
                "config",
                "remote.origin.uploadpack",
                script.to_str().unwrap(),
            ],
        );
    }

    fn configure_local_internal_race(&self, raced_object_id: &str) {
        let script = self.root.path().join("race-local-ref-upload-pack");
        let state = self.root.path().join("race-local-ref-upload-pack.count");
        let local_git = self.local.join(".git");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\ncount=0\nif [ -f '{state}' ]; then count=$(cat '{state}'); fi\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > '{state}'\nif [ \"$count\" -eq 2 ]; then env -u GIT_OBJECT_DIRECTORY -u GIT_ALTERNATE_OBJECT_DIRECTORIES git --git-dir='{local_git}' update-ref {INTERNAL_REF} {raced_object_id}; fi\nexec git-upload-pack \"$@\"\n",
            state = state.display(),
            local_git = local_git.display(),
        );
        fs::write(&script, script_contents).unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        git(
            &self.local,
            &[
                "config",
                "remote.origin.uploadpack",
                script.to_str().unwrap(),
            ],
        );
    }
}

fn internal_witness_for(reference: &str, object_id: &str) -> RequiredInternalRef {
    RequiredInternalRef::new(
        OrnaInternalRef::new(reference).unwrap(),
        NativeObjectId::new(object_id).unwrap(),
    )
}

fn internal_witness(object_id: &str) -> RequiredInternalRef {
    internal_witness_for(INTERNAL_REF, object_id)
}

fn request(
    ordinary: impl IntoIterator<Item = RequestedRef>,
    continuity: impl IntoIterator<Item = RequiredInternalRef>,
) -> FetchRequest {
    FetchRequest::new("origin", ordinary, continuity).unwrap()
}

#[test]
fn push_keeps_second_phase_atomic_after_allocator_first() {
    let fixture = Fixture::new();
    let initial = fixture.initial_head();
    git(
        &fixture.source,
        &["update-ref", ALLOCATOR_REF, &initial],
    );
    let allocator_refspec = format!("{ALLOCATOR_REF}:{ALLOCATOR_REF}");
    git(&fixture.source, &["push", "origin", &allocator_refspec]);
    fs::write(
        fixture.source.join("main.orna"),
        "module main;\n\n// next\n",
    )
    .unwrap();
    git(&fixture.source, &["add", "main.orna"]);
    git(&fixture.source, &["commit", "-m", "next"]);
    let next = git(&fixture.source, &["rev-parse", "HEAD"]);
    git(
        &fixture.source,
        &["update-ref", ALLOCATOR_REF, &next],
    );
    git(
        &fixture.source,
        &["update-ref", INTERNAL_REF, &next],
    );
    git(
        &fixture.source,
        &["update-ref", OTHER_INTERNAL_REF, &next],
    );

    let hook = fixture.remote.join("hooks/update");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"{OTHER_INTERNAL_REF}\" ]; then exit 1; fi\nexit 0\n"
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&hook, permissions).unwrap();

    let repository = Repository::discover(&fixture.source).unwrap();
    let request = PushRequest::new(
        "origin",
        "main",
        [
            OrnaInternalRef::new(ALLOCATOR_REF).unwrap(),
            OrnaInternalRef::new(INTERNAL_REF).unwrap(),
            OrnaInternalRef::new(OTHER_INTERNAL_REF).unwrap(),
        ],
    )
    .unwrap();
    let error = repository
        .push(&request)
        .expect_err("a rejected second-phase ref must fail the push");

    assert!(matches!(error, FetchError::PushFailed));
    assert_eq!(
        git(&fixture.remote, &["rev-parse", ALLOCATOR_REF]),
        next
    );
    assert_eq!(
        git(&fixture.remote, &["rev-parse", INTERNAL_REF]),
        initial
    );
    assert_eq!(
        git(&fixture.remote, &["rev-parse", "refs/heads/main"]),
        initial
    );
    assert!(!git_status(
        &fixture.remote,
        &["show-ref", "--verify", "--quiet", "--", OTHER_INTERNAL_REF]
    ));
}

#[test]
fn fetch_preconditions_ignore_inherited_git_routing_child() {
    let Ok(local) = std::env::var(FETCH_CHILD_LOCAL) else {
        return;
    };
    let repository = Repository::discover(local).unwrap();
    let report = repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .unwrap();
    assert!(report.ordinary()[0].updated());
}

#[test]
fn clone_preconditions_ignore_inherited_git_routing_child() {
    let Ok(remote) = std::env::var(CLONE_CHILD_REMOTE) else {
        return;
    };
    let Ok(destination) = std::env::var(CLONE_CHILD_DESTINATION) else {
        return;
    };
    Repository::clone_from(remote, destination).unwrap();
}

#[test]
fn clone_preconditions_ignore_inherited_git_routing() {
    let fixture = Fixture::new();
    let other = fixture.root.path().join("other");
    git(
        fixture.root.path(),
        &[
            "clone",
            fixture.remote.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    let destination = fixture.root.path().join("routing-safe-clone");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "clone_preconditions_ignore_inherited_git_routing_child",
            "--nocapture",
        ])
        .env(CLONE_CHILD_REMOTE, fixture.remote.to_str().unwrap())
        .env(CLONE_CHILD_DESTINATION, destination.to_str().unwrap())
        .env("GIT_DIR", other.join(".git").to_str().unwrap())
        .env("GIT_WORK_TREE", other.to_str().unwrap())
        .env("GIT_INDEX_FILE", other.join(".git/index").to_str().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git(&destination, &["rev-parse", INTERNAL_REF]),
        fixture.initial_head()
    );
    assert!(!destination.join("main.orna").exists());
}

#[test]
fn fetch_preconditions_ignore_inherited_git_routing() {
    let fixture = Fixture::new();
    let next = fixture.advance_branch_only();
    let other = fixture.root.path().join("other");
    git(
        fixture.root.path(),
        &[
            "clone",
            fixture.remote.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );

    let repository = fixture.repository();
    let head_before = repository.head().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fetch_preconditions_ignore_inherited_git_routing_child",
            "--nocapture",
        ])
        .env(FETCH_CHILD_LOCAL, fixture.local.to_str().unwrap())
        .env(FETCH_CHILD_OTHER, other.to_str().unwrap())
        .env("GIT_DIR", other.join(".git").to_str().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child fetch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        next
    );
    assert_eq!(repository.head().unwrap(), head_before);
}

#[test]
fn fetch_does_not_dereference_a_symbolic_destination() {
    let fixture = Fixture::new();
    let initial = fixture.initial_head();
    let next = fixture.advance_branch_only();
    git(&fixture.local, &["update-ref", "refs/heads/main", &initial]);
    git(&fixture.local, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(
        &fixture.local,
        &[
            "symbolic-ref",
            "refs/remotes/origin/main",
            "refs/heads/main",
        ],
    );
    let repository = fixture.repository();
    let head_before = repository.head().unwrap();
    let index_before = repository.index_generation().unwrap();
    let worktree_before = repository.worktree_state().unwrap();
    let runtime_marker = repository.runtime_paths().root().join("transport-marker");
    repository.runtime_paths().ensure_exists().unwrap();
    fs::write(&runtime_marker, b"preserve").unwrap();

    let report = repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .unwrap();
    assert!(report.ordinary()[0].updated());
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        next
    );
    assert!(!git_status(
        &fixture.local,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/main"]
    ));
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/heads/main"]),
        initial
    );
    assert_eq!(repository.head().unwrap(), head_before);
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), worktree_before);
    assert_eq!(fs::read(&runtime_marker).unwrap(), b"preserve");
}

#[test]
fn fetch_updates_branch_and_internal_refs_without_mutating_local_state() {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    let initial = fixture.initial_head();
    fixture.install_local_internal(&initial);
    let next = fixture.advance_branch_and_internal();
    let runtime_marker = repository.runtime_paths().root().join("transport-marker");
    repository.runtime_paths().ensure_exists().unwrap();
    fs::write(&runtime_marker, b"preserve").unwrap();

    let head_before = repository.head().unwrap();
    let index_before = repository.index_generation().unwrap();
    let worktree_before = repository.worktree_state().unwrap();
    let report = repository
        .fetch(&request(
            [
                RequestedRef::branch("main").unwrap(),
                RequestedRef::tag("v1").unwrap(),
            ],
            [internal_witness(&next)],
        ))
        .unwrap();

    assert_eq!(report.continuity(), Some(RemoteContinuity::Continuous));
    assert_eq!(report.ordinary().len(), 2);
    assert!(report.ordinary()[0].updated());
    assert!(!report.ordinary()[1].updated());
    assert_eq!(report.internal().len(), 1);
    assert!(report.internal()[0].updated());
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        next
    );
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), next);
    assert_eq!(repository.head().unwrap(), head_before);
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), worktree_before);
    assert_eq!(fs::read(&runtime_marker).unwrap(), b"preserve");
}

#[test]
fn fetch_after_remote_set_url_updates_requested_ordinary_and_internal_refs() {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    let initial = fixture.initial_head();
    fixture.install_local_internal(&initial);
    let relocated = fixture.relocate_source_origin();
    let next = fixture.advance_branch_and_internal();
    git(
        &fixture.local,
        &["remote", "set-url", "origin", relocated.to_str().unwrap()],
    );

    let head_before = repository.head().unwrap();
    let index_before = repository.index_generation().unwrap();
    let worktree_before = repository.worktree_state().unwrap();
    let runtime_marker = repository.runtime_paths().root().join("transport-marker");
    repository.runtime_paths().ensure_exists().unwrap();
    fs::write(&runtime_marker, b"preserve-after-relocation").unwrap();
    let runtime_before = fs::read(&runtime_marker).unwrap();

    let report = repository
        .fetch(&request(
            [
                RequestedRef::branch("main").unwrap(),
                RequestedRef::tag("v1").unwrap(),
            ],
            [internal_witness(&next)],
        ))
        .unwrap();

    assert_eq!(
        git(&fixture.local, &["remote", "get-url", "origin"]),
        relocated.to_str().unwrap()
    );
    assert_eq!(report.continuity(), Some(RemoteContinuity::Continuous));
    assert!(report.ordinary()[0].updated());
    assert!(!report.ordinary()[1].updated());
    assert!(report.internal()[0].updated());
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        next
    );
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), next);
    assert_eq!(repository.head().unwrap(), head_before);
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), worktree_before);
    assert_eq!(fs::read(&runtime_marker).unwrap(), runtime_before);
}

#[test]
fn fetch_rejects_a_force_rewound_remote_branch_without_mutating_local_state() {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    let initial = fixture.initial_head();
    let advanced = fixture.advance_branch_only();

    let report = repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .unwrap();
    assert!(report.ordinary()[0].updated());
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        advanced
    );

    git(
        &fixture.remote,
        &["update-ref", "refs/heads/main", &initial, &advanced],
    );
    assert_eq!(
        git(&fixture.remote, &["rev-parse", "refs/heads/main"]),
        initial
    );

    let tracking_before = git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]);
    let head_before = repository.head().unwrap();
    let index_before = repository.index_generation().unwrap();
    let worktree_before = repository.worktree_state().unwrap();
    let runtime_marker = repository.runtime_paths().root().join("transport-marker");
    repository.runtime_paths().ensure_exists().unwrap();
    fs::write(&runtime_marker, b"preserve-after-rewind").unwrap();
    let runtime_before = fs::read(&runtime_marker).unwrap();

    let error = repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .expect_err("a force-rewound remote branch must fail closed");

    assert!(matches!(error, FetchError::RefConflict));
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        tracking_before
    );
    assert_eq!(repository.head().unwrap(), head_before);
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), worktree_before);
    assert_eq!(fs::read(&runtime_marker).unwrap(), runtime_before);
}

#[test]
fn fetch_rejects_a_non_commit_branch_target_without_installing_it() {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    let blob = git(&fixture.source, &["rev-parse", "HEAD:main.orna"]);
    git(
        &fixture.local,
        &["update-ref", "-d", "refs/remotes/origin/main"],
    );
    fs::write(fixture.remote.join("refs/heads/main"), format!("{blob}\n")).unwrap();

    let error = repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .expect_err("a branch must not install a non-commit target");

    assert!(matches!(error, FetchError::MalformedAdvertisement));
    assert!(!git_status(
        &fixture.local,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            "--",
            "refs/remotes/origin/main"
        ]
    ));
}

#[test]
fn fetch_reports_missing_internal_ref_without_fabricating_it() {
    let fixture = Fixture::new();
    let initial = fixture.initial_head();
    git(&fixture.remote, &["update-ref", "-d", INTERNAL_REF]);
    let repository = fixture.repository();

    let report = repository
        .fetch(&request(
            [RequestedRef::branch("main").unwrap()],
            [internal_witness(&initial)],
        ))
        .unwrap();

    assert_eq!(report.continuity(), Some(RemoteContinuity::Missing));
    assert_eq!(report.internal(), &[]);
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        initial
    );
    assert!(!git_status(
        &fixture.local,
        &["show-ref", "--verify", "--quiet", "--", INTERNAL_REF]
    ));
}

#[test]
fn fetch_reports_stale_internal_ref_and_preserves_the_local_ref() {
    let fixture = Fixture::new();
    let initial = fixture.initial_head();
    let next = fixture.advance_branch_only();
    fixture.install_local_internal(&initial);
    let repository = fixture.repository();

    let report = repository
        .fetch(&request(
            [RequestedRef::branch("main").unwrap()],
            [internal_witness(&next)],
        ))
        .unwrap();

    assert_eq!(report.continuity(), Some(RemoteContinuity::Stale));
    assert_eq!(report.internal(), &[]);
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), initial);
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        next
    );
}

#[test]
fn fetch_synchronizes_matching_internal_refs_when_another_witness_is_missing() {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    let initial = fixture.initial_head();
    fixture.install_local_internal(&initial);
    git(
        &fixture.source,
        &["update-ref", OTHER_INTERNAL_REF, &initial],
    );
    let other_refspec = format!("{OTHER_INTERNAL_REF}:{OTHER_INTERNAL_REF}");
    git(&fixture.source, &["push", "origin", &other_refspec]);
    let next = fixture.advance_branch_and_internal();
    git(&fixture.remote, &["update-ref", "-d", OTHER_INTERNAL_REF]);

    fs::write(fixture.local.join("unrelated.txt"), "staged\n").unwrap();
    git(&fixture.local, &["add", "unrelated.txt"]);
    fs::write(
        fixture.local.join("unrelated.txt"),
        "staged plus unstaged\n",
    )
    .unwrap();
    let staged_before = git(&fixture.local, &["show", ":unrelated.txt"]);
    let worktree_before = fs::read(fixture.local.join("unrelated.txt")).unwrap();
    let index_before = repository.index_generation().unwrap();
    let state_before = repository.worktree_state().unwrap();

    let report = repository
        .fetch(&request(
            [RequestedRef::branch("main").unwrap()],
            [
                internal_witness(&next),
                internal_witness_for(OTHER_INTERNAL_REF, &initial),
            ],
        ))
        .unwrap();

    assert_eq!(report.continuity(), Some(RemoteContinuity::Missing));
    assert_eq!(report.internal().len(), 1);
    assert_eq!(report.internal()[0].destination(), INTERNAL_REF);
    assert!(report.internal()[0].updated());
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), next);
    assert!(!git_status(
        &fixture.local,
        &["show-ref", "--verify", "--quiet", "--", OTHER_INTERNAL_REF]
    ));
    assert_eq!(
        git(&fixture.local, &["show", ":unrelated.txt"]),
        staged_before
    );
    assert_eq!(
        fs::read(fixture.local.join("unrelated.txt")).unwrap(),
        worktree_before
    );
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), state_before);
}

#[test]
fn fetch_synchronizes_matching_refs_while_missing_and_stale_witnesses_fail_closed() {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    let initial = fixture.initial_head();
    fixture.install_local_internal(&initial);
    git(
        &fixture.source,
        &["update-ref", OTHER_INTERNAL_REF, &initial],
    );
    git(
        &fixture.source,
        &["update-ref", STALE_INTERNAL_REF, &initial],
    );
    let other_refspec = format!("{OTHER_INTERNAL_REF}:{OTHER_INTERNAL_REF}");
    let stale_refspec = format!("{STALE_INTERNAL_REF}:{STALE_INTERNAL_REF}");
    git(
        &fixture.source,
        &["push", "origin", &other_refspec, &stale_refspec],
    );
    let next = fixture.advance_branch_and_internal();
    git(&fixture.remote, &["update-ref", "-d", OTHER_INTERNAL_REF]);
    git(
        &fixture.local,
        &["update-ref", STALE_INTERNAL_REF, &initial],
    );

    fs::write(fixture.local.join("unrelated.txt"), "staged\n").unwrap();
    git(&fixture.local, &["add", "unrelated.txt"]);
    fs::write(
        fixture.local.join("unrelated.txt"),
        "staged plus unstaged\n",
    )
    .unwrap();
    let staged_before = git(&fixture.local, &["show", ":unrelated.txt"]);
    let worktree_before = fs::read(fixture.local.join("unrelated.txt")).unwrap();
    let index_before = repository.index_generation().unwrap();
    let state_before = repository.worktree_state().unwrap();

    let report = repository
        .fetch(&request(
            [RequestedRef::branch("main").unwrap()],
            [
                internal_witness(&next),
                internal_witness_for(OTHER_INTERNAL_REF, &initial),
                internal_witness_for(STALE_INTERNAL_REF, &next),
            ],
        ))
        .unwrap();

    assert_eq!(report.continuity(), Some(RemoteContinuity::Missing));
    assert_eq!(report.internal().len(), 1);
    assert_eq!(report.internal()[0].destination(), INTERNAL_REF);
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), next);
    assert_eq!(
        git(&fixture.local, &["rev-parse", STALE_INTERNAL_REF]),
        initial
    );
    assert!(!git_status(
        &fixture.local,
        &["show-ref", "--verify", "--quiet", "--", OTHER_INTERNAL_REF]
    ));
    assert_eq!(
        git(&fixture.local, &["show", ":unrelated.txt"]),
        staged_before
    );
    assert_eq!(
        fs::read(fixture.local.join("unrelated.txt")).unwrap(),
        worktree_before
    );
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), state_before);
}

#[test]
fn fetch_rejects_a_newer_local_tracking_ref_without_overwriting_it() {
    let fixture = Fixture::new();
    fs::write(
        fixture.local.join("main.orna"),
        "module main;\n\n// local\n",
    )
    .unwrap();
    git(&fixture.local, &["add", "main.orna"]);
    git(&fixture.local, &["commit", "-m", "local newer"]);
    let local_newer = git(&fixture.local, &["rev-parse", "HEAD"]);
    git(
        &fixture.local,
        &["update-ref", "refs/remotes/origin/main", &local_newer],
    );
    let repository = fixture.repository();

    let error = repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .expect_err("newer local tracking ref must not be overwritten");

    assert!(matches!(error, FetchError::RefConflict));
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        local_newer
    );
}

#[test]
fn fetch_errors_are_redacted_and_request_names_are_validated_before_git() {
    assert!(matches!(
        RequestedRef::branch("../unsafe"),
        Err(FetchError::InvalidRef)
    ));
    assert!(matches!(
        FetchRequest::new(
            "-origin",
            [],
            [internal_witness("0000000000000000000000000000000000000000")]
        ),
        Err(FetchError::InvalidRemote)
    ));

    let fixture = Fixture::new();
    let repository = fixture.repository();
    let secret_url = "https://account:secret@example.invalid/private.git";
    git(&fixture.local, &["remote", "set-url", "origin", secret_url]);
    let error = repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .expect_err("unreachable remote");
    let rendered = format!("{error:?} {error}");
    assert!(matches!(error, FetchError::RemoteUnavailable));
    assert!(!rendered.contains("secret"));
    assert!(!rendered.contains(secret_url));
    assert!(!rendered.contains(&fixture.root.path().display().to_string()));
}

#[test]
fn fetch_rejects_a_remote_change_between_advertisement_and_install() {
    let fixture = Fixture::new();
    let initial = fixture.initial_head();
    fixture.install_local_internal(&initial);
    fs::write(
        fixture.source.join("main.orna"),
        "module main;\n\n// race\n",
    )
    .unwrap();
    git(&fixture.source, &["add", "main.orna"]);
    git(&fixture.source, &["commit", "-m", "race"]);
    let raced = git(&fixture.source, &["rev-parse", "HEAD"]);
    let race_refspec = "HEAD:refs/heads/race".to_string();
    git(&fixture.source, &["push", "origin", &race_refspec]);
    fixture.configure_fetch_race(&raced);

    let repository = fixture.repository();
    let error = repository
        .fetch(&request(
            [RequestedRef::branch("main").unwrap()],
            [internal_witness(&initial)],
        ))
        .expect_err("remote change must fail closed before local CAS");

    assert!(matches!(error, FetchError::RemoteChanged));
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        initial
    );
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), initial);
}

#[test]
fn fetch_rejects_a_local_change_to_an_unchanged_destination() {
    let fixture = Fixture::new();
    let initial = fixture.initial_head();
    fixture.install_local_internal(&initial);
    let next = fixture.advance_branch_and_internal();
    git(
        &fixture.local,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--refmap=",
            "origin",
            "refs/heads/main:",
        ],
    );
    fixture.install_local_internal(&next);
    fixture.configure_local_internal_race(&initial);

    let repository = fixture.repository();
    let error = repository
        .fetch(&request(
            [RequestedRef::branch("main").unwrap()],
            [internal_witness(&next)],
        ))
        .expect_err("a raced unchanged destination must fail the ref transaction");

    assert!(matches!(error, FetchError::RefConflict));
    assert_eq!(
        git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]),
        initial
    );
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), initial);
}

#[test]
fn clone_synchronizes_internal_refs_without_checking_out_or_mutating_worktree() {
    let fixture = Fixture::new();
    let destination = fixture.root.path().join("cloned");

    Repository::clone_from(fixture.remote.to_str().unwrap(), &destination).unwrap();

    assert_eq!(
        git(&destination, &["rev-parse", INTERNAL_REF]),
        fixture.initial_head()
    );
    assert!(git(&destination, &["symbolic-ref", "--quiet", "HEAD"]).starts_with("refs/heads/"));
    assert!(!destination.join("main.orna").exists());
    assert!(git_path_bytes(&destination, "index").is_none());
    assert!(git_path_bytes(&destination, "FETCH_HEAD").is_none());
}

fn filtered_clone() -> Option<(TempDir, PathBuf, String)> {
    let fixture = tempfile::tempdir().ok()?;
    let origin = fixture.path().join("origin.git");
    let seed = fixture.path().join("seed");
    let clone = fixture.path().join("partial");
    git(fixture.path(), &["init", "--bare", origin.to_str()?]);
    git(fixture.path(), &["clone", origin.to_str()?, seed.to_str()?]);
    git(
        &seed,
        &["config", "user.email", "transport@example.invalid"],
    );
    git(&seed, &["config", "user.name", "Transport test"]);
    git(&seed, &["config", "commit.gpgsign", "false"]);
    git(&seed, &["branch", "-M", "main"]);
    fs::write(seed.join("visible.txt"), "visible\n").ok()?;
    fs::write(seed.join("promised.txt"), "promised\n").ok()?;
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "initial"]);
    git(&seed, &["push", "origin", "HEAD:refs/heads/main"]);
    git(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&origin, &["config", "uploadpack.allowFilter", "true"]);
    let origin_url = format!("file://{}", origin.display());
    let output = Command::new("git")
        .current_dir(fixture.path())
        .args([
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &origin_url,
            clone.to_str()?,
        ])
        .output()
        .ok()?;
    if !output.status.success()
        || git(
            &clone,
            &["config", "--local", "--get", "remote.origin.promisor"],
        ) != "true"
    {
        return None;
    }
    let promised = git(&seed, &["rev-parse", "HEAD:promised.txt"]);
    let missing = Command::new("git")
        .current_dir(&clone)
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["cat-file", "-e", &promised])
        .output()
        .ok()?;
    if missing.status.success() {
        return None;
    }
    Some((fixture, clone, promised))
}

#[test]
fn fetch_preserves_materialized_promised_and_unavailable_object_states() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        panic!("the release gate requires a real filtered-clone classification fixture");
    };
    let repository = Repository::discover(&clone).unwrap();
    let materialized = git(&clone, &["rev-parse", "HEAD"]);
    let unavailable = "0".repeat(materialized.len());
    let before = [
        repository.observe_git_object(&materialized).unwrap(),
        repository.observe_git_object(&promised).unwrap(),
        repository.observe_git_object(&unavailable).unwrap(),
    ];
    assert!(matches!(
        before[0],
        GitObjectState::Materialized {
            kind: GitObjectKind::Commit,
            ..
        }
    ));
    assert_eq!(before[1], GitObjectState::Promised);
    assert_eq!(before[2], GitObjectState::Unavailable);

    repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .unwrap();

    let after = [
        repository.observe_git_object(&materialized).unwrap(),
        repository.observe_git_object(&promised).unwrap(),
        repository.observe_git_object(&unavailable).unwrap(),
    ];
    assert_eq!(after, before);
    drop(fixture);
}

#[test]
fn hydrate_materializes_only_the_requested_promised_object() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        panic!("the release gate requires a real filtered-clone hydration fixture");
    };
    let repository = Repository::discover(&clone).unwrap();
    let other_promised = git(
        &fixture.path().join("seed"),
        &["rev-parse", "HEAD:visible.txt"],
    );
    let head_before = repository.head().unwrap();
    let head_identity_before = git(&clone, &["rev-parse", "HEAD^{object}"]);
    let head_attachment_before = git(&clone, &["symbolic-ref", "--quiet", "HEAD"]);
    let head_bytes_before = git_path_bytes(&clone, "HEAD");
    let fetch_head_before = git_path_bytes(&clone, "FETCH_HEAD");
    let index_before = repository.index_generation().unwrap();
    let worktree_before = repository.worktree_state().unwrap();
    let refs_before = git(
        &clone,
        &["for-each-ref", "--format=%(refname)=%(objectname)"],
    );
    let promised_before = promised_inventory(&clone);
    assert_eq!(
        repository.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        repository.observe_git_object(&other_promised).unwrap(),
        GitObjectState::Promised
    );
    assert!(promised_before.iter().any(|id| id == &promised));
    assert!(promised_before.iter().any(|id| id == &other_promised));

    repository.hydrate_promised_object(&promised).unwrap();

    let promised_after = promised_inventory(&clone);
    let expected_promised_after = promised_before
        .iter()
        .filter(|id| *id != &promised)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(promised_after, expected_promised_after);

    assert!(matches!(
        repository.observe_git_object(&promised).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            ..
        }
    ));
    assert_eq!(
        repository.observe_git_object(&other_promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(repository.head().unwrap(), head_before);
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD^{object}"]),
        head_identity_before
    );
    assert_eq!(
        git(&clone, &["symbolic-ref", "--quiet", "HEAD"]),
        head_attachment_before
    );
    assert_eq!(git_path_bytes(&clone, "HEAD"), head_bytes_before);
    assert_eq!(git_path_bytes(&clone, "FETCH_HEAD"), fetch_head_before);
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), worktree_before);
    assert_eq!(
        git(
            &clone,
            &["for-each-ref", "--format=%(refname)=%(objectname)"],
        ),
        refs_before
    );
    drop(fixture);
}

#[test]
fn hydrate_fails_closed_with_an_unusable_promisor_without_changing_promises() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        panic!("the release gate requires a real filtered-clone hydration fixture");
    };
    let repository = Repository::discover(&clone).unwrap();
    let other_promised = git(
        &fixture.path().join("seed"),
        &["rev-parse", "HEAD:visible.txt"],
    );
    let unusable_promisor = fixture.path().join("unusable-promisor.git");
    git(
        &clone,
        &[
            "config",
            "remote.origin.url",
            unusable_promisor.to_str().unwrap(),
        ],
    );

    let promised_before = promised_inventory(&clone);
    let head_identity_before = git(&clone, &["rev-parse", "HEAD^{object}"]);
    let head_attachment_before = git(&clone, &["symbolic-ref", "--quiet", "HEAD"]);
    let head_bytes_before = git_path_bytes(&clone, "HEAD");
    let fetch_head_before = git_path_bytes(&clone, "FETCH_HEAD");
    assert_eq!(
        repository.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        repository.observe_git_object(&other_promised).unwrap(),
        GitObjectState::Promised
    );

    assert!(matches!(
        repository.hydrate_promised_object(&promised),
        Err(FetchError::ObjectUnavailable)
    ));

    assert_eq!(promised_inventory(&clone), promised_before);
    assert_eq!(
        repository.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        repository.observe_git_object(&other_promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD^{object}"]),
        head_identity_before
    );
    assert_eq!(
        git(&clone, &["symbolic-ref", "--quiet", "HEAD"]),
        head_attachment_before
    );
    assert_eq!(git_path_bytes(&clone, "HEAD"), head_bytes_before);
    assert_eq!(git_path_bytes(&clone, "FETCH_HEAD"), fetch_head_before);
    drop(fixture);
}

#[test]
fn hydration_and_fetch_ignore_unavailable_submodules() {
    let (fixture, clone, promised) = filtered_clone()
        .expect("transport isolation requires a real filtered clone with promised blobs");
    let seed = fixture.path().join("seed");
    // Avoid `submodule add`: its index refresh hydrates the parent's blobs.
    git(&clone, &["clone", seed.to_str().unwrap(), "dependency"]);
    fs::write(
        clone.join(".gitmodules"),
        format!(
            "[submodule \"dependency\"]\n\tpath = dependency\n\turl = {}\n",
            seed.display()
        ),
    )
    .unwrap();
    git(
        &clone,
        &["config", "submodule.dependency.url", seed.to_str().unwrap()],
    );
    let dependency_commit = git(&seed, &["rev-parse", "HEAD"]);
    git(
        &clone,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{dependency_commit},dependency"),
        ],
    );
    let dependency = clone.join("dependency");
    let unavailable = fixture.path().join("unavailable.git");
    git(
        &dependency,
        &["remote", "set-url", "origin", unavailable.to_str().unwrap()],
    );
    git(&clone, &["config", "fetch.recurseSubmodules", "true"]);

    let repository = Repository::discover(&clone).unwrap();
    let head_before = git_path_bytes(&clone, "HEAD");
    let fetch_head_before = git_path_bytes(&clone, "FETCH_HEAD");
    let config_before = git_path_bytes(&clone, "config");
    let index_before = repository.index_generation().unwrap();
    let worktree_before = repository.worktree_state().unwrap();
    let refs_before = git(
        &clone,
        &["for-each-ref", "--format=%(refname)=%(objectname)"],
    );
    let dependency_refs_before = git(
        &dependency,
        &["for-each-ref", "--format=%(refname)=%(objectname)"],
    );
    let dependency_fetch_head_before = git_path_bytes(&dependency, "FETCH_HEAD");
    let promises_before = promised_inventory(&clone);
    assert_eq!(
        repository.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );

    repository.hydrate_promised_object(&promised).unwrap();
    repository
        .fetch(&request([RequestedRef::branch("main").unwrap()], []))
        .unwrap();

    assert!(matches!(
        repository.observe_git_object(&promised).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            ..
        }
    ));
    assert_eq!(
        promised_inventory(&clone),
        promises_before
            .into_iter()
            .filter(|id| id != &promised)
            .collect::<Vec<_>>()
    );
    assert_eq!(git_path_bytes(&clone, "HEAD"), head_before);
    assert_eq!(git_path_bytes(&clone, "FETCH_HEAD"), fetch_head_before);
    assert_eq!(git_path_bytes(&clone, "config"), config_before);
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), worktree_before);
    assert_eq!(
        git(
            &clone,
            &["for-each-ref", "--format=%(refname)=%(objectname)"]
        ),
        refs_before
    );
    assert_eq!(
        git(
            &dependency,
            &["for-each-ref", "--format=%(refname)=%(objectname)"]
        ),
        dependency_refs_before
    );
    assert_eq!(
        git_path_bytes(&dependency, "FETCH_HEAD"),
        dependency_fetch_head_before
    );
}

#[test]
fn hydrate_rejects_unpromised_or_malformed_objects_before_fetch() {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    let materialized = fixture.initial_head();
    let unavailable = "0".repeat(materialized.len());

    assert!(matches!(
        repository.hydrate_promised_object(&unavailable),
        Err(FetchError::ObjectNotPromised)
    ));
    assert!(matches!(
        repository.hydrate_promised_object("not-an-object-id"),
        Err(FetchError::InvalidObjectId)
    ));
    assert!(repository.observe_git_object(&materialized).is_ok());
}
