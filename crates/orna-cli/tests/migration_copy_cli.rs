use std::{fs, path::Path, process::Command};

use orna_repository::Repository;
use orna_runtime::{Mutation, NoFault, RuntimeState};
use sha2::{Digest, Sha256};
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
    initialize_legacy_source_with_format(
        root,
        include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"),
    );
}

/// Commits a legacy source whose `.orna/format.orna` holds `format_record`,
/// beside the legacy `database_id` sidecar.
fn initialize_legacy_source_with_format(root: &Path, format_record: &str) {
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
    fs::write(root.join(".orna/format.orna"), format_record).unwrap();
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
    Command::new(env!("CARGO_BIN_EXE_orna"))
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

fn run_abort(source: &Path, destination: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            source.to_str().unwrap(),
            "--debug",
            "migrate",
            "abort",
            destination.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

#[test]
#[allow(clippy::too_many_lines)]
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
    let oversized = vec![b'x'; 64 * 1024 + 1];
    for (label, metadata_file, marker) in [
        (
            "mixed",
            ".orna/format.orna",
            &b"{repository_format: 3, storage_profile: \"compact-storage-v1\"}\n"[..],
        ),
        (
            "unknown",
            ".orna/format.orna",
            &b"{repository_format: 99, storage_profile: \"compact-storage-v1\"}\n"[..],
        ),
        (
            "changed database identity",
            ".orna/database.orna",
            &b"{database_id: \"00000000-0000-4000-8000-000000000000\"}\n"[..],
        ),
        (
            "oversized format",
            ".orna/format.orna",
            oversized.as_slice(),
        ),
        (
            "oversized database",
            ".orna/database.orna",
            oversized.as_slice(),
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

#[test]
fn cli_migration_copy_names_unparseable_committed_format_record() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    // The bare `format 1` spelling is never written by a repository writer, so
    // committed metadata holding it must fail with its own cause, not a generic
    // copy failure.
    initialize_legacy_source_with_format(&source, "format 1\n");
    let destination = parent.path().join("migrated");
    let trace = parent.path().join("git-trace.log");

    let output = run_copy(&source, &destination, "0", &trace);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("E2100"), "{stderr}");
    assert!(
        stderr.contains("migration source could not be admitted: repository metadata is invalid"),
        "{stderr}"
    );
    assert!(stderr.contains("ORNA-REPO-CONTEXT-002"), "{stderr}");
    assert!(!destination.exists(), "{stderr}");
}

/// ORNA-UPGRADE-001, ORNA-UPGRADE-002, ORNA-META-002: a source whose
/// `format.orna` holds the format-1 record `OrnaDB` 1.0's `orna init` wrote
/// migrates like any other legacy source. The journal names the committed
/// record bytes as the file the migration removes. The legacy source stays
/// read-only, and only the prepared copy admits a runtime, so `migrate copy`
/// can carry checkpoints into it. The format-2 record is refused by name in
/// `cli_migration_copy_refuses_format_two_source_by_name` (MIGRATION.md:3).
#[test]
fn cli_migration_copy_prepares_a_candidate_from_recorded_legacy_metadata() {
    let (format_record, format) = (
        include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"),
        1,
    );
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source_with_format(&source, format_record);
    let source_head = String::from_utf8(git_output(&source, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    let source_status = git_output(
        &source,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let refusal = Repository::discover(&source)
        .unwrap()
        .require_writable_head_format()
        .expect_err("a legacy source is read-only");
    assert_eq!(refusal.format(), format);
    let destination = parent.path().join("migrated");
    let git_trace = parent.path().join("git-trace.log");

    let output = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        output.status.success(),
        "format-{format} migration copy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
            stdout.starts_with(&format!(
                "prepared or recovered format-3 migration copy: source_format={format} source_commit={source_head} "
            )),
            "{stdout}"
        );
    assert_eq!(
        git_output(
            &source,
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_status
    );

    let migrated = Repository::discover(&destination).unwrap();
    let journal = migrated
        .read_publication_journal()
        .unwrap()
        .expect("the migration copy journals its candidate");
    let format_entry = journal
        .entries()
        .iter()
        .find(|entry| entry.path().as_path() == Path::new(".orna/format.orna"))
        .expect("the journal removes the legacy format record");
    assert_eq!(format_entry.expected(), Some(format_record.as_bytes()));
    assert_eq!(format_entry.next(), None);
    assert_eq!(
        migrated
            .open_format_context()
            .unwrap()
            .repository_format_number(),
        format
    );
    let refusal = migrated
        .require_writable_head_format()
        .expect_err("a prepared copy is not writable through ordinary runtime writes");
    assert_eq!(refusal.format(), format);
    assert_eq!(refusal.code(), "ORNA-REPO-CONTEXT-015");
    assert_eq!(migrated.require_migration_carry_head_format(), Ok(()));
    // With a non-empty migration store, the candidate tracks the sidecar and the
    // store root beside it (ORNA-REPO-004). The no-store candidate also tracks
    // the store root, which the migration_copy tests assert exactly.
    let candidate = journal.new_head().as_str().to_owned();
    assert_eq!(
        String::from_utf8(git_output(
            &destination,
            &["ls-tree", "--name-only", &candidate, "--", ".orna/"],
        ))
        .unwrap(),
        ".orna/database.orna\n.orna/store\n"
    );

    let retry = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        retry.status.success(),
        "format-{format} migration retry: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(
        Repository::discover(&destination)
            .unwrap()
            .read_publication_journal()
            .unwrap(),
        Some(journal)
    );
}

/// ORNA-UPGRADE-003, MIGRATION.md:3, UPGRADE-3 step 3: a format-2 source is
/// refused by name. Its row layout has no production decoder, and replacing
/// its header would present draft data as format 3. The refusal names the
/// missing conversion and writes neither a destination nor source state.
#[test]
fn cli_migration_copy_refuses_format_two_source_by_name() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source_with_format(
        &source,
        include_str!("fixtures/format-context/format-2-draft-1.1.0.orna"),
    );
    let source_status = git_output(
        &source,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let destination = parent.path().join("migrated");
    let git_trace = parent.path().join("git-trace.log");

    let output = run_copy(&source, &destination, "0", &git_trace);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("format-2 row conversion is not implemented"),
        "{stderr}"
    );
    assert!(!destination.exists(), "{stderr}");
    assert_eq!(
        git_output(
            &source,
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_status
    );
}

#[test]
fn cli_migration_copy_runtime_intent_is_identical_across_preparations() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let first = parent.path().join("first");
    let second = parent.path().join("second");
    for destination in [&first, &second] {
        let output = run_copy(&source, destination, "0", &git_trace);
        assert!(
            output.status.success(),
            "migration copy: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let runtime_intent = |destination: &Path| {
        Repository::discover(destination)
            .unwrap()
            .read_publication_journal()
            .unwrap()
            .expect("prepared migration journal")
            .runtime_intent_id()
            .expect("prepared migration runtime intent")
    };
    assert_eq!(
        runtime_intent(first.as_path()),
        runtime_intent(second.as_path())
    );
}

#[test]
fn cli_migration_copy_abort_discards_only_the_prepared_candidate() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let destination = parent.path().join("candidate");
    let prepared = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let source_head = git_output(&source, &["rev-parse", "HEAD"]);
    let source_format = fs::read(source.join(".orna/format.orna")).unwrap();

    // The source worktree, and the directory that contains it, are never a
    // discard target.
    for refused in [source.as_path(), parent.path()] {
        let output = run_abort(&source, refused);
        assert!(!output.status.success(), "abort of {}", refused.display());
    }
    assert!(source.join("main.orna").is_file());

    // An unrelated directory is refused and keeps its contents.
    let unrelated = parent.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("keep.txt"), b"keep\n").unwrap();
    let output = run_abort(&source, &unrelated);
    assert!(!output.status.success(), "abort of an unrelated directory");
    assert_eq!(fs::read(unrelated.join("keep.txt")).unwrap(), b"keep\n");

    let discarded = run_abort(&source, &destination);
    assert!(
        discarded.status.success(),
        "migration abort: {}",
        String::from_utf8_lossy(&discarded.stderr)
    );
    assert!(!destination.exists());
    assert_eq!(git_output(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(
        fs::read(source.join(".orna/format.orna")).unwrap(),
        source_format
    );

    let recopied = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        recopied.status.success(),
        "migration copy after abort: {}",
        String::from_utf8_lossy(&recopied.stderr)
    );
    assert!(
        Repository::discover(&destination)
            .unwrap()
            .read_publication_journal()
            .unwrap()
            .is_some()
    );
}

/// The reserved sibling a discard renames a prepared copy to.
fn quarantine_of(destination: &Path) -> std::path::PathBuf {
    orna_repository::migration_copy::discard_quarantine_path(destination).unwrap()
}

/// Whether `copy` still holds the publication journal of a prepared migration.
fn journal_is_present(copy: &Path) -> bool {
    Repository::discover(copy)
        .unwrap()
        .read_publication_journal()
        .unwrap()
        .is_some()
}

/// Asserts that a migration command refused without panicking: exit status 1
/// and the E2100 diagnostic titled `title`.
fn assert_refused(output: &std::process::Output, title: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("E2100"), "{stderr}");
    assert!(stderr.contains(title), "{stderr}");
}

#[test]
fn cli_migration_copy_abort_finishes_a_discard_interrupted_after_the_quarantine_rename() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let destination = parent.path().join("candidate");
    let prepared = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    // The rename is the commit point: the source may move on afterwards.
    let quarantine = quarantine_of(&destination);
    fs::rename(&destination, &quarantine).unwrap();
    fs::write(
        source.join("notes.txt"),
        b"committed after the commit point\n",
    )
    .unwrap();
    git_output(&source, &["add", "notes.txt"]);
    git_output(
        &source,
        &["commit", "-m", "source moved after the commit point"],
    );

    let finished = run_abort(&source, &destination);
    assert!(
        finished.status.success(),
        "migration abort: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    assert!(
        String::from_utf8_lossy(&finished.stdout)
            .contains("discarded prepared format-3 migration copy")
    );
    assert!(!quarantine.exists());
    assert!(!destination.exists());

    let repeated = run_abort(&source, &destination);
    assert!(
        repeated.status.success(),
        "repeated migration abort: {}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("nothing to discard"));
}

#[test]
fn cli_migration_copy_abort_finishes_a_quarantine_whose_git_metadata_is_gone() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let destination = parent.path().join("candidate");
    let prepared = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let quarantine = quarantine_of(&destination);
    fs::rename(&destination, &quarantine).unwrap();
    // A quarantine is removed without being validated again.
    fs::remove_dir_all(quarantine.join(".git")).unwrap();

    let finished = run_abort(&source, &destination);
    assert!(
        finished.status.success(),
        "migration abort: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    assert!(!quarantine.exists());
}

#[test]
fn cli_migration_copy_abort_of_a_never_prepared_destination_changes_nothing() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let destination = parent.path().join("candidate");
    let entries = |directory: &Path| {
        let mut names: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = entries(parent.path());
    let output = run_abort(&source, &destination);
    assert!(
        output.status.success(),
        "migration abort: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("nothing to discard"));
    assert_eq!(entries(parent.path()), before);
}

#[cfg(unix)]
#[test]
fn cli_migration_copy_abort_refuses_a_symlink_to_the_prepared_candidate() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let destination = parent.path().join("candidate");
    let prepared = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let link = parent.path().join("link");
    std::os::unix::fs::symlink(&destination, &link).unwrap();

    let output = run_abort(&source, &link);
    assert_refused(&output, "migration copy could not be discarded");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(destination.is_dir());
    assert!(journal_is_present(&destination));
}

#[test]
fn cli_migration_copy_abort_refuses_an_unstaged_source_edit_until_it_is_undone() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let destination = parent.path().join("candidate");
    let prepared = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let original = fs::read(source.join("main.orna")).unwrap();
    fs::write(source.join("main.orna"), b"unstaged edit after prepare\n").unwrap();

    let refused = run_abort(&source, &destination);
    assert_refused(&refused, "migration copy could not be discarded");
    assert!(destination.is_dir());
    assert!(journal_is_present(&destination));

    fs::write(source.join("main.orna"), &original).unwrap();
    let discarded = run_abort(&source, &destination);
    assert!(
        discarded.status.success(),
        "migration abort: {}",
        String::from_utf8_lossy(&discarded.stderr)
    );
    assert!(!destination.exists());
}

#[test]
fn cli_migration_copy_abort_refuses_a_source_commit_made_after_prepare() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let destination = parent.path().join("candidate");
    let prepared = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    fs::write(source.join("notes.txt"), b"committed after prepare\n").unwrap();
    git_output(&source, &["add", "notes.txt"]);
    git_output(&source, &["commit", "-m", "source moved after prepare"]);

    let refused = run_abort(&source, &destination);
    assert_refused(&refused, "migration copy could not be discarded");
    assert!(destination.is_dir());
    assert!(journal_is_present(&destination));
}

