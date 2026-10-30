#![cfg(target_os = "linux")]

//! FUSE mount cases for ORNA-VFS-017 (lazy/pinned directory iteration),
//! ORNA-VFS-019 (cache coherence and handle identity), and ORNA-VFS-020
//! (multi-file source saves). The nested-collection directory case from
//! `profiles/vfs-1.md` is tracked by bead ornadb-5il1.464 and is not a test yet.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::Command,
    time::Duration,
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
    require_fuse_or_skip, retained_drafts, spawn_mount_process, spawn_snapshot_mount_process,
    write_and_sync, MountedProcess,
};

const EPERM: i32 = 1;
const EROFS: i32 = 30;
const EIO: i32 = 5;
const EINVAL: i32 = 22;
const EAGAIN: i32 = 11;

fn music_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn read_view(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read accepted state through a new open")
}

/// The candidate of each retained record. A record is a header, a blank line, and the candidate
/// exactly as it was refused (`orna-vfs-retained-draft 1` in `fuse_mount.rs`).
fn retained_candidates(records: Vec<String>) -> Vec<String> {
    records
        .into_iter()
        .map(|record| {
            let (_, candidate) = record.split_once("\n\n").expect("a record has a header");
            candidate.to_owned()
        })
        .collect()
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
/// edits are covered by `vfs_020_source_module_group_is_admitted_together_or_not_at_all` and
/// `multi_file_source_edit_requires_grouped_validation`;
/// no-evaluation-during-speculative-writes (ORNA-VFS-020 proper) is NOT covered; see bead ornadb-9npn.
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

/// ORNA-VFS-020 committed read path: a large source module reads its committed bytes
/// exactly in 128 KiB ranges, including the short final range. Reads borrow the committed
/// bytes and copy only the requested window, so every range must match the source.
#[tokio::test]
async fn vfs_020_large_source_module_reads_committed_bytes_by_range() {
    if !require_fuse_or_skip() {
        return;
    }
    use std::os::unix::fs::FileExt;
    let module: String = (0..40_000).map(|line| format!("// committed line {line:08}\n")).collect();
    let (repository_directory, _repository, _relation_id, _context) =
        format3::format3_repository_with_context_and_modules(
            "use big;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
            "Music",
            &[("big.orna", module.as_str())],
        );
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let big = mountpoint.join("big.orna");
    mounted.await_path(&big);

    let file = File::open(&big).expect("the large source module opens for reading");
    let expected = module.as_bytes();
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut offset = 0_usize;
    while offset < expected.len() {
        let read = file.read_at(&mut buffer, offset as u64).expect("a range read succeeds");
        assert!(read > 0, "the module ends before its committed length at {offset}");
        assert!(
            buffer[..read] == expected[offset..offset + read],
            "the range at {offset} returns the committed bytes"
        );
        offset += read;
    }
    assert_eq!(file.read_at(&mut buffer, offset as u64).expect("an end-of-file read"), 0);
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

/// ORNA-VFS-020 and 41-vfs.md "Failure/recovery": a malformed source candidate is refused
/// at sync with EINVAL, the draft is kept, and the committed module keeps its bytes.
#[tokio::test]
async fn vfs_020_invalid_source_candidate_is_refused_with_einval_and_committed_bytes_kept() {
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

    assert_errno(
        write_and_sync(&notes, "pub fn broken( {\n"),
        &[EINVAL],
        "a malformed source candidate is refused at sync with EINVAL",
    );
    assert_eq!(read_view(&notes), NOTES_SOURCE, "the committed source module is unchanged");
}

/// ORNA-VFS-020: speculative source writes are not evaluated. A half-written file that
/// would not parse is held in the open handle; a new open still reads the committed
/// bytes, and only the synced complete candidate is visible.
#[tokio::test]
async fn vfs_020_speculative_source_write_is_not_visible_until_sync() {
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

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&notes)
        .expect("a write-intent open of a source module is accepted");
    file.write_all(b"pub fn half").expect("a speculative write is accepted into the draft");
    assert_eq!(
        read_view(&notes),
        NOTES_SOURCE,
        "a new open reads the committed bytes while the draft is unsynced"
    );
    file.write_all(b"(): Int = 1;\n").expect("the rest of the candidate is accepted");
    file.sync_all().expect("the complete candidate is admitted at sync");
    assert_eq!(read_view(&notes), "pub fn half(): Int = 1;\n");
}

/// ORNA-VFS-020: a truncating open and an append open of a source module are both
/// accepted; the append continues from the committed bytes.
#[tokio::test]
async fn vfs_020_source_module_append_open_continues_committed_bytes() {
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

    let mut appended = std::fs::OpenOptions::new()
        .append(true)
        .open(&notes)
        .expect("an append open of a source module is accepted");
    appended.write_all(EXTRA_SOURCE.as_bytes()).expect("the append is accepted");
    appended.sync_all().expect("the appended candidate is admitted at sync");
    assert_eq!(read_view(&notes), format!("{NOTES_SOURCE}{EXTRA_SOURCE}"));
}

/// ORNA-VFS-020 and 41-vfs.md errno table: a commit compares each disk file with the
/// baseline its candidate was derived from. A module another writer changed after the
/// open is refused with EAGAIN, and the mount keeps the other writer's bytes.
#[tokio::test]
async fn vfs_020_stale_source_candidate_is_refused_with_eagain() {
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

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&notes)
        .expect("a write-intent open of a source module is accepted");
    file.write_all(b"pub fn mine(): Int = 2;\n").expect("the candidate is accepted into the draft");
    const OTHER: &str = "pub fn other(): Int = 9;\n";
    std::fs::write(repository_directory.path().join("notes.orna"), OTHER)
        .expect("another writer changes the module on disk");
    assert_errno(
        file.sync_all(),
        &[EAGAIN],
        "a candidate whose disk baseline changed is refused with EAGAIN",
    );
    drop(file);
    assert_eq!(read_view(&notes), OTHER, "the mount keeps the other writer's bytes");
}

