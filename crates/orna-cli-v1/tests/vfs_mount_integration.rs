//! Gate E.1: a historical format-3 view is a real Linux FUSE mount backed by the
//! published row graph, and Blob reads are served as verified byte ranges.

#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
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

/// A dirty handle on a mounted document, held by a helper process rather than
/// the test process. A fork from the test process while a handle is open copies
/// its descriptor, and the copy's close during exec issues a FUSE FLUSH that
/// EDIT-1 step 5 may accept. Keeping the handle out of the test process means
/// other tests' forks cannot trigger that flush.
struct DirtyDocument {
    child: Child,
    replies: BufReader<ChildStdout>,
}

const DIRTY_DOCUMENT_HELPER: &str = r#"import os, sys
fd = os.open(sys.argv[1], os.O_WRONLY | os.O_TRUNC)
data = sys.argv[2].encode()
try:
    while data:
        data = data[os.write(fd, data):]
    print("ready", flush=True)
except OSError as error:
    print("err%d" % error.errno, flush=True)
for line in sys.stdin:
    if line.strip() == "fsync":
        try:
            os.fsync(fd)
            print("ok", flush=True)
        except OSError as error:
            print("err%d" % error.errno, flush=True)
"#;

impl DirtyDocument {
    /// Opens `path` for writing with truncation in a helper process and writes
    /// `body` through that handle.
    fn open_and_write(path: &Path, body: &str) -> Self {
        let mut child = Command::new("python3")
            .arg("-c")
            .arg(DIRTY_DOCUMENT_HELPER)
            .arg(path)
            .arg(body)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 starts the dirty document helper");
        let mut replies = BufReader::new(child.stdout.take().expect("helper stdout"));
        let mut reply = String::new();
        replies
            .read_line(&mut reply)
            .expect("dirty document helper reply");
        assert_eq!(
            reply.trim_end(),
            "ready",
            "dirty document helper could not write {}",
            path.display()
        );
        Self { child, replies }
    }

    /// `fsync`s the handle through the helper, as `File::sync_all` would.
    fn sync_all(&mut self) {
        let stdin = self.child.stdin.as_mut().expect("helper stdin");
        writeln!(stdin, "fsync").expect("send fsync to dirty document helper");
        let mut reply = String::new();
        self.replies
            .read_line(&mut reply)
            .expect("dirty document fsync reply");
        assert_eq!(reply.trim_end(), "ok", "dirty document fsync failed");
    }
}

impl Drop for DirtyDocument {
    fn drop(&mut self) {
        // EOF ends the helper loop; its exit closes the handle as `drop(File)` would.
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn run_unmount(repository: &Path, mountpoint: &Path) {
    run_unmount_from(repository, Path::new("."), mountpoint);
}

fn run_unmount_from(repository: &Path, working_directory: &Path, mountpoint: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(working_directory)
        .args([
            "--db",
            repository.to_str().expect("repository path is UTF-8"),
            "unmount",
        ])
        .arg(mountpoint)
        .output()
        .expect("run `orna unmount`");
    assert!(
        output.status.success(),
        "orna unmount failed; stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn git_output(repository: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repository)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
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
        Err(reason) if required => {
            panic!("required FUSE integration prerequisites failed: {reason}")
        }
        Err(reason) => {
            eprintln!("SKIP: FUSE integration prerequisites are unavailable: {reason}");
            false
        }
    }
}

/// Whether python3, which runs the dirty document helper, is available. Its
/// tests skip with a note when it is missing, and fail under `ORNA_REQUIRE_FUSE=1`.
fn dirty_document_helper_available() -> bool {
    let available = Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !available {
        assert!(
            !fuse_test_required(),
            "ORNA_REQUIRE_FUSE=1 requires python3 for the dirty document helper"
        );
        eprintln!("SKIP: python3 is unavailable for the dirty document helper");
    }
    available
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

/// Decodes the octal escapes (`\040`, `\011`, `\012`, `\134`) the kernel applies to a
/// /proc/self/mountinfo path field, so a mountpoint containing spaces, tabs, newlines or
/// backslashes compares equal to its real path. Any other backslash is kept as written.
fn unescape_mountinfo_field(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let is_octal_escape = bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4].iter().all(|digit| (b'0'..=b'7').contains(digit));
        if is_octal_escape {
            let value = bytes[index + 1..index + 4]
                .iter()
                .fold(0u32, |acc, digit| acc * 8 + u32::from(digit - b'0'));
            out.push(value as u8);
            index += 4;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mountinfo_contains(mountpoint: &Path) -> bool {
    // The kernel records the resolved mountpoint, so a symlinked TMPDIR must be resolved too.
    let mountpoint =
        std::fs::canonicalize(mountpoint).unwrap_or_else(|_| mountpoint.to_path_buf());
    let mountpoint = mountpoint.to_str().expect("mountpoint is UTF-8");
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read Linux mount table")
        .lines()
        .any(|line| {
            line.split_whitespace()
                .nth(4)
                .is_some_and(|field| unescape_mountinfo_field(field) == mountpoint)
        })
}

#[test]
fn unescape_mountinfo_field_decodes_octal_escapes() {
    assert_eq!(unescape_mountinfo_field("/tmp/a\\040b"), "/tmp/a b");
    assert_eq!(unescape_mountinfo_field("x\\011y\\012z\\134w"), "x\ty\nz\\w");
    assert_eq!(unescape_mountinfo_field("plain/path"), "plain/path");
    assert_eq!(unescape_mountinfo_field("trailing\\04"), "trailing\\04");
}

#[tokio::test]
/// ORNA-VFS-002, ORNA-VFS-003, ORNA-VFS-005, ORNA-VFS-018, and ORNA-VFS-023: a pinned view projects shared rows and Blob bytes, survives backing HEAD movement, and hides repository administration paths.
async fn mount_at_head_parent_pins_store_row_and_media_after_head_advances() {
    if !fuse_test_available() {
        return;
    }
    let media_root = TempDir::new().unwrap();
    let payload_len = 3 * 1_048_576 + 17;
    let mut entropy = 0x0123_4567_89ab_cdef_u64;
    let expected = (0..payload_len)
        .map(|_| {
            entropy = entropy
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (entropy >> 32) as u8
        })
        .collect::<Vec<_>>();
    std::fs::write(media_root.path().join("tone.mp3"), &expected).unwrap();
    let source =
        "pub table Music(id: Str) { title: Str, artist: Str, content: Blob, cover: Blob?, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let row_source = format!(
        "{{\n    title: \"Live in London\",\n    artist: \"Kieran\",\n    content: sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 4194304), \"audio/mpeg\"),\n    cover: null,\n}}\n"
    );
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(source, "Music");

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(4 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 4194304), \"audio/mpeg\")"
    );
    let blob_captures = [("content", capture.as_str())];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: &row_source,
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
    drop(state);

    assert_eq!(
        format3::committed_store_rows(&repository, relation_id).len(),
        1
    );
    let current = repository.head().unwrap().unwrap();
    let historical_commit = current.clone();
    let historical_rows = format3::committed_store_rows(&repository, relation_id);
    // Advance HEAD past the historical commit by publishing a retitled store
    // row, so a mount that followed HEAD would show the newer title. Rows
    // live only in the store (ORNA-REPO-004).
    let revised_row = format!(
        "{{\n    title: \"Recorded in London\",\n    artist: \"Kieran\",\n    content: {capture},\n    cover: null,\n}}\n"
    );
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let revised_context = format3::format3_context_for_snapshot(&repository, current.as_str());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &revised_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &blob_captures,
            source_row: &revised_row,
            ordinal: 0x84,
            insert_only: false,
        }],
    )
    .await;
    drop(state);
    assert_ne!(
        repository.head().unwrap().unwrap(),
        historical_commit
    );
    assert_ne!(
        format3::committed_store_rows(&repository, relation_id)[0].value(),
        historical_rows[0].value(),
        "HEAD carries the retitled row"
    );
    let historical_commit_selector = "HEAD~1";
    let (selected_commit, historical_format) = repository
        .open_pinned_format_context_with_commit(historical_commit_selector)
        .expect("resolve the same prior commit through the repository format context");
    assert_eq!(selected_commit.as_str(), historical_commit.as_str());
    let row_map = historical_format
        .load_row_map(relation_id)
        .expect("load the prior commit relation row map");
    let graph = historical_format
        .open_native_graph(&row_map)
        .expect("open the prior commit native store graph");
    let scope = graph.open_read_scope().expect("open a historical graph read scope");
    let stored_row = graph
        .lookup_row(
            &orna_repository_v1::TypedKey::Text("london".to_owned()),
            &scope,
        )
        .expect("read london from the historical graph")
        .expect("the historical graph contains the mounted row");
    assert!(
        !stored_row.value().dependencies().is_empty(),
        "the historical store row references the captured media objects"
    );
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
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--at",
            historical_commit_selector,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --at historical commit");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };

    let data_path = mountpoint.join("Music/london/data.orna");
    let blob_path = mountpoint.join("Music/london/content.mp3");
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path(), blob_path.as_path()]) {
        return;
    }

    let output = Command::new("cat")
        .arg(&data_path)
        .output()
        .expect("read data.orna with cat");
    assert!(output.status.success());
    let document = String::from_utf8(output.stdout).unwrap();
    let identities = orna_project_v1::ObjectIdentityManifest::read_committed(&repository, &current)
        .expect("read the committed field identities");
    let mut expected_fields = vec![
        ("title", "\"Live in London\""),
        (
            "content",
            "{ path: \"./content.mp3\", media_type: \"audio/mpeg\" }",
        ),
        ("artist", "\"Kieran\""),
        ("cover", "null"),
    ];
    expected_fields.sort_by_key(|(name, _)| {
        let qualified_name = format!("Music.{name}");
        *identities
            .id(orna_project_v1::ObjectKind::Field, &qualified_name)
            .expect("stored field identity exists")
    });
    let expected_fields = expected_fields
        .into_iter()
        .map(|(name, value)| format!("    {name}: {value},\n"))
        .collect::<String>();
    let expected_document = format!("{{\n{expected_fields}}}\n");
    assert_eq!(document, expected_document);

    let options = mount_options(&mountpoint);
    let root_entries = directory_entries(&mountpoint);
    assert!(
        !root_entries
            .split_whitespace()
            .any(|entry| matches!(entry, ".git" | ".orna")),
        "the view must not expose repository administration paths: {root_entries}"
    );
    for required in ["ro", "nodev", "nosuid", "noexec"] {
        assert!(
            options.split(',').any(|option| option == required),
            "missing {required} in mount options {options}"
        );
    }

    let write_open_error = OpenOptions::new()
        .write(true)
        .open(&blob_path)
        .expect_err("opening a projected Blob for writing must fail");
    assert_eq!(
        write_open_error.raw_os_error(),
        Some(30),
        "write-open must fail with Linux EROFS",
    );
    let create_dir_error = std::fs::create_dir(mountpoint.join("Music/london/new-directory"))
        .expect_err("creating a directory in the mount must fail");
    assert_eq!(
        create_dir_error.raw_os_error(),
        Some(30),
        "mkdir must fail with Linux EROFS",
    );
    let remove_file_error =
        std::fs::remove_file(&data_path).expect_err("removing a projected file must fail");
    assert_eq!(
        remove_file_error.raw_os_error(),
        Some(30),
        "unlink must fail with Linux EROFS",
    );
    let chmod_error = std::fs::set_permissions(&blob_path, std::fs::Permissions::from_mode(0o600))
        .expect_err("changing permissions in the mount must fail");
    assert_eq!(
        chmod_error.raw_os_error(),
        Some(30),
        "chmod must fail with Linux EROFS",
    );

    let current_commit = current.to_string();
    let parent_ref = format!("{current_commit}^");
    let parent = git_output(
        repository_directory.path(),
        &["rev-parse", parent_ref.as_str()],
    );
    let parent_tree_ref = format!("{parent}^{{tree}}");
    let parent_tree = git_output(
        repository_directory.path(),
        &["rev-parse", parent_tree_ref.as_str()],
    );
    let moved_head = git_output(
        repository_directory.path(),
        &[
            "commit-tree",
            parent_tree.as_str(),
            "-m",
            "move FUSE test HEAD",
        ],
    );
    git_output(
        repository_directory.path(),
        &["update-ref", "HEAD", moved_head.as_str()],
    );
    assert_ne!(moved_head, current_commit);
    assert_eq!(
        git_output(repository_directory.path(), &["rev-parse", "HEAD"]),
        moved_head,
        "the worktree HEAD moved away from the mounted commit",
    );
    git_output(repository_directory.path(), &["gc", "--prune=now"]);
    git_output(repository_directory.path(), &["repack", "-ad"]);
    assert_eq!(
        std::fs::read_to_string(&data_path).unwrap(),
        expected_document,
        "the mounted snapshot remains visible after HEAD moves and GC",
    );

    let mut blob = File::open(&blob_path).expect("open projected Blob");
    assert_eq!(
        blob.metadata().unwrap().len() as usize,
        expected.len(),
        "the projected Blob size comes from its stored descriptor",
    );
    let boundary_range_len = 1_048_576 + 1;
    let mut actual = vec![0u8; boundary_range_len];
    blob.seek(SeekFrom::Start(0)).unwrap();
    blob.read_exact(&mut actual).unwrap();
    assert_eq!(actual.as_slice(), &expected[..boundary_range_len]);

    drop(blob);
    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount process exits cleanly after unmount; status={status:?}; stderr={stderr}"
    );
}

