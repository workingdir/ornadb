#![cfg(target_os = "linux")]

//! Gate E FUSE path matrix (bead ornadb-5il1.371, S10): a reserved key spelling that the mount
//! must project gets a lookup, a read and a save round trip through its escaped directory. The
//! row keyed `data.orna` is projected as `Music/%64ata.orna/data.orna`, because the first byte of
//! the reserved component is escaped (profiles/vfs-1.md:44-50). A Blob field `data` with suffix
//! `orna` is checked to project as `%64ata.orna` through `project_content_name`, not as the row
//! document name.
//! This file adds only the `data.orna` spelling; the other reserved spellings are exercised by the
//! existing key-escape tests in `vfs_projection_features.rs`, `vfs_key_escape_spellings.rs` and
//! `vfs_composite_key_mount.rs`.

#[path = "support/editor_mount.rs"]
mod editor_mount;
#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;

use std::{fs, path::Path, time::Duration};

use editor_mount::{
    repository_with_music_rows, require_fuse_or_skip, run_orna_unmount, spawn_mount_process,
    write_and_sync,
};
use tempfile::TempDir;

fn music_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn sorted_names(directory: &Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
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

/// profiles/vfs-1.md:44-50: the reserved key `data.orna` escapes its first byte, so its row
/// directory is `%64ata.orna` and its row document is `%64ata.orna/data.orna`. The directory
/// lists with no literal `data.orna` entry, the row reads back under the escaped path, and a save
/// through that path is acknowledged and read back.
#[tokio::test]
async fn data_orna_key_escapes_first_byte_and_round_trips() {
    if !require_fuse_or_skip() {
        return;
    }
    let reserved = music_row("data.orna");
    let plain = music_row("plain");
    let (repository_directory, _repository, _relation_id) =
        repository_with_music_rows(&[("data.orna", reserved.as_str()), ("plain", plain.as_str())])
            .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let mut mounted = spawn_mount_process(repository_directory.path(), &mountpoint);
    let music = mountpoint.join("Music");
    let row = music.join("%64ata.orna/data.orna");
    mounted.await_path(&row);

    assert_eq!(
        sorted_names(&music),
        vec!["%64ata.orna".to_owned(), "plain".to_owned()],
        "the reserved key has one escaped directory and no literal data.orna directory",
    );
    let read = fs::read_to_string(&row).expect("read the escaped row document");
    assert!(
        read.contains("title: \"data.orna\""),
        "the escaped directory reads the row keyed data.orna: {read}",
    );

    let saved = music_row("data.orna saved");
    write_and_sync(&row, &saved).expect("a valid save through the escaped row is acknowledged");
    let reread = fs::read_to_string(&row).expect("read the saved row document");
    assert!(
        reread.contains("title: \"data.orna saved\""),
        "a new open reads the saved row through the escaped path: {reread}",
    );

    run_orna_unmount(repository_directory.path(), &mountpoint);
    let status = mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("the mount process exits after unmount");
    assert!(status.success(), "mount exited with {status}");
}

/// profiles/vfs-1.md:44-50: a Blob field `data` with suffix `orna` projects as `%64ata.orna`,
/// not as the row document name. The mount names Blob files through this same
/// `project_content_name` (`fuse_mount.rs` call sites).
#[test]
fn blob_data_with_orna_suffix_projects_as_escaped_name() {
    let name =
        orna_runtime::vfs::project_content_name("data", "application/octet-stream", Some("orna"))
            .expect("octet-stream with suffix orna is a valid annotation");
    assert_eq!(name, "%64ata.orna");
}