/// ORNA-VFS-020 and 41-vfs.md: source candidates commit as one group. Two modules edited
/// together are admitted together. When one candidate is malformed, the sync is refused
/// as a unit and neither committed module changes.
#[tokio::test]
async fn vfs_020_source_module_group_is_admitted_together_or_not_at_all() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, _repository, _relation_id, _context) =
        format3::format3_repository_with_context_and_modules(
            "use notes;\nuse extra;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
            "Music",
            &[("notes.orna", NOTES_SOURCE), ("extra.orna", EXTRA_SOURCE)],
        );
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let notes = mountpoint.join("notes.orna");
    let extra = mountpoint.join("extra.orna");
    mounted.await_path(&notes);
    mounted.await_path(&extra);

    const NOTES_ACCEPTED: &str = "pub fn group_a(): Int = 1;\n";
    const EXTRA_ACCEPTED: &str = "pub fn group_b(): Int = 2;\n";
    let mut notes_handle = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&notes)
        .expect("a write-intent open of notes.orna is accepted");
    let mut extra_handle = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&extra)
        .expect("a write-intent open of extra.orna is accepted");
    notes_handle.write_all(NOTES_ACCEPTED.as_bytes()).expect("the notes candidate is accepted");
    extra_handle.write_all(EXTRA_ACCEPTED.as_bytes()).expect("the extra candidate is accepted");
    notes_handle.sync_all().expect("the valid group is admitted at sync");
    assert_eq!(read_view(&notes), NOTES_ACCEPTED);
    assert_eq!(read_view(&extra), EXTRA_ACCEPTED);
    drop(extra_handle);
    drop(notes_handle);

    let mut notes_handle = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&notes)
        .expect("a second write-intent open of notes.orna is accepted");
    let mut extra_handle = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&extra)
        .expect("a second write-intent open of extra.orna is accepted");
    notes_handle.write_all(b"pub fn bad_a( {\n").expect("the malformed candidate is accepted");
    extra_handle.write_all(b"pub fn good_b(): Int = 3;\n").expect("the valid candidate is accepted");
    assert_errno(
        notes_handle.sync_all(),
        &[EINVAL],
        "a group with one malformed candidate is refused as a unit with EINVAL",
    );
    assert_eq!(read_view(&notes), NOTES_ACCEPTED, "the malformed group leaves notes committed");
    assert_eq!(read_view(&extra), EXTRA_ACCEPTED, "the valid sibling is not admitted alone");
}

