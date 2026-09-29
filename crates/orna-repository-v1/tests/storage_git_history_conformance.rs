#[path = "../src/test_support.rs"]
mod test_support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use orna_repository_v1::{
    FetchRequest, GitRepositoryMode, OrnaInternalRef, PushRequest, Repository, RequestedRef,
};
use tempfile::TempDir;

const INTERNAL_REF: &str = "refs/orna/ids/0123456789abcdef";
const ALLOCATOR_REF: &str = "refs/orna/allocator";
const MAIN_FIXTURE: &str = include_str!("fixtures/git-repository-main.orna");
const NESTED_FIXTURE: &str = include_str!("fixtures/git-repository-nested-tool.orna");

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

struct Fixture {
    root: TempDir,
    source: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let remote = root.path().join("remote.git");
        fs::create_dir(&source).unwrap();
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(&source, &["init", "-b", "main"]);
        test_support::configure_fixture_git_identity(&source);
        git(&source, &["config", "commit.gpgsign", "false"]);
        fs::write(source.join("main.orna"), MAIN_FIXTURE).unwrap();
        git(&source, &["add", "main.orna"]);
        git(&source, &["commit", "-m", "initial fixture"]);
        let initial = git(&source, &["rev-parse", "HEAD"]);
        git(&source, &["update-ref", INTERNAL_REF, &initial]);
        git(&source, &["update-ref", ALLOCATOR_REF, &initial]);
        git(
            &source,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let internal = format!("{INTERNAL_REF}:{INTERNAL_REF}");
        let allocator = format!("{ALLOCATOR_REF}:{ALLOCATOR_REF}");
        git(
            &source,
            &[
                "push",
                "origin",
                "refs/heads/main:refs/heads/main",
                &internal,
                &allocator,
            ],
        );
        Self {
            root,
            source,
            remote,
        }
    }

    fn advance(&self) -> String {
        fs::write(self.source.join("main.orna"), NESTED_FIXTURE).unwrap();
        git(&self.source, &["add", "main.orna"]);
        git(&self.source, &["commit", "-m", "next fixture"]);
        let next = git(&self.source, &["rev-parse", "HEAD"]);
        git(&self.source, &["update-ref", INTERNAL_REF, &next]);
        git(&self.source, &["update-ref", ALLOCATOR_REF, &next]);
        next
    }
}

#[test]
fn fixture_edits_remain_reviewable_and_prior_snapshot_content_stays_reachable() {
    let fixture = Fixture::new();
    let initial = git(&fixture.source, &["rev-parse", "HEAD"]);
    let next = fixture.advance();

    assert_eq!(
        git(&fixture.source, &["show", &format!("{initial}:main.orna")]),
        MAIN_FIXTURE.trim_end()
    );
    assert_eq!(
        git(&fixture.source, &["show", &format!("{next}:main.orna")]),
        NESTED_FIXTURE.trim_end()
    );
    let diff = git(
        &fixture.source,
        &["diff", &format!("{initial}..{next}"), "--", "main.orna"],
    );
    assert!(diff.contains("-module main;"));
    assert!(diff.contains("+module nested.tool;"));
}

#[test]
fn clone_synchronizes_internal_refs_without_checking_out_the_fixture() {
    let fixture = Fixture::new();
    let initial = git(&fixture.source, &["rev-parse", "HEAD"]);
    let destination = fixture.root.path().join("clone");
    Repository::clone_from(fixture.remote.to_str().unwrap(), &destination).unwrap();

    assert!(git(&destination, &["symbolic-ref", "--quiet", "HEAD"]).starts_with("refs/heads/"));
    assert_eq!(
        git(&destination, &["rev-parse", INTERNAL_REF]),
        initial
    );
    assert_eq!(
        git(&destination, &["rev-parse", ALLOCATOR_REF]),
        initial
    );
    assert!(!destination.join("main.orna").exists());
}

#[test]
fn fetch_synchronizes_advertised_internal_refs_with_the_requested_branch() {
    let fixture = Fixture::new();
    let destination = fixture.root.path().join("clone");
    let client = Repository::clone_from(fixture.remote.to_str().unwrap(), &destination).unwrap();
    let old = git(&fixture.source, &["rev-parse", "HEAD"]);
    let next = fixture.advance();
    let internal = format!("{INTERNAL_REF}:{INTERNAL_REF}");
    let allocator = format!("{ALLOCATOR_REF}:{ALLOCATOR_REF}");
    git(
        &fixture.source,
        &[
            "push",
            "origin",
            "refs/heads/main:refs/heads/main",
            &internal,
            &allocator,
        ],
    );

    assert_eq!(git(&destination, &["rev-parse", INTERNAL_REF]), old);
    client
        .fetch(
            &FetchRequest::new(
                "origin",
                [RequestedRef::Branch("main".to_owned())],
                [],
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(git(&destination, &["rev-parse", "refs/remotes/origin/main"]), next);
    assert_eq!(git(&destination, &["rev-parse", INTERNAL_REF]), next);
    assert_eq!(git(&destination, &["rev-parse", ALLOCATOR_REF]), next);
    assert!(!destination.join("main.orna").exists());
}

#[test]
#[cfg(unix)]
fn push_advances_allocator_before_making_the_branch_visible() {
    let fixture = Fixture::new();
    let next = fixture.advance();
    let hook_log = fixture.root.path().join("update-hook.log");
    let hook = fixture.remote.join("hooks/update");
    fs::write(
        &hook,
        format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\n", hook_log.display()),
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
            OrnaInternalRef::new(INTERNAL_REF).unwrap(),
            OrnaInternalRef::new(ALLOCATOR_REF).unwrap(),
        ],
    )
    .unwrap();
    repository.push(&request).unwrap();

    let updates = fs::read_to_string(hook_log).unwrap();
    assert_eq!(updates.lines().next(), Some(ALLOCATOR_REF));
    assert_eq!(git(&fixture.remote, &["rev-parse", "refs/heads/main"]), next);
    assert_eq!(git(&fixture.remote, &["rev-parse", ALLOCATOR_REF]), next);
}

#[test]
fn sparse_checkout_and_partial_clone_are_reported_as_separate_capabilities() {
    let fixture = Fixture::new();
    let initial = git(&fixture.source, &["rev-parse", "HEAD"]);
    let repository = Repository::discover(&fixture.source).unwrap();
    git(&fixture.source, &["sparse-checkout", "init", "--no-cone"]);
    git(&fixture.source, &["sparse-checkout", "set", "missing.orna"]);
    git(
        &fixture.source,
        &["config", "extensions.partialClone", "origin"],
    );
    git(
        &fixture.source,
        &["config", "remote.origin.promisor", "true"],
    );
    git(
        &fixture.source,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );

    let capabilities = repository.observe_git_capabilities().unwrap();
    assert_eq!(capabilities.mode(), GitRepositoryMode::Combined);
    assert!(capabilities.sparse_checkout());
    assert!(capabilities.partial_clone());
    assert_eq!(git(&fixture.source, &["rev-parse", "HEAD"]), initial);
    assert!(!fixture.source.join("main.orna").exists());
}
