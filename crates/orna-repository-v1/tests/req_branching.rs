#[path = "../src/test_support.rs"]
mod test_support;

use orna_repository_v1::{IndexGeneration, Repository, RuntimeGeneration, WorktreeState};
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

/// Accepts every checkout target: these tests stage no assertion.
struct AcceptCheckout;

impl orna_repository_v1::CheckoutAssertionGate for AcceptCheckout {
    fn validate(
        &self, _: &orna_repository_v1::Repository, _: &orna_repository_v1::CheckoutPreflight,
    ) -> Result<(), orna_repository_v1::CandidateAssertionRefusal> {
        Ok(())
    }
}

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

fn repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    test_support::configure_fixture_git_identity(temp.path());
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        temp.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(temp.path().join(".orna")).unwrap();
    fs::write(temp.path().join(".orna/format.orna"), "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n").unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "initial"]);
    temp
}

fn git_state(
    repository: &Repository,
    root: &Path,
) -> (
    Option<orna_repository_v1::GitCommitRef>,
    IndexGeneration,
    WorktreeState,
    String,
    String,
) {
    (
        repository.head().unwrap(),
        repository.index_generation().unwrap(),
        repository.worktree_state().unwrap(),
        git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        git(root, &["config", "--local", "--null", "--list"]),
    )
}

#[test]
/// ORNA-IMPL-001, ORNA-BRANCH-001: creating a branch at HEAD preserves the active
/// CWD and Git index without creating a commit.
fn creating_branch_at_head_preserves_cwd_and_index_without_a_commit() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let runtime = RuntimeGeneration::new(201);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();

    let head_before = git(root.path(), &["rev-parse", "HEAD"]);
    let index_before = repo.index_generation().unwrap();
    let cwd_before = repo.cwd_generation(runtime).unwrap();
    let status_before = git(
        root.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let commits_before = git(root.path(), &["rev-list", "--all", "--count"]);

    repo.create_branch_at_head("experiment").unwrap();

    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(repo.cwd_generation(runtime).unwrap(), cwd_before);
    assert_eq!(
        git(
            root.path(),
            &["status", "--porcelain=v1", "--untracked-files=all"],
        ),
        status_before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/heads/experiment"]),
        head_before
    );
    assert_eq!(
        git(root.path(), &["rev-list", "--all", "--count"]),
        commits_before
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
}

#[test]
/// ORNA-BRANCH-003: a different-commit checkout carries safe independent Git state
/// into the target.
fn divergent_checkout_carries_nonconflicting_git_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("target.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "target.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(29))
        .unwrap();
    assert!(plan.git().conflicting_paths().is_empty());

    repo.execute_nonconflicting_git_checkout(&plan, &AcceptCheckout).unwrap();

    assert_eq!(git(root.path(), &["branch", "--show-current"]), "experiment");
    assert_eq!(
        fs::read_to_string(root.path().join("target.orna")).unwrap(),
        "target source\n"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged ordinary");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
/// ORNA-CHECKOUT-001, ORNA-BRANCH-003: conflicting checkout changes fail without
/// mutating CWD, index, or HEAD.
fn divergent_checkout_refuses_conflicting_git_state_without_mutating_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    fs::write(root.path().join("ordinary.txt"), "local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let runtime = RuntimeGeneration::new(30);
    let before = repo.cwd_generation(runtime).unwrap();
    let plan = repo.plan_checkout("experiment", runtime).unwrap();

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan, &AcceptCheckout),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(repo.cwd_generation(runtime).unwrap(), before);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "local");
    assert_eq!(
        fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
        "local\n"
    );
}

#[test]
/// ORNA-CHECKOUT-002: recovery re-resolves moved refs and never rewinds a newer branch tip.
fn checkout_recovery_preserves_an_external_branch_advance() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(43))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_after_discard_with_test_hook(
            &discard,
            &mut interrupt
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(repo.has_pending_pre_execution_checkout().unwrap());

    let target = git(root.path(), &["rev-parse", "refs/heads/experiment"]);
    let successor = git(
        root.path(),
        &[
            "commit-tree",
            "experiment^{tree}",
            "-p",
            "experiment",
            "-m",
            "external ref advance",
        ],
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/experiment", &successor, &target],
    );
    let before_recovery = git_state(&repo, root.path());

    assert!(matches!(
        repo.recover_pre_execution_checkout(),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/heads/experiment"]),
        successor
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git_state(&repo, root.path()), before_recovery);
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
}
