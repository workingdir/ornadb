use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use fs2::FileExt;
use orna_repository_v1::{
    CheckoutExecutionError, CheckoutTarget, GitObjectKind, GitObjectState, GitRepositoryMode,
    IndexGeneration, ManagedPath, Repository, RuntimeGeneration, WorktreeState,
};
use tempfile::TempDir;

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
    git(
        temp.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(temp.path(), &["config", "user.name", "Repository test"]);
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
    fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(temp.path().join(".orna")).unwrap();
    fs::write(temp.path().join(".orna/format.orna"), "format 1\n").unwrap();
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

fn with_remote(root: &Path) {
    git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://account:secret@example.invalid/private.git",
        ],
    );
    let head = git(root, &["rev-parse", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
}

fn with_partial_clone(root: &Path) {
    with_remote(root);
    git(root, &["config", "extensions.partialClone", "origin"]);
    git(root, &["config", "remote.origin.promisor", "true"]);
    git(
        root,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );
}

/// Builds a real filtered clone when the installed Git supports file-protocol
/// filtering. `None` means the platform cannot provide the fixture; callers
/// must then assert the conservative unavailable boundary rather than claim a
/// promised object from configuration alone.
fn filtered_clone() -> Option<(TempDir, PathBuf, String)> {
    let fixture = TempDir::new().ok()?;
    let origin = fixture.path().join("origin.git");
    let seed = fixture.path().join("seed");
    let clone = fixture.path().join("partial");
    git(fixture.path(), &["init", "--bare", "origin.git"]);
    git(fixture.path(), &["clone", "origin.git", "seed"]);
    git(&seed, &["config", "user.email", "test@example.invalid"]);
    git(&seed, &["config", "user.name", "Repository test"]);
    git(&seed, &["config", "commit.gpgsign", "false"]);
    fs::write(seed.join("visible.txt"), "visible\n").ok()?;
    fs::write(seed.join("promised.txt"), "promised\n").ok()?;
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "initial"]);
    git(&seed, &["push", "origin", "HEAD"]);
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
            "partial",
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
fn discovers_head_index_worktree_and_per_worktree_runtime_area() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    assert_eq!(repo.worktree(), root.path());
    assert!(repo.head().unwrap().is_some());
    assert!(repo.index_generation().unwrap().tree().is_some());
    assert!(repo.worktree_state().unwrap().is_clean());
    assert_eq!(
        repo.runtime_paths().root(),
        root.path()
            .join(git(root.path(), &["rev-parse", "--git-path", "orna"]))
    );
    assert!(
        !repo
            .runtime_paths()
            .root()
            .starts_with(root.path().join(".orna"))
    );
    repo.runtime_paths().ensure_exists().unwrap();
    assert!(
        repo.runtime_paths()
            .state_db()
            .starts_with(repo.runtime_paths().root())
    );
    let cwd = repo.cwd_generation(RuntimeGeneration::new(9)).unwrap();
    assert_eq!(cwd.runtime().get(), 9);
}

#[test]
fn linked_worktree_gets_its_own_git_resolved_runtime_area() {
    let root = repository();
    let linked = TempDir::new().unwrap();
    let linked_path = linked.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            linked_path.to_str().unwrap(),
        ],
    );
    let main = Repository::discover(root.path()).unwrap();
    let other = Repository::discover(&linked_path).unwrap();
    assert_ne!(main.runtime_paths().root(), other.runtime_paths().root());
    assert_eq!(
        other.runtime_paths().root(),
        Path::new(&git(&linked_path, &["rev-parse", "--git-path", "orna"]))
    );
}

#[test]
fn managed_staging_preserves_unrelated_index_and_worktree_changes() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join(".orna/format.orna"), "format 2\n").unwrap();
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    let before = repo.index_generation().unwrap();
    let after = repo
        .stage_managed(&before, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();
    assert_ne!(before, after);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    let stale = repo
        .stage_managed(&before, &[ManagedPath::new("main.orna").unwrap()])
        .unwrap_err();
    assert!(matches!(
        stale,
        orna_repository_v1::RepositoryError::StaleIndex { .. }
    ));
    assert!(ManagedPath::new("../ordinary.txt").is_err());
    assert!(ManagedPath::new(".git/config").is_err());
}

