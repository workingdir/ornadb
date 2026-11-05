#![cfg(target_os = "linux")]

//! The mount and the repository key path are two naming surfaces with two
//! documents. Neither borrows the other's spellings (bead ornadb-5il1.245.1):
//!
//! - The mount follows profiles/vfs-1.md:35-45: every byte outside
//!   `[A-Za-z0-9._-]` becomes `%HH` uppercase, a literal `%` and `~` are always
//!   escaped, and the empty key is `~empty`.
//! - The repository key path follows `tests/path-vectors.json` (ORNA-PATH-001
//!   through 012): `~hh` lowercase, and `~ff` for the empty component.
//!
//! A key component that *looks* like a repository escape is therefore literal
//! on the mount: a `~2e` key is `%7E2e` (it is not decoded to `.`), a `~ff`
//! key is `%7Eff` (it is not the empty key), and `~empty` stays the mount's
//! only empty-key spelling. Accepting `~hh` on the mount would alias one
//! canonical key to two host names, which profiles/vfs-1.md:40-43 forbids for
//! `~empty` and the same reasoning forbids for every other `~` spelling.

use std::path::Path;

use orna_runtime::vfs::{VfsComponent, project_component, unproject_component};
use tempfile::TempDir;

#[path = "support/editor_mount.rs"]
mod editor_mount;
#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;
use editor_mount::{repository_with_music_rows, require_fuse_or_skip, spawn_mount_process};

fn music_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn read_view(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read accepted state through a new open")
}

fn sorted_names(directory: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .expect("list the directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// profiles/vfs-1.md:35-45 against tests/path-vectors.json ORNA-PATH-001..012:
/// a row key whose bytes spell a repository escape (`~2e`, `~ff`) is projected
/// literally by the mount, and the empty key keeps its own spelling per surface
/// (`~empty` on the mount, `~ff` in a repository key path).
#[tokio::test]
async fn escape_looking_keys_stay_literal_on_the_mount_and_keep_tilde_spellings_repository_side() {
    if !require_fuse_or_skip() {
        return;
    }
    let tilde_2e = music_row("KeyTilde2e");
    let tilde_ff = music_row("KeyTildeFf");
    let literal_empty_spelling = music_row("KeyTildeEmpty");
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&[
        ("~2e", tilde_2e.as_str()),
        ("~ff", tilde_ff.as_str()),
        ("~empty", literal_empty_spelling.as_str()),
    ])
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let music = mountpoint.join("Music");
    // The rows are admitted by canonical key text; the mount is what spells
    // them, and its spelling escapes the literal `~` in `~2e`, `~ff` and
    // `~empty`, leaving the hex digits literal.
    let row_2e = music.join("%7E2e/data.orna");
    let row_ff = music.join("%7Eff/data.orna");
    let row_empty = music.join("%7Eempty/data.orna");
    mounted.await_path(&row_2e);
    mounted.await_path(&row_ff);
    mounted.await_path(&row_empty);

    // (1) A key containing the bytes `~2e` is `%7E2e`, not the escape `.`.
    assert_eq!(
        sorted_names(&music),
        vec!["%7E2e", "%7Eempty", "%7Eff"],
        "each `~`-bearing key takes one escaped spelling and no `~hh` alias appears"
    );
    assert!(
        read_view(&row_2e).contains("KeyTilde2e"),
        "`~2e` keys project their own row at %7E2e"
    );
    assert!(
        !music.join("%2E").exists() && !music.join("~2e").exists(),
        "`~2e` is not decoded to `.` and is never read as an escape"
    );

    // (2) A key containing the bytes `~ff` is `%7Eff` and is distinct from the
    // empty-key spelling `~empty`, which here names a different literal row.
    assert!(
        read_view(&row_ff).contains("KeyTildeFf"),
        "`~ff` keys project their own row at %7Eff"
    );
    assert!(
        !music.join("~ff").exists(),
        "`~ff` is not an escape on the mount"
    );
    assert!(
        !music.join("~empty").exists(),
        "the literal `~empty` row is %7Eempty; the empty key itself cannot be stored (ornadb-5il1.462.1) and is exercised by vfs_composite_key_mount::empty_key_component_projects_as_tilde_empty"
    );
    assert_ne!(
        read_view(&row_ff),
        read_view(&row_empty),
        "%7Eff and %7Eempty are two keys, not one"
    );

    // (3) One empty key, two spellings: `~empty` on the mount, `~ff` in a
    // repository key path (ORNA-PATH-001..012). The mount's own inverse rule,
    // which every lookup and write goes through, accepts exactly `~empty` and
    // refuses the repository spelling, so the two surfaces cannot alias. A
    // stored empty key component is exercised by
    // vfs_composite_key_mount::empty_key_component_projects_as_tilde_empty;
    // a single-component empty key cannot be stored yet (ornadb-5il1.462.1).
    assert_eq!(project_component(""), "~empty");
    assert_eq!(
        unproject_component("~empty"),
        Ok(VfsComponent::Canonical(String::new())),
        "`~empty` is the mount's empty-key spelling"
    );
    assert!(
        unproject_component("~ff").is_err(),
        "the repository's empty-component spelling is not an alias on the mount"
    );
    assert!(
        std::fs::read_to_string(music.join("~ff/data.orna")).is_err(),
        "no repository key-path spelling resolves on the mount"
    );

    // The repository key path itself: the same empty component is `~ff`, and a
    // literal tilde is `~7e` lowercase, so neither surface reads the other's
    // spellings. These are the ORNA-PATH encoders the vectors pin.
    assert_eq!(orna_value::path_encode_component("").unwrap(), "~ff");
    assert_eq!(orna_value::path_decode_component("~ff").unwrap(), "");
    assert_eq!(orna_value::path_encode_component("~ff").unwrap(), "~7eff");
    assert_eq!(orna_value::path_encode_component("~2e").unwrap(), "~7e2e");
    assert_eq!(
        orna_storage::LoosePath::for_key("Music", &[String::new()])
            .unwrap()
            .as_managed_path()
            .as_path(),
        Path::new("Music/~ff.orna"),
        "the repository key path spells the empty key `~ff`"
    );
    assert_eq!(
        orna_storage::LoosePath::for_key("Music", &["~2e".to_owned()])
            .unwrap()
            .as_managed_path()
            .as_path(),
        Path::new("Music/~7e2e.orna"),
        "the repository key path escapes a literal `~`, so `~2e` there is not `.`"
    );

    // The mount keeps its own spellings for the same keys.
    assert_eq!(project_component("~2e"), "%7E2e");
    assert_eq!(project_component("~ff"), "%7Eff");
    assert_eq!(project_component("~empty"), "%7Eempty");
}
