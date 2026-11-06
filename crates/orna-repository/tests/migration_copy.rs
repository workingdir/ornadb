#[path = "../src/test_support.rs"]
mod test_support;

use std::{fs, path::Path, process::Command};

use orna_repository::{
    LegacyAnnotationCandidate, ManagedPath, MigrationAnnotationDefaults, MigrationContinuityRecord,
    PublicationJournal, PublicationJournalEntry, Repository, RepositoryError,
    migration_copy::{self, DiscardOutcome, LegacyAnnotationInventory, MigrationStore},
};
use tempfile::TempDir;

/// The migration store of a source that has no legacy rows to convert: no
/// relations, no identities, zero watermarks and no retired legacy paths.
fn no_store(
    _: &Repository,
    _: orna_repository::DatabaseId,
) -> Result<MigrationStore, RepositoryError> {
    Ok(MigrationStore::default())
}

fn git_output(directory: &Path, arguments: &[&str]) -> Vec<u8> {
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
    output.stdout
}

fn git_text(directory: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_output(directory, arguments))
        .unwrap()
        .trim()
        .to_owned()
}

fn init_repository(root: &Path) {
    git_output(root, &["init", "-b", "main"]);
    test_support::configure_fixture_git_identity(root);
    git_output(root, &["config", "commit.gpgsign", "false"]);
}

