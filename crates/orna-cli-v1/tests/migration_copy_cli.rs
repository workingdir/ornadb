use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

fn git_output(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn initialize_legacy_source(root: &Path) {
    git_output(root, &["init", "-b", "main"]);
    git_output(root, &["config", "user.name", "kierandrewett"]);
    git_output(root, &["config", "user.email", "kieran@drewett.dev"]);
    git_output(root, &["config", "commit.gpgsign", "false"]);
    fs::write(
        root.join("main.orna"),
        include_str!("fixtures/project-core-main.orna"),
    )
    .unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(
        root.join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-1.orna"),
    )
    .unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(root, &["add", "."]);
    git_output(root, &["commit", "-m", "legacy source"]);
}

fn run_copy(
    source: &Path,
    destination: &Path,
    count: &str,
    git_trace: &Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE", git_trace)
        .args([
            "--db",
            source.to_str().unwrap(),
            "--debug",
            "migrate",
            "copy",
            destination.to_str().unwrap(),
            "--annotation-defaults",
            count,
            "--no-persistent-consumers",
        ])
        .output()
        .unwrap()
}

#[test]
fn cli_migration_copy_retry_recovers_the_same_candidate() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    fs::write(
        source.join("tracked-deletion.txt"),
        b"committed then deleted\n",
    )
    .unwrap();
    git_output(&source, &["add", "tracked-deletion.txt"]);
    git_output(&source, &["commit", "-m", "record deletion target"]);

    let dependency_source = parent.path().join("dependency-source");
    fs::create_dir(&dependency_source).unwrap();
    git_output(&dependency_source, &["init", "-b", "main"]);
    git_output(
        &dependency_source,
        &["config", "user.name", "kierandrewett"],
    );
    git_output(
        &dependency_source,
        &["config", "user.email", "kieran@drewett.dev"],
    );
    fs::write(
        dependency_source.join("tracked.txt"),
        b"dependency committed\n",
    )
    .unwrap();
    git_output(&dependency_source, &["add", "tracked.txt"]);
    git_output(&dependency_source, &["commit", "-m", "dependency source"]);
    let submodule_add = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(&source)
        .args([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
        ])
        .arg(&dependency_source)
        .arg("dependencies/example")
        .output()
        .unwrap();
    assert!(
        submodule_add.status.success(),
        "git submodule add: {}",
        String::from_utf8_lossy(&submodule_add.stderr)
    );
    git_output(&source, &["commit", "-m", "record dependency"]);
    git_output(&source, &["rm", "--", "tracked-deletion.txt"]);

    let source_submodule = source.join("dependencies/example");
    fs::write(source_submodule.join("tracked.txt"), b"staged dependency\n").unwrap();
    git_output(&source_submodule, &["add", "tracked.txt"]);
    git_output(&source_submodule, &["update-index", "--split-index"]);
    fs::write(
        source_submodule.join("tracked.txt"),
        b"unstaged dependency\n",
    )
    .unwrap();
    fs::write(
        source_submodule.join("local.txt"),
        b"untracked dependency\n",
    )
    .unwrap();
    let destination = parent.path().join("migrated");
    let git_trace = parent.path().join("git-trace.log");
    let source_head = String::from_utf8(git_output(&source, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    let source_status = git_output(
        &source,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let source_index_tree = git_output(&source, &["write-tree"]);

    let first = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        first.status.success(),
        "first migration copy: {}\nGit trace:\n{}",
        String::from_utf8_lossy(&first.stderr),
        fs::read_to_string(&git_trace).unwrap_or_else(|error| format!("<unavailable: {error}>")),
    );
    assert!(first.stderr.is_empty());
    let first_output = String::from_utf8(first.stdout).unwrap();
    assert!(first_output.starts_with(
        "prepared or recovered format-3 migration copy: source_format=1 source_commit="
    ));
    assert_eq!(git_output(&destination, &["write-tree"]), source_index_tree);
    assert!(
        git_output(
            &destination,
            &["ls-files", "--stage", "--", "tracked-deletion.txt"]
        )
        .is_empty()
    );
    assert_eq!(
        git_output(
            &destination,
            &[
                "diff",
                "--cached",
                "--name-status",
                "--",
                "tracked-deletion.txt"
            ],
        ),
        b"D\ttracked-deletion.txt\n"
    );
    let copied_submodule = destination.join("dependencies/example");
    assert_eq!(
        git_output(&copied_submodule, &["show", ":tracked.txt"]),
        b"staged dependency\n"
    );
    assert_eq!(
        fs::read(copied_submodule.join("tracked.txt")).unwrap(),
        b"unstaged dependency\n"
    );
    assert_eq!(
        fs::read(copied_submodule.join("local.txt")).unwrap(),
        b"untracked dependency\n"
    );

    let candidate = String::from_utf8(git_output(
        &destination,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/orna/migration-candidates/",
        ],
    ))
    .unwrap();
    let candidate = candidate.trim();
    assert!(
        !candidate.is_empty(),
        "the copy pins its prepared candidate"
    );
    assert_eq!(
        git_output(
            &destination,
            &["show", "-s", "--format=%an <%ae>", candidate],
        ),
        b"Orna migration <migration@orna.invalid>\n",
        "the migration commit identity is deterministic, not inherited from the host"
    );
    let retry = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        retry.status.success(),
        "migration retry: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(retry.stdout, first_output.as_bytes());
    assert!(retry.stderr.is_empty());
    fs::write(
        source.join("new-source.txt"),
        b"changed after preparation\n",
    )
    .unwrap();
    let changed_source = run_copy(&source, &destination, "0", &git_trace);
    assert_eq!(changed_source.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&changed_source.stderr).contains("E2100"));
    assert_eq!(
        String::from_utf8(git_output(
            &destination,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/orna/migration-candidates/"
            ],
        ))
        .unwrap()
        .trim(),
        candidate,
        "rejecting a changed source leaves the candidate intact"
    );
    fs::remove_file(source.join("new-source.txt")).unwrap();

    git_output(&source, &["reset", "--", "tracked-deletion.txt"]);
    let changed_source_index = run_copy(&source, &destination, "0", &git_trace);
    assert_eq!(changed_source_index.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&changed_source_index.stderr).contains("E2100"));
    git_output(&source, &["rm", "--", "tracked-deletion.txt"]);

    git_output(&copied_submodule, &["reset", "--", "tracked.txt"]);
    assert_eq!(
        git_output(&copied_submodule, &["show", ":tracked.txt"]),
        b"dependency committed\n"
    );
    let altered_candidate = run_copy(&source, &destination, "0", &git_trace);
    assert_eq!(altered_candidate.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&altered_candidate.stderr).contains("E2100"));
    assert_eq!(
        git_output(&copied_submodule, &["show", ":tracked.txt"]),
        b"dependency committed\n",
        "rejecting an altered candidate leaves its submodule index untouched"
    );
    assert_eq!(
        String::from_utf8(git_output(
            &destination,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/orna/migration-candidates/"
            ],
        ))
        .unwrap()
        .trim(),
        candidate
    );

    let metadata_destination = parent.path().join("metadata-state");
    let metadata_copy = run_copy(&source, &metadata_destination, "0", &git_trace);
    assert!(
        metadata_copy.status.success(),
        "metadata-state migration copy: {}",
        String::from_utf8_lossy(&metadata_copy.stderr)
    );
    let metadata_submodule = metadata_destination.join("dependencies/example");
    let source_submodule_head = git_output(&source_submodule, &["rev-parse", "HEAD"]);
    git_output(&metadata_submodule, &["branch", "alternate"]);
    git_output(
        &metadata_submodule,
        &["symbolic-ref", "HEAD", "refs/heads/alternate"],
    );
    assert_eq!(
        git_output(&metadata_submodule, &["rev-parse", "HEAD"]),
        source_submodule_head,
        "the active submodule commit is unchanged"
    );
    let changed_head = run_copy(&source, &metadata_destination, "0", &git_trace);
    assert_eq!(changed_head.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&changed_head.stderr).contains("E2100"));
    git_output(
        &metadata_submodule,
        &["symbolic-ref", "HEAD", "refs/heads/main"],
    );
    git_output(
        &metadata_submodule,
        &["update-ref", "-d", "refs/heads/alternate"],
    );

    let shared_index = String::from_utf8(git_output(
        &metadata_submodule,
        &["rev-parse", "--shared-index-path"],
    ))
    .unwrap();
    assert!(!shared_index.trim().is_empty(), "source uses a split index");
    let shared_index = Path::new(shared_index.trim());
    let shared_index = if shared_index.is_absolute() {
        shared_index.to_path_buf()
    } else {
        metadata_submodule.join(shared_index)
    };
    let original_shared_index = fs::read(&shared_index).unwrap();
    let changed_shared_index = [original_shared_index.as_slice(), b"changed"].concat();
    fs::write(&shared_index, &changed_shared_index).unwrap();
    let changed_split_index = run_copy(&source, &metadata_destination, "0", &git_trace);
    assert_eq!(changed_split_index.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&changed_split_index.stderr).contains("E2100"));
    assert_eq!(fs::read(&shared_index).unwrap(), changed_shared_index);
    let conflict = run_copy(&source, &destination, "1", &git_trace);
    assert_eq!(conflict.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("E2100"));
    let foreign = parent.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("marker"), b"foreign destination\n").unwrap();
    let foreign_copy = run_copy(&source, &foreign, "0", &git_trace);
    assert_eq!(foreign_copy.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&foreign_copy.stderr).contains("E2100"));
    assert_eq!(
        fs::read(foreign.join("marker")).unwrap(),
        b"foreign destination\n",
        "a foreign destination is never overwritten"
    );
    assert_eq!(
        String::from_utf8(git_output(&source, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim(),
        source_head
    );
    assert_eq!(
        git_output(
            &source,
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_status
    );
}

#[test]
fn cli_migration_copy_rejects_mixed_worktree_metadata() {
    for (label, metadata_file, marker) in [
        ("mixed", ".orna/format.orna", &b"format 3\n"[..]),
        ("unknown", ".orna/format.orna", &b"format 99\n"[..]),
        (
            "changed database identity",
            ".orna/database.orna",
            &b"{database_id: \"00000000-0000-4000-8000-000000000000\"}\n"[..],
        ),
    ] {
        let parent = TempDir::new().unwrap();
        let source = parent.path().join("source");
        fs::create_dir(&source).unwrap();
        initialize_legacy_source(&source);
        let source_head = git_output(&source, &["rev-parse", "HEAD"]);
        let source_index = git_output(&source, &["write-tree"]);
        let metadata_path = source.join(metadata_file);
        fs::write(&metadata_path, marker).unwrap();
        let source_status = git_output(
            &source,
            &["status", "--porcelain=v2", "--untracked-files=all"],
        );
        let destination = parent.path().join("migrated");
        let trace = parent.path().join("git-trace.log");

        let output = run_copy(&source, &destination, "0", &trace);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{label} worktree metadata must fail closed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("E2100"));
        assert!(
            !destination.exists(),
            "{label} source must not create a candidate"
        );
        assert_eq!(git_output(&source, &["rev-parse", "HEAD"]), source_head);
        assert_eq!(git_output(&source, &["write-tree"]), source_index);
        assert_eq!(
            git_output(
                &source,
                &["status", "--porcelain=v2", "--untracked-files=all"]
            ),
            source_status
        );
        assert_eq!(fs::read(metadata_path).unwrap(), marker);
    }
}