/// ORNA-VFS-020 and profiles/vfs-1.md (editable candidate and grouped validation): a source
/// module is validated together with the modules that import it. Renaming a function that
/// another module calls is refused when only the callee is saved: both modules keep their
/// committed bytes and the refused candidate is retained as a draft. The same rename saved
/// with its importer validates as a group and commits both files. A group whose candidates
/// both fail validation alone is refused, and neither module changes. A group whose candidates
/// each pass alone but fail together is refused as a unit: the callee drops a function that the
/// importer's candidate calls, and neither module changes.
#[tokio::test]
async fn multi_file_source_edit_requires_grouped_validation() {
    if !require_fuse_or_skip() {
        return;
    }
    const CALLEE: &str = "pub fn answer(): Int = 42;\n";
    const CALLER: &str = "use notes;\npub fn extra(): Int = notes.answer();\n";
    const RENAMED_CALLEE: &str = "pub fn renamed(): Int = 42;\n";
    const RENAMED_CALLER: &str = "use notes;\npub fn extra(): Int = notes.renamed();\n";
    const BROKEN_CALLEE: &str = "pub fn broken_callee( {\n";
    const DANGLING_CALLER: &str = "use notes;\npub fn extra(): Int = notes.missing();\n";
    const SPARE_CALLEE: &str = "pub fn renamed(): Int = 42;\npub fn spare(): Int = 1;\n";
    const SWAPPED_CALLEE: &str = "pub fn renamed(): Int = 42;\npub fn kept(): Int = 2;\n";
    const SPARE_CALLER: &str = "use notes;\npub fn extra(): Int = notes.spare();\n";
    let (repository_directory, repository, _relation_id, _context) =
        format3::format3_repository_with_context_and_modules(
            "use notes;\nuse extra;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
            "Music",
            &[("notes.orna", CALLEE), ("extra.orna", CALLER)],
        );
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let notes = mountpoint.join("notes.orna");
    let extra = mountpoint.join("extra.orna");
    mounted.await_path(&notes);
    mounted.await_path(&extra);

    // Saved alone, the rename leaves the importer calling a function that no longer exists.
    let mut notes_handle = open_source_for_write(&notes);
    notes_handle.write_all(RENAMED_CALLEE.as_bytes()).expect("the callee candidate is accepted");
    assert_errno(
        notes_handle.sync_all(),
        &[EINVAL],
        "a rename that breaks an importer is refused when the callee is saved alone",
    );
    assert_eq!(read_view(&notes), CALLEE, "the refused callee keeps its committed bytes");
    assert_eq!(read_view(&extra), CALLER, "the importer keeps its committed bytes");
    let candidates = retained_candidates(retained_drafts(&repository));
    assert!(
        candidates.iter().any(|draft| draft.as_str() == RENAMED_CALLEE),
        "the refused callee candidate is retained as a draft"
    );
    drop(notes_handle);

    // Saved together, the callee and its importer validate as one group and commit together.
    let mut notes_handle = open_source_for_write(&notes);
    let mut extra_handle = open_source_for_write(&extra);
    notes_handle.write_all(RENAMED_CALLEE.as_bytes()).expect("the callee candidate is accepted");
    extra_handle.write_all(RENAMED_CALLER.as_bytes()).expect("the importer candidate is accepted");
    notes_handle.sync_all().expect("the grouped rename validates as a whole and is admitted");
    assert_eq!(read_view(&notes), RENAMED_CALLEE, "the grouped callee is committed");
    assert_eq!(read_view(&extra), RENAMED_CALLER, "the grouped importer is committed");
    drop(extra_handle);
    drop(notes_handle);

    // Both candidates fail validation on their own: each is refused, so the group is refused.
    let mut notes_handle = open_source_for_write(&notes);
    let mut extra_handle = open_source_for_write(&extra);
    notes_handle.write_all(BROKEN_CALLEE.as_bytes()).expect("the malformed callee is accepted");
    extra_handle.write_all(DANGLING_CALLER.as_bytes()).expect("the dangling importer is accepted");
    assert_errno(
        notes_handle.sync_all(),
        &[EINVAL],
        "a group in which both candidates fail validation on their own is refused with EINVAL",
    );
    assert_eq!(read_view(&notes), RENAMED_CALLEE, "the refused group leaves the callee");
    assert_eq!(read_view(&extra), RENAMED_CALLER, "the refused group leaves the importer");
    drop(extra_handle);
    drop(notes_handle);
    let drafts = retained_candidates(retained_drafts(&repository));
    assert!(
        drafts.iter().any(|draft| draft.as_str() == BROKEN_CALLEE),
        "the malformed callee candidate is retained as a draft"
    );
    assert!(
        drafts.iter().any(|draft| draft.as_str() == DANGLING_CALLER),
        "the dangling importer candidate is retained as a draft"
    );

    // The callee gains `spare`, which no committed importer calls, so it is valid alone.
    let mut notes_handle = open_source_for_write(&notes);
    notes_handle.write_all(SPARE_CALLEE.as_bytes()).expect("the callee candidate is accepted");
    notes_handle.sync_all().expect("the callee with an uncalled spare is admitted alone");
    assert_eq!(read_view(&notes), SPARE_CALLEE, "the spare callee is committed");
    drop(notes_handle);

    // Each candidate passes alone against the other's committed bytes. The callee drops `spare`
    // and the importer calls it, so together the importer calls a function that no longer exists.
    let mut notes_handle = open_source_for_write(&notes);
    let mut extra_handle = open_source_for_write(&extra);
    notes_handle.write_all(SWAPPED_CALLEE.as_bytes()).expect("the callee candidate is accepted");
    extra_handle.write_all(SPARE_CALLER.as_bytes()).expect("the importer candidate is accepted");
    assert_errno(
        notes_handle.sync_all(),
        &[EINVAL],
        "a group whose candidates each pass alone but fail together is refused with EINVAL",
    );
    assert_eq!(read_view(&notes), SPARE_CALLEE, "the group-only refusal leaves the callee");
    assert_eq!(read_view(&extra), RENAMED_CALLER, "the group-only refusal leaves the importer");
    drop(extra_handle);
    drop(notes_handle);
    let drafts = retained_candidates(retained_drafts(&repository));
    assert!(
        drafts.iter().any(|draft| draft.as_str() == SWAPPED_CALLEE),
        "the callee candidate that fails only together is retained as a draft"
    );
    assert!(
        drafts.iter().any(|draft| draft.as_str() == SPARE_CALLER),
        "the importer candidate that fails only together is retained as a draft"
    );
}

