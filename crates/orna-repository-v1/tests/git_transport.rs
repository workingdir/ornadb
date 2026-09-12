use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use orna_repository_v1::{
    FetchError, FetchRequest, GitObjectKind, GitObjectState, NativeObjectId, OrnaInternalRef,
    RemoteContinuity, Repository, RequiredInternalRef, RequestedRef,
};
use tempfile::TempDir;

const INTERNAL_REF: &str = "refs/orna/ids/0123456789abcdef";
const FETCH_CHILD_LOCAL: &str = "ORNA_FETCH_ROUTING_LOCAL";
const FETCH_CHILD_OTHER: &str = "ORNA_FETCH_ROUTING_OTHER";

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
        git(&source, &["config", "user.email", "transport@example.invalid"]);
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
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().unwrap(),
            ],
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
            &[
                "clone",
                remote.to_str().unwrap(),
                local.to_str().unwrap(),
            ],
        );
        git(&local, &["config", "user.email", "transport@example.invalid"]);
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
}

fn internal_witness(object_id: &str) -> RequiredInternalRef {
    RequiredInternalRef::new(
        OrnaInternalRef::new(INTERNAL_REF).unwrap(),
        NativeObjectId::new(object_id).unwrap(),
    )
}

fn request(
    ordinary: impl IntoIterator<Item = RequestedRef>,
    continuity: impl IntoIterator<Item = RequiredInternalRef>,
) -> FetchRequest {
    FetchRequest::new("origin", ordinary, continuity).unwrap()
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
    assert_eq!(git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]), next);
    assert_eq!(repository.head().unwrap(), head_before);
}

#[test]
fn fetch_does_not_dereference_a_symbolic_destination() {
    let fixture = Fixture::new();
    let initial = fixture.initial_head();
    let next = fixture.advance_branch_only();
    git(&fixture.local, &["update-ref", "refs/heads/main", &initial]);
    git(
        &fixture.local,
        &["symbolic-ref", "HEAD", "refs/heads/main"],
    );
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
    assert_eq!(git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]), next);
    assert!(!git_status(
        &fixture.local,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/main"]
    ));
    assert_eq!(git(&fixture.local, &["rev-parse", "refs/heads/main"]), initial);
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
    assert_eq!(git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]), next);
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), next);
    assert_eq!(repository.head().unwrap(), head_before);
    assert_eq!(repository.index_generation().unwrap(), index_before);
    assert_eq!(repository.worktree_state().unwrap(), worktree_before);
    assert_eq!(fs::read(&runtime_marker).unwrap(), b"preserve");
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
    assert_eq!(git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]), initial);
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
    assert_eq!(git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]), next);
}

#[test]
fn fetch_rejects_a_newer_local_tracking_ref_without_overwriting_it() {
    let fixture = Fixture::new();
    fs::write(fixture.local.join("main.orna"), "module main;\n\n// local\n").unwrap();
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
    assert_eq!(git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]), local_newer);
}

#[test]
fn fetch_errors_are_redacted_and_request_names_are_validated_before_git() {
    assert!(matches!(
        RequestedRef::branch("../unsafe"),
        Err(FetchError::InvalidRef)
    ));
    assert!(matches!(
        FetchRequest::new("-origin", [], [internal_witness("0000000000000000000000000000000000000000")]),
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
    fs::write(fixture.source.join("main.orna"), "module main;\n\n// race\n").unwrap();
    git(&fixture.source, &["add", "main.orna"]);
    git(&fixture.source, &["commit", "-m", "race"]);
    let raced = git(&fixture.source, &["rev-parse", "HEAD"]);
    let race_refspec = format!("HEAD:refs/heads/race");
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
    assert_eq!(git(&fixture.local, &["rev-parse", "refs/remotes/origin/main"]), initial);
    assert_eq!(git(&fixture.local, &["rev-parse", INTERNAL_REF]), initial);
}

fn filtered_clone() -> Option<(TempDir, PathBuf, String)> {
    let fixture = tempfile::tempdir().ok()?;
    let origin = fixture.path().join("origin.git");
    let seed = fixture.path().join("seed");
    let clone = fixture.path().join("partial");
    git(fixture.path(), &["init", "--bare", origin.to_str()?]);
    git(fixture.path(), &["clone", origin.to_str()?, seed.to_str()?]);
    git(&seed, &["config", "user.email", "transport@example.invalid"]);
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