#[tokio::test]
/// ORNA-VFS-005: workspace editing stays attached to the selected worktree snapshot after HEAD advances.
async fn cwd_mount_edit_row_uses_pinned_snapshot_after_head_advances() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest).await.unwrap();
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
            source_row: initial,
            ordinal: 0x92,
            insert_only: true,
        }],
    )
    .await;
    let table = orna_runtime_v1::RuntimeTableIdentity::new(
        "Music",
        orna_runtime_v1::TableObjectId::new(relation_id),
    )
    .unwrap();
    let tables = [table];
    let before = state
        .begin_admitted_table_activation(&tables)
        .await
        .unwrap()
        .query_exact("Music", b"london")
        .unwrap()
        .unwrap()
        .to_vec();
    drop(state);

    let pinned_commit = repository.head().unwrap().unwrap();
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let row_path = mountpoint.join("Music/london");
    let document_path = row_path.join("data.orna");
    if !wait_for_mount_paths(&mut mounted, &[document_path.as_path()]) {
        return;
    }
    std::fs::write(
        repository_directory.path().join("main.orna"),
        "invalid @@@ source\n",
    )
    .unwrap();
    format3::git(repository_directory.path(), &["add", "main.orna"]);
    format3::git(
        repository_directory.path(),
        &[
            "commit",
            "--quiet",
            "-m",
            "advance HEAD while workspace mount stays pinned",
        ],
    );
    let head_after_move = repository.head().unwrap().unwrap();
    assert_ne!(head_after_move, pinned_commit);
    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("editor");
    std::fs::write(&editor, "#!/bin/sh\nprintf '%s\n' '{ title: \"after\", }' > \"$1\"\n").unwrap();
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
        .output()
        .expect("run edit --row against mounted absolute row directory");
    assert!(
        output.status.success(),
        "edit --row failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    assert!(status.success(), "mount exits cleanly after edit and unmount");
    let state = RuntimeState::open(&repository, runtime_identity, digest).await.unwrap();
    let after = state
        .begin_admitted_table_activation(&tables)
        .await
        .unwrap()
        .query_exact("Music", b"london")
        .unwrap()
        .unwrap()
        .to_vec();
    assert_eq!(
        orna_value_v1::Value::decode(&before)
            .expect("decode the original admitted row")
            .raw(),
        &orna_value_v1::Raw::Array(vec![orna_value_v1::Raw::Text("before".to_owned())]),
    );
    assert_eq!(
        orna_value_v1::Value::decode(&after)
            .expect("decode the edited admitted row")
            .raw(),
        &orna_value_v1::Raw::Array(vec![orna_value_v1::Raw::Text("after".to_owned())]),
    );
    assert_eq!(repository.head().unwrap().unwrap(), head_after_move);
}

/// A live `mount --cwd` over the committed `Music/london` row. The handles it
/// keeps let a test act as another writer and read what the store holds.
struct MountedMusicRow {
    mounted: MountedProcess,
    repository_directory: TempDir,
    repository: orna_repository_v1::Repository,
    runtime_identity: orna_runtime_v1::RuntimeIdentity,
    digest: [u8; 32],
    relation_id: [u8; 16],
    row_path: std::path::PathBuf,
    scripts: TempDir,
    _mount_parent: TempDir,
}

/// Seeds `Music/london` with `initial`, then mounts the cwd. Returns `None`
/// when FUSE is opted out or its prerequisites are optional and absent.
async fn mount_music_row(initial: &str) -> Option<MountedMusicRow> {
    if !fuse_test_available() {
        return None;
    }
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let row_path = mountpoint.join("Music/london");
    let document_path = row_path.join("data.orna");
    if !wait_for_mount_paths(&mut mounted, &[document_path.as_path()]) {
        return None;
    }
    Some(MountedMusicRow {
        mounted,
        repository_directory,
        repository,
        runtime_identity,
        digest,
        relation_id,
        row_path,
        scripts: TempDir::new().unwrap(),
        _mount_parent: mount_parent,
    })
}