#[test]
fn populated_format1_dirty_workspace_is_preserved_in_copy() {
    let source = TempDir::new().unwrap();
    init_repository(source.path());
    fs::write(
        source.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(source.path().join("tracked.txt"), b"committed bytes\n").unwrap();
    fs::create_dir_all(source.path().join(".orna")).unwrap();
    fs::write(
        source.path().join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"),
    )
    .unwrap();
    fs::write(
        source.path().join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(source.path(), &["add", "."]);
    git_output(source.path(), &["commit", "-m", "format-1 source"]);

    let dependency = TempDir::new().unwrap();
    init_repository(dependency.path());
    fs::write(
        dependency.path().join("dependency.txt"),
        b"committed dependency\n",
    )
    .unwrap();
    git_output(dependency.path(), &["add", "."]);
    git_output(dependency.path(), &["commit", "-m", "dependency source"]);
    let dependency_url = dependency.path().to_str().unwrap();
    let dependency_add = Command::new("git")
        .current_dir(source.path())
        .args(["-c", "protocol.file.allow=always", "submodule", "add"])
        .arg(dependency_url)
        .arg("dependencies/example")
        .output()
        .unwrap();
    assert!(
        dependency_add.status.success(),
        "git submodule add: {}",
        String::from_utf8_lossy(&dependency_add.stderr)
    );
    git_output(source.path(), &["commit", "-am", "record dependency"]);
    let source_head = git_text(source.path(), &["rev-parse", "HEAD"]);

    // Keep a distinct staged tree and worktree, plus untracked, ignored and
    // dirty submodule content. Preparing the copy may read this state but must
    // neither settle nor overwrite any part of the source workspace.
    fs::write(source.path().join("tracked.txt"), b"staged bytes\n").unwrap();
    git_output(source.path(), &["add", "tracked.txt"]);
    fs::write(source.path().join("tracked.txt"), b"unstaged bytes\n").unwrap();
    fs::write(source.path().join("new-source.txt"), b"untracked source\n").unwrap();
    fs::write(
        source.path().join("intent-to-add.txt"),
        b"intent-to-add bytes\n",
    )
    .unwrap();
    git_output(source.path(), &["add", "-N", "intent-to-add.txt"]);
    fs::write(source.path().join("new-executable.sh"), b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(
            source.path().join("new-executable.sh"),
            fs::Permissions::from_mode(0o751),
        )
        .unwrap();
    }
    fs::write(source.path().join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir_all(source.path().join("ignored")).unwrap();
    fs::write(
        source.path().join("ignored/data.bin"),
        b"ignored source bytes",
    )
    .unwrap();
    let submodule = source.path().join("dependencies/example");
    fs::write(submodule.join("dependency.txt"), b"staged dependency\n").unwrap();
    git_output(&submodule, &["add", "dependency.txt"]);
    fs::write(submodule.join("dependency.txt"), b"unstaged dependency\n").unwrap();
    fs::write(submodule.join("local.txt"), b"untracked dependency\n").unwrap();

    let repository = Repository::discover(source.path()).unwrap();
    let source_database_id = repository
        .open_format_context()
        .unwrap()
        .database_id()
        .unwrap();
    let source_index = repository.index_generation().unwrap();
    let staged_tree = source_index.tree().unwrap().as_str().to_owned();
    let source_status = git_output(
        source.path(),
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let committed_bytes = git_output(
        source.path(),
        &["show", &format!("{source_head}:tracked.txt")],
    );
    let intent_to_add_index = git_output(
        source.path(),
        &["ls-files", "--stage", "--", "intent-to-add.txt"],
    );
    let intent_to_add_fields = String::from_utf8(intent_to_add_index.clone()).unwrap();
    let intent_to_add_fields = intent_to_add_fields
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(intent_to_add_fields.len(), 4);
    assert_eq!(intent_to_add_fields[0], "100644");
    let source_status_text = String::from_utf8(source_status.clone()).unwrap();
    assert!(
        source_status_text
            .lines()
            .any(|line| line.starts_with("1 .A ") && line.ends_with(" intent-to-add.txt")),
        "git add -N must remain an intent-to-add worktree change: {source_status_text:?}"
    );
    assert_eq!(intent_to_add_fields[2], "0");
    assert_eq!(intent_to_add_fields[3], "intent-to-add.txt");
    let destination_parent = TempDir::new().unwrap();
    let destination = destination_parent.path().join("migrated");
    let defaults = MigrationAnnotationDefaults::new(1).unwrap();

    let copy = migration_copy::prepare_format3_migration(
        &repository,
        &destination,
        MigrationContinuityRecord::new(Vec::new()).unwrap(),
        defaults,
        &no_store,
    )
    .unwrap();
    assert_eq!(copy.database_id(), source_database_id);
    let expected_database = include_str!("fixtures/format-context/database-template.orna").replace(
        "00000000-0000-4000-8000-000000000000",
        &source_database_id.to_string(),
    );
    let candidate_database = git_output(
        copy.destination(),
        &[
            "show",
            &format!(
                "{}:.orna/database.orna",
                copy.candidate_journal().new_head().as_str()
            ),
        ],
    );
    assert_eq!(candidate_database, expected_database.as_bytes());

    assert_eq!(repository.head().unwrap().unwrap().as_str(), source_head);
    assert_eq!(repository.index_generation().unwrap(), source_index);
    assert_eq!(
        git_output(
            source.path(),
            &["status", "--porcelain=v2", "--untracked-files=all"],
        ),
        source_status,
        "migration preparation leaves every staged, unstaged and untracked source change intact"
    );
    assert_eq!(
        git_output(
            source.path(),
            &["ls-files", "--stage", "--", "intent-to-add.txt"],
        ),
        intent_to_add_index,
        "preparing the copy leaves the source intent-to-add entry unchanged"
    );
    assert_eq!(
        fs::read(source.path().join("intent-to-add.txt")).unwrap(),
        b"intent-to-add bytes\n"
    );
    assert_eq!(
        git_output(source.path(), &["show", ":tracked.txt"]),
        b"staged bytes\n",
        "the staged source blob remains in the original index"
    );
    assert_eq!(
        fs::read(source.path().join("tracked.txt")).unwrap(),
        b"unstaged bytes\n"
    );
    assert_eq!(
        fs::read(submodule.join("dependency.txt")).unwrap(),
        b"unstaged dependency\n"
    );
    assert_eq!(
        fs::read(submodule.join("local.txt")).unwrap(),
        b"untracked dependency\n"
    );

    let migrated = Repository::discover(copy.destination()).unwrap();
    let migrated_head = migrated.head().unwrap().unwrap();
    assert_eq!(migrated_head.as_str(), source_head);
    assert_eq!(copy.source_commit().as_str(), source_head);
    assert_eq!(
        migrated
            .open_format_context()
            .unwrap()
            .repository_format_number(),
        1,
        "the old commit remains readable under its recorded format"
    );
    assert_eq!(
        git_output(
            copy.destination(),
            &["show", &format!("{}:tracked.txt", migrated_head.as_str())],
        ),
        committed_bytes,
        "the old committed blob keeps its byte identity in the copy"
    );
    assert_eq!(
        git_output(copy.destination(), &["show", ":tracked.txt"]),
        b"staged bytes\n",
        "the copy preserves the staged source blob separately from the worktree"
    );
    assert_eq!(
        git_output(
            copy.destination(),
            &["ls-files", "--stage", "--", "intent-to-add.txt"],
        ),
        intent_to_add_index,
        "the copy retains the intent-to-add index entry"
    );
    assert_eq!(
        fs::read(copy.destination().join("intent-to-add.txt")).unwrap(),
        b"intent-to-add bytes\n",
        "the intent-to-add worktree bytes are copied separately"
    );
    assert_eq!(copy.source_index_generation(), &source_index);
    assert_eq!(
        copy.source_index_generation().tree().unwrap().as_str(),
        staged_tree
    );
    assert_eq!(
        fs::read(copy.destination().join("tracked.txt")).unwrap(),
        b"unstaged bytes\n"
    );
    assert_eq!(
        fs::read(copy.destination().join("new-source.txt")).unwrap(),
        b"untracked source\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(copy.destination().join("new-executable.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o751,
            "copying dirty source content preserves executable permissions"
        );
    }
    assert_eq!(
        fs::read(copy.destination().join("ignored/data.bin")).unwrap(),
        b"ignored source bytes"
    );
    assert_eq!(
        fs::read(
            copy.destination()
                .join("dependencies/example/dependency.txt")
        )
        .unwrap(),
        b"unstaged dependency\n"
    );
    assert_eq!(
        fs::read(copy.destination().join("dependencies/example/local.txt")).unwrap(),
        b"untracked dependency\n"
    );
    assert_eq!(copy.annotation_defaults(), defaults);
    assert_eq!(
        copy.annotation_defaults().entry_count(),
        1,
        "legacy annotation migration is a new semantic coordinate"
    );
    assert!(!copy.annotation_defaults().payload_inspected());
}
#[test]
fn populated_format2_dirty_workspace_is_preserved_in_copy() {
    let source = TempDir::new().unwrap();
    init_repository(source.path());
    fs::write(
        source.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(
        source.path().join("tracked.txt"),
        b"format-2 committed bytes\n",
    )
    .unwrap();
    fs::create_dir_all(source.path().join(".orna")).unwrap();
    fs::write(
        source.path().join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-2-draft-1.1.0.orna"),
    )
    .unwrap();
    fs::write(
        source.path().join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(source.path(), &["add", "."]);
    git_output(source.path(), &["commit", "-m", "format-2 source"]);

    let dependency = TempDir::new().unwrap();
    init_repository(dependency.path());
    fs::write(
        dependency.path().join("dependency.txt"),
        b"committed dependency\n",
    )
    .unwrap();
    git_output(dependency.path(), &["add", "."]);
    git_output(dependency.path(), &["commit", "-m", "dependency source"]);
    let dependency_url = dependency.path().to_str().unwrap();
    let added = Command::new("git")
        .current_dir(source.path())
        .args(["-c", "protocol.file.allow=always", "submodule", "add"])
        .arg(dependency_url)
        .arg("dependencies/example")
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "git submodule add: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    git_output(source.path(), &["commit", "-am", "record dependency"]);
    let source_head = git_text(source.path(), &["rev-parse", "HEAD"]);

    fs::write(source.path().join("tracked.txt"), b"staged bytes\n").unwrap();
    git_output(source.path(), &["add", "tracked.txt"]);
    fs::write(source.path().join("tracked.txt"), b"unstaged bytes\n").unwrap();
    fs::write(source.path().join("new-source.txt"), b"untracked source\n").unwrap();
    let submodule = source.path().join("dependencies/example");
    fs::write(submodule.join("dependency.txt"), b"staged dependency\n").unwrap();
    git_output(&submodule, &["add", "dependency.txt"]);
    fs::write(submodule.join("dependency.txt"), b"unstaged dependency\n").unwrap();
    fs::write(submodule.join("local.txt"), b"untracked dependency\n").unwrap();

    let repository = Repository::discover(source.path()).unwrap();
    let source_submodule_status = git_output(
        &submodule,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let source_submodule_stage = git_output(&submodule, &["show", ":dependency.txt"]);
    let source_gitlink_stage = git_output(
        source.path(),
        &["ls-files", "--stage", "--", "dependencies/example"],
    );
    let source_format_bytes = git_output(
        source.path(),
        &["show", &format!("{source_head}:.orna/format.orna")],
    );
    assert_ne!(
        include_bytes!("fixtures/format-context/format-2-draft-1.1.0.orna").as_slice(),
        include_bytes!("fixtures/format-context/format-1-ornadb-1.0.orna").as_slice()
    );
    assert_eq!(
        source_format_bytes,
        include_bytes!("fixtures/format-context/format-2-draft-1.1.0.orna")
    );
    let source_status = git_output(
        source.path(),
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let source_index = repository.index_generation().unwrap();
    let committed_bytes = git_output(
        source.path(),
        &["show", &format!("{source_head}:tracked.txt")],
    );
    let committed_format_bytes = source_format_bytes.clone();
    let destination_parent = TempDir::new().unwrap();
    let destination = destination_parent.path().join("migrated");
    let annotation_inventory =
        LegacyAnnotationInventory::Present(vec![LegacyAnnotationCandidate::new(
            b"unlabeled-source-row".to_vec(),
            None,
            None,
        )]);
    assert!(annotation_inventory.is_available());
    assert_eq!(annotation_inventory.rows()[0].annotation_media_type(), None);
    assert_eq!(annotation_inventory.rows()[0].suffix(), None);
    let defaults = annotation_inventory.annotation_defaults().unwrap();
    let continuity = MigrationContinuityRecord::new(Vec::new()).unwrap();
    let copy = migration_copy::prepare_format3_migration(
        &repository,
        &destination,
        continuity.clone(),
        defaults,
        &no_store,
    )
    .unwrap();
    let migrated = Repository::discover(copy.destination()).unwrap();
    let migrated_head = migrated.head().unwrap().unwrap();
    assert_eq!(
        git_output(
            source.path(),
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_status
    );
    assert_eq!(
        git_output(
            &submodule,
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_submodule_status
    );
    assert_eq!(
        git_output(&submodule, &["show", ":dependency.txt"]),
        source_submodule_stage
    );
    assert_eq!(
        git_output(
            source.path(),
            &["ls-files", "--stage", "--", "dependencies/example"]
        ),
        source_gitlink_stage
    );
    assert_eq!(
        fs::read(source.path().join("tracked.txt")).unwrap(),
        b"unstaged bytes\n"
    );
    assert_eq!(
        fs::read(submodule.join("dependency.txt")).unwrap(),
        b"unstaged dependency\n"
    );
    assert_eq!(
        fs::read(submodule.join("local.txt")).unwrap(),
        b"untracked dependency\n"
    );

    assert_eq!(copy.source_format_number(), 2);
    assert_eq!(format!("{:?}", copy.source_format()), "Legacy2");
    assert_eq!(
        copy.candidate_journal().migration_continuity(),
        Some(&continuity)
    );
    assert_eq!(
        copy.candidate_journal().migration_annotation_defaults(),
        Some(defaults)
    );
    assert_eq!(copy.continuity(), &continuity);
    let legacy_format_entry = copy
        .candidate_journal()
        .entries()
        .iter()
        .find(|entry| entry.path().as_path() == Path::new(".orna/format.orna"))
        .unwrap();
    assert_eq!(
        legacy_format_entry.expected(),
        Some(include_bytes!("fixtures/format-context/format-2-draft-1.1.0.orna").as_slice())
    );
    assert_eq!(legacy_format_entry.next(), None);
    assert_eq!(
        git_output(
            copy.destination(),
            &["show", &format!("{}:.orna/format.orna", source_head)]
        ),
        committed_format_bytes
    );
    // ORNA-META-001: the candidate tracks the format-3 database record and the
    // store root beside it under `.orna/`. The migration coordinates are not a
    // tracked file: the journal carries continuity and annotation defaults, and
    // the commit trailer names the source format.
    let candidate = copy.candidate_journal().new_head().as_str().to_owned();
    assert_eq!(
        git_text(
            copy.destination(),
            &["ls-tree", "--name-only", &candidate, "--", ".orna/"]
        ),
        ".orna/database.orna\n.orna/store"
    );
    let message = git_text(
        copy.destination(),
        &["log", "-1", "--format=%B", &candidate],
    );
    assert!(message.contains("Orna-Migration: format-2-to-3"));
    assert!(message.contains("1 rows take the MIME-1 default annotation"));
    assert_eq!(
        copy.candidate_journal()
            .entries()
            .iter()
            .map(|entry| entry.path().as_path().to_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec![
            ".orna/database.orna".to_owned(),
            ".orna/format.orna".to_owned()
        ]
    );
    let copy_submodule = copy.destination().join("dependencies/example");
    assert_eq!(
        git_output(&copy_submodule, &["show", ":dependency.txt"]),
        source_submodule_stage
    );
    assert_eq!(
        git_output(
            copy.destination(),
            &["ls-files", "--stage", "--", "dependencies/example"]
        ),
        source_gitlink_stage
    );
    assert_eq!(migrated_head.as_str(), source_head);
    assert_eq!(copy.source_commit().as_str(), source_head);
    assert_eq!(
        migrated
            .open_format_context()
            .unwrap()
            .repository_format_number(),
        2,
        "the old commit remains readable under its recorded format"
    );
    assert_eq!(
        git_output(
            copy.destination(),
            &["show", &format!("{}:tracked.txt", migrated_head.as_str())]
        ),
        committed_bytes,
        "the old committed blob keeps its byte identity in the copy"
    );
    assert_eq!(
        git_output(copy.destination(), &["show", ":tracked.txt"]),
        b"staged bytes\n"
    );
    assert_eq!(
        fs::read(copy.destination().join("tracked.txt")).unwrap(),
        b"unstaged bytes\n"
    );
    assert_eq!(
        fs::read(copy.destination().join("new-source.txt")).unwrap(),
        b"untracked source\n"
    );
    assert_eq!(
        fs::read(
            copy.destination()
                .join("dependencies/example/dependency.txt")
        )
        .unwrap(),
        b"unstaged dependency\n"
    );
    assert_eq!(
        fs::read(copy.destination().join("dependencies/example/local.txt")).unwrap(),
        b"untracked dependency\n"
    );
    assert_eq!(copy.source_index_generation(), &source_index);
    assert_eq!(copy.annotation_defaults(), defaults);
    assert_eq!(
        copy.annotation_defaults().entry_count(),
        1,
        "legacy annotation migration is a new semantic coordinate"
    );
    assert!(!copy.annotation_defaults().payload_inspected());
    // MIGRATION.md items 5 and 20-23: metadata-derived unlabeled-row defaults
    // are a new interpretation coordinate, distinct from unchanged raw bytes.
    let recovered = migration_copy::prepare_format3_migration(
        &repository,
        &destination,
        continuity.clone(),
        defaults,
        &no_store,
    )
    .unwrap();
    assert_eq!(
        recovered.candidate_journal().new_head(),
        copy.candidate_journal().new_head()
    );
    assert_eq!(recovered.source_index_generation(), &source_index);
    assert_eq!(
        git_output(&submodule, &["show", ":dependency.txt"]),
        source_submodule_stage
    );
    assert_eq!(
        git_output(
            source.path(),
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_status
    );

    // Simulate an incomplete/stale destination: recovery must refuse modified
    // prepared-copy contents rather than silently adopting them.
    fs::write(
        copy.destination().join(".orna/migration.json"),
        b"partial migration record\n",
    )
    .unwrap();
    assert!(
        migration_copy::prepare_format3_migration(
            &repository,
            &destination,
            continuity,
            defaults,
            &no_store
        )
        .is_err()
    );
    assert_eq!(
        git_output(
            source.path(),
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_status
    );
}

const MIGRATION_RECORD_PATH: &str = ".orna/migration.json";

fn git_with_index(directory: &Path, index: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_INDEX_FILE", index)
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

/// Rebuilds a freshly prepared copy into the shape a copy prepared before
/// ORNA-META-001 had: the candidate commit tracks `.orna/migration.json`, the
/// journal lists that file as a third entry, and the candidate pin follows the
/// rebuilt commit. The parent, index pin, continuity, and annotation defaults
/// stay the copy's own.
fn make_pre_change_copy(destination: &Path) {
    let repository = Repository::discover(destination).unwrap();
    let journal = repository.read_publication_journal().unwrap().unwrap();
    let old_head = journal.old_head().clone();
    let current_candidate = journal.new_head().clone();

    let scratch = TempDir::new().unwrap();
    let record = scratch.path().join("migration.json");
    let record_bytes = b"{\"source_format\": 2}\n".to_vec();
    fs::write(&record, &record_bytes).unwrap();
    let blob = git_text(
        destination,
        &["hash-object", "-w", record.to_str().unwrap()],
    );
    let index = scratch.path().join("index");
    let tree = git_text(
        destination,
        &[
            "rev-parse",
            &format!("{}^{{tree}}", current_candidate.as_str()),
        ],
    );
    git_with_index(destination, &index, &["read-tree", &tree]);
    git_with_index(
        destination,
        &index,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},{MIGRATION_RECORD_PATH}"),
        ],
    );
    let legacy_tree = git_with_index(destination, &index, &["write-tree"]);
    let legacy_commit = git_text(
        destination,
        &[
            "-c",
            "user.name=orna",
            "-c",
            "user.email=orna@example.invalid",
            "commit-tree",
            &legacy_tree,
            "-p",
            old_head.as_str(),
            "-m",
            "legacy migration candidate",
        ],
    );
    git_text(
        destination,
        &[
            "update-ref",
            &format!("refs/orna/migration-candidates/{legacy_commit}"),
            legacy_commit.as_str(),
        ],
    );
    let legacy = repository.resolve_snapshot(&legacy_commit).unwrap();

    let mut entries = journal.entries().to_vec();
    entries.push(PublicationJournalEntry::new(
        ManagedPath::new(MIGRATION_RECORD_PATH).unwrap(),
        None,
        Some(record_bytes),
    ));
    let rebuilt = PublicationJournal::new_with_runtime_intent(
        old_head,
        legacy.clone(),
        journal.base_index_tree().unwrap().clone(),
        journal.runtime_intent_id().unwrap(),
        entries,
    )
    .unwrap()
    .with_migration_continuity(journal.migration_continuity().unwrap().clone())
    .unwrap()
    .with_migration_annotation_defaults(journal.migration_annotation_defaults().unwrap())
    .unwrap();
    repository.write_publication_journal(&rebuilt).unwrap();
    git_text(
        destination,
        &[
            "update-ref",
            "-d",
            &format!(
                "refs/orna/migration-candidates/{}",
                current_candidate.as_str()
            ),
        ],
    );
}

#[test]
fn pre_change_prepared_copy_is_refused_and_discardable() {
    let source = TempDir::new().unwrap();
    init_repository(source.path());
    fs::write(
        source.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(
        source.path().join("tracked.txt"),
        b"format-2 committed bytes\n",
    )
    .unwrap();
    fs::create_dir_all(source.path().join(".orna")).unwrap();
    fs::write(
        source.path().join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-2-draft-1.1.0.orna"),
    )
    .unwrap();
    fs::write(
        source.path().join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(source.path(), &["add", "."]);
    git_output(source.path(), &["commit", "-m", "format-2 source"]);
    let repository = Repository::discover(source.path()).unwrap();
    let source_status = git_output(
        source.path(),
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let defaults = LegacyAnnotationInventory::Present(vec![LegacyAnnotationCandidate::new(
        b"unlabeled-source-row".to_vec(),
        None,
        None,
    )])
    .annotation_defaults()
    .unwrap();
    let continuity = MigrationContinuityRecord::new(Vec::new()).unwrap();
    let parent = TempDir::new().unwrap();
    let destination = parent.path().join("migrated");
    migration_copy::prepare_format3_migration(
        &repository,
        &destination,
        continuity.clone(),
        defaults,
        &no_store,
    )
    .unwrap();
    make_pre_change_copy(&destination);

    // A restarted preparation meets the old-shape copy: it is refused with a
    // typed error rather than rebuilt or silently accepted.
    let refused = migration_copy::prepare_format3_migration(
        &repository,
        &destination,
        continuity.clone(),
        defaults,
        &no_store,
    )
    .expect_err("the pre-change copy is refused by preparation");
    assert!(matches!(refused, RepositoryError::LegacyMigrationCopy));
    let copy_repository = Repository::discover(&destination).unwrap();
    let recovery = copy_repository
        .recover_publication()
        .expect_err("publication recovery refuses the pre-change copy");
    assert!(matches!(recovery, RepositoryError::LegacyMigrationCopy));

    // Discard still removes the copy, after which a fresh preparation builds
    // the format-3 candidate without the tracked migration record.
    let outcome =
        migration_copy::discard_prepared_format3_migration(&repository, &destination).unwrap();
    assert!(matches!(outcome, DiscardOutcome::Discarded));
    assert!(!destination.exists());
    assert_eq!(
        git_output(
            source.path(),
            &["status", "--porcelain=v2", "--untracked-files=all"]
        ),
        source_status
    );
    let rerun = migration_copy::prepare_format3_migration(
        &repository,
        &destination,
        continuity,
        defaults,
        &no_store,
    )
    .unwrap();
    assert_eq!(
        git_text(
            &destination,
            &[
                "ls-tree",
                "--name-only",
                rerun.candidate_journal().new_head().as_str(),
                "--",
                ".orna/",
            ]
        ),
        ".orna/database.orna\n.orna/store"
    );
}

#[test]
fn source_edited_after_the_store_is_built_is_refused_before_pinning() {
    let source = TempDir::new().unwrap();
    init_repository(source.path());
    fs::write(
        source.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(source.path().join("tracked.txt"), b"committed bytes\n").unwrap();
    fs::create_dir_all(source.path().join(".orna")).unwrap();
    fs::write(
        source.path().join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"),
    )
    .unwrap();
    fs::write(
        source.path().join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(source.path(), &["add", "."]);
    git_output(source.path(), &["commit", "-m", "format-1 source"]);
    let repository = Repository::discover(source.path()).unwrap();
    let source_head = git_text(source.path(), &["rev-parse", "HEAD"]);
    let parent = TempDir::new().unwrap();
    let destination = parent.path().join("migrated");
    let tracked = source.path().join("tracked.txt");
    // The store is built while the source is still free to move; the source is
    // edited between the store build and the pin.
    let edit_source_after_store = |_: &Repository,
                                   _: orna_repository::DatabaseId|
     -> Result<MigrationStore, RepositoryError> {
        fs::write(&tracked, b"edited after the store was built\n").unwrap();
        Ok(MigrationStore::default())
    };
    let error = migration_copy::prepare_format3_migration(
        &repository,
        &destination,
        MigrationContinuityRecord::new(Vec::new()).unwrap(),
        MigrationAnnotationDefaults::new(1).unwrap(),
        &edit_source_after_store,
    )
    .expect_err("a source edited after the store is built is refused");
    assert!(matches!(
        error,
        RepositoryError::PublicationRecoveryConflict
    ));
    assert!(!destination.exists());
    assert_eq!(git_text(source.path(), &["rev-parse", "HEAD"]), source_head);
    assert_eq!(
        fs::read(&tracked).unwrap(),
        b"edited after the store was built\n"
    );
}
