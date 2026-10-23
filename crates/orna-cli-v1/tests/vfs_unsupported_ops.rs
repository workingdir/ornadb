use std::{
    fs::OpenOptions,
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};

#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;

struct MountedFuse {
    child: Child,
    mountpoint: PathBuf,
}

impl Drop for MountedFuse {
    fn drop(&mut self) {
        let mounted = std::fs::read_to_string("/proc/self/mountinfo")
            .ok()
            .is_some_and(|mountinfo| {
                mountinfo.lines().any(|line| {
                    line.split_whitespace().nth(4) == self.mountpoint.to_str()
                })
            });
        if mounted {
            let configured = std::env::var_os("FUSERMOUNT_PATH");
            let helpers = configured
                .into_iter()
                .chain(["fusermount3".into(), "fusermount".into()]);
            for helper in helpers {
                if Command::new(helper)
                    .args(["-u", "--"])
                    .arg(&self.mountpoint)
                    .status()
                    .is_ok_and(|status| status.success())
                {
                    break;
                }
            }
        }
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn fuse_available() -> bool {
    let required = std::env::var_os("ORNA_REQUIRE_FUSE").as_deref()
        == Some(std::ffi::OsStr::new("1"));
    let skipped = std::env::var_os("ORNA_SKIP_FUSE_TEST").as_deref()
        == Some(std::ffi::OsStr::new("1"));
    assert!(!required || !skipped, "ORNA_REQUIRE_FUSE=1 forbids ORNA_SKIP_FUSE_TEST=1");
    if skipped {
        return false;
    }
    let prerequisites = (|| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fuse")
            .map_err(|error| format!("/dev/fuse unavailable: {error}"))?;
        let configured = std::env::var_os("FUSERMOUNT_PATH");
        let helpers = configured
            .into_iter()
            .chain(["fusermount3".into(), "fusermount".into()]);
        if helpers.into_iter().any(|helper| Command::new(helper).arg("-h").output().is_ok()) {
            Ok(())
        } else {
            Err("no fusermount helper is executable".to_owned())
        }
    })();
    match prerequisites {
        Ok(()) => true,
        Err(reason) => {
            assert!(!required, "ORNA_REQUIRE_FUSE=1 but FUSE prerequisites failed: {reason}");
            eprintln!("SKIP: FUSE unsupported-operation test prerequisites failed: {reason}");
            false
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
/// ORNA-VFS-014: link, special-node, and writable shared-mmap operations fail explicitly.
async fn unsupported_vfs_operations_return_explicit_errors() {
    if !fuse_available() {
        return;
    }

    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state.acquire_lease(runtime_identity.repository_id).await.unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x92,
            insert_only: true,
        }],
    )
    .await;
    drop(state);
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedFuse {
        child,
        mountpoint: mountpoint.clone(),
    };
    let document = mountpoint.join("Music/london/data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !document.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            panic!("mount exited before exposing the projected document: {status:?}");
        }
        assert!(Instant::now() < deadline, "FUSE mount did not expose the row document");
        thread::sleep(Duration::from_millis(20));
    }

    let row_directory = document.parent().expect("document has a projected row directory");
    let hard_link = row_directory.join("hard-link.orna");
    let error = std::fs::hard_link(&document, &hard_link).expect_err("hard links are unsupported");
    assert!(
        matches!(error.kind(), ErrorKind::Unsupported | ErrorKind::PermissionDenied),
        "hard link failure must be explicit: {error}"
    );
    assert!(!hard_link.exists(), "failed hard-link operation must not create a target");

    let symlink_path = row_directory.join("symbolic-link.orna");
    let error = std::os::unix::fs::symlink(&document, &symlink_path)
        .expect_err("symlinks are unsupported");
    assert!(
        matches!(error.kind(), ErrorKind::Unsupported | ErrorKind::PermissionDenied),
        "symlink failure must be explicit: {error}"
    );
    assert!(!symlink_path.exists(), "failed symlink operation must not create a target");

    let device_path = row_directory.join("device-node");
    let device_output = Command::new("mknod")
        .arg(&device_path)
        .args(["c", "1", "3"])
        .output()
        .expect("run mknod for the unsupported-device-node probe");
    assert!(
        !device_output.status.success(),
        "device-node creation unexpectedly succeeded: {}",
        String::from_utf8_lossy(&device_output.stderr)
    );