impl MountedMusicRow {
    /// Saves `body` through `edit --row` on the mounted absolute row directory.
    fn save(&self, name: &str, body: &str) -> std::process::Output {
        let editor = self.scripts.path().join(name);
        std::fs::write(
            &editor,
            format!("#!/bin/sh\nprintf '%s\\n' '{body}' > \"$1\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
        Command::new(env!("CARGO_BIN_EXE_orna"))
            .args([
                "--db",
                self.repository_directory.path().to_str().unwrap(),
                "edit",
                "--row",
            ])
            .arg(&self.row_path)
            .env("EDITOR", &editor)
            .output()
            .expect("run edit --row against mounted absolute row directory")
    }

    /// Commits `source_row` over `Music/london` through `orna edit`. The live
    /// mount owner executes that edit (ORNA-TURSO-005: other local writers
    /// coordinate with the owner or fail), so HEAD and the row advance without
    /// the mount's captured baseline following them.
    fn replace_row_through_owner(&self, source_row: &str) {
        let editor = self.scripts.path().join("owner-routed-edit");
        std::fs::write(
            &editor,
            format!("#!/bin/sh\nprintf '%s\\n' '{source_row}' > \"$1\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_orna"))
            .arg("--db")
            .arg(self.repository_directory.path())
            .args(["edit", "Music/london/data.orna"])
            .env("EDITOR", &editor)
            .output()
            .expect("run orna edit through the mount owner");
        assert_saved(&output, "owner-routed other-writer edit");
    }

    /// A writer outside the mount that takes the writer lease directly is
    /// refused while the mount owns it (ORNA-TURSO-005).
    async fn assert_direct_writer_refused(&self) {
        let state = RuntimeState::open(&self.repository, self.runtime_identity, self.digest)
            .await
            .unwrap();
        let Err(refusal) = state
            .acquire_lease(self.runtime_identity.repository_id)
            .await
        else {
            panic!("a direct foreign lease must be refused while the mount owns the writer");
        };
        assert!(
            matches!(refusal, orna_runtime_v1::RuntimeError::LeaseHeld),
            "a direct foreign lease is refused as LeaseHeld: {refusal:?}"
        );
    }

    /// The `Music/london` row bytes the store holds now. Call after `unmount`.
    async fn stored_row(&self) -> Vec<u8> {
        let state = RuntimeState::open(&self.repository, self.runtime_identity, self.digest)
            .await
            .unwrap();
        let table = orna_runtime_v1::RuntimeTableIdentity::new(
            "Music",
            orna_runtime_v1::TableObjectId::new(self.relation_id),
        )
        .unwrap();
        let tables = [table];
        state
            .begin_admitted_table_activation(&tables)
            .await
            .unwrap()
            .query_exact("Music", b"london")
            .unwrap()
            .unwrap()
            .to_vec()
    }

    fn unmount(&mut self) {
        run_unmount(
            self.repository_directory.path(),
            &self.mounted.mountpoint,
        );
        let status = self.mounted.child.wait().unwrap();
        assert!(status.success(), "mount exits cleanly after unmount");
    }
}

fn assert_saved(output: &std::process::Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_title(row: &[u8], expected: &str) {
    assert_eq!(
        orna_value_v1::Value::decode(row)
            .expect("decode the admitted row")
            .raw(),
        &orna_value_v1::Raw::Array(vec![orna_value_v1::Raw::Text(expected.to_owned())]),
    );
}

#[tokio::test]
/// ORNA-VFS-005: consecutive saves of one row in one mount both commit; the
/// second compares against the row the first save accepted.
async fn cwd_mount_consecutive_saves_of_one_row_both_commit() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    assert_saved(&row.save("first", "{ title: \"after-one\", }"), "first save");
    assert_saved(&row.save("second", "{ title: \"after-two\", }"), "second save");
    row.unmount();
    assert_title(&row.stored_row().await, "after-two");
}

#[tokio::test]
/// ORNA-VFS-005: a save after another writer changed the row and advanced HEAD
/// is refused as stale, and the other writer's row is kept.
async fn cwd_mount_save_after_another_writer_advances_head_is_stale() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    assert_saved(&row.save("first", "{ title: \"after-one\", }"), "first save");
    row.assert_direct_writer_refused().await;
    row.replace_row_through_owner("{ title: \"other\", }");
    let repository = row.repository_directory.path().to_path_buf();
    std::fs::write(repository.join("notes.txt"), "advance HEAD\n").unwrap();
    format3::git(&repository, &["add", "notes.txt"]);
    format3::git(
        &repository,
        &[
            "commit",
            "--quiet",
            "-m",
            "advance HEAD while another writer owns the row",
        ],
    );
    let second = row.save("second", "{ title: \"after-two\", }");
    let stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(
        !second.status.success(),
        "a save against a row another writer changed must be refused; stdout={stdout}; stderr={stderr}"
    );
    assert!(
        format!("{stdout}{stderr}").contains("stale"),
        "the refusal names the stale baseline; stdout={stdout}; stderr={stderr}"
    );
    row.unmount();
    assert_title(&row.stored_row().await, "other");
}

#[tokio::test]
/// ORNA-VFS-008/009/024: a dirty handle whose baseline the owner's `orna edit`
/// advanced is stale. Its close flush is refused and retained as a draft that
/// blocks unmount, the mount shows the owner's accepted row, and a save against
/// that row resolves the draft.
async fn cwd_mount_close_flush_after_owner_routed_edit_is_stale() {
    if !dirty_document_helper_available() {
        return;
    }
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let document = DirtyDocument::open_and_write(
        &row.row_path.join("data.orna"),
        "{ title: \"stale close\", }\n",
    );
    row.replace_row_through_owner("{ title: \"other\", }");
    drop(document);
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            row.repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&row.mounted.mountpoint)
        .output()
        .expect("run unmount after the stale close");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !refused.status.success(),
        "unmount accepted the stale candidate: {diagnostic}"
    );
    assert!(
        diagnostic.contains("unresolved workspace drafts remain")
            && diagnostic.contains("Music/london/data.orna")
            && diagnostic.contains("sys.vfs.stale_edit"),
        "unmount names the retained stale draft and its cause: {diagnostic}"
    );
    let shown = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(
        shown.contains("other") && !shown.contains("stale close"),
        "the mount shows the owner's accepted row, not the stale candidate: {shown}"
    );

    let mut resolving = DirtyDocument::open_and_write(
        &row.row_path.join("data.orna"),
        "{ title: \"after refresh\", }\n",
    );
    resolving.sync_all();
    drop(resolving);
    row.unmount();
    assert_title(&row.stored_row().await, "after refresh");
}

#[tokio::test]
/// ORNA-VFS-005: a HEAD rename of a mounted table keeps saves on the pinned
/// declaration name the mounted row mutations address.
async fn cwd_mount_save_after_head_renames_table_keeps_pinned_declaration() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let rename = Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(&repository)
        .args(["mv", "Music", "Archive"])
        .output()
        .expect("run mv at HEAD");
    assert!(
        rename.status.success(),
        "mv failed; stderr={}",
        String::from_utf8_lossy(&rename.stderr)
    );
    format3::git(&repository, &["add", "--all"]);
    format3::git(
        &repository,
        &["commit", "--quiet", "-m", "rename Music to Archive at HEAD"],
    );
    let head_source = std::fs::read_to_string(repository.join("main.orna")).unwrap();
    assert!(
        head_source.contains("Archive"),
        "HEAD declares the renamed table: {head_source}"
    );
    assert_saved(&row.save("save", "{ title: \"after\", }"), "save through pinned name");
    row.unmount();
    assert_title(&row.stored_row().await, "after");
}

#[tokio::test]
/// ORNA-VFS-005: a historical snapshot mount cannot be retargeted or edited.
async fn edit_row_refuses_read_only_historical_mount_without_mutation() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest).await.unwrap();
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
            source_row: initial,
            ordinal: 0x93,
            insert_only: true,
        }],
    )
    .await;
    let head_before = repository.head().unwrap().unwrap();
    let rows_before = format3::committed_store_rows(&repository, relation_id);
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("history");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--at",
            "HEAD",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --at HEAD");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let row_path = mountpoint.join("Music/london");
    let document_path = row_path.join("data.orna");
    if !wait_for_mount_paths(&mut mounted, &[document_path.as_path()]) {
        return;
    }

    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("editor");
    let marker = scripts.path().join("editor-was-run");
    std::fs::write(&editor, "#!/bin/sh\nprintf invoked > \"$ORNA_EDIT_MARKER\"\n").unwrap();
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
        .env("ORNA_EDIT_MARKER", &marker)
        .output()
        .expect("run edit --row on read-only historical mount");
    assert!(
        !output.status.success(),
        "edit --row must refuse a read-only --at mount"
    );
    assert!(!marker.exists(), "read-only refusal must happen before launching the editor");
    assert_eq!(repository.head().unwrap().unwrap(), head_before);
    assert_eq!(format3::committed_store_rows(&repository, relation_id), rows_before);

    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    assert!(status.success(), "historical mount exits cleanly after unmount");
}

#[tokio::test]
/// ORNA-VFS-011: replacing content through a scratch rename compares the captured baseline and preserves row identity.
async fn cwd_mount_replace_content_updates_blob_descriptor_and_preserves_row_identity() {
    if !fuse_test_available() {
        return;
    }

    let media_root = TempDir::new().unwrap();
    let original = b"original-mounted-content";
    let replacement = b"replacement-mounted-content-with-a-new-hash";
    let source_file = media_root.path().join("tone.mp3");
    std::fs::write(&source_file, original).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let row_source = format!(
        "{{\n    content: sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 65536), \"audio/mpeg\"),\n}}\n"
    );
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(source, "Music");

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: &row_source,
        ordinal: 0x91,
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

    let table = orna_runtime_v1::RuntimeTableIdentity::new(
        "Music",
        orna_runtime_v1::TableObjectId::new(relation_id),
    )
    .unwrap();
    let tables = [table];
    let before = state
        .begin_admitted_table_activation(&tables)
        .await
        .unwrap();
    let old_row = before
        .query_exact("Music", b"london")
        .unwrap()
        .unwrap()
        .to_vec();
    drop(before);
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let blob_path = mountpoint.join("Music/london/content.mp3");
    if !wait_for_mount_paths(&mut mounted, &[blob_path.as_path()]) {
        return;
    }

    let scratch_path = mountpoint.join("Music/london/.content.mp3.replace");
    std::fs::write(&scratch_path, replacement).unwrap();
    if let Err(error) = std::fs::rename(&scratch_path, &blob_path) {
        run_unmount(repository_directory.path(), &mountpoint);
        let _ = mounted.child.wait();
        let mut stderr = String::new();
        if let Some(mut pipe) = mounted.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        panic!("mounted Blob replacement rename failed: {error}; stderr={stderr}");
    }
    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);

    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount process exits cleanly after replacement and unmount; status={status:?}; stderr={stderr}"
    );

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let after = state
        .begin_admitted_table_activation(&tables)
        .await
        .unwrap();
    let new_row = after.query_exact("Music", b"london").unwrap().unwrap();
    assert_ne!(
        old_row.as_slice(),
        new_row,
        "the same row key must now carry the replacement Blob descriptor/hash"
    );
}
#[tokio::test]
/// ORNA-VFS-010, ORNA-VFS-018: metadata-only saves do not fetch corrupt sibling content; reading that content reports I/O failure.
async fn cwd_mount_metadata_save_avoids_corrupt_sibling_payload() {
    if !fuse_test_available() {
        return;
    }

    let media_root = TempDir::new().unwrap();
    let expected = b"corrupt-chunk-read-test-content";
    let source_file = media_root.path().join("tone.mp3");
    std::fs::write(&source_file, expected).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let row_source = format!(
        "{{\n    content: sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 65536), \"audio/mpeg\"),\n}}\n"
    );
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(source, "Music");

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: &row_source,
        ordinal: 0x91,
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
    drop(state);

    let chunk_oid = git_output(
        repository_directory.path(),
        &["hash-object", source_file.to_str().unwrap()],
    );
    let objects = repository_directory.path().join(git_output(
        repository_directory.path(),
        &["rev-parse", "--git-path", "objects"],
    ));
    let chunk_path = objects.join(&chunk_oid[..2]).join(&chunk_oid[2..]);
    assert!(
        chunk_path.is_file(),
        "captured chunk is stored as a loose object"
    );
    std::fs::set_permissions(&chunk_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&chunk_path, b"corrupt Git loose object bytes").unwrap();

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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let blob_path = mountpoint.join("Music/london/content.mp3");
    if !wait_for_mount_paths(&mut mounted, &[blob_path.as_path()]) {
        return;
    }


    let metadata = Command::new("cat")
        .arg(mountpoint.join("Music/london/data.orna"))
        .output()
        .expect("read row metadata after corrupting the media chunk");
    assert!(
        metadata.status.success(),
        "metadata read must not fetch corrupt media payload"
    );
    let metadata = String::from_utf8(metadata.stdout).unwrap();
    assert!(metadata.contains(
        "content: { path: \"./content.mp3\", media_type: \"audio/mpeg\" }"
    ));

    let data_path = mountpoint.join("Music/london/data.orna");
    let replacement_document = "{\n    content: { path: \"./content.mp3\", media_type: \"audio/mpeg\" },\n}\n";
    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .expect("open projected row document for metadata-only save");
    document.write_all(replacement_document.as_bytes()).unwrap();
    document
        .sync_all()
        .expect("metadata-only save must not fetch corrupt sibling payload");
    drop(document);

    let mut blob = File::open(&blob_path).expect("open projected Blob");
    let mut byte = [0u8; 1];
    let error = blob
        .read_exact(&mut byte)
        .expect_err("a corrupt chunk object must not return data");
    assert_eq!(
        error.raw_os_error(),
        Some(5),
        "a corrupt chunk object must surface as Linux EIO",
    );

    drop(blob);
    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount process exits cleanly after unmount; status={status:?}; stderr={stderr}"
    );
}

