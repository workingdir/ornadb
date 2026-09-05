use std::{fs, path::Path, process::Command};

use fs2::FileExt;
use orna_repository_v1::{ManagedPath, Repository, RuntimeGeneration};
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
