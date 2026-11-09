use std::ffi::OsStr;

use super::{Directory, FileType, FsBuilder, NodeKind, ROOT_INODE};

/// A builder whose directory `dir` lists `names` in order; returns the builder and `dir`.
fn directory_with(names: &[&str]) -> (FsBuilder, u64) {
    let mut builder = FsBuilder::new();
    let dir = builder
        .insert_node(
            ROOT_INODE,
            "dir",
            NodeKind::Directory(Directory::new()),
            FileType::Directory,
        )
        .expect("create the test directory");
    for name in names {
        builder
            .add_file(dir, name, NodeKind::Bytes(Vec::new()))
            .expect("add a test entry");
    }
    (builder, dir)
}

/// Lists `dir` from the kernel cursor `start`, keeping at most `limit` entries. Each item is
/// the offset the kernel passes back for that entry and its name.
fn list(builder: &mut FsBuilder, dir: u64, start: u64, limit: usize) -> Vec<(u64, String)> {
    let mut seen = Vec::new();
    builder
        .readdir_page(dir, start, |offset, entry| {
            seen.push((offset, entry.name.to_string_lossy().into_owned()));
            seen.len() < limit
        })
        .expect("list the test directory");
    seen
}

fn names(listing: &[(u64, String)]) -> Vec<&str> {
    listing.iter().map(|(_, name)| name.as_str()).collect()
}

fn offset_of(listing: &[(u64, String)], name: &str) -> u64 {
    listing
        .iter()
        .find(|(_, listed)| listed == name)
        .map(|(offset, _)| *offset)
        .expect("the entry is listed")
}

/// ORNA-VFS-017: a listing held across a removal resumes after its cursor. Positional
/// offsets would skip `c` here, because removing `a` shifts every later index down.
#[test]
fn held_listing_resumes_after_its_cursor_when_an_earlier_entry_is_removed() {
    let (mut builder, dir) = directory_with(&["a", "b", "c", "d"]);
    let first = list(&mut builder, dir, 0, 2);
    assert_eq!(names(&first), ["a", "b"]);
    let cursor = first.last().expect("two entries are listed").0;
    builder
        .directory_mut(dir)
        .and_then(|directory| directory.remove_entry(OsStr::new("a")))
        .expect("a is present before removal");
    let rest = list(&mut builder, dir, cursor, usize::MAX);
    assert_eq!(
        names(&rest),
        ["c", "d"],
        "entries after the cursor are neither skipped nor repeated"
    );
}

/// ORNA-VFS-017: a surviving entry keeps the offset it was listed with.
#[test]
fn surviving_entries_keep_their_offsets_after_an_earlier_removal() {
    let (mut builder, dir) = directory_with(&["a", "b", "c", "d"]);
    let before = list(&mut builder, dir, 0, usize::MAX);
    builder
        .directory_mut(dir)
        .and_then(|directory| directory.remove_entry(OsStr::new("a")))
        .expect("a is present before removal");
    let after = list(&mut builder, dir, 0, usize::MAX);
    assert_eq!(
        offset_of(&after, "c"),
        offset_of(&before, "c"),
        "c keeps its offset"
    );
    assert_eq!(
        offset_of(&after, "d"),
        offset_of(&before, "d"),
        "d keeps its offset"
    );
}