/// Opens a source module for a whole-file replacement: the candidate starts empty.
fn open_source_for_write(path: &Path) -> File {
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .expect("a write-intent open of a source module is accepted")
}

/// ORNA-VFS-020/024 and 41-vfs.md: a source candidate written while its handle is still open
/// is dirty and unsynced when the mount stops. Stopping must not report it as saved: the
/// candidate is retained as a draft and named with `sys.vfs.draft_retained`. The committed
/// module keeps its bytes. The spec does not fix the stop exit status, so none is asserted.
#[tokio::test]
async fn vfs_020_dirty_source_draft_is_retained_at_unmount() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, repository, _relation_id, _context) =
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

    const UNSYNCED: &str = "pub fn unsynced_draft(): Int = 9;\n";
    let mut handle = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&notes)
        .expect("a write-intent open of notes.orna is accepted");
    handle.write_all(UNSYNCED.as_bytes()).expect("the candidate is written");

    let signal = Command::new("kill")
        .args(["-TERM", &mounted.pid().to_string()])
        .status()
        .expect("send SIGTERM to the mount process");
    assert!(signal.success(), "kill -TERM failed");
    // The open handle keeps the mount up (ORNA-VFS-024); stop retains the dirty candidate first.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !retained_drafts(&repository).iter().any(|draft| draft.contains("unsynced_draft")) {
        assert!(std::time::Instant::now() < deadline, "stop did not retain the dirty source draft");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(handle);
    mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("the mount stops once the handle is closed");
    let stderr = mounted.stderr_text();
    assert!(
        stderr.contains("sys.vfs.draft_retained") && stderr.contains("notes.orna"),
        "stop reports the retained source draft: {stderr}"
    );
}