    let mmap_probe = r#"import mmap, os, sys
fd = os.open(sys.argv[1], os.O_RDWR)
try:
    mapping = mmap.mmap(fd, 1, flags=mmap.MAP_SHARED, prot=mmap.PROT_READ | mmap.PROT_WRITE)
except OSError as error:
    print(error.errno)
    sys.exit(1)
else:
    mapping.close()
    sys.exit(0)"#;
    let mmap_output = Command::new("python3")
        .args(["-c", mmap_probe])
        .arg(&document)
        .output()
        .expect("run writable shared-mmap probe");
    assert!(
        !mmap_output.status.success(),
        "writable shared mmap unexpectedly succeeded: {}",
        String::from_utf8_lossy(&mmap_output.stderr)
    );
    let errno = String::from_utf8_lossy(&mmap_output.stdout);
    assert!(
        errno.trim().parse::<i32>().is_ok_and(|errno| errno != 0),
        "mmap must fail with a reported OS error; stdout={errno:?}, stderr={:?}",
        String::from_utf8_lossy(&mmap_output.stderr)
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
/// ORNA-VFS-015: `edit --row` admits a row while retaining its owned Blob binding.
async fn edit_row_admits_source_and_preserves_blob_binding() {
    if !fuse_available() {
        return;
    }

    let media_root = TempDir::new().unwrap();
    let original = b"original-grouped-content";
    std::fs::write(media_root.path().join("tone.mp3"), original).unwrap();
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let source = "pub table Music(id: Str) { title: Str, content: Blob, }\n";
    let row_source = format!(
        "{{ title: \"before\", content: sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 65536), \"audio/mpeg\"), }}\n"
    );
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            "Music/london.orna",
            &row_source,
        );
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state.acquire_lease(runtime_identity.repository_id).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 65536), \"audio/mpeg\")"
    );
    let blob_captures = [("content", capture.as_str())];
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &blob_captures,
            source_row: None,
            ordinal: 0x93,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedFuse {
        child,
        mountpoint: mountpoint.clone(),
    };
    let row_path = mountpoint.join("Music/london");
    let blob_path = row_path.join("content.mp3");
    let document_path = row_path.join("data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !document_path.exists() || !blob_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            panic!("mount exited before exposing the projected row: {status:?}");
        }
        assert!(Instant::now() < deadline, "FUSE mount did not expose the projected row");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read(&blob_path).unwrap(), original);
    let initial_document = std::fs::read_to_string(&document_path).unwrap();
    assert!(initial_document.contains("\"before\""));
    let updated_document = initial_document.replacen("\"before\"", "\"after\"", 1);
    assert_ne!(initial_document, updated_document);

    let scripts = TempDir::new().unwrap();
    let edited_source = scripts.path().join("edited-row.orna");
    std::fs::write(&edited_source, updated_document).unwrap();
    let editor = scripts.path().join("editor");
    std::fs::write(&editor, "#!/bin/sh\ncp \"$ORNA_ROW_SOURCE\" \"$1\"\n").unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "edit",
            "--row",
        ])
        .arg(&row_path)
        .env("EDITOR", &editor)
        .env("ORNA_ROW_SOURCE", &edited_source)
        .output()
        .expect("run edit --row against the projected row directory");
    assert!(
        output.status.success(),
        "edit --row failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let unmount = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("unmount after explicit grouped row edit");
    assert!(
        unmount.status.success(),
        "unmount failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&unmount.stdout),
        String::from_utf8_lossy(&unmount.stderr),
    );
    assert!(mounted.child.wait().unwrap().success());

    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("remount the updated workspace view");
    let mut refreshed = MountedFuse {
        child,
        mountpoint: mountpoint.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !document_path.exists() || !blob_path.exists() {
        if let Some(status) = refreshed.child.try_wait().unwrap() {
            panic!("remount exited before exposing the updated row: {status:?}");
        }
        assert!(
            Instant::now() < deadline,
            "refreshed mount did not expose the updated row"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let saved_document = std::fs::read_to_string(&document_path).unwrap();
    assert!(
        saved_document.contains("\"after\""),
        "row edit was not persisted: {saved_document}"
    );
    assert_eq!(
        std::fs::read(&blob_path).unwrap(),
        original,
        "row edit must retain the owner-issued Blob binding"
    );

    let unmount = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("unmount refreshed view after grouped row verification");
    assert!(
        unmount.status.success(),
        "refreshed unmount failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&unmount.stdout),
        String::from_utf8_lossy(&unmount.stderr),
    );
    assert!(refreshed.child.wait().unwrap().success());
}