#[test]
fn cli_migration_copy_leaves_the_source_refs_unchanged() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let destination = parent.path().join("candidate");
    let refs_before = git_output(&source, &["for-each-ref"]);
    let prepared = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let refs_prepared = git_output(&source, &["for-each-ref"]);
    assert_eq!(refs_prepared, refs_before);
    assert!(!String::from_utf8_lossy(&refs_prepared).contains("refs/orna/migration-"));

    let discarded = run_abort(&source, &destination);
    assert!(
        discarded.status.success(),
        "migration abort: {}",
        String::from_utf8_lossy(&discarded.stderr)
    );
    assert_eq!(git_output(&source, &["for-each-ref"]), refs_before);
}

#[test]
fn cli_migration_copy_unstaged_source_edit_changes_the_runtime_intent() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let first = parent.path().join("first");
    let prepared = run_copy(&source, &first, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    fs::write(
        source.join("main.orna"),
        b"unstaged edit after the first prepare\n",
    )
    .unwrap();
    let second = parent.path().join("second");
    let prepared = run_copy(&source, &second, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let runtime_intent = |destination: &Path| {
        Repository::discover(destination)
            .unwrap()
            .read_publication_journal()
            .unwrap()
            .expect("prepared migration journal")
            .runtime_intent_id()
            .expect("prepared migration runtime intent")
    };
    assert_ne!(
        runtime_intent(first.as_path()),
        runtime_intent(second.as_path())
    );
}

#[test]
fn cli_migration_copy_refuses_while_a_discard_quarantine_is_pending() {
    let parent = TempDir::new().unwrap();
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    let git_trace = parent.path().join("git-trace.log");
    let destination = parent.path().join("candidate");
    let prepared = run_copy(&source, &destination, "0", &git_trace);
    assert!(
        prepared.status.success(),
        "migration copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let quarantine = quarantine_of(&destination);
    fs::rename(&destination, &quarantine).unwrap();

    let refused = run_copy(&source, &destination, "0", &git_trace);
    assert_refused(
        &refused,
        "migration copy could not be prepared or recovered",
    );
    assert!(!destination.exists());
    assert!(journal_is_present(&quarantine));

    let discarded = run_abort(&source, &destination);
    assert!(
        discarded.status.success(),
        "migration abort: {}",
        String::from_utf8_lossy(&discarded.stderr)
    );
    assert!(!quarantine.exists());
}

/// The source's pending runtime mutations, read straight from its state database.
/// A source that never opened its runtime has no state database and an empty tail.
fn pending_mutation_count(source: &Path) -> i64 {
    let state_db = Repository::discover(source)
        .unwrap()
        .runtime_paths()
        .state_db();
    if !state_db.exists() {
        return 0;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let database = libsql::Builder::new_local(&state_db).build().await.unwrap();
            let connection = database.connect().unwrap();
            let mut rows = connection
                .query("SELECT COUNT(*) FROM pending_mutation", ())
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
        })
}

/// The bytes of a state database and its WAL and shared-memory sidecars, each
/// `None` when the file is absent. Comparing these proves a refusal wrote
/// nothing to the source runtime, including the sidecars `SQLite` creates on open.
fn state_files(state_db: &Path) -> Vec<Option<Vec<u8>>> {
    ["", "-wal", "-shm"]
        .into_iter()
        .map(|suffix| {
            let path = std::path::PathBuf::from(format!("{}{suffix}", state_db.display()));
            match fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("read {}: {error}", path.display()),
            }
        })
        .collect()
}