#[tokio::test]
/// ORNA-VFS-016: an invalid duplicate-Blob row does not hide unrelated valid keys.
async fn mount_keeps_good_rows_available_when_one_row_has_duplicate_blob_values() {
    if !fuse_test_available() {
        return;
    }

    let media_root = TempDir::new().unwrap();
    let expected = b"row-local-error-test-content";
    std::fs::write(media_root.path().join("tone.mp3"), expected).unwrap();
    let source = "pub table Music(id: Str) { title: Str, content: Blob, duplicate: Blob?, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let capture = format!("sys.blob.capture_file({root}, \"tone.mp3\", 65536)");
    let bad_row = format!(
        "{{\n    title: \"bad\",\n    content: {capture},\n    duplicate: {capture},\n}}\n",
    );
    let good_row =
        format!("{{\n    title: \"good\",\n    content: {capture},\n    duplicate: null,\n}}\n",);
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(source, "Music");

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let duplicate_blob_fields = [
        ("content", capture.as_str()),
        ("duplicate", capture.as_str()),
    ];
    let single_blob_fields = [("content", capture.as_str())];
    let capture_rows = [
        media_commit::SourceCaptureRow {
            key: "bad",
            blob_fields: &duplicate_blob_fields,
            source_row: &bad_row,
            ordinal: 0x81,
            insert_only: true,
        },
        media_commit::SourceCaptureRow {
            key: "good",
            blob_fields: &single_blob_fields,
            source_row: &good_row,
            ordinal: 0x82,
            insert_only: true,
        },
    ];
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let good_data_path = mountpoint.join("Music/good/data.orna");
    let bad_data_path = mountpoint.join("Music/bad/data.orna");
    if !wait_for_mount_paths(&mut mounted, &[good_data_path.as_path()]) {
        return;
    }

    let rows = std::fs::read_dir(mountpoint.join("Music"))
        .expect("indexed table listing succeeds despite a malformed row")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(rows.iter().any(|name| name == "bad"));
    assert!(rows.iter().any(|name| name == "good"));
    let bad_row_error = std::fs::read(&bad_data_path).expect_err("malformed row is isolated");
    assert_eq!(
        bad_row_error.raw_os_error(),
        Some(5),
        "a malformed row returns Linux EIO without failing the mount",
    );
    let good_document = std::fs::read_to_string(&good_data_path).unwrap();
    assert!(good_document.contains("title: \"good\""));
    assert!(mounted.child.try_wait().unwrap().is_none());

    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount process exits cleanly after unmount; status={status:?}; stderr={stderr}"
    );
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
    let stderr_log = std::fs::File::create(mount_parent.path().join("mount-stderr.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_log))
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    if !wait_for_mount_paths(&mut mounted, &[mountpoint.as_path()]) {
        return;
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
        assert!(
            Instant::now() < deadline,
            "mount did not stop after SIGTERM"
        );
        thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        let stderr = std::fs::read_to_string(mount_parent.path().join("mount-stderr.log")).unwrap_or_default();
        panic!("mount process exits cleanly after SIGTERM ({status}): {stderr}");
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while mountinfo_contains(&mountpoint) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !mountinfo_contains(&mountpoint),
        "SIGTERM left the FUSE mountpoint in /proc/self/mountinfo"
    );
    let status = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            "status",
            "--json",
        ])
        .output()
        .expect("read mount status after SIGTERM");
    assert!(
        status.status.success(),
        "mount status failed: {:?}",
        status.stderr
    );
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
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    if !wait_for_mount_paths(&mut mounted, &[mountpoint.as_path()]) {
        return;
    }

    assert_eq!(repository.mount_status().unwrap().len(), 1);

    // SIGKILL the daemon and reap it before anything touches the view record. The record stays
    // registered to a dead owner, so the daemon's signal helper (which unmounts only once the record
    // is gone) never runs: the mount stays orphaned and the directory request below sees ENOTCONN.
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
        assert!(
            Instant::now() < deadline,
            "mount did not exit after SIGKILL"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        !child_status.success(),
        "SIGKILL should terminate the mount child"
    );
    assert!(
        mountinfo_contains(&mountpoint),
        "the orphaned FUSE mount remains until orna unmount releases it"
    );
    let dead_mount_error = std::fs::read_dir(&mountpoint)
        .and_then(|entries| entries.collect::<std::io::Result<Vec<_>>>().map(|_| ()))
        .err();

    repository.unmount_view(&mountpoint).unwrap();
    assert!(repository.mount_status().unwrap().is_empty());
    run_unmount_from(
        repository_directory.path(),
        mount_parent.path(),
        Path::new("library"),
    );

    assert!(
        !mountinfo_contains(&mountpoint),
        "orna unmount left the orphaned FUSE mount in /proc/self/mountinfo"
    );
    assert_eq!(repository.mount_status().unwrap().len(), 0);
    run_unmount_from(
        repository_directory.path(),
        mount_parent.path(),
        Path::new("library"),
    );
    assert_eq!(
        dead_mount_error.map(|error| error.kind()),
        Some(std::io::ErrorKind::NotConnected),
        "a directory request to the dead FUSE mount should fail with ENOTCONN"
    );
}
#[tokio::test]
/// ORNA-VFS-024: unmount refuses an unresolved draft and succeeds after fsync accepts it.
async fn cwd_mount_refuses_unmount_with_dirty_draft_and_allows_clean_unmount() {
    if !dirty_document_helper_available() {
        return;
    }
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let data_path = mountpoint.join("Music/london/data.orna");
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    let mut document = DirtyDocument::open_and_write(&data_path, "{ title: \"after\", }\n");
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("run dirty-state unmount");
    assert!(!refused.status.success(), "dirty mount unmount was accepted");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        diagnostic.contains("unresolved workspace drafts remain"),
        "unmount refusal did not explain draft recovery: {diagnostic}"
    );
    assert!(mountinfo_contains(&mountpoint));
    assert_eq!(repository.mount_status().unwrap().len(), 1);

    document.sync_all();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
}
#[tokio::test]
/// ORNA-VFS-024: unmount refuses a dirty draft whose candidate is schema-invalid, and succeeds after a corrected candidate is synced.
async fn cwd_mount_refuses_unmount_with_invalid_dirty_draft_and_allows_clean_unmount() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let data_path = mountpoint.join("Music/london/data.orna");
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    // A schema-invalid candidate stays unsynced in this open handle. The handle is
    // dirty, so unmount is refused before any sync accepts the candidate (ORNA-VFS-024).
    document.write_all(b"{ title: 123, }\n").unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("run dirty-state unmount");
    assert!(!refused.status.success(), "dirty mount unmount was accepted");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        diagnostic.contains("unresolved workspace drafts remain"),
        "unmount refusal did not explain draft recovery: {diagnostic}"
    );
    assert!(mountinfo_contains(&mountpoint));
    assert_eq!(repository.mount_status().unwrap().len(), 1);

    // Replace the refused candidate with a valid row and sync it: the accepted
    // sync clears the unresolved draft, so unmount can succeed.
    document.set_len(0).unwrap();
    document.seek(SeekFrom::Start(0)).unwrap();
    document.write_all(b"{ title: \"after\", }\n").unwrap();
    document.sync_all().unwrap();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
}
#[tokio::test]
/// ORNA-VFS-024: released drafts are discarded; path-based truncates stay pending until fsync.
async fn cwd_mount_tracks_released_drafts_and_path_truncates() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
            ordinal: 0x93,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let start_mount = || {
        Command::new(env!("CARGO_BIN_EXE_orna"))
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
            .expect("start mount --cwd")
    };
    let data_path = mountpoint.join("Music/london/data.orna");
    let mut mounted = MountedProcess {
        child: start_mount(),
        mountpoint: mountpoint.clone(),
    };
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"unretained\", }\n").unwrap();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
    drop(mounted);

    let mut mounted = MountedProcess {
        child: start_mount(),
        mountpoint: mountpoint.clone(),
    };
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    let truncated = Command::new("python3")
        .args(["-c", "import os, sys; os.truncate(sys.argv[1], 0)"])
        .arg(&data_path)
        .output()
        .expect("run path-based truncate");
    assert!(
        truncated.status.success(),
        "path-based truncate failed: {}",
        String::from_utf8_lossy(&truncated.stderr)
    );
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("run pending path-truncate unmount");
    assert!(!refused.status.success(), "pending path truncate was discarded");
    assert!(mountinfo_contains(&mountpoint));

    let mut document = OpenOptions::new().write(true).open(&data_path).unwrap();
    document.write_all(b"{ title: \"accepted\", }\n").unwrap();
    document.sync_all().unwrap();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
}

