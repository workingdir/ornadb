#![allow(dead_code)]

use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
use orna_sys_v1::EnvironmentProvider;
use tempfile::TempDir;

use crate::format3;
use crate::media_commit::{SourceCaptureRow, commit_source_captures_for_table};

pub struct MountedProcess {
    child: Child,
    mountpoint: PathBuf,
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

fn mountinfo_contains(mountpoint: &Path) -> bool {
    let mountpoint = mountpoint.to_str().expect("mountpoint is UTF-8");
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read Linux mount table")
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some(mountpoint))
}

pub fn require_fuse_or_skip() -> bool {
    if std::env::var_os("ORNA_SKIP_FUSE_TEST").as_deref() == Some(std::ffi::OsStr::new("1")) {
        assert_ne!(
            std::env::var_os("ORNA_REQUIRE_FUSE").as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "ORNA_REQUIRE_FUSE=1 forbids skipping FUSE tests",
        );
        eprintln!("SKIP: ORNA_SKIP_FUSE_TEST=1");
        return false;
    }
    let available = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .is_ok()
        && (std::env::var_os("FUSERMOUNT_PATH").is_some()
            || ["fusermount3", "fusermount"]
                .into_iter()
                .any(|helper| Command::new(helper).arg("-h").output().is_ok()));
    if !available
        && std::env::var_os("ORNA_REQUIRE_FUSE").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        panic!("required FUSE integration prerequisites are unavailable");
    }
    if !available {
        eprintln!("SKIP: FUSE integration prerequisites are unavailable");
    }
    available
}

pub fn write_and_sync(path: &Path, document: &str) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(document.as_bytes())?;
    file.sync_all()
}

pub struct EditorMount {
    pub _mounted: MountedProcess,
    pub repository: orna_repository_v1::Repository,
    pub relation_id: [u8; 16],
    pub row_directory: PathBuf,
    pub data_path: PathBuf,
    pub _mount_parent: TempDir,
    pub _repository_directory: TempDir,
}

pub async fn start_editor_mount(foreign_owner: bool) -> EditorMount {
    let source = "pub table Music(id: Str) { title: Str, artist: Str, }\n";
    let initial = "{ title: \"Original\", artist: \"Kieran\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let lease_owner = if foreign_owner {
        [0x83; 16]
    } else {
        runtime_identity.repository_id
    };
    let writer = state.acquire_lease(lease_owner).await.unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x90,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

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
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };

    let row_directory = mountpoint.join("Music/london");
    let data_path = row_directory.join("data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let stderr = mounted
                .child
                .stderr
                .take()
                .map(|mut pipe| {
                    use std::io::Read;
                    let mut text = String::new();
                    let _ = pipe.read_to_string(&mut text);
                    text
                })
                .unwrap_or_default();
            panic!("mount exited {status}: {stderr}");
        }
        assert!(Instant::now() < deadline, "mount did not expose data.orna");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("Original")
    );
    EditorMount {
        _mounted: mounted,
        repository,
        relation_id,
        row_directory,
        data_path,
        _mount_parent: mount_parent,
        _repository_directory: repository_directory,
    }
}

/// True when the durable admitted row's stored canonical value contains `needle`.
pub fn durable_row_contains(rows: &[orna_repository_v1::AdmittedRow], needle: &str) -> bool {
    rows.iter().any(|row| match row.value() {
        orna_repository_v1::RowValue::Inline { encoded, .. } => encoded
            .windows(needle.len())
            .any(|window| window == needle.as_bytes()),
        _ => false,
    })
}
