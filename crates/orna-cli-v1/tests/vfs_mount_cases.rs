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
use editor_mount::{
    assert_errno, repository_with_music_rows, repository_with_music_rows_and_modules,
    require_fuse_or_skip, spawn_mount_process, write_and_sync,
};

const EINVAL: i32 = 22;

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

const NOTES_SOURCE: &str = "pub fn answer(): Int = 42;\n";
const EXTRA_SOURCE: &str = "pub fn extra(): Int = 7;\n";

/// ORNA-VFS-020 and profiles/vfs-1.md: a reachable ordinary source module is
/// projected at its logical path with its committed bytes, beside the table
/// directories. Read projection only; writes are not covered here.
#[tokio::test]
async fn vfs_020_reachable_source_module_is_projected_with_committed_bytes() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, _repository, _relation_id, _context) =
        format3::format3_repository_with_context_and_modules(
            "use notes;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
            "Music",
            &[("notes.orna", NOTES_SOURCE)],
        );
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let notes = mountpoint.join("notes.orna");
    mounted.await_path(&notes);

    assert_eq!(read_view(&notes), NOTES_SOURCE, "the source module reads its committed bytes");
    let root = sorted_names(&mountpoint);
    assert!(root.contains(&"notes.orna".to_owned()), "the source module is listed: {root:?}");
    assert!(root.contains(&"Music".to_owned()), "the table directory stays beside it: {root:?}");
}

/// profiles/vfs-1.md:31-33 and ORNA-VFS-020: a source path keeps its plain name on
/// collision. The source directory `Music` stays `Music` and lists only its source
/// entries; the table `Music` is projected under the first-byte escape `%4Dusic` with
/// its rows intact. Projection is read at mount open. Not covered: projection after a
/// later commit changes the rows, since this fixture has no remount helper.
#[tokio::test]
async fn vfs_020_source_path_keeps_name_and_table_projects_escaped() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows_and_modules(
        "use Music.extra;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
        &[("Music/extra.orna", EXTRA_SOURCE)],
        &[("london", music_row("Original").as_str())],
    )
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let extra = mountpoint.join("Music/extra.orna");
    let data = mountpoint.join("%4Dusic/london/data.orna");
    mounted.await_path(&extra);
    mounted.await_path(&data);

    assert_eq!(read_view(&extra), EXTRA_SOURCE, "the colliding source module reads its bytes");
    assert_eq!(
        sorted_names(&mountpoint),
        vec!["%4Dusic".to_owned(), "Music".to_owned(), "main.orna".to_owned()],
        "the source path keeps Music; the table is projected under its escaped name"
    );
    assert_eq!(
        sorted_names(&mountpoint.join("Music")),
        vec!["extra.orna".to_owned()],
        "the source directory lists only its source entries"
    );
    assert!(read_view(&data).contains("Original"), "the escaped table keeps its committed row");
}

/// ORNA-VFS-020 and profiles/vfs-1.md: a source module is editable through the
/// workspace source path. A write-intent write that is synced is accepted, and the
/// accepted bytes are visible through a new open.
#[tokio::test]
async fn vfs_020_source_module_write_is_accepted_and_visible_after_sync() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, _repository, _relation_id, _context) =
        format3::format3_repository_with_context_and_modules(
            "use notes;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
            "Music",
            &[("notes.orna", NOTES_SOURCE)],
        );
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let notes = mountpoint.join("notes.orna");
    mounted.await_path(&notes);

    const EDITED: &str = "// edited through the mount\n";
    write_and_sync(&notes, EDITED).expect("a synced source module write is accepted");
    assert_eq!(read_view(&notes), EDITED, "the accepted source edit is visible through a new open");
}