/// A stat without a handle reports the size of the open writer's candidate (profiles/vfs-1 stat clause).
#[tokio::test]
async fn cwd_mount_stat_while_open_reports_writer_size() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
            ordinal: 0x94,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let mut mounted = spawn_cwd_mount(repository_directory.path(), &mountpoint);
    let data_path = mountpoint.join("Music/london/data.orna");
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    let candidate = "{ title: \"writer candidate is longer than the accepted row\", }\n";
    let mut document = OpenOptions::new().write(true).open(&data_path).unwrap();
    document.write_all(candidate.as_bytes()).unwrap();
    // Entry and attribute TTLs are zero, so this path stat reaches the mount without a handle.
    assert_eq!(
        std::fs::metadata(&data_path).unwrap().len(),
        candidate.len() as u64
    );
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
}

/// A path truncate after an unsynced draft was released retains the truncated latest candidate, not the accepted baseline.
#[tokio::test]
async fn cwd_mount_truncate_after_close_keeps_unsynced_writes() {
    truncate_after_close_keeps_unsynced_writes(false).await;
}

#[tokio::test]
async fn cwd_mount_truncate_after_release_keeps_unsynced_writes() {
    truncate_after_close_keeps_unsynced_writes(true).await;
}

async fn truncate_after_close_keeps_unsynced_writes(release_barrier: bool) {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
            ordinal: 0x95,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let mut mounted = spawn_cwd_mount(repository_directory.path(), &mountpoint);
    let data_path = mountpoint.join("Music/london/data.orna");
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    // Released without fsync: the candidate is retained as a draft and not accepted.
    let candidate = "{ title: \"unsynced writes kept\", }\n";
    let mut document = OpenOptions::new().write(true).open(&data_path).unwrap();
    document.write_all(candidate.as_bytes()).unwrap();
    drop(document);
    if release_barrier {
        // RELEASE is delivered asynchronously; let the closed writer's candidate be
        // retained before the path truncate, so the truncate meets the released draft.
        thread::sleep(Duration::from_millis(500));
    }
    let truncated = Command::new("python3")
        .args(["-c", "import os, sys; os.truncate(sys.argv[1], 13)"])
        .arg(&data_path)
        .output()
        .expect("run path-based truncate");
    assert!(
        truncated.status.success(),
        "path-based truncate failed: {}",
        String::from_utf8_lossy(&truncated.stderr)
    );
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("run pending path-truncate unmount");
    assert!(!refused.status.success(), "pending path truncate was discarded");
    assert!(mountinfo_contains(&mountpoint));
    // Records are written only when the mount process stops (ORNA-VFS-024), so stop it and inspect the store.
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
    let stop_stderr = take_child_stderr(&mut mounted.child);
    assert!(!status.success(), "mount stopped with unresolved drafts must exit non-zero");
    assert!(
        stop_stderr.contains("sys.vfs.draft_retained") && stop_stderr.contains("Music/london/data.orna"),
        "stop reports the retained draft: {stop_stderr}"
    );

    // The truncate applies to the unsynced candidate: its first 13 bytes are retained.
    let records_of = || -> Vec<String> {
        let directory = repository.runtime_paths().root().join("vfs-retained-drafts-v1");
        match std::fs::read_dir(&directory) {
            Ok(entries) => entries
                .map(|entry| entry.expect("list retained draft").path())
                .filter(|path| path.is_file())
                .map(|path| String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned())
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("list retained drafts: {error}"),
        }
    };
    let records = records_of();
    assert!(
        records
            .iter()
            .any(|record| record.contains("\nbytes 13\n") && record.ends_with("{ title: \"uns")),
        "truncated unsynced candidate was not retained: {records:?}"
    );
}

#[tokio::test]
/// ORNA-VFS-024: a rejected Document draft survives close and still blocks unmount until accepted.
async fn cwd_mount_refuses_unmount_after_rejected_document_draft_is_released() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
            ordinal: 0x94,
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let data_path = mountpoint.join("Music/london/data.orna");
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"rejected\",\n").unwrap();
    assert!(
        document.sync_all().is_err(),
        "malformed Document candidate must be rejected by fsync"
    );
    drop(document);

    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("run rejected-draft unmount");
    assert!(
        !refused.status.success(),
        "unmount discarded a rejected Document draft after its handle was released"
    );
    assert!(mountinfo_contains(&mountpoint));
    assert_eq!(repository.mount_status().unwrap().len(), 1);

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"accepted\", }\n").unwrap();
    document.sync_all().unwrap();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
    drop(mounted);
}

#[tokio::test]
/// ORNA-VFS-024: an accepted atomic replacement supersedes the rejected Document draft, so unmount succeeds.
async fn cwd_mount_atomic_replacement_clears_rejected_draft_for_unmount() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
            ordinal: 0x95,
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let data_path = mountpoint.join("Music/london/data.orna");
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"rejected\",\n").unwrap();
    assert!(
        document.sync_all().is_err(),
        "malformed Document candidate must be rejected by fsync"
    );
    drop(document);

    let scratch = mountpoint.join("Music/london/.data.orna.replacement-tmp");
    let mut replacement = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .expect("create replacement scratch beside data.orna");
    replacement.write_all(b"{ title: \"replaced\", }\n").unwrap();
    replacement.sync_all().expect("accept replacement scratch");
    drop(replacement);
    std::fs::rename(&scratch, &data_path).expect("atomically replace data.orna");
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("replaced")
    );

    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
    drop(mounted);
}

#[tokio::test]
/// ORNA-VFS-024: fsync on a Document handle that submitted no candidate does not block unmount.
async fn cwd_mount_unmodified_document_fsync_does_not_block_unmount() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
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
            source_row: initial,
            ordinal: 0x96,
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let data_path = mountpoint.join("Music/london/data.orna");
    if !wait_for_mount_paths(&mut mounted, &[data_path.as_path()]) {
        return;
    }

    let document = OpenOptions::new()
        .write(true)
        .open(&data_path)
        .unwrap();
    // The handle submitted no candidate, so the fsync result is not a rejection and is not asserted.
    let _ = document.sync_all();
    drop(document);

    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
    drop(mounted);
}

/// Deterministic bytes for Blob fixtures; distinct seeds make a wrong generation fail the comparison.
fn lcg_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut entropy = seed;
    (0..len)
        .map(|_| {
            entropy = entropy
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (entropy >> 32) as u8
        })
        .collect()
}

/// Canonical 44-byte RIFF/WAVE header over 16-bit mono PCM `samples` at `sample_rate`.
fn pcm_wav(sample_rate: u32, samples: &[u8]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len()).unwrap();
    let block_align: u16 = 2;
    let byte_rate = sample_rate * u32::from(block_align);
    let mut wav = Vec::with_capacity(44 + samples.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(samples);
    wav
}

/// A live `mount --cwd` over `Music/london`, whose `content` Blob was captured from the source bytes.
struct MountedBlobRow {
    mounted: MountedProcess,
    repository_directory: TempDir,
    _repository: orna_repository_v1::Repository,
    relation_id: [u8; 16],
    _media_root: TempDir,
    _mount_parent: TempDir,
}

fn spawn_cwd_mount(repository: &Path, mountpoint: &Path) -> MountedProcess {
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository.to_str().expect("repository path is UTF-8"),
            "mount",
        ])
        .arg(mountpoint)
        .arg("--cwd")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    MountedProcess {
        child,
        mountpoint: mountpoint.to_path_buf(),
    }
}

