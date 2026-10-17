#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::EnvironmentProvider;
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::{commit_source_captures_for_table, SourceCaptureRow};

struct MountedProcess {
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

fn require_fuse_or_skip() -> bool {
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
    if !available && std::env::var_os("ORNA_REQUIRE_FUSE").as_deref() == Some(std::ffi::OsStr::new("1")) {
        panic!("required FUSE integration prerequisites are unavailable");
    }
    if !available {
        eprintln!("SKIP: FUSE integration prerequisites are unavailable");
    }
    available
}

fn write_and_sync(path: &Path, document: &str) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(document.as_bytes())?;
    file.sync_all()
}

#[tokio::test]
async fn editor_saves_use_the_validated_vfs_row_path() {
    if !require_fuse_or_skip() {
        return;
    }

    let source = "pub table Music(id: Str) { title: Str, artist: Str, }\n";
    let initial = "{ title: \"Original\", artist: \"Kieran\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            "Music/london.orna",
            initial,
        );
    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x81; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x82; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x83; 16]).await.unwrap();
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
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Original"));
    let mut private_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&data_path)
        .unwrap();
    let mut observer = File::open(&data_path).unwrap();
    private_writer
        .write_all(b"{ title: \"Private draft\", artist: \"Kieran\", }\n")
        .unwrap();
    private_writer.seek(SeekFrom::Start(0)).unwrap();
    let mut writer_view = String::new();
    private_writer.read_to_string(&mut writer_view).unwrap();
    assert!(writer_view.contains("Private draft"));
    let mut observer_view = String::new();
    observer.read_to_string(&mut observer_view).unwrap();
    assert!(observer_view.contains("Original"));
    drop(private_writer);
    drop(observer);
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Original"));
    write_and_sync(
        &data_path,
        "{ title: \"In place\", artist: \"Kieran\", }\n",
    )
    .expect("in-place editor save must validate and sync");
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("In place"));

    let temporary = row_directory.join(".data.orna.editor-tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("editor creates temporary sibling");
    file.write_all(b"{ title: \"Renamed\", artist: \"Kieran\", }\n")
        .unwrap();
    file.sync_all().expect("editor syncs temporary file");
    drop(file);
    std::fs::rename(&temporary, &data_path).expect("editor atomically replaces data.orna");
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Renamed"));

    let durable_before_rejection = format3::committed_store_rows(&repository, relation_id);
    let mut invalid = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&data_path)
        .expect("open row document for invalid edit");
    invalid.set_len(0).unwrap();
    invalid
        .write_all(b"{ title: 123, artist: \"Kieran\", }\n")
        .unwrap();
    assert!(invalid.sync_all().is_err(), "invalid row must be rejected at sync");
    invalid.seek(SeekFrom::Start(0)).unwrap();
    let mut rejected_draft = String::new();
    invalid.read_to_string(&mut rejected_draft).unwrap();
    assert!(rejected_draft.contains("title: 123"));
    drop(invalid);
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Renamed"));
    let durable_after_rejection = format3::committed_store_rows(&repository, relation_id);
    assert_eq!(
        durable_after_rejection, durable_before_rejection,
        "rejected editor draft must leave the durable row unchanged",
    );
}