/// Waits until the mount's stderr contains `needle`. A refusal is logged before its sync
/// returns, but the stderr reader thread may lag behind the reply.
fn await_stderr(mounted: &MountedProcess, needle: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !mounted.stderr_text().contains(needle) {
        assert!(
            std::time::Instant::now() < deadline,
            "the mount stderr lacks {needle}: {}",
            mounted.stderr_text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// ORNA-VFS-020 and 41-vfs.md: a truncating open alone empties the candidate. The open
/// handle reports the empty candidate, while a new open still reads the committed bytes.
#[tokio::test]
async fn vfs_020_o_trunc_only_open_holds_an_empty_candidate_until_sync() {
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

    let file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&notes)
        .expect("a truncating open of a source module is accepted");
    assert_eq!(
        file.metadata().expect("attributes of the open candidate").len(),
        0,
        "O_TRUNC alone empties the candidate"
    );
    assert_eq!(read_view(&notes), NOTES_SOURCE, "a new open reads the committed bytes");
}

/// ORNA-VFS-020 and 41-vfs.md: a snapshot mount (`mount --at`) has no writer, so a
/// write-intent open of a source module is refused with EROFS and the snapshot is unchanged.
#[tokio::test]
async fn vfs_020_source_write_on_snapshot_mount_is_refused_with_erofs() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, repository, _relation_id, _context) =
        format3::format3_repository_with_context_and_modules(
            "use notes;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
            "Music",
            &[("notes.orna", NOTES_SOURCE)],
        );
    let head = repository.head().unwrap().unwrap();
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_snapshot_mount_process(
        repository_directory.path(),
        &mountpoint,
        head.as_str(),
    );
    let notes = mountpoint.join("notes.orna");
    mounted.await_path(&notes);

    assert_errno(
        OpenOptions::new().write(true).truncate(true).open(&notes).map(|_| ()),
        &[EROFS],
        "a write-intent open on a snapshot mount is refused",
    );
    assert_eq!(read_view(&notes), NOTES_SOURCE, "the snapshot keeps its committed bytes");
}

/// ORNA-VFS-020: a source module is edited in place. Renaming or removing it on the live
/// mount is refused with EPERM, and the committed module is unchanged.
#[tokio::test]
async fn vfs_020_source_module_rename_and_unlink_are_refused_with_eperm() {
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

    assert_errno(
        std::fs::rename(&notes, mountpoint.join("renamed.orna")).map(|_| ()),
        &[EPERM],
        "a source module cannot be renamed",
    );
    assert_errno(
        std::fs::remove_file(&notes),
        &[EPERM],
        "a source module cannot be removed",
    );
    assert_eq!(read_view(&notes), NOTES_SOURCE, "the committed source module is unchanged");
}