/// Deadline for a `mount --cwd` daemon to expose its mountpoint and the requested paths. Startup
/// under load can exceed ten seconds, so readiness is polled against the kernel mount table.
const MOUNT_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Waits until `mounted` is attached at its mountpoint (listed in /proc/self/mountinfo) and every
/// path in `paths` resolves through it. Returns `false` when FUSE setup is optionally unavailable
/// and the skip is allowed. Panics with the daemon's exit status and stderr when the daemon exits
/// before readiness, and with its stderr when it is still not ready after `MOUNT_READY_TIMEOUT`.
fn wait_for_mount_paths(mounted: &mut MountedProcess, paths: &[&Path]) -> bool {
    let deadline = Instant::now() + MOUNT_READY_TIMEOUT;
    while !(mountinfo_contains(&mounted.mountpoint) && paths.iter().all(|path| path.exists())) {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let stderr = take_child_stderr(&mut mounted.child);
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return false;
            }
            panic!("mount exited {status} before {paths:?} was exposed: {stderr}");
        }
        if Instant::now() >= deadline {
            let _ = mounted.child.kill();
            let _ = mounted.child.wait();
            let stderr = take_child_stderr(&mut mounted.child);
            panic!(
                "FUSE mount did not expose {paths:?} within {MOUNT_READY_TIMEOUT:?} (mountinfo has mountpoint: {}; root=[{}]); mount stderr: {stderr}",
                mountinfo_contains(&mounted.mountpoint),
                directory_entries(&mounted.mountpoint),
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Reads what the mount process wrote to stderr, once it has exited or been killed.
fn take_child_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    stderr
}

/// Unmounts through `orna unmount` and requires the mount process to exit cleanly.
fn unmount_and_reap(repository: &Path, mounted: &mut MountedProcess) {
    run_unmount(repository, &mounted.mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount exits cleanly after unmount; status={status:?}; stderr={stderr}"
    );
}

/// Captures `original` as `Music/london/content.mp3` (`audio/mpeg`) and mounts the cwd.
/// Returns `None` when FUSE is opted out or its prerequisites are optional and absent.
async fn mount_captured_blob(original: &[u8]) -> Option<MountedBlobRow> {
    mount_captured_media(original, "tone.mp3", "audio/mpeg", "content.mp3").await
}

/// Captures `original` from `file_name` as a `media_type` Blob in `Music/london` and mounts the cwd.
/// The exported filename is `mounted_name`.
/// Returns `None` when FUSE is opted out or its prerequisites are optional and absent.
async fn mount_captured_media(
    original: &[u8],
    file_name: &str,
    media_type: &str,
    mounted_name: &str,
) -> Option<MountedBlobRow> {
    if !fuse_test_available() {
        return None;
    }
    // The host cap bounds the source; it must admit the whole fixture, so it matches the provider limit.
    let capture_limit: usize = 4 << 20;
    let media_root = TempDir::new().unwrap();
    std::fs::write(media_root.path().join(file_name), original).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"{file_name}\", {capture_limit}), \"{media_type}\")"
    );
    let row_source = format!("{{\n    content: {capture},\n}}\n");
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(source, "Music");
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(capture_limit, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let blob_captures = [("content", capture.as_str())];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: &row_source,
        ordinal: 0x93,
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
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let mut mounted = spawn_cwd_mount(repository_directory.path(), &mountpoint);
    let captured_path = mountpoint.join("Music/london").join(mounted_name);
    if !wait_for_mount_paths(&mut mounted, &[captured_path.as_path()]) {
        return None;
    }
    Some(MountedBlobRow {
        mounted,
        repository_directory,
        _repository: repository,
        relation_id,
        _media_root: media_root,
        _mount_parent: mount_parent,
    })
}

#[tokio::test]
/// profiles/vfs-1.md Blob fields and ACCEPTANCE Gate E: a rename-over replacement gives new opens the new generation while a handle opened earlier keeps the captured bytes.
async fn cwd_mount_open_blob_handle_keeps_captured_generation_across_replacement() {
    let original = lcg_bytes(0x0123_4567_89ab_cdef, 1_048_576 + 12_345);
    let replacement = lcg_bytes(0xfedc_ba98_7654_3210, 2_097_152 + 777);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");
    let window = 64 * 1024;

    let mut old_handle = File::open(&blob_path).unwrap();
    let mut head = vec![0u8; window];
    old_handle.read_exact(&mut head).unwrap();
    assert_eq!(head.as_slice(), &original[..window]);

    let scratch_path = blob_path.with_file_name(".content.mp3.replace");
    std::fs::write(&scratch_path, &replacement).unwrap();
    std::fs::rename(&scratch_path, &blob_path).unwrap();
    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);

    assert_eq!(
        old_handle.metadata().unwrap().len(),
        original.len() as u64,
        "an open handle reports the size of the generation it captured"
    );
    let tail_start = original.len() - window;
    old_handle.seek(SeekFrom::Start(tail_start as u64)).unwrap();
    let mut tail = vec![0u8; window];
    old_handle.read_exact(&mut tail).unwrap();
    assert_eq!(tail.as_slice(), &original[tail_start..]);
    drop(old_handle);

    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// profiles/vfs-1.md Blob fields and ACCEPTANCE Gate E: the acknowledged replacement survives a restart of the mount process.
async fn cwd_mount_rename_replacement_survives_remount() {
    let original = lcg_bytes(0x0123_4567_89ab_cdef, 1_048_576 + 12_345);
    let replacement = lcg_bytes(0xfedc_ba98_7654_3210, 2_097_152 + 777);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    let scratch_path = blob_path.with_file_name(".content.mp3.replace");
    std::fs::write(&scratch_path, &replacement).unwrap();
    std::fs::rename(&scratch_path, &blob_path).unwrap();
    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);

    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);

    let remount_parent = TempDir::new().unwrap();
    let remount_point = remount_parent.path().join("library");
    std::fs::create_dir(&remount_point).unwrap();
    let mut remounted = spawn_cwd_mount(row.repository_directory.path(), &remount_point);
    let remounted_blob = remount_point.join("Music/london/content.mp3");
    assert!(
        wait_for_mount_paths(&mut remounted, &[remounted_blob.as_path()]),
        "FUSE remount must expose the committed Blob after a successful mount"
    );
    let restarted = std::fs::read(&remounted_blob).unwrap();
    unmount_and_reap(row.repository_directory.path(), &mut remounted);
    assert_eq!(
        restarted, replacement,
        "a restarted mount serves the acknowledged replacement"
    );
}

#[tokio::test]
/// profiles/vfs-1.md Blob fields and ACCEPTANCE Gate E: an optional field committed as null and later replaced with a Blob renders that Blob as a file, and the replacement survives a restart of the mount process.
async fn cwd_mount_optional_null_blob_replacement_survives_remount() {
    if !fuse_test_available() {
        return;
    }
    let media_root = TempDir::new().unwrap();
    let content = lcg_bytes(0x0123_4567_89ab_cdef, 1_048_576 + 12_345);
    let cover = lcg_bytes(0xfedc_ba98_7654_3210, 262_144 + 777);
    std::fs::write(media_root.path().join("tone.mp3"), &content).unwrap();
    std::fs::write(media_root.path().join("cover.png"), &cover).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, cover: Blob?, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let content_capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 4194304), \"audio/mpeg\")"
    );
    let cover_capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"cover.png\", 4194304), \"image/png\")"
    );
    let committed_row = format!("{{\n    content: {content_capture},\n    cover: null,\n}}\n");
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(source, "Music");
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(4 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    // The first commit publishes the committed row: `content` is a Blob and `cover` stays null.
    let content_fields = [("content", content_capture.as_str())];
    let content_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &content_fields,
        source_row: &committed_row,
        ordinal: 0x95,
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
        &content_rows,
    )
    .await;
    // The first publish advances the repository head, so the replacement is
    // admitted against a format context pinned to that new snapshot.
    let replacement_commit = repository.head().unwrap().unwrap();
    let replacement_format_context =
        format3::format3_context_for_snapshot(&repository, replacement_commit.as_str());
    // The mount's save captures a replacement Blob at the head it is admitted against, so the
    // replacement pins belong to that head's graph rather than the one the bindings opened before
    // the first publish.
    let replacement_capability = repository.capture_capability(relation_id).unwrap();
    let mut bindings = bindings.with_repository_capture_capability(replacement_capability);
    // The second commit replaces the row so `cover` changes from null to a Blob, but it stays
    // unpublished: the runtime holds `cover` as a Blob while the head still commits it as null.
    // As the mount's candidate() does, the row omits both Blob fields and binds them.
    let replacement_row = "{\n}\n";
    let replacement_fields = [
        ("content", content_capture.as_str()),
        ("cover", cover_capture.as_str()),
    ];
    let replacement_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &replacement_fields,
        source_row: replacement_row,
        ordinal: 0xB5,
        insert_only: false,
    }];
    media_commit::commit_source_replacement_through_save_admission(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &replacement_format_context,
        "Music",
        &replacement_rows[0],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let mut mounted = spawn_cwd_mount(repository_directory.path(), &mountpoint);
    let cover_path = mountpoint.join("Music/london/cover.png");
    if !wait_for_mount_paths(&mut mounted, &[cover_path.as_path()]) {
        return;
    }
    assert_eq!(std::fs::read(&cover_path).unwrap(), cover);
    assert_eq!(
        std::fs::read(mountpoint.join("Music/london/content.mp3")).unwrap(),
        content
    );
    unmount_and_reap(repository_directory.path(), &mut mounted);

    let remount_parent = TempDir::new().unwrap();
    let remount_point = remount_parent.path().join("library");
    std::fs::create_dir(&remount_point).unwrap();
    let mut remounted = spawn_cwd_mount(repository_directory.path(), &remount_point);
    let remounted_cover = remount_point.join("Music/london/cover.png");
    assert!(
        wait_for_mount_paths(&mut remounted, &[remounted_cover.as_path()]),
        "FUSE remount must expose the replacement Blob"
    );
    let restarted = std::fs::read(&remounted_cover).unwrap();
    unmount_and_reap(repository_directory.path(), &mut remounted);
    assert_eq!(
        restarted, cover,
        "a restarted mount serves the replacement Blob"
    );
}

