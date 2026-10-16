//! Gate E.1: the workspace view is a real Linux FUSE mount backed by the
//! published row graph, and Blob reads are served as verified byte ranges.

#![cfg(target_os = "linux")]

use std::{
    fs::File,
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
        let _ = Command::new("fusermount3")
            .args(["-u", self.mountpoint.to_str().unwrap_or_default()])
            .status();
        let _ = self.child.kill();
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
        "`orna unmount` failed: {}",
        String::from_utf8_lossy(&output.stderr)
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

#[tokio::test]
async fn cwd_mount_projects_row_document_and_reads_blob_ranges() {
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let (repository_directory, repository, relation_id) =
        format3::format3_repository_for_table(source, "Music");
    let media_root = TempDir::new().unwrap();
    let expected = b"ID3-orna-range-read-test-content";
    std::fs::write(media_root.path().join("tone.mp3"), expected).unwrap();

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
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let capture = format!("sys.blob.capture_file({root}, \"tone.mp3\", 65536)");
    media_commit::commit_capture_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        &capture,
        "Music",
        "london",
        0x80,
        true,
        relation_id,
    )
    .await;

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
        .expect("start `orna mount --cwd`");
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
            panic!("mount exited {status}: {stderr}");
        }
        assert!(Instant::now() < deadline, "FUSE mount did not expose the row files");
        thread::sleep(Duration::from_millis(20));
    }

    let output = Command::new("cat")
        .arg(&data_path)
        .output()
        .expect("read data.orna with cat");
    assert!(output.status.success());
    let document = String::from_utf8(output.stdout).unwrap();
    assert!(document.contains("content: { path: \"./content.mp3\", media_type: \"audio/mpeg\" }"));

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

    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    assert!(status.success(), "mount process exits cleanly after unmount");
}
