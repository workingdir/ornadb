#![cfg(target_os = "linux")]

//! ORNA-VFS-019 schema-change half (bead ornadb-eufo). A `--cwd` mount is
//! attached at one HEAD commit and keeps that schema generation while it is
//! live (ORNA-VFS-005 pinned declaration). A schema change committed by the
//! owner while the mount is live must:
//!
//! - leave every open handle on its captured row bytes (no retargeting);
//! - keep the live directory listing and attributes coherent with the pinned
//!   generation rather than mixing old and new shapes.
//!
//! A write whose open captured the old generation must fail EAGAIN (profiles/vfs-1.md
//! :91-92 and :138; bead ornadb-eufo.1), and is proven live. One gap stays an ignored red
//! proof rather than weakened here: a restarted mount must expose the new shape without EIO
//! (ORNA-VFS-019; bead ornadb-eufo.2).

#[allow(dead_code)]
#[path = "support/editor_mount.rs"]
mod editor_mount;
#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;
#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;
#[allow(dead_code)]
#[path = "support/product.rs"]
mod product;

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use editor_mount::{
    assert_errno, require_fuse_or_skip, run_orna_unmount, spawn_mount_process, start_mount_process,
    wait_for_mount, write_and_sync,
};
use product::{commit_source, head, initialized_database, publish, repl_writes};
use tempfile::TempDir;

const EAGAIN: i32 = 11;
const EINVAL: i32 = 22;

const V1: &str = "pub table Music(id: Str) {\n    title: Str,\n    artist: Str,\n}\n";
const V2: &str =
    "pub table Music(id: Str) {\n    title: Str,\n    artist: Str,\n    note: Str?,\n}\n";

const ORIGINAL: &str = "{\n    artist: \"Kieran\",\n    title: \"Original\",\n}\n";
const STALE: &str = "{\n    artist: \"Kieran\",\n    title: \"Stale\",\n}\n";
const NOTED: &str =
    "{\n    artist: \"Kieran\",\n    note: \"fresh\",\n    title: \"Original\",\n}\n";

fn sorted_names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .expect("list the collection directory")
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

/// Compares two rendered rows by their field lines, ignoring field order: the
/// mount's rendering order is not part of the row contract.
fn same_row(actual: &str, expected: &str) -> bool {
    let mut actual: Vec<&str> = actual.lines().map(str::trim).collect();
    let mut expected: Vec<&str> = expected.lines().map(str::trim).collect();
    actual.sort_unstable();
    expected.sort_unstable();
    actual == expected
}

/// Initialises a v1 database with one committed `london` row and returns its root.
fn seed_library(directory: &TempDir) -> PathBuf {
    let root = initialized_database(directory, V1);
    commit_source(&root, "Declare Music");
    repl_writes(
        &root,
        "main.Music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" })",
    );
    publish(&root, "Add london");
    root
}

/// ORNA-VFS-019 live half: an owner-committed schema change under a live
/// `--cwd` mount leaves open handles, attributes and the listing on the pinned
/// generation.
#[test]
fn vfs_019_schema_change_under_live_mount_pins_handles_and_listing() {
    if !require_fuse_or_skip() {
        return;
    }
    let directory = TempDir::new().expect("create the test directory");
    let root = seed_library(&directory);

    let mount_parent = TempDir::new().expect("create the mount parent");
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(&root, &mountpoint);
    let music = mountpoint.join("Music");
    let london = music.join("london/data.orna");
    mounted.await_path(&london);
    let listing_before = sorted_names(&music);
    let baseline = fs::read_to_string(&london).expect("read the pinned row");
    assert!(
        same_row(&baseline, ORIGINAL),
        "the mount shows the accepted v1 row: {baseline:?}"
    );

    let mut read_handle = File::open(&london).expect("open a read handle before the change");

    // The owner commits the schema change while the mount is live.
    let before_change = head(&root);
    fs::write(root.join("main.orna"), V2).expect("write the v2 declaration");
    commit_source(&root, "Add optional note to Music");
    assert_ne!(head(&root), before_change, "the schema change is committed");

    // Open handles keep their captured bytes; attributes match them.
    read_handle
        .seek(SeekFrom::Start(0))
        .expect("rewind the read handle");
    let mut through_handle = String::new();
    read_handle
        .read_to_string(&mut through_handle)
        .expect("read through the old handle");
    assert_eq!(
        through_handle, baseline,
        "an open handle does not retarget to the new shape"
    );
    assert_eq!(
        fs::metadata(&london).expect("stat the live row").len(),
        baseline.len() as u64,
        "the live attribute size matches the pinned generation"
    );
    assert_eq!(
        sorted_names(&music),
        listing_before,
        "the live listing keeps its row set"
    );
    drop(read_handle);

    // A fresh open on the live mount is still on its pinned v1 declaration.
    assert_errno(
        write_and_sync(&london, NOTED),
        &[EINVAL],
        "the live mount does not adopt the v2 field before remount",
    );
    // The refused candidate is retained as a draft that blocks unmount until the
    // change is dropped: reopen the file, write the current row, and fsync it.
    write_and_sync(&london, ORIGINAL)
        .expect("dropping the refused change and fsyncing the current row clears the draft");

    run_orna_unmount(&root, &mountpoint);
    let status = mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("the mount process exits after unmount");
    assert!(status.success(), "mount exited with {status}");
    drop(mounted);
}