/// The bytes of the runtime owner state file, or `None` when it is absent.
/// The owner-lock probe rewrites this file, so a refusal must restore it.
fn owner_state_file(source: &Path) -> Option<Vec<u8>> {
    let path = Repository::discover(source)
        .unwrap()
        .runtime_paths()
        .locks()
        .join("runtime-owner.state");
    match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}

/// Appends one real unpublished mutation to the legacy source through the
/// runtime, under the worktree-keyed source identity the runtime keys its tail by.
fn append_unpublished_mutation(source: &Path) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let repository = Repository::discover(source).unwrap();
            let (identity, digest) =
                orna_runtime::migration_source_runtime_identity(&repository).unwrap();
            let state = RuntimeState::open(&repository, identity, digest)
                .await
                .unwrap();
            let lease = state.acquire_lease([45; 16]).await.unwrap();
            let capture = state.capture().await.unwrap();
            let payload = b"unpublished legacy-source row".to_vec();
            let mutation = Mutation {
                id: [1; 16],
                digest: Sha256::digest(&payload).into(),
                payload,
            };
            state
                .commit(lease, &capture, &mutation, [2; 32], &NoFault)
                .await
                .unwrap();
        });
}

#[test]
fn migration_refuses_a_source_with_unpublished_rows_and_changes_nothing() {
    let parent = TempDir::new().unwrap();
    let git_trace = parent.path().join("git-trace.log");

    // An empty tail still migrates.
    let empty = parent.path().join("empty");
    fs::create_dir(&empty).unwrap();
    initialize_legacy_source(&empty);
    assert_eq!(pending_mutation_count(&empty), 0);
    let empty_destination = parent.path().join("empty-migrated");
    let migrated = run_copy(&empty, &empty_destination, "0", &git_trace);
    assert!(
        migrated.status.success(),
        "migration copy of an empty tail: {}",
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert!(empty_destination.is_dir());

    // A source with an unpublished mutation is refused and left as it was.
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    append_unpublished_mutation(&source);
    assert_eq!(pending_mutation_count(&source), 1);
    let state_db = Repository::discover(&source)
        .unwrap()
        .runtime_paths()
        .state_db();
    let head_before = git_output(&source, &["rev-parse", "HEAD"]);
    let status_before = git_output(
        &source,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let refs_before = git_output(&source, &["for-each-ref"]);
    let state_files_before = state_files(&state_db);
    let owner_state_before = owner_state_file(&source);

    let destination = parent.path().join("refused");
    let refused = run_copy(&source, &destination, "0", &git_trace);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "a source with a tail is refused");
    assert!(
        stderr.contains("unpublished_mutations=1"),
        "the refusal names the count: {stderr}"
    );
    assert!(
        !destination.exists(),
        "no destination is created for a refused source"
    );

    assert_eq!(git_output(&source, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        git_output(
            &source,
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        status_before
    );
    assert_eq!(git_output(&source, &["for-each-ref"]), refs_before);
    for (name, (after, before)) in ["state.db", "state.db-wal", "state.db-shm"]
        .into_iter()
        .zip(state_files(&state_db).iter().zip(state_files_before.iter()))
    {
        assert_eq!(after, before, "{name} changed when the copy was refused");
    }
    assert_eq!(pending_mutation_count(&source), 1);
    assert_eq!(
        owner_state_file(&source),
        owner_state_before,
        "runtime-owner.state changed when the copy was refused"
    );
}

/// A live runtime owner holds the worktree owner lock and its WAL sidecars.
/// The refusal probe runs in another process: it must not take the lock, must
/// not remove those sidecars, and must still refuse the unpublished tail.
#[test]
fn migration_refusal_leaves_a_live_owners_sidecars_intact() {
    let parent = TempDir::new().unwrap();
    let git_trace = parent.path().join("git-trace.log");
    let source = parent.path().join("source");
    fs::create_dir(&source).unwrap();
    initialize_legacy_source(&source);
    append_unpublished_mutation(&source);

    // Keep a runtime open for the whole probe: it owns the lock and the sidecars.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let live = runtime.block_on(async {
        let repository = Repository::discover(&source).unwrap();
        let (identity, digest) =
            orna_runtime::migration_source_runtime_identity(&repository).unwrap();
        RuntimeState::open(&repository, identity, digest)
            .await
            .unwrap()
    });
    let state_db = Repository::discover(&source)
        .unwrap()
        .runtime_paths()
        .state_db();
    let sidecars_before = state_files(&state_db);
    assert!(
        sidecars_before[1].is_some() && sidecars_before[2].is_some(),
        "the live owner holds its WAL and shared-memory sidecars"
    );

    let destination = parent.path().join("refused");
    let refused = run_copy(&source, &destination, "0", &git_trace);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a live source with a tail is refused"
    );
    assert!(
        stderr.contains("unpublished_mutations=1"),
        "the refusal names the count: {stderr}"
    );
    assert!(!destination.exists());
    assert_eq!(
        state_files(&state_db),
        sidecars_before,
        "a live owner's state.db and sidecars are left byte-identical"
    );

    drop(live);
    drop(runtime);
}
