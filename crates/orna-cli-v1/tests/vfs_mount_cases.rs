#![cfg(target_os = "linux")]

//! FUSE mount cases for ORNA-VFS-017 (lazy/pinned directory iteration),
//! ORNA-VFS-019 (cache coherence and handle identity), and ORNA-VFS-020
//! (multi-file source saves). The nested-collection directory case from
//! `profiles/vfs-1.md` is tracked by bead ornadb-5il1.464 and is not a test yet.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use tempfile::TempDir;

#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;
#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;
#[allow(dead_code)]
#[path = "support/editor_mount.rs"]
mod editor_mount;
use editor_mount::{assert_errno, repository_with_music_rows, require_fuse_or_skip, retained_drafts, spawn_mount_process, write_and_sync};

const EINVAL: i32 = 22;
const EPERM: i32 = 1;

fn music_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn read_view(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read accepted state through a new open")
}

fn sorted_names(directory: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .expect("list the collection directory")
        .map(|entry| entry.expect("directory entry").file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// ORNA-VFS-015 row-document coverage (not ORNA-VFS-020). Saves to several row
/// documents under one mount are each admitted at their own sync, in the order
/// the caller performs them. A rejected later save does not roll back an
/// earlier accepted one. No cross-file transaction is asserted. Grouped source
/// edits and no-evaluation-during-speculative-writes (ORNA-VFS-020 proper) are
/// NOT covered; see bead ornadb-9npn.
#[tokio::test]
async fn vfs_020_multi_file_saves_are_admitted_in_order_without_cross_file_rollback() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&[
        ("london", music_row("Original").as_str()),
        ("paris", music_row("Paris").as_str()),
    ])
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let london = mountpoint.join("Music/london/data.orna");
    let paris = mountpoint.join("Music/paris/data.orna");
    mounted.await_path(&london);
    mounted.await_path(&paris);

    write_and_sync(&london, &music_row("London edit")).expect("first save is admitted");
    assert!(read_view(&london).contains("London edit"));
    write_and_sync(&paris, &music_row("Paris edit")).expect("second save is admitted");
    assert!(read_view(&paris).contains("Paris edit"));
    assert!(
        read_view(&london).contains("London edit"),
        "the second save must not roll back the first"
    );

    let invalid = "{ title: \"Bad\", artist: 123, }\n";
    assert_errno(
        write_and_sync(&paris, invalid),
        &[EINVAL],
        "an invalid candidate for Music/paris is refused at sync with EINVAL",
    );
    assert!(read_view(&paris).contains("Paris edit"), "rejected candidate keeps accepted paris");
    assert!(read_view(&london).contains("London edit"), "rejected paris save keeps accepted london");
}

/// ORNA-VFS-020 conservative guard only. Grouped source edits and candidate
/// validation of source modules are NOT implemented: the mount projects no
/// source module (see bead ornadb-9npn). This pins that a source-shaped name
/// cannot be created through the mount at the root or inside a table directory:
/// the create is refused with EPERM, nothing is written to the backing worktree,
/// no draft is retained and HEAD does not move.
#[tokio::test]
async fn vfs_020_unprojected_source_names_are_refused_without_writes() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, repository, _relation_id) =
        repository_with_music_rows(&[("london", music_row("Original").as_str())]).await;
    let head_before = repository.head().unwrap().unwrap();
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    mounted.await_path(&mountpoint.join("Music/london/data.orna"));
    let root_names_before = sorted_names(&mountpoint);
    let music_names_before = sorted_names(&mountpoint.join("Music"));

    let root_candidate = mountpoint.join("Workspace.orna");
    let table_candidate = mountpoint.join("Music/source.orna");
    for candidate in [&root_candidate, &table_candidate] {
        assert_errno(
            write_and_sync(candidate, "{ title: \"Source\", }\n"),
            &[EPERM],
            "a source-shaped name is not a projected source module and is refused",
        );
        assert!(!candidate.exists(), "a refused source-shaped name must not appear");
    }
    assert_eq!(sorted_names(&mountpoint), root_names_before, "root listing is unchanged");
    assert_eq!(sorted_names(&mountpoint.join("Music")), music_names_before, "table listing is unchanged");
    assert!(
        !repository_directory.path().join("Workspace.orna").exists(),
        "a refused source-shaped name must not reach the backing worktree"
    );
    assert!(retained_drafts(&repository).is_empty(), "a refused create retains no draft");
    assert_eq!(repository.head().unwrap().unwrap(), head_before, "HEAD does not move");
}

/// ORNA-VFS-019 (first half only): an open read handle keeps its captured row
/// and never retargets to another row after a sibling row is rewritten through
/// the mount. A fresh open sees the write. NOT covered: the cache-invalidation
/// and reorder halves of VFS-019, and the schema-change remount half.
#[tokio::test]
async fn vfs_019_open_handle_does_not_retarget_after_sibling_row_write() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&[
        ("london", music_row("Original").as_str()),
        ("paris", music_row("Paris").as_str()),
    ])
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let london = mountpoint.join("Music/london/data.orna");
    let paris = mountpoint.join("Music/paris/data.orna");
    mounted.await_path(&london);
    mounted.await_path(&paris);
    let music = mountpoint.join("Music");
    let listing_before = sorted_names(&music);

    let mut paris_handle = File::open(&paris).expect("open paris read-only");
    write_and_sync(&london, &music_row("London edit")).expect("sibling row save is admitted");

    paris_handle.seek(SeekFrom::Start(0)).unwrap();
    let mut through_handle = String::new();
    paris_handle.read_to_string(&mut through_handle).unwrap();
    assert!(
        through_handle.contains("Paris") && !through_handle.contains("London edit"),
        "an open handle stays on its captured row: {through_handle:?}"
    );
    assert_eq!(sorted_names(&music), listing_before, "the Music listing keeps its row set");
    assert!(read_view(&london).contains("London edit"), "a fresh open sees the write");
}

/// ORNA-VFS-017, multi-page listing consistency only: over 512 rows (more than
/// one `ROW_PAGE_LIMIT` = 256 page), a known late key resolves and two directory
/// passes list the same row set. This does not prove laziness or page-cursor
/// behaviour from outside the daemon; that is covered by unit test
/// `workspace_readdir_uses_lazy_indexed_pages`.
#[tokio::test]
async fn vfs_017_large_collection_lookup_and_pinned_listing() {
    if !require_fuse_or_skip() {
        return;
    }
    const ROWS: usize = 512;
    let keys = (0..ROWS).map(|index| format!("row-{index:03}")).collect::<Vec<_>>();
    let documents = (0..ROWS).map(|index| music_row(&format!("title-{index:03}"))).collect::<Vec<_>>();
    let rows = keys
        .iter()
        .zip(&documents)
        .map(|(key, document)| (key.as_str(), document.as_str()))
        .collect::<Vec<_>>();
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&rows).await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let music = mountpoint.join("Music");
    let last = music.join("row-511/data.orna");
    mounted.await_path(&last);

    let known = read_view(&last);
    assert!(known.contains("title-511"), "a known late key resolves through the index: {known:?}");

    let first = sorted_names(&music);
    let second = sorted_names(&music);
    assert_eq!(first.len(), ROWS, "every row of the multi-page collection is listed");
    assert_eq!(first, second, "two passes over the same mount list the same rows");
}
