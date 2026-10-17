//! Gate E.1: the workspace view is a real Linux FUSE mount backed by the
//! published row graph, and Blob reads are served as verified byte ranges.

#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;

struct MountedProcess {
    child: Child,
    mountpoint: std::path::PathBuf,
}

impl Drop for MountedProcess {
    fn drop(&mut self) {
        if mountinfo_contains(&self.mountpoint) {
            if let Some(program) = std::env::var_os("FUSERMOUNT_PATH") {
                let _ = Command::new(program)
                    .args(["-u", "--"])
                    .arg(&self.mountpoint)
                    .status();
            } else {
                for program in ["fusermount3", "fusermount"] {
                    if Command::new(program)
                        .args(["-u", "--"])
                        .arg(&self.mountpoint)
                        .status()
                        .is_ok_and(|status| status.success())
                    {
                        break;
                    }
                }
            }
        }
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn run_unmount(repository: &Path, mountpoint: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository.to_str().expect("repository path is UTF-8"),
            "unmount",
            mountpoint.to_str().expect("mountpoint is UTF-8"),
        ])
        .output()
        .expect("run `orna unmount`");
    assert!(
        output.status.success(),
        "orna unmount failed; stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn mount_options(mountpoint: &Path) -> String {
    let mountpoint = mountpoint.to_str().expect("mountpoint is UTF-8");
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read Linux mount table")
        .lines()
        .find_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.get(4).copied() == Some(mountpoint)).then(|| fields[5].to_owned())
        })
        .expect("FUSE mount is present in the kernel mount table")
}

fn directory_entries(path: &Path) -> String {
    match std::fs::read_dir(path) {
        Ok(entries) => entries
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .unwrap_or_else(|error| format!("<entry error: {error}>"))
            })
            .collect::<Vec<_>>()
            .join(", "),
        Err(error) => format!("<directory error: {error}>"),
    }
}
fn fuse_test_required() -> bool {
    std::env::var_os("ORNA_REQUIRE_FUSE").as_deref() == Some(std::ffi::OsStr::new("1"))
}

fn fuse_test_available() -> bool {
    let required = fuse_test_required();
    let explicitly_skipped =
        std::env::var_os("ORNA_SKIP_FUSE_TEST").as_deref() == Some(std::ffi::OsStr::new("1"));
    assert!(
        !required || !explicitly_skipped,
        "ORNA_REQUIRE_FUSE=1 forbids ORNA_SKIP_FUSE_TEST=1"
    );
    if explicitly_skipped {
        eprintln!("SKIP: FUSE integration tests explicitly opted out with ORNA_SKIP_FUSE_TEST=1");
        return false;
    }

    let prerequisites = (|| {
        let _device = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fuse")
            .map_err(|error| format!("/dev/fuse is unavailable: {error}"))?;
        if let Some(helper) = std::env::var_os("FUSERMOUNT_PATH") {
            Command::new(helper)
                .arg("-h")
                .output()
                .map_err(|error| format!("FUSERMOUNT_PATH is not executable: {error}"))?;
        } else if !["fusermount3", "fusermount"]
            .into_iter()
            .any(|helper| Command::new(helper).arg("-h").output().is_ok())
        {
            return Err("neither fusermount3 nor fusermount is executable".to_owned());
        }
        Ok::<(), String>(())
    })();
    match prerequisites {
        Ok(()) => true,
        Err(reason) if required => panic!("required FUSE integration prerequisites failed: {reason}"),
        Err(reason) => {
            eprintln!("SKIP: FUSE integration prerequisites are unavailable: {reason}");
            false
        }
    }
}

fn is_optional_fuse_setup_failure(stderr: &str) -> bool {
    if fuse_test_required() {
        return false;
    }
    let stderr = stderr.to_ascii_lowercase();
    (stderr.contains("fuse") || stderr.contains("fusermount") || stderr.contains("/dev/fuse"))
        && [
            "permission denied",
            "operation not permitted",
            "no such file or directory",
            "not found",
        ]
        .iter()
        .any(|reason| stderr.contains(reason))
}

fn mountinfo_contains(mountpoint: &Path) -> bool {
    let mountpoint = mountpoint.to_str().expect("mountpoint is UTF-8");
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read Linux mount table")
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some(mountpoint))
}