/// ORNA-VFS-017 (refresh): a row refresh through `reconcile_row_files` retires a file the row
/// no longer projects and appends one it now does. A listing held at a cursor past the
/// retired file must see the appended file exactly once, and no two entries may share an
/// offset.
#[test]
fn held_listing_sees_a_row_refresh_that_retires_one_file_and_adds_another_once() {
    let (mut builder, row) = directory_with(&["data.orna", "a", "b", "c", "d"]);
    let data = builder
        .directory(row)
        .and_then(|directory| directory.entry(OsStr::new("data.orna")))
        .expect("the row projects data.orna")
        .inode;
    let before = list(&mut builder, row, 0, usize::MAX);
    assert_eq!(names(&before), ["data.orna", "a", "b", "c", "d"]);
    let projected = vec![
        ("a".to_owned(), NodeKind::Bytes(Vec::new())),
        ("c".to_owned(), NodeKind::Bytes(Vec::new())),
        ("d".to_owned(), NodeKind::Bytes(Vec::new())),
        ("e".to_owned(), NodeKind::Bytes(Vec::new())),
    ];
    builder
        .reconcile_row_files(data, None, projected)
        .expect("the refresh reconciles the row");
    let after_c = list(&mut builder, row, offset_of(&before, "c"), usize::MAX);
    assert_eq!(
        names(&after_c),
        ["d", "e"],
        "a cursor at c neither skips nor repeats the appended file"
    );
    let after_d = list(&mut builder, row, offset_of(&before, "d"), usize::MAX);
    assert_eq!(
        names(&after_d),
        ["e"],
        "a cursor at d neither skips nor repeats the appended file"
    );
    let all = list(&mut builder, row, 0, usize::MAX);
    let mut offsets: Vec<u64> = all.iter().map(|(offset, _)| *offset).collect();
    offsets.sort_unstable();
    offsets.dedup();
    assert_eq!(
        offsets.len(),
        all.len(),
        "every entry carries its own offset"
    );
}

/// A child replaced in place (refresh, Blob generation) keeps its position in a held listing.
#[test]
fn replaced_child_keeps_its_position_for_a_held_listing() {
    let (mut builder, dir) = directory_with(&["a", "b", "c"]);
    let first = list(&mut builder, dir, 0, 1);
    let cursor = first.last().expect("one entry is listed").0;
    builder
        .replace_child(dir, OsStr::new("b"), NodeKind::Bytes(vec![1]))
        .expect("replace b");
    let rest = list(&mut builder, dir, cursor, usize::MAX);
    assert_eq!(names(&rest), ["b", "c"]);
}

/// A directory retired while a listing is held stays loaded, so the held cursor still
/// enumerates the entries after it (ORNA-VFS-017, 41-vfs.md:39-40). Its last release
/// removes it.
#[test]
fn directory_retired_after_a_listing_opened_still_enumerates() {
    let (mut builder, dir) = directory_with(&["a", "b", "c"]);
    let first = list(&mut builder, dir, 0, 1);
    let cursor = first.last().expect("one entry is listed").0;
    builder.track_open(dir);
    builder.retire_node(dir);
    assert!(
        builder.directory(dir).is_some(),
        "a held retired directory stays loaded"
    );
    let rest = list(&mut builder, dir, cursor, usize::MAX);
    assert_eq!(names(&rest), ["b", "c"]);
    builder.track_release(dir);
    assert!(
        builder.directory(dir).is_none(),
        "the last release reclaims the directory"
    );
}

/// A retired directory with no open handle below it is removed with its children.
#[test]
fn directory_retired_without_an_open_handle_is_removed_with_its_children() {
    let (mut builder, dir) = directory_with(&["a", "b"]);
    let directory = builder.directory(dir).expect("the directory is loaded");
    let children: Vec<u64> = directory.entries.iter().map(|entry| entry.inode).collect();
    builder.retire_node(dir);
    assert!(
        builder.directory(dir).is_none(),
        "an unpinned retired directory is removed"
    );
    assert!(
        children
            .iter()
            .all(|child| !builder.nodes.contains_key(child)),
        "its children are removed with it"
    );
}

/// An open handle on a child pins the retired directory above it.
#[test]
fn directory_retired_while_a_child_is_open_stays_loaded() {
    let (mut builder, dir) = directory_with(&["a", "b"]);
    let directory = builder.directory(dir).expect("the directory is loaded");
    let child = directory.entries[0].inode;
    builder.track_open(child);
    builder.retire_node(dir);
    assert!(
        builder.directory(dir).is_some(),
        "an open child keeps its parent loaded"
    );
    builder.track_release(child);
    assert!(
        builder.directory(dir).is_none(),
        "the release reclaims the parent"
    );
}