/// Red proof for bead ornadb-eufo.2: a mount restarted after an owner schema
/// change must expose the new generation. The remounted row currently fails
/// validation and reads EIO: the schema commit rewrites committed rows but not
/// the CWD runtime row baseline, which stays v1-encoded and is validated against
/// the v2 descriptor.
#[test]
#[ignore = "ornadb-eufo.2.1"]
fn vfs_019_remount_after_schema_change_shows_new_shape() {
    if !require_fuse_or_skip() {
        return;
    }
    let directory = TempDir::new().expect("create the test directory");
    let root = seed_library(&directory);

    let mount_parent = TempDir::new().expect("create the mount parent");
    let mountpoint = mount_parent.path().join("library");
    let london = mountpoint.join("Music/london/data.orna");

    let mut first = spawn_mount_process(&root, &mountpoint);
    first.await_path(&london);
    run_orna_unmount(&root, &mountpoint);
    let status = first
        .wait_for_exit(Duration::from_secs(10))
        .expect("the first mount process exits after unmount");
    assert!(status.success(), "first mount exited with {status}");
    drop(first);

    fs::write(root.join("main.orna"), V2).expect("write the v2 declaration");
    commit_source(&root, "Add optional note to Music");

    let mut remounted = wait_for_mount(start_mount_process(&root, &mountpoint));
    remounted.await_path(&london);
    assert!(
        same_row(
            &fs::read_to_string(&london).expect("read the remounted row"),
            ORIGINAL
        ),
        "the remounted row keeps its accepted content; mount stderr: {}",
        remounted.stderr_text()
    );
    write_and_sync(&london, NOTED).expect("the remounted v2 generation admits the optional field");
    assert!(
        fs::read_to_string(&london)
            .expect("read the admitted row")
            .contains("note"),
        "the admitted v2 row carries the new field"
    );
    run_orna_unmount(&root, &mountpoint);
    let status = remounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("the remounted process exits after unmount");
    assert!(status.success(), "remounted process exited with {status}");
}

/// Proof for bead ornadb-eufo.1: a write whose open captured the v1 schema
/// generation is refused EAGAIN after the owner commits a schema change
/// (profiles/vfs-1.md:91-92 captures schema/source generation; :138 EAGAIN is a
/// stale expected version).
#[test]
fn vfs_019_stale_generation_write_after_schema_change_is_eagain() {
    if !require_fuse_or_skip() {
        return;
    }
    let directory = TempDir::new().expect("create the test directory");
    let root = seed_library(&directory);

    let mount_parent = TempDir::new().expect("create the mount parent");
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(&root, &mountpoint);
    let london = mountpoint.join("Music/london/data.orna");
    mounted.await_path(&london);

    let mut write_handle = OpenOptions::new()
        .write(true)
        .open(&london)
        .expect("a write-intent open captures the v1 generation");

    let before_change = head(&root);
    fs::write(root.join("main.orna"), V2).expect("write the v2 declaration");
    commit_source(&root, "Add optional note to Music");
    assert_ne!(head(&root), before_change, "the schema change is committed");

    write_handle
        .set_len(0)
        .expect("truncate the private candidate");
    write_handle
        .write_all(STALE.as_bytes())
        .expect("the bytes are accepted into the handle");
    let refused = write_handle
        .sync_all()
        .expect_err("a v1-generation write after the schema change is stale");
    assert_eq!(
        refused.raw_os_error(),
        Some(EAGAIN),
        "stale generation is EAGAIN: {refused}; mount stderr: {}",
        mounted.stderr_text()
    );
    drop(write_handle);
    assert!(
        same_row(
            &fs::read_to_string(&london).expect("read the accepted row"),
            ORIGINAL
        ),
        "a stale-generation write keeps the accepted row"
    );

    // The refused candidate stays an unresolved draft that blocks unmount until the
    // change is dropped: reopen the file, write the current row, and fsync it
    // (profiles/vfs-1.md:144).
    write_and_sync(&london, ORIGINAL)
        .expect("dropping the refused change and fsyncing the current row clears the draft");

    run_orna_unmount(&root, &mountpoint);
    let status = mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("the mount process exits after unmount");
    assert!(status.success(), "mount exited with {status}");
}