#[tokio::test]
/// profiles/vfs-1.md Blob fields and ACCEPTANCE Gate E: a WAV Blob row exports as `content.wav`, serves the seek and range reads a media decoder issues, and exports byte-for-byte.
async fn cwd_mount_wav_blob_row_plays_through_seek_reads_and_exports_byte_for_byte() {
    let samples = lcg_bytes(0x0016_0000_5eed, 640_000);
    let original = pcm_wav(16_000, &samples);
    let Some(mut row) =
        mount_captured_media(&original, "tone.wav", "audio/wav", "content.wav").await
    else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.wav");
    let mut handle = File::open(&blob_path).unwrap();
    assert_eq!(handle.metadata().unwrap().len(), original.len() as u64);

    // Format probe: the canonical header is served before any sample is decoded.
    let mut header = [0u8; 44];
    handle.read_exact(&mut header).unwrap();
    assert_eq!(header.as_slice(), &original[..44]);
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(&header[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([header[20], header[21]]), 1, "PCM");
    assert_eq!(u16::from_le_bytes([header[22], header[23]]), 1, "mono");
    assert_eq!(
        u32::from_le_bytes(header[24..28].try_into().unwrap()),
        16_000
    );
    assert_eq!(
        u16::from_le_bytes([header[34], header[35]]),
        16,
        "16-bit samples"
    );
    assert_eq!(&header[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes(header[40..44].try_into().unwrap()),
        640_000
    );

    // Sequential 64 KiB reads cover the whole Blob, then report EOF.
    let window = 64 * 1024;
    handle.seek(SeekFrom::Start(0)).unwrap();
    let mut streamed = Vec::with_capacity(original.len());
    let mut chunk = vec![0u8; window];
    loop {
        let read = handle.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        streamed.extend_from_slice(&chunk[..read]);
        assert!(streamed.len() <= original.len(), "read past the Blob end");
    }
    assert_eq!(streamed, original);
    assert_eq!(
        handle.read(&mut chunk).unwrap(),
        0,
        "EOF stays at the Blob end"
    );

    // Container tail probe from the end of the file.
    let tail_len = 4096;
    handle.seek(SeekFrom::End(-(tail_len as i64))).unwrap();
    let mut tail = vec![0u8; tail_len];
    handle.read_exact(&mut tail).unwrap();
    assert_eq!(tail.as_slice(), &original[original.len() - tail_len..]);

    // Seeking decoder: deterministic random offsets, each range compared with the row bytes.
    let mut entropy = 0x5eed_5eed_5eed_5eed_u64;
    for _ in 0..64 {
        entropy = entropy
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let len = 1 + ((entropy >> 48) as usize % 8_192);
        let offset = (entropy as usize) % (original.len() - len);
        handle.seek(SeekFrom::Start(offset as u64)).unwrap();
        let mut range = vec![0u8; len];
        handle.read_exact(&mut range).unwrap();
        assert_eq!(
            range.as_slice(),
            &original[offset..offset + len],
            "range at {offset} length {len}"
        );
    }

    // Real media player: mpv opens and seeks the mounted WAV; skipped only when mpv is absent.
    let mpv_installed = Command::new("mpv")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if mpv_installed {
        let output = Command::new("mpv")
            .args([
                "--no-config",
                "--no-video",
                "--ao=null",
                "--no-terminal",
                "--msg-level=all=error",
                "--start=10",
                "--end=11",
            ])
            .arg(&blob_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "mpv plays the mounted WAV; status={:?}; stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        eprintln!("SKIP: mpv is not installed; the seek and range reads are the decoder leg");
    }

    // Binary export: a copy through the mount is byte-identical to the row.
    let export_dir = TempDir::new().unwrap();
    let export_path = export_dir.path().join("content.wav");
    std::fs::copy(&blob_path, &export_path).unwrap();
    assert_eq!(std::fs::read(&export_path).unwrap(), original);
    drop(handle);

    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-1 (41-vfs.md EDIT-1): a write-mode open of a projected Blob opens a private candidate;
/// write and fsync are accepted and the committed bytes read back.
async fn cwd_mount_blob_write_open_accepts_write_and_fsync() {
    let original = lcg_bytes(0x5a5a_1111_2222_3333, 96 * 1024);
    let replacement = lcg_bytes(0x9999_8888_7777_6666, 96 * 1024);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    let mut handle = OpenOptions::new()
        .write(true)
        .open(&blob_path)
        .expect("write-mode open of a projected Blob must succeed");
    handle
        .write_all(&replacement)
        .expect("write through the Blob candidate must succeed");
    handle
        .sync_all()
        .expect("fsync of the Blob candidate must succeed");
    drop(handle);

    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-1 (41-vfs.md EDIT-1): O_TRUNC on a projected Blob truncates the private candidate, so the
/// file reads as empty before any write and zero-length Blob content is legal.
async fn cwd_mount_blob_write_open_truncate_empties_candidate() {
    let original = lcg_bytes(0x0bad_f00d_dead_beef, 64 * 1024);
    let replacement = lcg_bytes(0x1357_9bdf_2468_ace0, 8 * 1024);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    let mut handle = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&blob_path)
        .expect("O_TRUNC write-mode open of a projected Blob must succeed");
    assert_eq!(
        handle.metadata().unwrap().len(),
        0,
        "O_TRUNC must truncate the Blob candidate before any write"
    );
    handle.write_all(&replacement).unwrap();
    handle.sync_all().unwrap();
    drop(handle);

    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-011 / EDIT-1 step 1: the expected baseline is captured when a write-open handle opens.
/// A sibling handle that accepts first changes the row, so the older handle's fsync must be refused
/// and the accepted bytes must stay on disk (no last-writer-wins overwrite).
async fn cwd_mount_blob_sibling_handle_with_stale_baseline_is_refused() {
    let original = lcg_bytes(0x7777_1111_2222_3333, 32 * 1024);
    let first = lcg_bytes(0x1111_0000_aaaa_bbbb, 32 * 1024);
    let second = lcg_bytes(0x2222_0000_cccc_dddd, 32 * 1024);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    let mut older = OpenOptions::new()
        .write(true)
        .open(&blob_path)
        .expect("first write-mode open must succeed");
    let mut newer = OpenOptions::new()
        .write(true)
        .open(&blob_path)
        .expect("second write-mode open must succeed");

    newer.write_all(&first).unwrap();
    newer.sync_all().expect("the first accept must succeed");
    drop(newer);

    older.write_all(&second).unwrap();
    assert!(
        older.sync_all().is_err(),
        "a stale-baseline sibling fsync must be refused, not overwrite the accepted row"
    );
    drop(older);

    assert_eq!(
        std::fs::read(&blob_path).unwrap(),
        first,
        "the accepted bytes must remain the row content after the refused sibling"
    );
    // The refused candidate stays a retained draft, so unmount names it and is refused.
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(row.repository_directory.path())
        .arg("unmount")
        .arg(&row.mounted.mountpoint)
        .output()
        .expect("run orna unmount");
    assert!(!refused.status.success(), "a retained stale blob draft must refuse unmount");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("sys.vfs.stale_edit"),
        "unmount must name the retained stale candidate: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    drop(row);
}

#[tokio::test]
/// ORNA-VFS-1 (41-vfs.md EDIT-1 step 3, profiles/vfs-1.md): after a sync is accepted on a handle, a
/// later write on the same handle begins a new candidate from the accepted bytes and its fsync is
/// accepted too.
async fn cwd_mount_blob_write_after_accept_on_same_handle_begins_new_candidate() {
    let original = lcg_bytes(0x3333_4444_5555_6666, 32 * 1024);
    let first = lcg_bytes(0x4444_5555_6666_7777, 32 * 1024);
    let second = lcg_bytes(0x5555_6666_7777_8888, 32 * 1024);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    let mut handle = OpenOptions::new()
        .write(true)
        .open(&blob_path)
        .expect("write-mode open of a projected Blob must succeed");
    handle.write_all(&first).unwrap();
    handle.sync_all().expect("the first accept must succeed");
    handle.seek(SeekFrom::Start(0)).unwrap();
    handle.write_all(&second).unwrap();
    handle
        .sync_all()
        .expect("a write after accept on the same handle must begin a new candidate");
    drop(handle);

    assert_eq!(std::fs::read(&blob_path).unwrap(), second);
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-1 (41-vfs.md EDIT-1): a read-write open of a projected Blob sees the captured bytes and
/// its own writes coherently through the same candidate.
async fn cwd_mount_blob_read_write_open_is_coherent() {
    let original = lcg_bytes(0x2468_ace0_1357_9bdf, 64 * 1024);
    let patch = lcg_bytes(0x1111_2222_3333_4444, 4 * 1024);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    let mut handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&blob_path)
        .expect("read-write open of a projected Blob must succeed");
    let mut head = vec![0u8; 4 * 1024];
    handle.read_exact(&mut head).unwrap();
    assert_eq!(head.as_slice(), &original[..4 * 1024]);

    handle.seek(SeekFrom::Start(0)).unwrap();
    handle.write_all(&patch).unwrap();
    handle.seek(SeekFrom::Start(0)).unwrap();
    let mut readback = vec![0u8; 4 * 1024];
    handle.read_exact(&mut readback).unwrap();
    assert_eq!(readback, patch, "a read after a write on one handle must see the write");
    handle.sync_all().unwrap();
    drop(handle);

    let committed = std::fs::read(&blob_path).unwrap();
    assert_eq!(&committed[..4 * 1024], patch.as_slice());
    assert_eq!(&committed[4 * 1024..], &original[4 * 1024..]);
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

/// POSIX `EPERM` (Linux errno 1): the documented refusal for unsupported VFS mutations (41-vfs.md:103-105).
const EPERM: i32 = 1;
/// POSIX `EAGAIN` (Linux errno 11): a stale-baseline CAS refusal for a Blob replacement (ORNA-VFS-009).
const EAGAIN: i32 = 11;
/// POSIX `EDQUOT` (Linux errno 122): the per-mount unbound scratch cap (41-vfs.md:67).
const EDQUOT: i32 = 122;

#[tokio::test]
/// ORNA-VFS-014: symlinks and hard links are unsupported in VFS-1 and fail explicitly with EPERM; the row directory gains no entry and the stored row is unchanged.
async fn cwd_mount_symlink_and_hard_link_fail_eperm_without_row_change() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let row_path = row.row_path.clone();
    let symlink_error = std::os::unix::fs::symlink("data.orna", row_path.join("alias"))
        .expect_err("a symlink in a row directory is refused");
    assert_eq!(symlink_error.raw_os_error(), Some(EPERM));
    let link_error = std::fs::hard_link(row_path.join("data.orna"), row_path.join("twin"))
        .expect_err("a hard link to data.orna is refused");
    assert_eq!(link_error.raw_os_error(), Some(EPERM));
    let mut names: Vec<String> = std::fs::read_dir(&row_path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["data.orna"], "refused links leave no directory entry");
    row.unmount();
    assert_title(&row.stored_row().await, "before");
}

#[tokio::test]
/// ORNA-VFS-013 / profiles/vfs-1.md "Editable candidate and syscall rules": unlinking data.orna, creating a directory in a row, and renaming data.orna fail with EPERM and keep the row.
async fn cwd_mount_unlink_mkdir_and_document_rename_fail_eperm_without_row_change() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let row_path = row.row_path.clone();
    let unlink_error = std::fs::remove_file(row_path.join("data.orna"))
        .expect_err("data.orna cannot be unlinked");
    assert_eq!(unlink_error.raw_os_error(), Some(EPERM));
    let mkdir_error = std::fs::create_dir(row_path.join("nested"))
        .expect_err("plain mkdir does not invent schema or key semantics");
    assert_eq!(mkdir_error.raw_os_error(), Some(EPERM));
    let rename_error = std::fs::rename(row_path.join("data.orna"), row_path.join("renamed.orna"))
        .expect_err("data.orna cannot be renamed to another name");
    assert_eq!(rename_error.raw_os_error(), Some(EPERM));
    let document = std::fs::read_to_string(row_path.join("data.orna")).unwrap();
    assert!(
        document.contains("title: \"before\""),
        "the mounted document still shows the accepted row: {document}"
    );
    row.unmount();
    assert_title(&row.stored_row().await, "before");
}

#[tokio::test]
/// profiles/vfs-1.md "Editable candidate and syscall rules": chmod cannot change database authority. This mount refuses it with EPERM; the spec does not name chmod, so EPERM is a conservative choice. The row keeps its content and permissions.
async fn cwd_mount_chmod_of_document_fails_eperm_without_row_change() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let document = row.row_path.join("data.orna");
    let mode_before = std::fs::metadata(&document).unwrap().permissions().mode() & 0o777;
    let chmod_error = std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o644))
        .expect_err("chmod of a database-backed document is refused");
    assert_eq!(chmod_error.raw_os_error(), Some(EPERM));
    assert_eq!(
        std::fs::metadata(&document).unwrap().permissions().mode() & 0o777,
        mode_before,
        "a refused chmod leaves the mode unchanged"
    );
    row.unmount();
    assert_title(&row.stored_row().await, "before");
}

#[tokio::test]
/// ORNA-VFS-012: renaming a Blob's suffix (content.mp3 -> content.wav) does not transcode bytes or rename the field. The mount refuses the suffix change with EPERM; the spec does not name a suffix rename, so this refusal is a conservative choice. The Blob keeps its bytes.
async fn cwd_mount_blob_suffix_rename_is_refused_without_transcoding() {
    let original = lcg_bytes(0x5151_0000_0000_0001, 64 * 1024 + 3);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let row_path = row.mounted.mountpoint.join("Music/london");
    let blob_path = row_path.join("content.mp3");

    let renamed = std::fs::rename(&blob_path, row_path.join("content.wav"))
        .expect_err("renaming a Blob to another suffix is refused");
    assert_eq!(renamed.raw_os_error(), Some(EPERM));
    assert_eq!(std::fs::read(&blob_path).unwrap(), original, "the Blob keeps its bytes");
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-011/012 and 41-vfs.md "Temporary-save replacement" (lines 64-72), profiles/vfs-1.md:98: an unknown file inside a Blob-field row directory is private editor scratch. Its creation captures the row baseline and its fsync persists scratch only. Renaming scratch onto the Blob admits one replacement through that baseline; scratch whose baseline went stale is refused with EAGAIN and keeps its bytes (ORNA-VFS-009).
async fn cwd_mount_blob_scratch_create_in_row_directory_is_supported() {
    let original = lcg_bytes(0x5151_0000_0000_0002, 64 * 1024 + 3);
    let second = lcg_bytes(0x5151_0000_0000_0003, 16 * 1024 + 7);
    let replacement = lcg_bytes(0x5151_0000_0000_0004, 8 * 1024 + 1);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let row_path = row.mounted.mountpoint.join("Music/london");
    let blob_path = row_path.join("content.mp3");

    // Captures the original row baseline; it becomes stale once the Blob is replaced below.
    let stale = row_path.join(".content.wav.replace");
    create_scratch(&stale, b"stale candidate");
    assert_eq!(std::fs::read(&blob_path).unwrap(), original, "scratch does not change the Blob");

    // A Blob-bound scratch accepts its own rename and replaces the Blob.
    let bound = row_path.join(".content.mp3.replace");
    create_scratch(&bound, &second);
    std::fs::rename(&bound, &blob_path).expect("renaming bound scratch over the Blob is accepted");
    assert_eq!(std::fs::read(&blob_path).unwrap(), second);

    // The earlier scratch kept the original baseline, so its rename is a stale CAS refusal.
    let refused = std::fs::rename(&stale, &blob_path)
        .expect_err("a scratch with a stale baseline must not replace the Blob");
    assert_eq!(refused.raw_os_error(), Some(EAGAIN));
    assert!(stale.exists(), "the refused scratch remains recoverable");
    assert_eq!(std::fs::read(&blob_path).unwrap(), second, "the accepted Blob is unchanged");

    // An unbound scratch, created after the accepted replacement, commits to the Blob its rename names.
    let unbound = row_path.join(".clip.wav.replace");
    create_scratch(&unbound, &replacement);
    let document_before = std::fs::read(row_path.join("data.orna")).unwrap();
    std::fs::rename(&unbound, &blob_path).expect("renaming unbound scratch over the Blob is accepted");
    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);
    // The row document carries the Blob's path and media type, not its bytes, so an accepted byte replacement leaves those names in place; the remount check below proves the document against the store.
    let document_after = std::fs::read(row_path.join("data.orna")).unwrap();
    let document_after_text = String::from_utf8(document_after.clone()).expect("data.orna is UTF-8");
    assert!(document_after_text.contains("./content.mp3"), "data.orna still names the Blob");
    assert!(!unbound.exists(), "the accepted scratch is consumed by its rename");

    // A retained stale candidate blocks unmount until removed (sys.vfs.stale_edit); remove it so the mount drains.
    std::fs::remove_file(&stale).expect("removing the refused stale scratch");
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);

    // Remount: data.orna and the Blob bytes must be rebuilt from the committed store row that holds the accepted replacement.
    let committed = format3::committed_store_rows(&row._repository, row.relation_id);
    assert_eq!(committed.len(), 1, "the relation keeps one committed row");
    assert!(committed[0].value().encoded_fields().is_some(), "the accepted row is inline");
    let mountpoint = row.mounted.mountpoint.clone();
    let mut remounted = spawn_cwd_mount(row.repository_directory.path(), &mountpoint);
    assert!(wait_for_mount_paths(&mut remounted, &[blob_path.as_path()]), "remount exposes the Blob");
    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement, "remount serves the accepted Blob bytes");
    assert_eq!(std::fs::read(row_path.join("data.orna")).unwrap(), document_after, "remounted data.orna matches the document served before unmount");
    unmount_and_reap(row.repository_directory.path(), &mut remounted);
}

