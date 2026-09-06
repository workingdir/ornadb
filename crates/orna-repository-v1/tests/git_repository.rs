use std::{fs, path::Path, process::Command};

use fs2::FileExt;
use orna_repository_v1::{CheckoutTarget, ManagedPath, Repository, RuntimeGeneration};
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
    fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
    fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(temp.path().join(".orna")).unwrap();
    fs::write(temp.path().join(".orna/format.orna"), "format 1\n").unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "initial"]);
    temp
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