/// ORNA-VFS-019 restart hardening: a persisted temporary scratch record that cannot be read is
/// reported on mount stderr and left on disk, so one damaged record cannot stop the mount opening.
#[test]
fn vfs_019_unreadable_persisted_scratch_is_reported_and_left_on_disk() {
    if !require_fuse_or_skip() {
        return;
    }
    let directory = TempDir::new().expect("create the test directory");
    let root = seed_library(&directory);

    // The runtime scratch store lives under the repository's git directory and is private (0700).
    let scratch_directory = root.join(".git/orna/vfs-editor-scratch-v1");
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&scratch_directory)
            .expect("create the private scratch store");
    }
    let damaged = scratch_directory.join("0000.scratch");
    fs::write(&damaged, b"not a record").expect("write the damaged scratch record");

    let mount_parent = TempDir::new().expect("create the mount parent");
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(&root, &mountpoint);
    let london = mountpoint.join("Music/london/data.orna");
    mounted.await_path(&london);
    let mut stderr = mounted.stderr_text();
    for _ in 0..50 {
        if stderr.contains("unreadable persisted temporary scratch") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        stderr = mounted.stderr_text();
    }
    assert!(
        stderr.contains("unreadable persisted temporary scratch") && stderr.contains("0000.scratch"),
        "the damaged record is reported on mount stderr: {stderr:?}"
    );
    assert_eq!(
        fs::read(&damaged).expect("the damaged record still exists"),
        b"not a record",
        "the damaged record is left on disk untouched"
    );

    run_orna_unmount(&root, &mountpoint);
    let status = mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("the mount process exits after unmount");
    assert!(status.success(), "mount exited with {status}");
}

/// ORNA-VFS-019 restart: a temporary scratch fsynced under schema v1 is restored after the owner
/// commits a schema change while the mount is down. Its captured bytes survive intact; the scratch
/// keeps its creation generation (a later commit of it is refused EAGAIN, not retargeted).
/// Red under ornadb-eufo.2.1: the restarted v2 mount skips the v1 CWD row with
/// `InvalidValue` ("the CWD row failed validation against the mounted schema"), so the
/// persisted scratch is never exposed. Not weakened; ignored until the bead lands.
#[ignore = "ornadb-eufo.2.1"]
#[test]
fn vfs_019_persisted_scratch_is_restored_after_schema_change() {
    if !require_fuse_or_skip() {
        return;
    }
    let directory = TempDir::new().expect("create the test directory");
    let root = seed_library(&directory);

    let mount_parent = TempDir::new().expect("create the mount parent");
    let mountpoint = mount_parent.path().join("library");
    let mut first = spawn_mount_process(&root, &mountpoint);
    let row = mountpoint.join("Music/london");
    first.await_path(&row.join("data.orna"));
    let expected: &[u8] = b"durable scratch captured under schema v1";
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(row.join(".data.orna.persisted-scratch"))
        .expect("create temporary scratch through FUSE");
    scratch.write_all(expected).expect("write the scratch");
    scratch.sync_all().expect("fsync persists the scratch");
    drop(scratch);
    // A synced scratch holds the draft guard, so the first mount is torn down by dropping its
    // process rather than by a clean unmount (as vfs_editor_compat.rs does).
    drop(first);

    fs::write(root.join("main.orna"), V2).expect("write the v2 declaration");
    commit_source(&root, "Add optional note to Music");

    let restarted = mount_parent.path().join("restarted");
    let mut remounted = spawn_mount_process(&root, &restarted);
    let restarted_row = restarted.join("Music/london");
    let restored = restarted_row.join(".data.orna.persisted-scratch");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !restored.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        restored.exists(),
        "the v1 scratch is restored under the v2 mount; mount stderr: {}",
        remounted.stderr_text()
    );
    assert_eq!(
        fs::read(&restored).expect("read the restored scratch"),
        expected,
        "the scratch captured under v1 is restored intact after the schema change; mount stderr: {}",
        remounted.stderr_text()
    );
    drop(remounted);
}