#[tokio::test]
/// 41-vfs.md:67 and CHANGELOG: each mount admits at most 16 unbound scratch records; the 17th create is refused with EDQUOT and removing one record frees its slot.
async fn cwd_mount_unbound_scratch_cap_is_per_mount() {
    let original = lcg_bytes(0x5151_0000_0000_0005, 4 * 1024 + 9);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let row_path = row.mounted.mountpoint.join("Music/london");
    let names: Vec<_> = (0..16).map(|i| row_path.join(format!(".clip{i}.wav.replace"))).collect();
    for name in &names {
        create_scratch(name, b"draft");
    }
    let overflow = row_path.join(".clip16.wav.replace");
    let refused = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&overflow)
        .expect_err("the 17th unbound scratch exceeds the per-mount cap");
    assert_eq!(refused.raw_os_error(), Some(EDQUOT));

    std::fs::remove_file(&names[0]).expect("removing one unbound scratch");
    create_scratch(&overflow, b"draft");
    for name in names.iter().skip(1).chain(std::iter::once(&overflow)) {
        std::fs::remove_file(name).unwrap();
    }
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

/// Creates an editor scratch file with `bytes` and syncs it; the create itself must be supported.
fn create_scratch(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("creating scratch inside a Blob-field row directory is supported");
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

/// Mounted table directory for a commit with several rows, one commit per row.
struct MountedTableRows {
    mounted: MountedProcess,
    repository_directory: TempDir,
    _mount_parent: TempDir,
}

/// Commits each `(key, source)` row in its own commit against the same table, then mounts the
/// repository with `--cwd`. Returns `None` when FUSE is opted out.
async fn mount_table_rows(
    source: &str,
    table: &str,
    rows: &[(&str, &str)],
) -> Option<MountedTableRows> {
    if !fuse_test_available() {
        return None;
    }
    let (repository_directory, repository, relation_id, _format_context) =
        format3::format3_repository_with_context(source, table);
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    for (index, (key, source_row)) in rows.iter().enumerate() {
        let state = RuntimeState::open(&repository, runtime_identity, digest)
            .await
            .unwrap();
        let writer = state
            .acquire_lease(runtime_identity.repository_id)
            .await
            .unwrap();
        let context = format3::format3_context_for_snapshot(
            &repository,
            repository.head().unwrap().unwrap().as_str(),
        );
        media_commit::commit_source_captures_for_table(
            &repository,
            &state,
            writer,
            &mut bindings,
            relation_id,
            &context,
            table,
            &[media_commit::SourceCaptureRow {
                key: *key,
                blob_fields: &[],
                source_row: *source_row,
                ordinal: 0x92 + index as u16,
                insert_only: true,
            }],
        )
        .await;
        drop(state);
    }

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let mut mounted = spawn_cwd_mount(repository_directory.path(), &mountpoint);
    let table_path = mountpoint.join(table);
    if !wait_for_mount_paths(&mut mounted, &[table_path.as_path()]) {
        return None;
    }
    Some(MountedTableRows {
        mounted,
        repository_directory,
        _mount_parent: mount_parent,
    })
}

#[tokio::test]
/// ORNA-VFS-016 (case-folding half only) / profiles/vfs-1.md "Row/key directory mapping": Str case is preserved without folding, so `abc` and `ABC` are distinct row directories, each resolving to its own document with no alias entry. The reversible-escaping half of ORNA-VFS-016 is not exercised here.
async fn mount_case_distinct_keys_project_to_distinct_rows() {
    let rows: Vec<(&str, &str)> = vec![
        ("abc", "{ title: \"lower\", }\n"),
        ("ABC", "{ title: \"upper\", }\n"),
    ];
    let Some(mut mounted) =
        mount_table_rows("pub table Music(id: Str) { title: Str, }\n", "Music", &rows).await
    else {
        return;
    };
    let table_path = mounted.mounted.mountpoint.join("Music");
    let mut names: Vec<String> = std::fs::read_dir(&table_path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["ABC", "abc"], "case-distinct keys keep their own directories");
    for (name, title) in [("abc", "lower"), ("ABC", "upper")] {
        let document = std::fs::read_to_string(table_path.join(name).join("data.orna"))
            .unwrap_or_else(|error| panic!("read the row at {name}: {error}"));
        assert!(
            document.contains(&format!("title: \"{title}\"")),
            "{name} resolves to its own row: {document}"
        );
    }
    unmount_and_reap(mounted.repository_directory.path(), &mut mounted.mounted);
}

#[tokio::test]
/// A Str far above the 64 KiB inline bound saves through the document; the stored row holds the full text and the document read contains it. This does not exercise ORNA-VFS-010 (metadata-only saves) or ORNA-VFS-018 (listing/stat sniffing).
async fn cwd_mount_large_str_round_trips_through_document() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let large: String = (0..300_000u32)
        .map(|index| char::from(b'a' + (index % 26) as u8))
        .collect();
    assert_saved(
        &row.save("large", &format!("{{ title: \"{large}\", }}")),
        "large Str save",
    );
    let document = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(
        document.contains(&large),
        "the document read returns the full large Str"
    );
    row.unmount();
    assert_title(&row.stored_row().await, &large);
}
