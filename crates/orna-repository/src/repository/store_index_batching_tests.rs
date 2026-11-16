//! Publishing one row must not start Git per row already in the store. Index
//! binding and store-leaf reconciliation batch their work, so the Git processes
//! a publication starts depend on the change, not on how many rows exist.

use crate::git_command;
use crate::test_support;
use crate::*;
use std::fs;

/// Store rows already in HEAD for the small and the large publication.
const SMALL_STORE_ROWS: usize = 200;
const LARGE_STORE_ROWS: usize = 800;

/// The envelope and one payload chunk of store row `row`.
fn row_files(row: usize) -> Vec<(String, Vec<u8>)> {
    vec![
        (
            format!(".orna/store/refs/row-{row:05}/data"),
            format!("envelope {row}\n").into_bytes(),
        ),
        (
            format!(".orna/store/refs/row-{row:05}/chunk-0"),
            format!("payload {row}\n").into_bytes(),
        ),
    ]
}

/// Publishes one new row over a committed store of `rows` rows and returns the
/// number of Git processes the publication started.
fn publication_git_processes(rows: usize) -> usize {
    let root = tempfile::TempDir::new().unwrap();
    test_support::initialize_fixture_repository(root.path());
    test_support::commit_initial_module(root.path());
    let database = include_str!("../../tests/fixtures/format-context/database-final.orna");
    fs::create_dir_all(root.path().join(".orna")).unwrap();
    fs::write(root.path().join(".orna/database.orna"), database).unwrap();
    for row in 0..rows {
        for (path, bytes) in row_files(row) {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }
    test_support::git_ok(root.path(), &["add", ".orna"]);
    test_support::git_ok(root.path(), &["commit", "-m", "store rows"]);

    let repository = Repository::discover(root.path()).unwrap();
    let old_head = repository.head().unwrap().unwrap();
    let index = repository.index_generation().unwrap();
    let changes = row_files(rows)
        .into_iter()
        .map(|(path, bytes)| (ManagedPath::new(&path).unwrap(), bytes))
        .collect::<Vec<_>>();
    let managed = changes
        .iter()
        .map(|(path, bytes)| ManagedFileChange::new(path.clone(), Some(bytes.clone())))
        .collect::<Vec<_>>();
    let candidate = repository
        .build_private_commit(&old_head, &managed, "orna: publish store row")
        .unwrap();
    let store_root = test_support::git(
        root.path(),
        &[
            "rev-parse",
            &format!("{}:.orna/store", candidate.commit().as_str()),
        ],
    );
    let algorithm = if store_root.len() == 64 {
        GitHashAlgorithm::Sha256
    } else {
        GitHashAlgorithm::Sha1
    };
    let entries = changes
        .iter()
        .map(|(path, bytes)| PublicationJournalEntry::new(path.clone(), None, Some(bytes.clone())))
        .collect();
    let mut journal = PublicationJournal::new_with_runtime_intent(
        old_head,
        candidate.commit().clone(),
        index.tree().unwrap().clone(),
        [42; 16],
        entries,
    )
    .unwrap()
    .with_store_root(&NativeOid::from_hex(algorithm, &store_root).unwrap())
    .unwrap();

    git_command::reset_git_process_requests();
    let published = repository
        .publish_candidate(&index, &candidate, &mut journal)
        .unwrap();
    let processes = git_command::git_process_requests();

    assert_eq!(
        test_support::git(root.path(), &["rev-parse", "HEAD"]),
        candidate.commit().as_str()
    );
    assert_eq!(
        published.tree().map(|tree| tree.as_str()),
        Some(candidate.tree().as_str())
    );
    assert_eq!(
        test_support::git(root.path(), &["write-tree"]),
        candidate.tree().as_str()
    );
    processes
}

#[test]
fn publishing_one_row_starts_the_same_git_processes_for_any_store_size() {
    let small = publication_git_processes(SMALL_STORE_ROWS);
    let large = publication_git_processes(LARGE_STORE_ROWS);
    assert_eq!(
        small, large,
        "publication started {small} Git processes over {SMALL_STORE_ROWS} store rows \
         and {large} over {LARGE_STORE_ROWS}"
    );
}