#[test]
fn managed_unstage_and_coordination_lock_preserve_unrelated_staging() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "ordinary staged\n").unwrap();
    fs::write(root.path().join(".orna/format.orna"), "format staged\n").unwrap();
    git(root.path(), &["add", "ordinary.txt", ".orna/format.orna"]);
    let staged = repo.index_generation().unwrap();
    let after = repo
        .unstage_managed(&staged, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();
    assert_ne!(staged, after);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary staged"
    );
    assert_eq!(
        git(root.path(), &["show", ":.orna/format.orna"]),
        "format 1"
    );

    repo.runtime_paths().ensure_exists().unwrap();
    fs::create_dir_all(repo.runtime_paths().locks()).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(repo.runtime_paths().locks().join("coordination.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    let busy = repo
        .stage_managed(&after, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap_err();
    assert!(matches!(
        busy,
        orna_repository_v1::RepositoryError::RepositoryBusy
    ));
    lock.unlock().unwrap();
    // Advisory locks are held by the owner FD rather than the directory entry:
    // after owner death/close, a new repository operation recovers directly.
    repo.stage_managed(&after, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();

    // A normal Git writer owns this filename while it updates `index`; Orna
    // refuses rather than replacing an index under that writer.
    let index = root
        .path()
        .join(git(root.path(), &["rev-parse", "--git-path", "index"]));
    let generation = repo.index_generation().unwrap();
    let git_lock = index.with_extension("lock");
    fs::write(&git_lock, "ordinary Git writer").unwrap();
    assert!(matches!(
        repo.stage_managed(
            &generation,
            &[ManagedPath::new(".orna/format.orna").unwrap()]
        ),
        Err(orna_repository_v1::RepositoryError::GitIndexLockPresent)
    ));
    fs::remove_file(git_lock).unwrap();
}

#[test]
fn snapshot_resolution_accepts_commits_and_rejects_non_commits_and_malformed_ids() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["hash-object", "-w", "ordinary.txt"]);
    let blob = git(root.path(), &["hash-object", "ordinary.txt"]);
    let tree = git(root.path(), &["write-tree"]);
    assert!(repo.resolve_snapshot(&blob).is_err());
    assert!(repo.resolve_snapshot(&tree).is_err());
    assert!(repo.resolve_snapshot("abc").is_err());
    let dangling = git(
        root.path(),
        &["commit-tree", "HEAD^{tree}", "-m", "dangling"],
    );
    assert!(matches!(
        repo.resolve_snapshot(&dangling),
        Err(orna_repository_v1::RepositoryError::SnapshotNotReachable)
    ));
    git(root.path(), &["switch", "--detach", &dangling]);
    assert_eq!(repo.resolve_snapshot("HEAD").unwrap().as_str(), dangling);
}

#[test]
fn checkout_preflight_classifies_a_local_branch_without_mutating_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(17)).unwrap();

    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(17))
        .unwrap();

    assert_eq!(plan.cwd(), &before);
    assert_eq!(plan.expected_head(), before.head());
    assert_eq!(plan.target().branch_name(), Some("experiment"));
    assert_eq!(plan.target().commit(), before.head().unwrap());
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(17)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
}

#[test]
fn checkout_preflight_classifies_a_commit_as_detached_and_rejects_ambiguity() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let before = repo.cwd_generation(RuntimeGeneration::new(18)).unwrap();

    let plan = repo
        .plan_checkout(&head, RuntimeGeneration::new(18))
        .unwrap();
    assert!(matches!(plan.target(), CheckoutTarget::Detached { .. }));
    assert_eq!(plan.target().commit().as_str(), head);
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(18)).unwrap(),
        before
    );

    git(root.path(), &["tag", "main"]);
    let before_ambiguous = repo.cwd_generation(RuntimeGeneration::new(18)).unwrap();
    assert!(matches!(
        repo.plan_checkout("main", RuntimeGeneration::new(18)),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(18)).unwrap(),
        before_ambiguous
    );
}

#[test]
fn checkout_preflight_invalid_selector_is_strictly_non_mutating() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(19)).unwrap();
    assert!(matches!(
        repo.plan_checkout("-force", RuntimeGeneration::new(19)),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(19)).unwrap(),
        before
    );
}