#[tokio::test]
async fn cwd_mount_projects_row_document_and_reads_blob_ranges() {
    if !fuse_test_available() {
        return;
    }
    let media_root = TempDir::new().unwrap();
    let expected = b"ID3-orna-range-read-test-content";
    std::fs::write(media_root.path().join("tone.mp3"), expected).unwrap();
    let source = "pub table Music(id: Str) { title: Str, artist: Str, content: Blob, cover: Blob?, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let row_source = format!(
        "{{\n    title: \"Live in London\",\n    artist: \"Kieran\",\n    content: sys.blob.capture_file({root}, \"tone.mp3\", 65536),\n    cover: null,\n}}\n"
    );
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            "Music/london.orna",
            &row_source,
        );

    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x71; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x72; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x73; 16]).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let capture = format!("sys.blob.capture_file({root}, \"tone.mp3\", 65536)");
    let blob_captures = [("content", capture.as_str())];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: None,
        ordinal: 0x80,
        insert_only: true,
    }];
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &capture_rows,
    )
    .await;

    assert_eq!(format3::committed_store_rows(&repository, relation_id).len(), 1);
    let current = repository.head().unwrap().unwrap();
    let schema = orna_project_v1::ProjectLoader::default()
        .resolve_committed_source_table_schema(
            &repository,
            &current,
            orna_runtime_v1::TableObjectId::new(relation_id),
        )
        .expect("format-3 row data is not source text");
    assert_eq!(schema.declaration_name(), "Music");

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess { child, mountpoint: mountpoint.clone() };

    let data_path = mountpoint.join("Music/london/data.orna");
    let blob_path = mountpoint.join("Music/london/content.mp3");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() || !blob_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not expose the row files; root=[{}], Music=[{}], london=[{}]",
            directory_entries(&mountpoint),
            directory_entries(&mountpoint.join("Music")),
            directory_entries(&mountpoint.join("Music/london")),
        );
        thread::sleep(Duration::from_millis(20));
    }

    let output = Command::new("cat")
        .arg(&data_path)
        .output()
        .expect("read data.orna with cat");
    assert!(output.status.success());
    let document = String::from_utf8(output.stdout).unwrap();
    let expected_document = "{\n    title: \"Live in London\",\n    artist: \"Kieran\",\n    content: { path: \"./content.mp3\", media_type: \"audio/mpeg\" },\n    cover: null,\n}\n";
    assert_eq!(document, expected_document);

    let options = mount_options(&mountpoint);
    for required in ["ro", "nodev", "nosuid", "noexec"] {
        assert!(options.split(',').any(|option| option == required), "missing {required} in mount options {options}");
    }

    let mut blob = File::open(&blob_path).expect("open projected Blob");
    let start = 7usize;
    let mut actual = [0u8; 9];
    blob.seek(SeekFrom::Start(start as u64)).unwrap();
    let count = blob.read(&mut actual).unwrap();
    assert_eq!(&actual[..count], &expected[start..start + count]);

    drop(blob);
    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    assert!(status.success(), "mount process exits cleanly after unmount");
}

#[test]
fn cwd_mount_sigterm_unmounts_and_detaches_the_view() {
    if !fuse_test_available() {
        return;
    }
    let source = "pub table Music(id: Str) { title: Str, }\n";
    let (repository_directory, _repository, _relation_id) =
        format3::format3_repository_for_table(source, "Music");
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess { child, mountpoint: mountpoint.clone() };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mountinfo_contains(&mountpoint) {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited before appearing in mountinfo ({status}): {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not appear in /proc/self/mountinfo"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let signal = Command::new("kill")
        .args(["-TERM", &mounted.child.id().to_string()])
        .status()
        .expect("send SIGTERM to mount child");
    assert!(signal.success(), "kill -TERM failed");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "mount did not stop after SIGTERM");
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "mount process exits cleanly after SIGTERM");

    let deadline = Instant::now() + Duration::from_secs(10);
    while mountinfo_contains(&mountpoint) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !mountinfo_contains(&mountpoint),
        "SIGTERM left the FUSE mountpoint in /proc/self/mountinfo"
    );
    let status = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            "status",
            "--json",
        ])
        .output()
        .expect("read mount status after SIGTERM");
    assert!(status.status.success(), "mount status failed: {:?}", status.stderr);
    assert_eq!(status.stdout, b"{\"mounts\":[]}\n");
}

#[test]
fn unmount_releases_a_dead_unregistered_fuse_mount() {
    if !fuse_test_available() {
        return;
    }
    let source = "pub table Music(id: Str) { title: Str, }\n";
    let (repository_directory, repository, _relation_id) =
        format3::format3_repository_for_table(source, "Music");
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess { child, mountpoint: mountpoint.clone() };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mountinfo_contains(&mountpoint) {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited before appearing in mountinfo ({status}): {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not appear in /proc/self/mountinfo"
        );
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(repository.mount_status().unwrap().len(), 1);
    repository.unmount_view(&mountpoint).unwrap();
    assert!(repository.mount_status().unwrap().is_empty());

    let signal = Command::new("kill")
        .args(["-KILL", &mounted.child.id().to_string()])
        .status()
        .expect("send SIGKILL to mount child");
    assert!(signal.success(), "kill -KILL failed");
    let deadline = Instant::now() + Duration::from_secs(10);
    let child_status = loop {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "mount did not exit after SIGKILL");
        thread::sleep(Duration::from_millis(20));
    };
    assert!(!child_status.success(), "SIGKILL should terminate the mount child");
    let canonicalization_error = std::fs::canonicalize(&mountpoint).err();

    run_unmount(repository_directory.path(), &mountpoint);
    assert!(
        !mountinfo_contains(&mountpoint),
        "orna unmount left the orphaned FUSE mount in /proc/self/mountinfo"
    );
    assert_eq!(repository.mount_status().unwrap().len(), 0);
    assert_eq!(
        canonicalization_error.map(|error| error.kind()),
        Some(std::io::ErrorKind::NotConnected),
        "dead FUSE mountpoint canonicalization should fail with ENOTCONN"
    );
}