/// ORNA-VFS-020 and profiles/vfs-1.md: a sync reports only its own candidate. A malformed
/// `extra.orna` is refused with EINVAL on its own sync. A sync whose group holds it fails
/// with EIO rather than the sibling's cause. The valid `notes.orna` is admitted on its own
/// sync once the malformed candidate has been refused.
#[tokio::test]
async fn vfs_020_sibling_refusal_is_not_reported_on_another_modules_sync() {
    if !require_fuse_or_skip() {
        return;
    }
    let (repository_directory, _repository, _relation_id, _context) =
        format3::format3_repository_with_context_and_modules(
            "use notes;\nuse extra;\npub table Music(id: Str) { title: Str, artist: Str, }\n",
            "Music",
            &[("notes.orna", NOTES_SOURCE), ("extra.orna", EXTRA_SOURCE)],
        );
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let notes = mountpoint.join("notes.orna");
    let extra = mountpoint.join("extra.orna");
    mounted.await_path(&notes);
    mounted.await_path(&extra);

    const NOTES_ACCEPTED: &str = "pub fn sibling_a(): Int = 1;\n";
    let mut notes_handle = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&notes)
        .expect("a write-intent open of notes.orna is accepted");
    let mut extra_handle = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&extra)
        .expect("a write-intent open of extra.orna is accepted");
    notes_handle.write_all(NOTES_ACCEPTED.as_bytes()).expect("the notes candidate is accepted");
    extra_handle.write_all(b"pub fn bad_b( {\n").expect("the malformed candidate is accepted");
    assert_errno(
        notes_handle.sync_all(),
        &[EIO],
        "a group sync with a malformed sibling fails with EIO, not the sibling's EINVAL",
    );
    assert_eq!(read_view(&notes), NOTES_SOURCE, "the failed group is not written");
    assert_errno(
        extra_handle.sync_all(),
        &[EINVAL],
        "the malformed candidate is refused with EINVAL on its own sync",
    );
    await_stderr(&mounted, "sys.vfs.invalid_document");
    notes_handle.sync_all().expect("the valid candidate is admitted once its sibling is refused");
    assert_eq!(read_view(&notes), NOTES_ACCEPTED);
    assert_eq!(read_view(&extra), EXTRA_SOURCE, "the refused sibling keeps its committed bytes");
}

/// ORNA-VFS-020 and 41-vfs.md errno table: a stale candidate is refused with EAGAIN, and
/// the refusal names `sys.vfs.stale_edit`. The open handle keeps its own candidate; it is
/// not reseeded from the disk bytes that replaced it.
#[tokio::test]
async fn vfs_020_stale_source_candidate_is_held_on_its_handle_not_reseeded() {
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

    const MINE: &str = "pub fn mine(): Int = 2;\n";
    const OTHER: &str = "pub fn other(): Int = 9;\n";
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .open(&notes)
        .expect("a write-intent open of notes.orna is accepted");
    file.write_all(MINE.as_bytes()).expect("the candidate is accepted into the draft");
    std::fs::write(repository_directory.path().join("notes.orna"), OTHER)
        .expect("another writer changes the module on disk");
    assert_errno(
        file.sync_all(),
        &[EAGAIN],
        "a candidate whose disk baseline changed is refused with EAGAIN",
    );
    assert_eq!(read_view(&notes), OTHER, "the mount keeps the other writer's bytes");
    await_stderr(&mounted, "sys.vfs.stale_edit");
    file.seek(SeekFrom::Start(0)).expect("seek the held candidate");
    let mut held = String::new();
    file.read_to_string(&mut held).expect("read the held candidate through its handle");
    assert_eq!(held, MINE, "the handle still reads its own candidate, not the replacement bytes");
}