#[test]
fn checkout_preflight_revalidation_accepts_unchanged_state_and_rejects_worktree_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(20))
        .unwrap();
    assert!(repo.verify_checkout_preflight(&plan).is_ok());

    fs::write(root.path().join("main.orna"), "changed after planning\n").unwrap();
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn checkout_preflight_revalidation_rejects_index_head_and_branch_tip_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(21))
        .unwrap();

    fs::write(root.path().join("ordinary.txt"), "index drift\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(22))
        .unwrap();
    git(
        root.path(),
        &["commit", "--allow-empty", "-m", "head drift"],
    );
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(23))
        .unwrap();
    let next = git(
        root.path(),
        &[
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-m",
            "tip drift",
        ],
    );
    git(
        root.path(),
        &[
            "update-ref",
            "refs/heads/experiment",
            &next,
            &git(root.path(), &["rev-parse", "experiment"]),
        ],
    );
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn checkout_force_authorization_is_canonical_and_stale_state_is_rejected() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let branch = repo
        .plan_checkout("experiment", RuntimeGeneration::new(24))
        .unwrap();
    let unchanged = repo
        .plan_checkout("experiment", RuntimeGeneration::new(24))
        .unwrap();
    assert_eq!(branch.force_token(), unchanged.force_token());
    assert!(
        repo.authorize_checkout_force(&branch, true, Some(&branch.force_token()))
            .is_ok()
    );
    assert!(matches!(
        repo.authorize_checkout_force(&branch, false, Some(&branch.force_token())),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert!(matches!(
        repo.authorize_checkout_force(&branch, true, None),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let detached = repo
        .plan_checkout(&head, RuntimeGeneration::new(24))
        .unwrap();
    assert_ne!(branch.force_token(), detached.force_token());

    fs::write(root.path().join("main.orna"), "changed after planning\n").unwrap();
    assert!(matches!(
        repo.authorize_checkout_force(&branch, true, Some(&branch.force_token())),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn same_commit_checkout_switches_attachment_without_discarding_local_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(26))
        .unwrap();

    repo.execute_same_commit_checkout(&plan).unwrap();

    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    let detached = repo
        .plan_checkout(
            &git(root.path(), &["rev-parse", "HEAD"]),
            RuntimeGeneration::new(26),
        )
        .unwrap();
    repo.execute_same_commit_checkout(&detached).unwrap();
    assert!(git(root.path(), &["branch", "--show-current"]).is_empty());
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
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
fn same_commit_checkout_rejects_a_divergent_target_without_mutating_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    let before = repo.cwd_generation(RuntimeGeneration::new(27)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(27))
        .unwrap();

    assert!(matches!(
        repo.execute_same_commit_checkout(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(27)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
}

#[test]
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

    repo.execute_nonconflicting_git_checkout(&plan).unwrap();

    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("target.orna")).unwrap(),
        "target source\n"
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
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
fn divergent_checkout_logical_validation_rejection_fences_git_mutation() {
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
    let before = repo.cwd_generation(RuntimeGeneration::new(31)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(31))
        .unwrap();

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout_with_validation(&plan, |repository, observed| {
            assert_eq!(
                repository.head().unwrap(),
                observed.expected_head().cloned()
            );
            assert_eq!(observed.target().branch_name(), Some("experiment"));
            Err::<(), _>("candidate schema assertion rejected")
        }),
        Err(CheckoutExecutionError::Validation(
            "candidate schema assertion rejected"
        ))
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(31)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    assert!(!root.path().join("target.orna").exists());
}

#[test]
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
    let before = repo.cwd_generation(RuntimeGeneration::new(30)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(30))
        .unwrap();

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(30)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "local");
}

#[test]
fn checkout_subplan_classifies_target_and_local_path_sets() {
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
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(25))
        .unwrap();
    let ordinary = ManagedPath::new("ordinary.txt").unwrap();
    assert_eq!(plan.git().affected_paths(), std::slice::from_ref(&ordinary));
    assert_eq!(
        plan.git().conflicting_paths(),
        std::slice::from_ref(&ordinary)
    );
    assert_eq!(
        plan.git().discardable_paths(),
        std::slice::from_ref(&ordinary)
    );

    fs::write(root.path().join("main.orna"), "carried\n").unwrap();
    let carried = repo
        .plan_checkout("experiment", RuntimeGeneration::new(25))
        .unwrap();
    assert_eq!(
        carried.git().affected_paths(),
        &[
            ManagedPath::new("main.orna").unwrap(),
            ManagedPath::new("ordinary.txt").unwrap()
        ]
    );
    assert_eq!(
        carried.git().conflicting_paths(),
        plan.git().conflicting_paths()
    );
    assert_ne!(carried.force_token(), plan.force_token());
}

#[test]
fn checkout_discard_set_requires_the_canonical_force_witness_and_exact_paths() {
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
    fs::write(root.path().join("main.orna"), "unstaged local\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(28))
        .unwrap();
    let token = plan.force_token();
    let before = repo.cwd_generation(RuntimeGeneration::new(28)).unwrap();

    let _validated = repo
        .validate_checkout_discard_set(&plan, true, Some(&token), plan.git().discardable_paths())
        .unwrap();
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(28)).unwrap(),
        before
    );

    assert!(matches!(
        repo.validate_checkout_discard_set(&plan, true, Some(&token), &[]),
        Err(orna_repository_v1::RepositoryError::CheckoutDiscardSetMismatch)
    ));
    assert!(matches!(
        repo.validate_checkout_discard_set(
            &plan,
            false,
            Some(&token),
            plan.git().discardable_paths(),
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(28)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged local\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn checkout_discard_set_logical_validation_rejection_fences_force_admission() {
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
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(32))
        .unwrap();
    let token = plan.force_token();
    let before = repo.cwd_generation(RuntimeGeneration::new(32)).unwrap();

    assert!(matches!(
        repo.validate_checkout_discard_set_with_validation(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            |repository, observed| {
                assert_eq!(
                    repository.head().unwrap(),
                    observed.expected_head().cloned()
                );
                assert_eq!(observed.target().branch_name(), Some("experiment"));
                Err::<(), _>("candidate schema assertion rejected")
            },
        ),
        Err(orna_repository_v1::CheckoutExecutionError::Validation(
            "candidate schema assertion rejected"
        ))
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(32)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn verify_cwd_rejects_a_worktree_only_interleaving() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let observed = repo.cwd_generation(RuntimeGeneration::new(7)).unwrap();
    fs::write(root.path().join("main.orna"), "changed only in worktree\n").unwrap();
    assert!(matches!(
        repo.verify_cwd(&observed),
        Err(orna_repository_v1::RepositoryError::StaleCwd)
    ));
}

#[test]
fn ordinary_git_add_interleavings_make_stage_and_unstage_stale() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new(".orna/format.orna").unwrap();
    fs::write(root.path().join(".orna/format.orna"), "candidate stage\n").unwrap();
    let expected_stage = repo.index_generation().unwrap();
    let mut stage_race = || {
        fs::write(root.path().join("ordinary.txt"), "ordinary race one\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
    };
    assert!(matches!(
        repo.stage_managed_with_test_hook(
            &expected_stage,
            std::slice::from_ref(&managed),
            &mut stage_race
        ),
        Err(orna_repository_v1::RepositoryError::StaleIndex { .. })
    ));
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary race one"
    );
    assert_eq!(
        git(root.path(), &["show", ":.orna/format.orna"]),
        "format 1"
    );

    let expected_unstage = repo.index_generation().unwrap();
    let mut unstage_race = || {
        fs::write(root.path().join("ordinary.txt"), "ordinary race two\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
    };
    assert!(matches!(
        repo.unstage_managed_with_test_hook(
            &expected_unstage,
            std::slice::from_ref(&managed),
            &mut unstage_race
        ),
        Err(orna_repository_v1::RepositoryError::StaleIndex { .. })
    ));
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary race two"
    );
}

#[test]
fn conflicted_index_fails_closed_and_errors_redact_local_paths() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["switch", "-c", "other"]);
    fs::write(root.path().join("ordinary.txt"), "other\n").unwrap();
    git(root.path(), &["commit", "-am", "other"]);
    git(root.path(), &["switch", "main"]);
    fs::write(root.path().join("ordinary.txt"), "main\n").unwrap();
    git(root.path(), &["commit", "-am", "main"]);
    let output = Command::new("git")
        .current_dir(root.path())
        .args(["merge", "other"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(matches!(
        repo.index_generation(),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    let error = Repository::discover(root.path().join("missing").join("file")).unwrap_err();
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains(&root.path().display().to_string()));
}

#[test]
fn explicit_snapshot_branch_and_remote_preserve_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    assert_eq!(repo.resolve_snapshot("HEAD").unwrap(), head);
    let index = repo.index_generation().unwrap();
    repo.create_branch_at_head("experiment").unwrap();
    assert!(repo.create_branch_at_head("experiment").is_err());
    assert_eq!(repo.index_generation().unwrap(), index);
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/heads/experiment"]),
        head.as_str()
    );
    git(
        root.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/orna.git",
        ],
    );
    assert_eq!(repo.remote_names().unwrap(), vec!["origin"]);
}

#[test]
fn observes_an_ordinary_repository_and_materialized_head_without_mutation() {
    let root = repository();
    with_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    let capabilities = repo.observe_git_capabilities().unwrap();
    assert_eq!(capabilities.mode(), GitRepositoryMode::Ordinary);
    assert!(!capabilities.sparse_checkout());
    assert!(!capabilities.partial_clone());
    assert!(capabilities.promisor_remotes().is_empty());
    assert!(format!("{capabilities:?}").contains("Ordinary"));
    assert!(!format!("{capabilities:?}").contains("example.invalid"));
    assert!(!format!("{capabilities:?}").contains("account"));
    assert!(!format!("{capabilities:?}").contains("secret"));
    assert!(!format!("{capabilities:?}").contains(&root.path().display().to_string()));

    let head = git(root.path(), &["rev-parse", "HEAD"]);
    assert!(matches!(
        repo.observe_git_object(&head).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Commit,
            size: 1..
        }
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn distinguishes_sparse_checkout_from_partial_clone() {
    let root = repository();
    git(root.path(), &["sparse-checkout", "init", "--no-cone"]);
    git(
        root.path(),
        &["sparse-checkout", "set", "main.orna", "ordinary.txt"],
    );
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());
    let capabilities = repo.observe_git_capabilities().unwrap();

    assert_eq!(capabilities.mode(), GitRepositoryMode::SparseCheckout);
    assert!(capabilities.sparse_checkout());
    assert!(!capabilities.partial_clone());
    assert!(capabilities.promisor_remotes().is_empty());
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn observes_a_partial_clone_and_a_promised_object_without_hydrating() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // A configuration-only repository never authorizes a Promised result.
        let root = repository();
        with_partial_clone(root.path());
        let repo = Repository::discover(root.path()).unwrap();
        assert_eq!(
            repo.observe_git_object("0000000000000000000000000000000000000000")
                .unwrap(),
            GitObjectState::Unavailable
        );
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let capabilities = repo.observe_git_capabilities().unwrap();

    assert_eq!(capabilities.mode(), GitRepositoryMode::PartialClone);
    assert!(!capabilities.sparse_checkout());
    assert!(capabilities.partial_clone());
    assert_eq!(capabilities.promisor_remotes(), &["origin".to_owned()]);
    assert_eq!(
        repo.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(git_state(&repo, &clone), before);
    drop(fixture);
}

#[test]
fn observes_combined_sparse_and_partial_capabilities() {
    let root = repository();
    with_partial_clone(root.path());
    git(root.path(), &["config", "core.sparseCheckout", "true"]);
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    let capabilities = repo.observe_git_capabilities().unwrap();
    assert_eq!(capabilities.mode(), GitRepositoryMode::Combined);
    assert!(capabilities.sparse_checkout());
    assert!(capabilities.partial_clone());
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn reports_malformed_configuration_and_object_ids_without_leaking_state() {
    let root = repository();
    with_remote(root.path());
    git(
        root.path(),
        &["config", "extensions.partialClone", "missing-remote"],
    );
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_git_capabilities().unwrap().mode(),
        GitRepositoryMode::Malformed
    );
    assert_eq!(
        repo.observe_git_object("not-a-native-object-id").unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(
        repo.observe_git_object("0000000000000000000000000000000000000000")
            .unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn reports_an_unavailable_object_distinct_from_a_promised_object() {
    let root = repository();
    with_partial_clone(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    // Valid promisor configuration alone is not proof that an arbitrary
    // object is promised by a reachable local commit or tree.
    assert_eq!(
        repo.observe_git_object("0000000000000000000000000000000000000000")
            .unwrap(),
        GitObjectState::Unavailable
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn observes_materialized_tree_blob_and_tag_without_mutation() {
    let root = repository();
    git(root.path(), &["tag", "-a", "release", "-m", "release"]);
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());
    let tree = git(root.path(), &["rev-parse", "HEAD^{tree}"]);
    let blob = git(root.path(), &["rev-parse", "HEAD:ordinary.txt"]);
    let tag = git(root.path(), &["rev-parse", "release^{tag}"]);

    for (object, kind) in [
        (tree, GitObjectKind::Tree),
        (blob, GitObjectKind::Blob),
        (tag, GitObjectKind::Tag),
    ] {
        assert!(matches!(
            repo.observe_git_object(&object).unwrap(),
            GitObjectState::Materialized { kind: observed, .. } if observed == kind
        ));
    }
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn reports_a_corrupt_local_object_as_malformed_without_mutation() {
    let root = repository();
    let object = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let object_directory = root.path().join(".git").join("objects").join("aa");
    fs::create_dir_all(&object_directory).unwrap();
    fs::write(object_directory.join(&object[2..]), b"not a Git object").unwrap();
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_git_object(object).unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn managed_materialization_is_atomic_and_conflict_fenced() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let path = ManagedPath::new("generated/books/one.orna").unwrap();

    repo.materialize_managed_file(&path, None, Some(b"first"))
        .unwrap();
    assert_eq!(
        repo.managed_file_bytes(&path).unwrap(),
        Some(b"first".to_vec())
    );
    assert_eq!(
        fs::read(root.path().join(path.as_path())).unwrap(),
        b"first"
    );

    fs::write(root.path().join(path.as_path()), b"editor change").unwrap();
    assert!(matches!(
        repo.materialize_managed_file(&path, Some(b"first"), Some(b"second")),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(
        fs::read(root.path().join(path.as_path())).unwrap(),
        b"editor change"
    );
    repo.materialize_managed_file(&path, Some(b"editor change"), None)
        .unwrap();
    assert!(!root.path().join(path.as_path()).exists());
}

#[cfg(unix)]
#[test]
fn managed_materialization_rejects_symlinked_parents() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("generated")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("generated/linked")).unwrap();
    let path = ManagedPath::new("generated/linked/row.orna").unwrap();

    assert!(matches!(
        repo.materialize_managed_file(&path, None, Some(b"row")),
        Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
    ));
    assert!(!outside.path().join("row.orna").exists());
}

#[test]
fn private_candidate_uses_head_and_preserves_ordinary_cwd_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let worktree_before = repo.worktree_state().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                ManagedPath::new("generated/row.orna").unwrap(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();

    assert_ne!(candidate.commit(), &head);
    assert_eq!(repo.head().unwrap().unwrap(), head);
    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(repo.worktree_state().unwrap(), worktree_before);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        git(
            root.path(),
            &[
                "show",
                &format!("{}:generated/row.orna", candidate.commit())
            ]
        ),
        "candidate row"
    );
    assert_eq!(
        git(
            root.path(),
            &["show", &format!("{}:ordinary.txt", candidate.commit())]
        ),
        "base"
    );
    assert_eq!(
        git(
            root.path(),
            &["rev-parse", &format!("{}^", candidate.commit())]
        ),
        head.as_str()
    );
}

#[test]
fn private_candidate_advances_current_branch_with_compare_and_set() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                ManagedPath::new("generated/row.orna").unwrap(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();

    repo.advance_current_ref(&head, &candidate).unwrap();
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    let index_after = repo.index_generation().unwrap();
    assert_eq!(index_after.tree(), index_before.tree());
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
    let status = git(root.path(), &["status", "--short"]);
    assert!(status.contains("ordinary.txt"));
    assert!(status.contains("main.orna"));
    assert!(status.contains("generated/row.orna"));
    assert!(matches!(
        repo.advance_current_ref(&head, &candidate),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
}

#[test]
fn published_candidate_reconciles_only_managed_index_entries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    let reconciled = repo
        .reconcile_published_index(&index_before, &candidate, &[managed])
        .unwrap();
    assert_eq!(reconciled.head(), Some(candidate.commit()));
    assert_eq!(
        git(root.path(), &["show", ":generated/row.orna"]),
        "candidate row"
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
}

#[test]
fn publication_journal_round_trips_atomically_and_advances_monotonically() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new(
        head.clone(),
        candidate.commit().clone(),
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.write_publication_journal(&journal).unwrap();
    assert_eq!(
        repo.read_publication_journal().unwrap(),
        Some(journal.clone())
    );
    let mut resumed = repo.read_publication_journal().unwrap().unwrap();
    resumed
        .advance(orna_repository_v1::PublicationJournalStage::RefAdvanced)
        .unwrap();
    assert!(matches!(
        resumed.advance(orna_repository_v1::PublicationJournalStage::Complete),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    repo.write_publication_journal(&resumed).unwrap();
    assert_eq!(
        repo.read_publication_journal().unwrap().unwrap().stage(),
        orna_repository_v1::PublicationJournalStage::RefAdvanced
    );
    repo.clear_publication_journal().unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn publish_candidate_completes_ref_index_and_worktree_boundaries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [1; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.publish_candidate(&index_before, &candidate, &mut journal)
        .unwrap();
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"candidate row\n"
    );
    assert_eq!(
        git(root.path(), &["show", ":generated/row.orna"]),
        "candidate row"
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
    repo.mark_runtime_complete([1; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn publication_pauses_for_an_existing_git_index_lock_before_ref_change() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [9; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    fs::write(root.path().join(".git/index.lock"), b"ordinary writer\n").unwrap();
    assert!(matches!(
        repo.publish_candidate(&index_before, &candidate, &mut journal),
        Err(orna_repository_v1::RepositoryError::GitIndexLockPresent)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(
        journal.stage(),
        orna_repository_v1::PublicationJournalStage::Prepared
    );
}

#[test]
fn recovery_resumes_after_ref_and_index_boundaries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head,
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [2; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&journal.old_head().clone(), &candidate)
        .unwrap();
    repo.reconcile_published_index(&index_before, &candidate, std::slice::from_ref(&managed))
        .unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"candidate row\n"
    );
    let mut journal = repo.read_publication_journal().unwrap().unwrap();
    repo.mark_runtime_complete([2; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn runtime_completion_preserves_the_journal_when_head_moved_after_reconciliation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head,
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [28; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.publish_candidate(&index_before, &candidate, &mut journal)
        .unwrap();
    fs::write(root.path().join("ordinary.txt"), "ordinary commit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(
        root.path(),
        &["commit", "-m", "ordinary commit after publication"],
    );

    assert!(matches!(
        repo.mark_runtime_complete([28; 16], &mut journal),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
    assert_eq!(
        journal.stage(),
        orna_repository_v1::PublicationJournalStage::WorktreeReconciled
    );
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_keeps_a_pre_ref_publication_pending() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [8; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::PublicationPending)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_rejects_a_post_ref_journal_whose_bytes_differ_from_the_candidate() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [19; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"journal row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    assert_eq!(repo.head().unwrap(), Some(candidate.commit().clone()));
    assert_eq!(repo.index_generation().unwrap().tree(), index_before.tree());
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_rejects_a_journal_candidate_outside_the_recorded_head_lineage() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let old_head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let unrelated = git(
        root.path(),
        &[
            "commit-tree",
            "HEAD^{tree}",
            "-m",
            "unrelated recovery candidate",
        ],
    );
    git(root.path(), &["switch", "--detach", &unrelated]);
    let unrelated_head = repo.head().unwrap().unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        old_head,
        unrelated_head,
        index_before.tree().unwrap().clone(),
        [18; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    assert_eq!(repo.head().unwrap().unwrap(), *journal.new_head());
    assert_eq!(repo.index_generation().unwrap().tree(), index_before.tree());
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_preserves_post_ref_external_conflict_and_can_resume() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [3; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();
    fs::create_dir_all(root.path().join("generated")).unwrap();
    fs::write(root.path().join(managed.as_path()), b"editor").unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"editor"
    );
    assert_eq!(
        repo.read_publication_journal().unwrap().unwrap().stage(),
        orna_repository_v1::PublicationJournalStage::IndexReconciled
    );

    fs::write(root.path().join(managed.as_path()), b"candidate row\n").unwrap();
    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    let mut journal = repo.read_publication_journal().unwrap().unwrap();
    repo.mark_runtime_complete([3; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}
