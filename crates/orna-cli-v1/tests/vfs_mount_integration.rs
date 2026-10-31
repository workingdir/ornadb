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

/// A writable handle on a mounted document, held by a helper process rather than
/// the test process. A fork from the test process while a handle is open copies
/// its descriptor, and the copy's close during exec issues a FUSE FLUSH that
/// EDIT-1 step 5 may accept. Keeping the handle out of the test process means
/// other tests' forks cannot trigger that flush.
struct DirtyDocument {
    child: Child,
    replies: BufReader<ChildStdout>,
}

const DIRTY_DOCUMENT_HELPER: &str = r#"import os, sys
body = sys.argv[2:3]
fd = os.open(sys.argv[1], os.O_WRONLY | (os.O_TRUNC if body else 0))
try:
    for text in body:
        data = text.encode()
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
        Self::spawn(path, Some(body))
    }

    /// Opens `path` for writing without truncation and writes nothing, so the
    /// handle is clean: its fsync has no candidate to accept.
    fn open_unwritten(path: &Path) -> Self {
        Self::spawn(path, None)
    }

    fn spawn(path: &Path, body: Option<&str>) -> Self {
        let mut command = Command::new("python3");
        command.arg("-c").arg(DIRTY_DOCUMENT_HELPER).arg(path);
        if let Some(body) = body {
            command.arg(body);
        }
        let mut child = command
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

    /// `fsync`s the handle through the helper, as `File::sync_all` would, and
    /// returns the helper's reply: `ok`, or `err<errno>` when the fsync fails.
    fn sync_reply(&mut self) -> String {
        let stdin = self.child.stdin.as_mut().expect("helper stdin");
        writeln!(stdin, "fsync").expect("send fsync to dirty document helper");
        let mut reply = String::new();
        self.replies
            .read_line(&mut reply)
            .expect("dirty document fsync reply");
        reply.trim_end().to_owned()
    }

    /// `fsync`s the handle and requires it to succeed.
    fn sync_all(&mut self) {
        assert_eq!(self.sync_reply(), "ok", "dirty document fsync failed");
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
    // A comment-only head advance leaves the Music declaration, and so its committed schema identity, unchanged.
    let advanced = format!("{MUSIC_SOURCE}// head advances without changing the Music declaration\n");
    let Some(run) = edit_row_after_head_advances(&advanced).await else {
        return;
    };
    assert!(
        run.output.status.success(),
        "edit --row failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr),
    );
    assert_eq!(
        orna_value_v1::Value::decode(&run.before)
            .expect("decode the original admitted row")
            .raw(),
        &orna_value_v1::Raw::Array(vec![orna_value_v1::Raw::Text("before".to_owned())]),
    );
    assert_eq!(
        orna_value_v1::Value::decode(&run.after)
            .expect("decode the edited admitted row")
            .raw(),
        &orna_value_v1::Raw::Array(vec![orna_value_v1::Raw::Text("after".to_owned())]),
    );
    assert!(run.head_unchanged_by_edit, "edit --row leaves the advanced HEAD in place");
}

#[tokio::test]
/// ORNA-VFS-019: a head advance that changes the Music declaration is a schema change. The pinned
/// mount can no longer serve the row under the changed declaration, so `edit --row` is refused before
/// any write: the refusal is definite, not "outcome is unknown", and the admitted row is unchanged.
/// The refusal is EIO (profiles/vfs-1.md:134 and :140, backing unavailability), not EAGAIN: the
/// edit cannot read the row, so no expected version or draft exists to be stale.
async fn cwd_mount_edit_row_refused_after_head_changes_music_schema() {
    let advanced = "pub table Music(id: Str) { title: Str, subtitle: Str, }\n";
    let Some(run) = edit_row_after_head_advances(advanced).await else {
        return;
    };
    let stderr = String::from_utf8_lossy(&run.output.stderr).into_owned();
    assert!(
        !run.output.status.success(),
        "a changed Music schema refuses the pinned edit; stderr={stderr}"
    );
    assert!(
        !stderr.contains("outcome is unknown"),
        "a definite refusal is not reported as unknown; stderr={stderr}"
    );
    assert!(
        stderr.contains("mounted entry could not be read")
            && stderr.contains("Input/output error"),
        "the mount refuses to serve the row under the changed schema with EIO; stderr={stderr}"
    );
    assert_eq!(
        run.after, run.before,
        "the refused edit leaves the admitted row unchanged"
    );
    assert!(run.head_unchanged_by_edit, "the refused edit leaves the advanced HEAD in place");
}

const MUSIC_SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";

/// What one pinned `edit --row` run after a HEAD advance produced.
struct PinnedRowEdit {
    output: std::process::Output,
    before: Vec<u8>,
    after: Vec<u8>,
    head_unchanged_by_edit: bool,
}

/// Attaches `mount --cwd` to the committed `Music/london` row, advances HEAD by committing
/// `advanced_main` as `main.orna`, then runs `orna edit --row` on the mounted row. Returns
/// `None` when FUSE is unavailable or the mount does not come up.
async fn edit_row_after_head_advances(advanced_main: &str) -> Option<PinnedRowEdit> {
    if !fuse_test_available() {
        return None;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(MUSIC_SOURCE, "Music");
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
        return None;
    }
    std::fs::write(repository_directory.path().join("main.orna"), advanced_main).unwrap();
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
            "--debug",
            "--db",
            repository_directory.path().to_str().unwrap(),
            "edit",
            "--row",
        ])
        .arg(&row_path)
        .env("EDITOR", &editor)
        .output()
        .expect("run edit --row against mounted absolute row directory");
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
    Some(PinnedRowEdit {
        output,
        before,
        after,
        head_unchanged_by_edit: repository.head().unwrap().unwrap() == head_after_move,
    })
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
    mount_music_row_with_source("pub table Music(id: Str) { title: Str, }\n", initial).await
}

/// [`mount_music_row`] over the table `source` declares, seeded with `initial`.
async fn mount_music_row_with_source(source: &str, initial: &str) -> Option<MountedMusicRow> {
    mount_music_rows_with_source(source, &[("london", initial)]).await
}

/// [`mount_music_row_with_source`] seeding every `(key, source row)` in `rows` under `Music`. The
/// returned handle's `row_path` is the first row; the other rows sit beside it.
async fn mount_music_rows_with_source(source: &str, rows: &[(&str, &str)]) -> Option<MountedMusicRow> {
    if !fuse_test_available() {
        return None;
    }
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(source, "Music");
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    let captures = rows
        .iter()
        .enumerate()
        .map(|(index, &(key, source_row))| media_commit::SourceCaptureRow {
            key,
            blob_fields: &[],
            source_row,
            ordinal: 0x92 + index as u16,
            insert_only: true,
        })
        .collect::<Vec<_>>();
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &captures,
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
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let row_path = mountpoint.join("Music/london");
    let document_paths = rows
        .iter()
        .map(|(key, _)| mountpoint.join("Music").join(key).join("data.orna"))
        .collect::<Vec<_>>();
    let document_path_refs = document_paths.iter().map(std::path::PathBuf::as_path).collect::<Vec<_>>();
    if !wait_for_mount_paths(&mut mounted, &document_path_refs) {
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

/// The draft files recovery storage keeps for `row` (ORNA-VFS-009); empty when none was retained.
fn retained_drafts(row: &MountedMusicRow) -> Vec<std::path::PathBuf> {
    let directory = row.repository.runtime_paths().root().join("vfs-retained-drafts-v1");
    match std::fs::read_dir(&directory) {
        Ok(entries) => entries
            .map(|entry| entry.expect("list retained draft").path())
            .filter(|path| path.is_file())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("list {}: {error}", directory.display()),
    }
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
/// ORNA-VFS-011: a pinned writer compares against the row it captured at open. A grouped
/// `edit --row` the mount accepted while that writer is open advances the shared save context,
/// not the writer's captured baseline, so the writer's close flush is refused as stale and
/// retained, and the owner's grouped row is kept.
async fn cwd_mount_close_flush_after_grouped_edit_is_stale() {
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
    assert_saved(
        &row.save("grouped", "{ title: \"grouped\", }"),
        "grouped save while the document is open",
    );
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
        "unmount accepted the stale candidate over the grouped row: {diagnostic}"
    );
    assert!(
        diagnostic.contains("unresolved workspace drafts remain")
            && diagnostic.contains("Music/london/data.orna")
            && diagnostic.contains("sys.vfs.stale_edit"),
        "unmount names the retained stale draft and its cause: {diagnostic}"
    );
    let shown = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(
        shown.contains("grouped") && !shown.contains("stale close"),
        "the mount shows the owner's grouped row, not the stale candidate: {shown}"
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
/// ORNA-VFS-011: each open writer compares against the row it captured at open. The second
/// writer's close is accepted and advances the row; the first writer's close then compares
/// against the row the second writer committed, so it is refused as stale and retained, and
/// the second writer's row is kept (41-vfs.md:80).
async fn cwd_mount_first_writer_close_after_second_writer_commit_is_stale() {
    if !dirty_document_helper_available() {
        return;
    }
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let first = DirtyDocument::open_and_write(
        &row.row_path.join("data.orna"),
        "{ title: \"first writer\", }\n",
    );
    let second = DirtyDocument::open_and_write(
        &row.row_path.join("data.orna"),
        "{ title: \"second writer\", }\n",
    );
    drop(second);
    drop(first);
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
        "unmount accepted the first writer's stale candidate over the second writer's row: {diagnostic}"
    );
    assert!(
        diagnostic.contains("unresolved workspace drafts remain")
            && diagnostic.contains("Music/london/data.orna")
            && diagnostic.contains("sys.vfs.stale_edit"),
        "unmount names the retained stale draft and its cause: {diagnostic}"
    );
    let shown = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(
        shown.contains("second writer") && !shown.contains("first writer"),
        "the mount shows the second writer's row, not the stale candidate: {shown}"
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
/// ORNA-VFS-011, 41-vfs.md:38-40: a grouped `edit --row` that supersedes the document a writer
/// holds retires that generation. When a later grouped edit returns the row to the bytes the
/// writer captured, the writer's compare would pass, but the superseded generation still refuses
/// its fsync and close flush as stale and retained, so the owner's row is kept.
async fn cwd_mount_close_flush_on_superseded_generation_is_stale_when_row_returns_to_capture() {
    if !dirty_document_helper_available() {
        return;
    }
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let mut document = DirtyDocument::open_and_write(
        &row.row_path.join("data.orna"),
        "{ title: \"stale close\", }\n",
    );
    assert_saved(
        &row.save("grouped", "{ title: \"grouped\", }"),
        "grouped save while the document is open",
    );
    assert_saved(
        &row.save("restore", "{ title: \"before\", }"),
        "grouped save returning the row to the captured bytes",
    );
    assert_eq!(
        document.sync_reply(),
        "err11",
        "the dirty superseded generation's fsync is refused with EAGAIN",
    );
    assert!(
        !retained_drafts(&row).is_empty(),
        "the refused candidate is retained in recovery storage (ORNA-VFS-009)",
    );
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
        "unmount accepted the superseded generation's stale candidate: {diagnostic}"
    );
    assert!(
        diagnostic.contains("unresolved workspace drafts remain")
            && diagnostic.contains("Music/london/data.orna")
            && diagnostic.contains("sys.vfs.stale_edit"),
        "unmount names the retained stale draft and its cause: {diagnostic}"
    );
    let shown = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(
        shown.contains("before") && !shown.contains("stale close"),
        "the mount shows the owner's grouped row, not the stale candidate: {shown}"
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
/// ORNA-VFS-011 and EDIT-1 step 2 (41-vfs.md:38-40): a clean handle whose generation a grouped
/// edit superseded submitted no bytes, so it has no candidate to refuse. Its fsync is
/// acknowledged, no draft is retained, and unmount succeeds with the grouped row stored.
async fn cwd_mount_clean_fsync_on_superseded_generation_is_acknowledged() {
    if !dirty_document_helper_available() {
        return;
    }
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let mut clean = DirtyDocument::open_unwritten(&row.row_path.join("data.orna"));
    assert_saved(
        &row.save("grouped", "{ title: \"grouped\", }"),
        "grouped save while the clean document is open",
    );
    clean.sync_all();
    drop(clean);
    assert!(
        retained_drafts(&row).is_empty(),
        "a clean superseded handle retains no draft: {:?}",
        retained_drafts(&row),
    );
    row.unmount();
    assert_title(&row.stored_row().await, "grouped");
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
    truncate_after_close_keeps_unsynced_writes().await;
}

async fn truncate_after_close_keeps_unsynced_writes() {
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

    // The truncate applies to the unsynced candidate: its first 13 bytes are retained.
    let records = records_of();
    assert!(
        records
            .iter()
            .any(|record| record.contains("\nbytes 13\n") && record.ends_with("{ title: \"uns")),
        "truncated unsynced candidate was not retained: {records:?}"
    );
}

/// A `mount --cwd` of a one-row Music repository whose `london` row holds `initial`.
struct CwdMusicMount {
    repository_directory: TempDir,
    repository: orna_repository_v1::Repository,
    _mount_parent: TempDir,
    mountpoint: std::path::PathBuf,
    data_path: std::path::PathBuf,
    mounted: MountedProcess,
}

async fn start_cwd_music_mount(initial: &str) -> Option<CwdMusicMount> {
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
        return None;
    }
    Some(CwdMusicMount {
        repository_directory,
        repository,
        _mount_parent: mount_parent,
        mountpoint,
        data_path,
        mounted,
    })
}

/// Sets the permission bits of `path`; a read-only directory is the fault retention hits.
fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Truncates `path` by name, as `truncate(2)` does: the mount sees a setattr with no open handle.
fn path_truncate(path: &Path, size: u64) {
    let output = Command::new("python3")
        .args(["-c", "import os, sys; os.truncate(sys.argv[1], int(sys.argv[2]))"])
        .arg(path)
        .arg(size.to_string())
        .output()
        .expect("run path-based truncate");
    assert!(
        output.status.success(),
        "path-based truncate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Truncates `path` by name like `path_truncate`, retrying while the mount refuses with EBUSY. A stop
/// retry holds the mount quiescing only while it probes retention and resumes it right after, so a
/// refused cut succeeds once that probe has finished.
fn path_truncate_retrying_busy(path: &Path, size: u64) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let output = Command::new("python3")
            .args(["-c", "import os, sys; os.truncate(sys.argv[1], int(sys.argv[2]))"])
            .arg(path)
            .arg(size.to_string())
            .output()
            .expect("run path-based truncate");
        if output.status.success() {
            return;
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            stderr.contains("Device or resource busy") && Instant::now() < deadline,
            "path-based truncate failed: {stderr}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Sends SIGTERM to the mount process without waiting for it to exit.
fn send_sigterm(mounted: &MountedProcess) {
    let status = Command::new("kill")
        .args(["-TERM", &mounted.child.id().to_string()])
        .status()
        .expect("send SIGTERM to mount child");
    assert!(status.success(), "kill -TERM failed");
}

/// Waits for the mount process to exit after a stop, failing after thirty seconds.
fn wait_for_mount_exit(mounted: &mut MountedProcess) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while mounted.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "mount did not stop after SIGTERM");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the mount process writes a stderr line containing `needle`, failing after thirty
/// seconds. A thread takes over stderr and reads it to its end, so the mount never blocks on a
/// full pipe; lines written after the wait returns are read and discarded.
fn wait_for_stderr_line(mounted: &mut MountedProcess, needle: &str) {
    let stderr = mounted.child.stderr.take().expect("mount stderr is piped");
    let (sender, receiver) = std::sync::mpsc::channel::<String>();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) if line.contains(needle) => return,
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("mount did not report {needle:?} within thirty seconds")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("mount stopped before reporting {needle:?}")
            }
        }
    }
}

/// The contents of every draft record recovery storage holds (ORNA-VFS-009); empty when none.
fn retained_record_texts(repository: &orna_repository_v1::Repository) -> Vec<String> {
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
}

/// The body bytes of every draft record recovery storage holds, sorted, without the record header.
fn retained_record_bodies(repository: &orna_repository_v1::Repository) -> Vec<String> {
    let mut bodies: Vec<String> = retained_record_texts(repository)
        .into_iter()
        .map(|record| {
            // The header ends with an empty line; the candidate bytes follow it.
            let body_start = record.find("\n\n").expect("draft record has a header") + 2;
            record[body_start..].to_owned()
        })
        .collect();
    bodies.sort();
    bodies
}

/// The recovery directory of `repository`, where retained draft records are written.
fn record_directory(repository: &orna_repository_v1::Repository) -> std::path::PathBuf {
    let directory = repository.runtime_paths().root().join("vfs-retained-drafts-v1");
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

#[tokio::test]
/// ORNA-VFS-009 and EDIT-1 step 4: a path truncate of a candidate whose retention failed keeps the
/// truncated prefix once retention recovers, and the untruncated copy is not retained as well.
async fn cwd_mount_truncate_after_failed_retention_keeps_one_truncated_record() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let record_dir = record_directory(&mount.repository);
    set_mode(&record_dir, 0o500);
    let mut document = OpenOptions::new().write(true).open(&mount.data_path).unwrap();
    document.write_all(b"{ title: \"unsynced writes kept\", }\n").unwrap();
    drop(document);
    // FUSE release is asynchronous to close(2); let the release refuse retention first.
    thread::sleep(Duration::from_millis(300));
    path_truncate(&mount.data_path, 13);
    send_sigterm(&mount.mounted);
    // Stop keeps the mount up while its drafts cannot be kept (ORNA-VFS-024). The mount reports
    // each failed retention before it waits to retry, so the check runs once that report arrives.
    wait_for_stderr_line(&mut mount.mounted, "retrying draft retention before unmount");
    assert!(
        mount.mounted.child.try_wait().unwrap().is_none(),
        "mount stopped while its drafts could not be kept"
    );
    set_mode(&record_dir, 0o700);
    wait_for_mount_exit(&mut mount.mounted);
    let records = retained_record_texts(&mount.repository);
    assert_eq!(
        records.len(),
        1,
        "the truncated candidate is the only retained record: {records:?}"
    );
    assert!(
        records[0].contains("\nbytes 13\n") && records[0].ends_with("{ title: \"uns"),
        "retained record is not the 13-byte truncated prefix: {records:?}"
    );
}

#[tokio::test]
/// ORNA-VFS-009 and EDIT-1 step 4 (ornadb-s5gh): a second path truncate that lands while a stop-retry
/// is pending cuts the same candidate again. Exactly one record must be retained, holding the newest cut.
async fn cwd_mount_truncate_during_pending_stop_retry_keeps_one_record() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let record_dir = record_directory(&mount.repository);
    set_mode(&record_dir, 0o500);
    let baseline = std::fs::read(&mount.data_path).unwrap();
    path_truncate(&mount.data_path, 13);
    send_sigterm(&mount.mounted);
    // Stop keeps the mount up while the cut cannot be kept, then retries retention.
    wait_for_stderr_line(&mut mount.mounted, "retrying draft retention before unmount");
    assert!(
        mount.mounted.child.try_wait().unwrap().is_none(),
        "mount stopped while its truncate could not be kept"
    );
    path_truncate_retrying_busy(&mount.data_path, 9);
    set_mode(&record_dir, 0o700);
    wait_for_mount_exit(&mut mount.mounted);
    let records = retained_record_texts(&mount.repository);
    assert_eq!(
        records.len(),
        1,
        "the newest truncate is the only retained record: {records:?}"
    );
    let cut = std::str::from_utf8(&baseline[..9]).unwrap();
    assert!(
        records[0].contains("\nbytes 9\n") && records[0].ends_with(cut),
        "retained record is not the 9-byte prefix of the accepted baseline {cut:?}: {records:?}"
    );
}

#[tokio::test]
/// ORNA-VFS-009: a candidate whose release retention failed, then cut by a path truncate, is
/// retained once as its truncated record; the untruncated copy is superseded, not retained again.
async fn cwd_mount_truncate_after_failed_release_retains_one_record() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let record_dir = record_directory(&mount.repository);
    set_mode(&record_dir, 0o500);
    let mut document = OpenOptions::new().write(true).open(&mount.data_path).unwrap();
    document.write_all(b"{ title: \"unsynced writes kept\", }\n").unwrap();
    drop(document);
    // FUSE release is asynchronous to close(2); let the release refuse retention first.
    thread::sleep(Duration::from_millis(300));
    set_mode(&record_dir, 0o700);
    path_truncate(&mount.data_path, 13);
    send_sigterm(&mount.mounted);
    wait_for_mount_exit(&mut mount.mounted);
    let records = retained_record_texts(&mount.repository);
    assert_eq!(
        records.len(),
        1,
        "one candidate must retain one record: {records:?}"
    );
    assert!(
        records[0].contains("\nbytes 13\n") && records[0].ends_with("{ title: \"uns"),
        "retained record is not the 13-byte truncated prefix: {records:?}"
    );
}

#[tokio::test]
/// ORNA-VFS-024 / EDIT-1 (5il1.366.4): a refused candidate and a truncated live draft of the same
/// inode are both kept; the truncate cuts only the live draft, never the accepted baseline.
async fn cwd_mount_truncate_after_same_inode_refusal_keeps_both_drafts() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let record_dir = record_directory(&mount.repository);
    set_mode(&record_dir, 0o500);
    let mut first = OpenOptions::new()
        .write(true)
        .open(&mount.data_path)
        .unwrap();
    first
        .write_all(b"{ title: \"unsynced writes kept\",\n")
        .unwrap();
    drop(first);
    // FUSE release is asynchronous to close(2); let the release refuse retention first.
    thread::sleep(Duration::from_millis(300));
    let mut second = OpenOptions::new()
        .write(true)
        .open(&mount.data_path)
        .unwrap();
    second.write_all(b"{ title: \"second draft\",\n").unwrap();
    path_truncate(&mount.data_path, 13);
    send_sigterm(&mount.mounted);
    // Stop keeps the mount up while its drafts cannot be kept (ORNA-VFS-024).
    thread::sleep(Duration::from_millis(500));
    assert!(
        mount.mounted.child.try_wait().unwrap().is_none(),
        "mount stopped while its drafts could not be kept"
    );
    set_mode(&record_dir, 0o700);
    drop(second);
    wait_for_mount_exit(&mut mount.mounted);
    assert_eq!(
        retained_record_bodies(&mount.repository),
        ["{ title: \"sec", "{ title: \"unsynced writes kept\",\n",],
        "the truncated live draft (13 bytes) and the refused candidate are each retained in full"
    );
}

#[tokio::test]
/// ORNA-VFS-009 and ORNA-VFS-024 (5il1.366.4): two candidates of one inode, each refused on release,
/// are both kept at stop; the second refusal must not replace the first one's unretained draft.
async fn cwd_mount_release_after_same_inode_refusal_keeps_both_drafts() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let record_dir = record_directory(&mount.repository);
    set_mode(&record_dir, 0o500);
    let mut first = OpenOptions::new()
        .write(true)
        .open(&mount.data_path)
        .unwrap();
    first
        .write_all(b"{ title: \"unsynced writes kept\",\n")
        .unwrap();
    drop(first);
    // FUSE release is asynchronous to close(2); let the release refuse retention first.
    thread::sleep(Duration::from_millis(300));
    let mut second = OpenOptions::new()
        .write(true)
        .open(&mount.data_path)
        .unwrap();
    second
        .write_all(b"{ title: \"second draft is kept\",\n")
        .unwrap();
    drop(second);
    thread::sleep(Duration::from_millis(300));
    set_mode(&record_dir, 0o700);
    send_sigterm(&mount.mounted);
    wait_for_mount_exit(&mut mount.mounted);
    assert_eq!(
        retained_record_bodies(&mount.repository),
        [
            "{ title: \"second draft is kept\",\n",
            "{ title: \"unsynced writes kept\",\n",
        ],
        "both refused candidates are retained in full"
    );
}

#[tokio::test]
/// ORNA-VFS-009 and EDIT-1 step 4: a refused candidate whose retention failed, then cut by a path
/// truncate, is retained once as its truncated record; the untruncated refused copy is not retained.
async fn cwd_mount_truncate_after_refused_unretained_candidate_retains_one_record() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let record_dir = record_directory(&mount.repository);
    set_mode(&record_dir, 0o500);
    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&mount.data_path)
        .unwrap();
    document.write_all(b"{ title: \"rejected\",\n").unwrap();
    assert!(
        document.sync_all().is_err(),
        "malformed candidate must be refused by fsync"
    );
    drop(document);
    // FUSE release is asynchronous to close(2); let any release retention also fail first.
    thread::sleep(Duration::from_millis(300));
    path_truncate(&mount.data_path, 13);
    send_sigterm(&mount.mounted);
    // Stop keeps the mount up while the candidate cannot be kept, then retries retention.
    wait_for_stderr_line(&mut mount.mounted, "retrying draft retention before unmount");
    assert!(
        mount.mounted.child.try_wait().unwrap().is_none(),
        "mount stopped while its candidate could not be kept"
    );
    set_mode(&record_dir, 0o700);
    wait_for_mount_exit(&mut mount.mounted);
    let records = retained_record_texts(&mount.repository);
    assert_eq!(
        records.len(),
        1,
        "the truncated candidate is the only retained record: {records:?}"
    );
    assert!(
        records[0].contains("\nbytes 13\n") && records[0].ends_with("{ title: \"rej"),
        "retained record is not the 13-byte truncated prefix: {records:?}"
    );
}

#[tokio::test]
/// ORNA-VFS-024 and EDIT-1 steps 1-2, 4: an fsync accepts the candidate, which becomes the accepted
/// baseline. A path truncate then cuts that baseline, and a reopen for write starts from the cut.
/// Regression pin for the baseline rule, not a fix: it also passes on the pre-fix `fuse_mount.rs`
/// (b357b91f2). It does not depend on the close-time flush, because the handle is clean when it
/// closes (EDIT-1 step 5 "may repeat or not occur").
async fn cwd_mount_reopen_after_path_truncate_applies_truncate_to_accepted_baseline() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let mut earlier = OpenOptions::new().write(true).open(&mount.data_path).unwrap();
    earlier.write_all(b"{ title: \"unsynced writes kept\", }\n").unwrap();
    earlier.sync_all().unwrap();
    drop(earlier);
    path_truncate(&mount.data_path, 13);
    let mut reopened = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&mount.data_path)
        .unwrap();
    let mut seen = Vec::new();
    reopened.read_to_end(&mut seen).unwrap();
    assert_eq!(
        seen,
        b"{ title: \"uns".to_vec(),
        "the reopened writer must start from the accepted baseline cut to 13 bytes"
    );
    drop(reopened);
    send_sigterm(&mount.mounted);
    wait_for_mount_exit(&mut mount.mounted);
}

#[tokio::test]
/// ORNA-VFS-009 and ORNA-VFS-024: a read-only open of a file with a pending path truncate retains
/// nothing; the truncate stays pending and is retained once, when the mount stops.
async fn cwd_mount_read_only_open_after_path_truncate_retains_nothing_early() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let baseline = std::fs::read(&mount.data_path).unwrap();
    path_truncate(&mount.data_path, 13);
    let read = std::fs::read(&mount.data_path).unwrap();
    assert_eq!(read, &baseline[..13], "a read sees the truncated view");
    // FUSE release is asynchronous to close(2); give the read-only release time to run.
    thread::sleep(Duration::from_millis(300));
    assert!(
        retained_record_texts(&mount.repository).is_empty(),
        "a read-only open retained a draft before the mount stopped"
    );
    send_sigterm(&mount.mounted);
    wait_for_mount_exit(&mut mount.mounted);
    let records = retained_record_texts(&mount.repository);
    assert_eq!(records.len(), 1, "the pending truncate is retained once: {records:?}");
    assert!(
        records[0].contains("\nbytes 13\n")
            && records[0].ends_with(std::str::from_utf8(&baseline[..13]).unwrap()),
        "retained record is not the 13-byte truncated baseline: {records:?}"
    );
}

#[tokio::test]
/// ORNA-VFS-024 and bead 366.5: an accepted atomic replacement clears the pending path truncate of
/// the file it replaces, so a clean unmount is not refused by a truncate of superseded bytes.
async fn cwd_mount_atomic_replacement_clears_pending_path_truncate() {
    let Some(mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    path_truncate(&mount.data_path, 13);
    let scratch = mount.mountpoint.join("Music/london/.data.orna.replacement-tmp");
    let mut replacement = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .expect("create replacement scratch beside data.orna");
    replacement.write_all(b"{ title: \"replaced\", }\n").unwrap();
    replacement.sync_all().expect("accept replacement scratch");
    drop(replacement);
    std::fs::rename(&scratch, &mount.data_path).expect("atomically replace data.orna");
    let accepted = std::fs::read_to_string(&mount.data_path).unwrap();
    assert!(accepted.contains("replaced"), "the accepted replacement reads back: {accepted:?}");

    run_unmount_from(mount.repository_directory.path(), Path::new("."), &mount.mountpoint);
    assert!(!mountinfo_contains(&mount.mountpoint));
    assert!(mount.repository.mount_status().unwrap().is_empty());
}

#[tokio::test]
/// ORNA-VFS-009 and ORNA-VFS-024 (41-vfs.md:68-72): a rename refused as stale keeps the destination's pending path truncate. The refused replacement stays in its scratch, and the destination's truncated prefix is retained once when the mount stops.
async fn cwd_mount_stale_replacement_keeps_pending_path_truncate() {
    let Some(mut mount) = start_cwd_music_mount("{ title: \"before\", }\n").await else {
        return;
    };
    let repository = mount.repository_directory.path().to_path_buf();
    let scratch = mount
        .mountpoint
        .join("Music/london/.data.orna.replacement-tmp");
    // The scratch captures the row baseline before another writer advances the row.
    create_scratch(&scratch, b"{ title: \"replaced\", }\n");
    owner_edit_row_text(&repository, "title: \"before\"", "title: \"before-owner\"");
    // The mount shows the owner's accepted row; its 13-byte prefix is the truncated view.
    let baseline = std::fs::read(&mount.data_path).unwrap();
    path_truncate(&mount.data_path, 13);
    let refused = std::fs::rename(&scratch, &mount.data_path)
        .expect_err("a replacement against a row another writer advanced is stale");
    assert_eq!(refused.raw_os_error(), Some(EAGAIN));
    assert!(
        scratch.exists(),
        "the refused replacement stays in its scratch"
    );
    send_sigterm(&mount.mounted);
    wait_for_mount_exit(&mut mount.mounted);
    let records = retained_record_texts(&mount.repository);
    let data_records: Vec<&String> = records
        .iter()
        .filter(|record| record.contains("path Music/london/data.orna\n"))
        .collect();
    assert!(
        data_records.len() == 1
            && data_records[0].contains("\ncause unsynced\nbytes 13\n")
            && data_records[0].ends_with(std::str::from_utf8(&baseline[..13]).unwrap()),
        "a stale rename must keep the destination's 13-byte truncate, retained once: {records:?}"
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
            if !fuse_test_required() && is_optional_fuse_setup_failure(&stderr) {
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

/// Stops the mount process with SIGTERM without a clean unmount, retaining unresolved drafts.
fn stop_mount_retaining_drafts(mounted: &mut MountedProcess) {
    let status = Command::new("kill")
        .args(["-TERM", &mounted.child.id().to_string()])
        .status()
        .expect("send SIGTERM to the mount process");
    assert!(status.success(), "SIGTERM is delivered to the mount process");
    let _ = mounted.child.wait().unwrap();
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

/// Peak resident set size of `pid` in bytes, from its `VmHWM` status line.
fn peak_rss_bytes(pid: u32) -> u64 {
    let kib: u64 = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("Linux reports VmHWM");
    kib * 1024
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
    mount_captured_media_with_limit(original, file_name, media_type, mounted_name, 4 << 20).await
}

/// Captures `original` from `file_name` with a host cap of `capture_limit`, which must admit the whole fixture.
/// Returns `None` when FUSE is opted out or its prerequisites are optional and absent.
async fn mount_captured_media_with_limit(
    original: &[u8],
    file_name: &str,
    media_type: &str,
    mounted_name: &str,
    capture_limit: usize,
) -> Option<MountedBlobRow> {
    if !fuse_test_available() {
        return None;
    }
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

/// Mounts `Music/london` over the plain titled cover schema: a `title` Str, a required Blob
/// `content` (`tone.mp3`) and an optional Blob `cover` (`cover.png`), both captured from a media
/// root. Returns `None` when FUSE is opted out or its prerequisites are optional and absent.
async fn mount_titled_cover_row(content: &[u8], cover: &[u8]) -> Option<MountedBlobRow> {
    mount_blob_row_with_source(
        "pub table Music(id: Str) { title: Str, content: Blob, cover: Blob?, }\n",
        content,
        cover,
    )
    .await
}

/// Mounts `Music/london` over `source`, which declares the `title`, `content` and `cover` fields
/// of [`mount_titled_cover_row`]. Returns `None` when FUSE is opted out or its prerequisites are
/// optional and absent.
async fn mount_blob_row_with_source(
    source: &str,
    content: &[u8],
    cover: &[u8],
) -> Option<MountedBlobRow> {
    if !fuse_test_available() {
        return None;
    }
    let media_root = TempDir::new().unwrap();
    std::fs::write(media_root.path().join("tone.mp3"), content).unwrap();
    std::fs::write(media_root.path().join("cover.png"), cover).unwrap();
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let content_capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 4194304), \"audio/mpeg\")"
    );
    let cover_capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"cover.png\", 4194304), \"image/png\")"
    );
    let row_source = format!(
        "{{\n    title: \"before\",\n    content: {content_capture},\n    cover: {cover_capture},\n}}\n"
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
    let blob_captures = [
        ("content", content_capture.as_str()),
        ("cover", cover_capture.as_str()),
    ];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: &row_source,
        ordinal: 0x97,
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
    let cover_path = mountpoint.join("Music/london/cover.png");
    if !wait_for_mount_paths(&mut mounted, &[cover_path.as_path()]) {
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

/// Advances the row through the mount owner's `orna edit`, replacing `from` with `to` in the
/// document the editor receives. The Blob descriptors the mount wrote stay as they are, so the
/// owner commits a row another writer changed while the mount's captured baseline stays behind.
fn owner_edit_row_text(repository: &Path, from: &str, to: &str) {
    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("owner-text-edit");
    std::fs::write(
        &editor,
        format!("#!/bin/sh\nsed -i 's/{from}/{to}/' \"$1\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(repository)
        .args(["edit", "Music/london/data.orna"])
        .env("EDITOR", &editor)
        .output()
        .expect("run orna edit through the mount owner");
    assert_saved(&output, "owner-routed row edit");
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
        assert_ne!(
            std::env::var_os("ORNA_REQUIRE_MEDIA_PLAYER").as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "ORNA_REQUIRE_MEDIA_PLAYER=1 forbids skipping the mpv leg (mpv is not installed)"
        );
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
/// ORNA-VFS-1 (41-vfs.md EDIT-1; ornadb-5il1.366.3): a 64 MiB Blob candidate is written and read back
/// through the mount in 1 MiB ranges. Peak RSS growth of the mount daemon above its post-mount baseline
/// is checked after the write and read-back (16 MiB bound) and after the accept (32 MiB regression ceiling).
/// The ceiling is a regression bound with about 11 MiB headroom over the measured accept growth, not proof that no whole-candidate copy remains.
async fn cwd_mount_blob_64_mib_candidate_streams_in_bounded_rss() {
    const CHUNK_BYTES: usize = 1 << 20;
    const CHUNKS: usize = 64;
    const RSS_GROWTH_BOUND_BYTES: u64 = 16 * 1024 * 1024;
    const ACCEPT_CEILING_BYTES: u64 = 32 * 1024 * 1024;
    let original = lcg_bytes(0x0064_0000_0000_0001, 64 * 1024);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");
    let chunk_of = |chunk: usize| lcg_bytes(0x0064_0000_1000_0000 + chunk as u64, CHUNK_BYTES);
    // VmHWM is a high-water mark: the growth past this baseline is what the candidate adds.
    let baseline = peak_rss_bytes(row.mounted.child.id());

    let mut handle = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .open(&blob_path)
        .expect("write-mode open of a projected Blob must succeed");
    for chunk in 0..CHUNKS {
        handle.write_all(&chunk_of(chunk)).expect("write a 1 MiB candidate chunk");
    }
    assert_eq!(
        handle.metadata().unwrap().len(),
        (CHUNK_BYTES * CHUNKS) as u64,
        "the candidate reports its full 64 MiB length"
    );

    handle.seek(SeekFrom::Start(0)).unwrap();
    let mut read_back = vec![0u8; CHUNK_BYTES];
    for chunk in 0..CHUNKS {
        handle.read_exact(&mut read_back).expect("read a 1 MiB candidate chunk");
        assert!(read_back == chunk_of(chunk), "chunk {chunk} reads back from the candidate");
    }

    let growth = peak_rss_bytes(row.mounted.child.id()).saturating_sub(baseline);
    eprintln!("mount daemon peak RSS growth during 64 MiB candidate write and read-back: {growth} bytes");
    assert!(
        growth < RSS_GROWTH_BOUND_BYTES,
        "mount daemon peak RSS grew {growth} bytes during the candidate, reaching the bound {RSS_GROWTH_BOUND_BYTES} bytes"
    );

    handle.sync_all().expect("the 64 MiB candidate is accepted");
    // The accept commits the 64 MiB candidate through the mount. One-run measurements of the accept
    // growth above: 19,697,664 B (18.8 MiB) on d5afa463d (streaming commit 8f1cfe389 included), and
    // 20,045,824 B (19.1 MiB) on 7fa27a6eb; 88,264,704 B (84.2 MiB) on origin/main 9eab295fd. The 32 MiB
    // ceiling is a regression bound with about 11 MiB headroom. The 16 MiB goal over the floor is not yet met (ornadb-u715, partial).
    let accept_growth = peak_rss_bytes(row.mounted.child.id()).saturating_sub(baseline);
    eprintln!("mount daemon peak RSS growth through 64 MiB candidate accept: {accept_growth} bytes");
    assert!(
        accept_growth < ACCEPT_CEILING_BYTES,
        "mount daemon peak RSS grew {accept_growth} bytes through the accept, over the ceiling {ACCEPT_CEILING_BYTES} bytes"
    );
    drop(handle);
    assert_eq!(
        std::fs::metadata(&blob_path).unwrap().len(),
        (CHUNK_BYTES * CHUNKS) as u64,
        "the accepted Blob holds the full candidate"
    );
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-010 / profiles/vfs-1.md:93 (ornadb-5il1.366): an unbound editor scratch inside a
/// Blob-field row is disk-backed extents. A 64 MiB scratch written in 1 MiB ranges and read back
/// through the mount must finish within a wall-clock bound, and the mount daemon's peak RSS growth
/// must stay under 16 MiB. Regression guard: a whole-candidate copy per FUSE write would raise the
/// high-water mark by the candidate size, which exceeds that 16 MiB bound.
async fn cwd_mount_scratch_64_mib_chunked_write_is_bounded_time_and_rss() {
    const CHUNK_BYTES: usize = 1 << 20;
    const CHUNKS: usize = 64;
    const RSS_GROWTH_BOUND_BYTES: u64 = 16 * 1024 * 1024;
    const WALL_BOUND: Duration = Duration::from_secs(10);
    let original = lcg_bytes(0x0366_0000_0000_0001, 4 * 1024 + 9);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let row_path = row.mounted.mountpoint.join("Music/london");
    let scratch_path = row_path.join(".bounded64.wav.replace");
    let chunks: Vec<Vec<u8>> = (0..CHUNKS)
        .map(|chunk| lcg_bytes(0x0366_0000_1000_0000 + chunk as u64, CHUNK_BYTES))
        .collect();
    // VmHWM is a high-water mark: the growth past this baseline is what the scratch adds.
    let baseline = peak_rss_bytes(row.mounted.child.id());

    let mut handle = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&scratch_path)
        .expect("creating an unbound scratch inside a Blob-field row directory is supported");
    let started = Instant::now();
    for chunk in &chunks {
        handle.write_all(chunk).expect("write a 1 MiB scratch chunk");
    }
    handle.seek(SeekFrom::Start(0)).unwrap();
    let mut read_back = vec![0u8; CHUNK_BYTES];
    for chunk in &chunks {
        handle.read_exact(&mut read_back).expect("read a 1 MiB scratch chunk");
        assert!(read_back == *chunk, "scratch chunk reads back unchanged");
    }
    let elapsed = started.elapsed();
    let growth = peak_rss_bytes(row.mounted.child.id()).saturating_sub(baseline);
    eprintln!(
        "64 MiB unbound scratch write and read-back took {elapsed:?}; mount daemon peak RSS growth {growth} bytes"
    );
    assert_eq!(
        handle.metadata().unwrap().len(),
        (CHUNK_BYTES * CHUNKS) as u64,
        "the scratch reports its full 64 MiB length"
    );
    assert!(
        elapsed < WALL_BOUND,
        "64 MiB scratch write and read-back took {elapsed:?}, over the bound {WALL_BOUND:?}"
    );
    assert!(
        growth < RSS_GROWTH_BOUND_BYTES,
        "mount daemon peak RSS grew {growth} bytes during the scratch, reaching the bound {RSS_GROWTH_BOUND_BYTES} bytes"
    );
    drop(handle);
    std::fs::remove_file(&scratch_path).expect("removing the unbound scratch before unmount");
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
/// ORNA-VFS-019 and profiles/vfs-1.md:91-92 and :138: a Blob write handle opened under the v1 schema captured that generation. After the owner commits a schema change, its fsync is refused with EAGAIN, and the accepted Blob keeps its bytes.
async fn cwd_mount_blob_write_open_before_schema_change_is_stale_eagain() {
    let original = lcg_bytes(0x8181_0000_0000_0001, 32 * 1024);
    let replacement = lcg_bytes(0x8181_0000_0000_0002, 16 * 1024);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    let mut handle = OpenOptions::new()
        .write(true)
        .open(&blob_path)
        .expect("a write-mode open captures the v1 schema generation");

    std::fs::write(
        repository.join("main.orna"),
        "pub table Music(id: Str) { content: Blob, note: Str?, }\n",
    )
    .unwrap();
    format3::git(&repository, &["add", "main.orna"]);
    format3::git(
        &repository,
        &["commit", "--quiet", "-m", "add optional note to Music"],
    );

    handle.set_len(0).unwrap();
    handle.write_all(&replacement).unwrap();
    let refused = handle
        .sync_all()
        .expect_err("a Blob write captured under the v1 schema is stale after the schema change");
    assert_eq!(refused.raw_os_error(), Some(EAGAIN));
    drop(handle);
    assert_eq!(
        std::fs::read(&blob_path).unwrap(),
        original,
        "a stale-generation Blob write keeps the accepted bytes"
    );
    // The refused candidate stays a retained draft, so unmount names it and is refused.
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(&repository)
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
/// ORNA-VFS-011 / EDIT-1 (ornadb-5il1.366.1 M1): sibling handles opened before any accept race to sync
/// distinct candidates for one Blob. Each accepted sync commits one whole candidate, so the final bytes
/// must equal an accepted candidate or the untouched original, never a mix of two candidates.
async fn cwd_mount_blob_racing_sibling_accepts_never_commit_mixed_bytes() {
    const HANDLES: u64 = 4;
    const LEN: usize = 64 * 1024;
    let original = lcg_bytes(0x5555_0000_0000_0001, LEN);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    // Every handle opens and writes its candidate before any sync. A Blob refuses write-mode opens
    // once a candidate is accepted (its node becomes a read-only BlobBytes), so the race is between
    // syncs of handles that already hold a write baseline.
    let staged: Vec<(Vec<u8>, std::fs::File)> = (0..HANDLES)
        .map(|handle| {
            let bytes = lcg_bytes(0x5555_0000_1000_0000 + handle, LEN);
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&blob_path)
                .expect("write-mode open of a projected Blob must succeed before any accept");
            file.write_all(&bytes).expect("write a candidate");
            (bytes, file)
        })
        .collect();
    let barrier = std::sync::Barrier::new(staged.len());
    let accepted: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let workers: Vec<_> = staged
            .into_iter()
            .map(|(bytes, file)| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    file.sync_all().is_ok().then_some(bytes)
                })
            })
            .collect();
        workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap())
            .collect()
    });
    let final_bytes = std::fs::read(&blob_path).unwrap();
    // Siblings that lost the race leave `sys.vfs.stale_edit` drafts, which correctly refuse a clean
    // unmount (ORNA-VFS-009). Stop the mount process instead; E2000 help says this retains every draft
    // in recovery storage. The `MountedProcess` drop then detaches the FUSE mountpoint.
    stop_mount_retaining_drafts(&mut row.mounted);
    assert!(
        !accepted.is_empty(),
        "at least one candidate is accepted: the first sync has no sibling accept before its fsync"
    );
    assert!(
        final_bytes == original || accepted.iter().any(|candidate| *candidate == final_bytes),
        "the committed Blob must equal one whole accepted candidate or the original, never a mix"
    );
}

#[tokio::test]
/// ORNA-VFS-1 (41-vfs.md; ornadb-5il1.366.1 M3): a committed 64 MiB Blob served by a freshly started mount
/// is read back in 1 MiB ranges. The restarted daemon's RSS growth past its post-mount high-water mark
/// bounds the ranged read path for restored content. The bound is 24 MiB (25,165,824 B). Five measured
/// runs of this test grew 15,912,960, 15,015,936, 15,912,960, 15,912,960 and 14,950,400 B. The largest
/// sits 9,252,864 B (8.8 MiB) under 24 MiB; the earlier 16 MiB bound left 864,256 B (0.8 MiB). A
/// non-streaming restore would hold the whole 64 MiB Blob, so it would grow by about 64 MiB [INFERENCE: not measured], far above 24 MiB.
async fn cwd_mount_blob_64_mib_committed_restore_streams_in_bounded_rss() {
    const CHUNK_BYTES: usize = 1 << 20;
    const CHUNKS: usize = 64;
    const CAPTURE_LIMIT: usize = 65 << 20;
    const RSS_GROWTH_BOUND_BYTES: u64 = 24 * 1024 * 1024;
    let original = lcg_bytes(0x0064_0000_2000_0001, CHUNK_BYTES * CHUNKS);
    let Some(mut first) = mount_captured_media_with_limit(
        &original,
        "tone.mp3",
        "audio/mpeg",
        "content.mp3",
        CAPTURE_LIMIT,
    )
    .await
    else {
        return;
    };
    unmount_and_reap(first.repository_directory.path(), &mut first.mounted);

    let remount_parent = TempDir::new().unwrap();
    let remount_point = remount_parent.path().join("library");
    std::fs::create_dir(&remount_point).unwrap();
    let mut restored = spawn_cwd_mount(first.repository_directory.path(), &remount_point);
    let restored_blob = remount_point.join("Music/london/content.mp3");
    assert!(
        wait_for_mount_paths(&mut restored, &[restored_blob.as_path()]),
        "FUSE remount must expose the committed 64 MiB Blob"
    );
    // The restarted daemon's high-water mark starts at its post-mount level, so growth is what the reads add.
    let baseline = peak_rss_bytes(restored.child.id());
    let mut handle = File::open(&restored_blob).unwrap();
    let mut read_back = vec![0u8; CHUNK_BYTES];
    for chunk in 0..CHUNKS {
        handle.read_exact(&mut read_back).expect("read a 1 MiB range of the restored Blob");
        let range = &original[chunk * CHUNK_BYTES..(chunk + 1) * CHUNK_BYTES];
        assert!(read_back == range, "chunk {chunk} reads back from the restored Blob");
    }
    let growth = peak_rss_bytes(restored.child.id()).saturating_sub(baseline);
    eprintln!("mount daemon peak RSS growth during 64 MiB restored Blob read-back: {growth} bytes");
    assert!(
        growth < RSS_GROWTH_BOUND_BYTES,
        "restored mount daemon peak RSS grew {growth} bytes reading the Blob, reaching the bound {RSS_GROWTH_BOUND_BYTES} bytes"
    );
    drop(handle);
    unmount_and_reap(first.repository_directory.path(), &mut restored);
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
/// POSIX `EINVAL` (Linux errno 22): a document the schema refuses (profiles/vfs-1.md errno table).
const EINVAL: i32 = 22;
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
/// ORNA-VFS-012 and profiles/vfs-1.md: renaming a Blob to a compatible suffix (content.mp3 -> content.mp1, audio/mpeg admits mp1) does not transcode bytes or rename the field. Only the descriptor's suffix hint moves, committed through the row's CAS boundary, and the rename survives a remount. An incompatible suffix (content.wav) is refused with EPERM.
async fn cwd_mount_blob_compatible_suffix_rename_keeps_bytes() {
    let original = lcg_bytes(0x5151_0000_0000_0001, 64 * 1024 + 3);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let row_path = row.mounted.mountpoint.join("Music/london");
    let blob_path = row_path.join("content.mp3");

    let refused = std::fs::rename(&blob_path, row_path.join("content.wav"))
        .expect_err("renaming a Blob to a suffix its MIME does not admit is refused");
    assert_eq!(refused.raw_os_error(), Some(EPERM));
    assert_eq!(std::fs::read(&blob_path).unwrap(), original, "a refused rename keeps the Blob");

    std::fs::rename(&blob_path, row_path.join("content.mp1"))
        .expect("renaming a Blob to a compatible suffix is accepted");
    assert!(!blob_path.exists(), "the old suffix name is gone");
    let renamed = row_path.join("content.mp1");
    assert_eq!(std::fs::read(&renamed).unwrap(), original, "the bytes are not transcoded");
    let document = String::from_utf8(std::fs::read(row_path.join("data.orna")).unwrap()).unwrap();
    assert!(
        document.contains("content: { path: \"./content.mp1\", media_type: \"audio/mpeg\" }"),
        "data.orna names the renamed sibling: {document}"
    );
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);

    let remount_parent = TempDir::new().unwrap();
    let remount_point = remount_parent.path().join("library");
    std::fs::create_dir(&remount_point).unwrap();
    let mut remounted = spawn_cwd_mount(row.repository_directory.path(), &remount_point);
    let remounted_blob = remount_point.join("Music/london/content.mp1");
    assert!(
        wait_for_mount_paths(&mut remounted, &[remounted_blob.as_path()]),
        "a remount exposes the renamed Blob"
    );
    assert_eq!(std::fs::read(&remounted_blob).unwrap(), original);
    unmount_and_reap(row.repository_directory.path(), &mut remounted);
}

#[tokio::test]
/// ORNA-VFS-013 and profiles/vfs-1.md: a required Blob field is never unlinked. The mount refuses the unlink with EPERM and the Blob and its descriptor stay.
async fn cwd_mount_required_blob_unlink_is_refused() {
    let original = lcg_bytes(0x5151_0000_0000_0005, 64 * 1024 + 3);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let row_path = row.mounted.mountpoint.join("Music/london");
    let blob_path = row_path.join("content.mp3");

    let unlinked = std::fs::remove_file(&blob_path).expect_err("a required Blob is never unlinked");
    assert_eq!(unlinked.raw_os_error(), Some(EPERM));
    assert_eq!(std::fs::read(&blob_path).unwrap(), original, "the Blob keeps its bytes");
    let document = String::from_utf8(std::fs::read(row_path.join("data.orna")).unwrap()).unwrap();
    assert!(
        document.contains("content: { path: \"./content.mp3\", media_type: \"audio/mpeg\" }"),
        "data.orna still names the required Blob: {document}"
    );
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-013 and profiles/vfs-1.md: unlinking an optional Blob field commits its descriptor as `null` through the row's CAS boundary and removes its file. The other Blob keeps its bytes, and the removal survives a remount.
async fn cwd_mount_optional_blob_unlink_commits_null_and_survives_remount() {
    if !fuse_test_available() {
        return;
    }
    let content = lcg_bytes(0x0123_4567_89ab_cdef, 64 * 1024 + 5);
    let cover = lcg_bytes(0xfedc_ba98_7654_3210, 8 * 1024 + 9);
    let Some(mut row) = mount_titled_cover_row(&content, &cover).await else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let row_path = row.mounted.mountpoint.join("Music/london");
    let cover_path = row_path.join("cover.png");
    std::fs::remove_file(&cover_path).expect("unlinking an optional Blob is accepted");
    assert!(
        !cover_path.exists(),
        "the unlinked optional Blob has no file"
    );
    assert_eq!(
        std::fs::read(row_path.join("content.mp3")).unwrap(),
        content
    );
    let document = String::from_utf8(std::fs::read(row_path.join("data.orna")).unwrap()).unwrap();
    assert!(
        document.contains("cover: null"),
        "data.orna commits the optional Blob as null: {document}"
    );
    unmount_and_reap(&repository, &mut row.mounted);

    let remount_parent = TempDir::new().unwrap();
    let remount_point = remount_parent.path().join("library");
    std::fs::create_dir(&remount_point).unwrap();
    let mut remounted = spawn_cwd_mount(&repository, &remount_point);
    let remounted_content = remount_point.join("Music/london/content.mp3");
    assert!(
        wait_for_mount_paths(&mut remounted, &[remounted_content.as_path()]),
        "a remount exposes the Blob that stayed"
    );
    assert!(
        !remount_point.join("Music/london/cover.png").exists(),
        "the removal survives a remount"
    );
    assert_eq!(std::fs::read(&remounted_content).unwrap(), content);
    unmount_and_reap(&repository, &mut remounted);
}

#[tokio::test]
/// ORNA-VFS-009/012 and 41-vfs.md EDIT-1: after another writer advances the row, a Blob rename to a compatible suffix is refused with EAGAIN and keeps the Blob. The refusal refreshes the document to the current row, so the retried rename commits against the durable row.
async fn cwd_mount_blob_rename_after_owner_advances_row_is_stale_then_retries() {
    let content = lcg_bytes(0x6161_0000_0000_0001, 48 * 1024 + 11);
    let cover = lcg_bytes(0x6161_0000_0000_0002, 4 * 1024 + 3);
    let Some(mut row) = mount_titled_cover_row(&content, &cover).await else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let row_path = row.mounted.mountpoint.join("Music/london");
    let blob_path = row_path.join("content.mp3");
    let mp1_path = row_path.join("content.mp1");

    owner_edit_row_text(&repository, "title: \"before\"", "title: \"other\"");

    let stale = std::fs::rename(&blob_path, &mp1_path)
        .expect_err("a rename against a row another writer advanced is stale");
    assert_eq!(stale.raw_os_error(), Some(EAGAIN));
    assert_eq!(
        std::fs::read(&blob_path).unwrap(),
        content,
        "a stale rename keeps the Blob"
    );

    std::fs::rename(&blob_path, &mp1_path)
        .expect("the retried rename commits against the refreshed row");
    assert!(!blob_path.exists(), "the old suffix name is gone");
    assert_eq!(
        std::fs::read(&mp1_path).unwrap(),
        content,
        "the bytes are not transcoded"
    );
    let document = String::from_utf8(std::fs::read(row_path.join("data.orna")).unwrap()).unwrap();
    assert!(
        document.contains("title: \"other\""),
        "the retried commit keeps the other writer's title: {document}"
    );
    assert!(
        document.contains("content: { path: \"./content.mp1\", media_type: \"audio/mpeg\" }"),
        "data.orna names the renamed sibling: {document}"
    );
    unmount_and_reap(&repository, &mut row.mounted);

    let remount_parent = TempDir::new().unwrap();
    let remount_point = remount_parent.path().join("library");
    std::fs::create_dir(&remount_point).unwrap();
    let mut remounted = spawn_cwd_mount(&repository, &remount_point);
    let remounted_blob = remount_point.join("Music/london/content.mp1");
    assert!(
        wait_for_mount_paths(&mut remounted, &[remounted_blob.as_path()]),
        "a remount exposes the committed rename"
    );
    assert_eq!(std::fs::read(&remounted_blob).unwrap(), content);
    unmount_and_reap(&repository, &mut remounted);
}

#[tokio::test]
/// ORNA-VFS-013 and profiles/vfs-1.md: when the table schema refuses the `null` that unlinking an optional Blob would commit, the unlink fails with EINVAL, the file keeps its bytes and data.orna keeps the descriptor.
async fn cwd_mount_optional_blob_unlink_refused_by_schema_keeps_file() {
    if !fuse_test_available() {
        return;
    }
    let content = lcg_bytes(0x7171_0000_0000_0001, 16 * 1024 + 5);
    let cover = lcg_bytes(0x7171_0000_0000_0002, 8 * 1024 + 9);
    let Some(mut row) = mount_blob_row_with_source(
        "pub table Music(id: Str) {\n    title: Str,\n    content: Blob,\n    cover: Blob?,\n    assert every(track => track.cover != null);\n}\n",
        &content,
        &cover,
    )
    .await
    else {
        return;
    };
    let row_path = row.mounted.mountpoint.join("Music/london");
    let cover_path = row_path.join("cover.png");
    let document_before = std::fs::read(row_path.join("data.orna")).unwrap();
    let unlinked = std::fs::remove_file(&cover_path).expect_err("the schema refuses a null cover");
    assert_eq!(unlinked.raw_os_error(), Some(EINVAL));
    assert_eq!(
        std::fs::read(&cover_path).unwrap(),
        cover,
        "the optional Blob keeps its bytes"
    );
    assert_eq!(
        std::fs::read(row_path.join("data.orna")).unwrap(),
        document_before,
        "data.orna keeps the descriptor"
    );
    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-009/013 and profiles/vfs-1.md: after another writer advances the row, unlinking an optional Blob is refused with EAGAIN and keeps the file. A handle opened before the successful unlink still reads the captured bytes, and the retried unlink commits `null` against the refreshed row.
async fn cwd_mount_optional_blob_unlink_after_owner_advances_row_is_stale_then_retries() {
    let content = lcg_bytes(0x6262_0000_0000_0001, 16 * 1024 + 5);
    let cover = lcg_bytes(0x6262_0000_0000_0002, 8 * 1024 + 9);
    let Some(mut row) = mount_titled_cover_row(&content, &cover).await else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let row_path = row.mounted.mountpoint.join("Music/london");
    let cover_path = row_path.join("cover.png");
    let mut held = std::fs::File::open(&cover_path).expect("open the optional Blob before unlink");

    owner_edit_row_text(&repository, "title: \"before\"", "title: \"other\"");

    let stale = std::fs::remove_file(&cover_path)
        .expect_err("an unlink against a row another writer advanced is stale");
    assert_eq!(stale.raw_os_error(), Some(EAGAIN));
    assert_eq!(
        std::fs::read(&cover_path).unwrap(),
        cover,
        "a stale unlink keeps the file"
    );

    std::fs::remove_file(&cover_path)
        .expect("the retried unlink commits against the refreshed row");
    assert!(
        !cover_path.exists(),
        "the unlinked optional Blob has no file"
    );
    let mut held_bytes = Vec::new();
    held.read_to_end(&mut held_bytes)
        .expect("an open handle reads after unlink");
    assert_eq!(
        held_bytes, cover,
        "the open handle keeps the captured bytes"
    );
    drop(held);
    let document = String::from_utf8(std::fs::read(row_path.join("data.orna")).unwrap()).unwrap();
    assert!(
        document.contains("title: \"other\""),
        "the retried commit keeps the other writer's title: {document}"
    );
    assert!(
        document.contains("cover: null"),
        "data.orna commits the optional Blob as null: {document}"
    );
    unmount_and_reap(&repository, &mut row.mounted);
}

#[tokio::test]
/// ORNA-VFS-012 and profiles/vfs-1.md: a Blob renamed to a compatible suffix and renamed back keeps its bytes through each step, and the document names the suffix it has at the end.
async fn cwd_mount_blob_suffix_round_trip_keeps_bytes() {
    let original = lcg_bytes(0x6363_0000_0000_0001, 32 * 1024 + 7);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let row_path = row.mounted.mountpoint.join("Music/london");
    let mp3_path = row_path.join("content.mp3");
    let mp1_path = row_path.join("content.mp1");

    std::fs::rename(&mp3_path, &mp1_path).expect("renaming to a compatible suffix is accepted");
    assert_eq!(std::fs::read(&mp1_path).unwrap(), original);
    std::fs::rename(&mp1_path, &mp3_path)
        .expect("renaming back to the original suffix is accepted");
    assert!(!mp1_path.exists(), "the intermediate suffix name is gone");
    assert_eq!(
        std::fs::read(&mp3_path).unwrap(),
        original,
        "the bytes survive the round trip"
    );
    let document = String::from_utf8(std::fs::read(row_path.join("data.orna")).unwrap()).unwrap();
    assert!(
        document.contains("content: { path: \"./content.mp3\", media_type: \"audio/mpeg\" }"),
        "data.orna names the restored suffix: {document}"
    );
    unmount_and_reap(&repository, &mut row.mounted);
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
/// A Str over the 64 KiB inline bound is projected as a `.txt` file beside `data.orna`, and the document names it as `{text: "./title.txt", encoding: "utf-8"}` (profiles/vfs-1.md:76-80). This does not exercise ORNA-VFS-010 (metadata-only saves) or ORNA-VFS-018 (listing/stat sniffing).
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
        document.contains("./title.txt") && document.contains("utf-8"),
        "data.orna names the title.txt projection: {document}"
    );
    assert!(
        !document.contains(&large),
        "data.orna does not inline a Str over 64 KiB"
    );
    assert_eq!(
        std::fs::read_to_string(row.row_path.join("title.txt")).unwrap(),
        large,
        "title.txt holds the full large Str"
    );
    row.unmount();
    assert_title(&row.stored_row().await, &large);
}

#[tokio::test]
/// A save that shrinks a large Str to the 64 KiB bound or below returns it inline and removes its `.txt` sibling (profiles/vfs-1.md:76-79). A handle opened on the old `.txt` keeps reading the bytes it captured.
async fn cwd_mount_shrunk_str_drops_its_text_sibling() {
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
    let text_path = row.row_path.join("title.txt");
    let mut pinned = std::fs::File::open(&text_path).unwrap();
    assert_saved(
        &row.save("small", "{ title: \"small\", }"),
        "shrinking Str save",
    );
    assert!(
        !text_path.exists(),
        "the stale title.txt leaves the row directory once the Str is inline again"
    );
    let document = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(
        document.contains("\"small\""),
        "data.orna inlines the shrunk Str: {document}"
    );
    use std::io::Read as _;
    let mut captured = String::new();
    pinned.read_to_string(&mut captured).unwrap();
    drop(pinned);
    assert_eq!(
        captured, large,
        "an open handle keeps reading the pinned title.txt bytes"
    );
    row.unmount();
    assert_title(&row.stored_row().await, "small");
}

/// `count` bytes of ASCII text, so a Str built from it is over the inline bound when `count` is.
fn ascii_text(count: usize) -> String {
    (0..count).map(|index| char::from(b'a' + (index % 26) as u8)).collect()
}

/// Writes `document` to `path` through a write-intent open and syncs it, as an editor's save does.
fn write_text_and_sync(path: &Path, document: &str) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(document.as_bytes())?;
    file.sync_all()
}

/// The leading `"..."` string literal of `text`, honouring backslash escapes.
fn leading_string_literal(text: &str) -> &str {
    let bytes = text.as_bytes();
    assert_eq!(bytes.first(), Some(&b'"'), "a string literal starts the field: {text:?}");
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return &text[..=index],
            _ => index += 1,
        }
    }
    panic!("the string literal is unterminated: {text:?}");
}

#[tokio::test]
/// profiles/vfs-1.md:76-80: a `.txt` projection is byte-exact UTF-8 in both directions. NUL, quotes, backslashes, CR/LF, tabs, control characters, braces and astral scalars survive a large `.txt`, and survive the inline literal `data.orna` holds once the Str shrinks back under the bound.
async fn cwd_mount_text_round_trips_special_characters_through_document() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    assert_saved(
        &row.save("large", &format!("{{ title: \"{}\", }}", ascii_text(70 * 1024))),
        "large Str save",
    );
    let title = row.row_path.join("title.txt");
    let special = "nul\0 quote\" back\\slash\nlf\r\ncrlf\ttab {brace} bell\u{7} astral\u{1F600} é";
    let oversized = special.repeat(1_200);
    assert!(oversized.len() > 64 * 1024, "the replacement stays over the inline bound");
    write_text_and_sync(&title, &oversized).expect("a large UTF-8 replacement is accepted");
    assert_eq!(std::fs::read(&title).unwrap(), oversized.as_bytes(), "the .txt is byte-exact");

    write_text_and_sync(&title, special).expect("a short replacement is accepted");
    assert!(!title.exists(), "a short replacement is inline");
    let document = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    let start = document.find("title: ").expect("data.orna names the title") + "title: ".len();
    let literal = leading_string_literal(&document[start..]);
    let evaluated = orna_evaluator_v1::evaluate_expression(
        literal,
        &std::collections::BTreeMap::new(),
        orna_evaluator_v1::Limits::default(),
    )
    .expect("the inline title literal evaluates");
    assert_eq!(
        evaluated.raw(),
        &orna_foundation_v1::OvbRaw::Text(special.to_owned()),
        "the literal evaluates back to the written text: {literal}"
    );
    row.unmount();
    assert_title(&row.stored_row().await, special);
}

#[tokio::test]
/// ORNA-VFS-009 and 41-vfs.md EDIT-1: a `.txt` write whose baseline predates another writer's row change is refused with EAGAIN and keeps the accepted Str. A later write commits against the refreshed row, and the other writer's field survives.
async fn cwd_mount_text_write_after_owner_advances_row_is_stale_then_retries() {
    let Some(mut row) = mount_music_row_with_source(
        "pub table Music(id: Str) { title: Str, artist: Str, }\n",
        "{ title: \"before\", artist: \"Kieran\", }\n",
    )
    .await
    else {
        return;
    };
    let big = ascii_text(70 * 1024);
    assert_saved(
        &row.save("large", &format!("{{ title: \"{big}\", artist: \"Kieran\", }}")),
        "large Str save",
    );
    let repository = row.repository_directory.path().to_path_buf();
    let title = row.row_path.join("title.txt");
    let mut stale = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&title)
        .expect("a write-intent open captures the baseline");
    owner_edit_row_text(&repository, "artist: \"Kieran\"", "artist: \"Other\"");
    stale.write_all(b"stale replacement").expect("the bytes are accepted into the handle");
    let refused = stale
        .sync_all()
        .expect_err("a write against a row another writer advanced is stale");
    assert_eq!(refused.raw_os_error(), Some(EAGAIN));
    drop(stale);
    assert_eq!(std::fs::read(&title).unwrap(), big.as_bytes(), "a stale write keeps the Str");
    let retained = std::fs::read_dir(row.repository.runtime_paths().root().join("vfs-retained-drafts-v1"))
        .expect("the refused candidate is kept in recovery storage (ORNA-VFS-009)")
        .map(|entry| entry.expect("list retained draft").path())
        .filter(|path| path.is_file())
        .map(|path| std::fs::read(path).unwrap())
        .map(|record| String::from_utf8_lossy(&record).into_owned())
        .collect::<Vec<_>>();
    assert!(
        retained
            .iter()
            .any(|record| record.contains("\nbytes 17\n") && record.ends_with("stale replacement")),
        "the stale candidate was not retained: {retained:?}"
    );

    write_text_and_sync(&title, "retried replacement")
        .expect("the retried write commits against the refreshed row");
    let document = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(
        document.contains("title: \"retried replacement\"") && document.contains("artist: \"Other\""),
        "the retry keeps the other writer's artist: {document}"
    );
    row.unmount();
}

const TWO_FIELD_MUSIC: &str = "pub table Music(id: Str) { title: Str, artist: Str, }\n";
const TWO_FIELD_MUSIC_WITH_NOTE: &str =
    "pub table Music(id: Str) { title: Str, artist: Str, note: Str?, }\n";

/// Commits `source` as the repository's `main.orna` through git, as a declaration change by the owner does. A git commit takes no orna writer lease.
fn commit_source_change(repository: &Path, source: &str, message: &str) {
    std::fs::write(repository.join("main.orna"), source).unwrap();
    format3::git(repository, &["add", "main.orna"]);
    format3::git(repository, &["commit", "--quiet", "-m", message]);
}

#[tokio::test]
/// ORNA-VFS-019: a commit that leaves the Music declaration unchanged (here an unrelated Notes table) is not a schema change for Music, so it does not refuse an open document handle's save.
async fn cwd_mount_unrelated_commit_does_not_refuse_held_document_handle() {
    let Some(mut row) = mount_music_row_with_source(
        TWO_FIELD_MUSIC,
        "{ title: \"before\", artist: \"Kieran\", }\n",
    )
    .await
    else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let document = row.row_path.join("data.orna");
    let mut held = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&document)
        .expect("a write-intent open captures the baseline");
    commit_source_change(
        &repository,
        &format!("{TWO_FIELD_MUSIC}pub table Notes(id: Str) {{ body: Str, }}\n"),
        "Add unrelated Notes table",
    );
    held.write_all(b"{ title: \"Unrelated\", artist: \"Kieran\", }\n")
        .expect("the bytes are accepted into the handle");
    held.sync_all()
        .expect("a commit that leaves Music unchanged does not refuse the held handle");
    drop(held);
    let stored = std::fs::read_to_string(&document).unwrap();
    assert!(stored.contains("Unrelated"), "the held save is accepted: {stored}");
    row.unmount();
}

#[tokio::test]
/// ORNA-VFS-019: one schema change refuses every open document handle on the changed table, each for its own row. Two handles on two rows are both refused with EAGAIN.
async fn cwd_mount_two_handles_on_two_rows_are_both_refused_after_schema_change() {
    let Some(mut row) = mount_music_rows_with_source(
        TWO_FIELD_MUSIC,
        &[
            ("london", "{ title: \"before\", artist: \"Kieran\", }\n"),
            ("paris", "{ title: \"before\", artist: \"Other\", }\n"),
        ],
    )
    .await
    else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let london = row.row_path.join("data.orna");
    let paris = row.row_path.parent().unwrap().join("paris/data.orna");
    let mut london_handle = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&london)
        .expect("a write-intent open on london captures its baseline");
    let mut paris_handle = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&paris)
        .expect("a write-intent open on paris captures its baseline");
    commit_source_change(&repository, TWO_FIELD_MUSIC_WITH_NOTE, "Add optional note to Music");
    for (name, handle) in [("london", &mut london_handle), ("paris", &mut paris_handle)] {
        handle
            .write_all(b"{ title: \"Stale\", artist: \"Kieran\", }\n")
            .expect("the bytes are accepted into the handle");
        let refused = handle
            .sync_all()
            .expect_err("a write against the changed schema is stale");
        assert_eq!(refused.raw_os_error(), Some(EAGAIN), "the {name} handle is refused EAGAIN: {refused}");
    }
    drop(london_handle);
    drop(paris_handle);
    stop_mount_retaining_drafts(&mut row.mounted);
    drop(row);
}

#[tokio::test]
/// profiles/vfs-1.md:91-92 and ORNA-VFS-019: a `.txt` handle opened before a schema change is refused with EAGAIN, and the accepted Str is kept.
async fn cwd_mount_text_handle_opened_before_schema_change_is_eagain() {
    let Some(mut row) = mount_music_row_with_source(
        TWO_FIELD_MUSIC,
        "{ title: \"before\", artist: \"Kieran\", }\n",
    )
    .await
    else {
        return;
    };
    let big = ascii_text(70 * 1024);
    assert_saved(
        &row.save("large", &format!("{{ title: \"{big}\", artist: \"Kieran\", }}")),
        "large Str save",
    );
    let repository = row.repository_directory.path().to_path_buf();
    let title = row.row_path.join("title.txt");
    let mut held = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&title)
        .expect("a write-intent open captures the .txt baseline");
    commit_source_change(&repository, TWO_FIELD_MUSIC_WITH_NOTE, "Add optional note to Music");
    held.write_all(b"schema replacement")
        .expect("the bytes are accepted into the handle");
    let refused = held
        .sync_all()
        .expect_err("a .txt write against the changed schema is stale");
    assert_eq!(refused.raw_os_error(), Some(EAGAIN), "stale .txt is EAGAIN: {refused}");
    drop(held);
    assert_eq!(
        std::fs::read(&title).unwrap(),
        big.as_bytes(),
        "a refused .txt write keeps the accepted Str"
    );
    stop_mount_retaining_drafts(&mut row.mounted);
    drop(row);
}

#[tokio::test]
/// ORNA-VFS-019: a schema change between an accepted save and the next save on the same handle refuses that next save with EAGAIN. The accepted save stays.
async fn cwd_mount_schema_change_between_saves_refuses_the_next_save_on_same_handle() {
    let Some(mut row) = mount_music_row_with_source(
        TWO_FIELD_MUSIC,
        "{ title: \"before\", artist: \"Kieran\", }\n",
    )
    .await
    else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let document = row.row_path.join("data.orna");
    let mut held = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&document)
        .expect("a write-intent open captures the baseline");
    held.write_all(b"{ title: \"First\", artist: \"Kieran\", }\n")
        .expect("the first bytes are accepted into the handle");
    held.sync_all()
        .expect("the save before the schema change is accepted");
    assert!(
        std::fs::read_to_string(&document).unwrap().contains("First"),
        "the accepted save is stored before the schema change"
    );
    commit_source_change(&repository, TWO_FIELD_MUSIC_WITH_NOTE, "Add optional note to Music");
    held.set_len(0).expect("truncate the private candidate");
    held.seek(SeekFrom::Start(0)).expect("rewind the handle to the start");
    held.write_all(b"{ title: \"Second\", artist: \"Kieran\", }\n")
        .expect("the next bytes are accepted into the handle");
    let refused = held
        .sync_all()
        .expect_err("the next save on the handle is stale after the schema change");
    assert_eq!(refused.raw_os_error(), Some(EAGAIN), "next save is EAGAIN: {refused}");
    drop(held);
    // Reads through the mount fail with EIO once the schema has changed (see the edit --row test), so the
    // refusal is checked by EAGAIN above. The accepted save was checked before the change.
    stop_mount_retaining_drafts(&mut row.mounted);
    drop(row);
}

#[tokio::test]
/// profiles/vfs-1.md:76-80 and 41-vfs.md: a large Str `.txt` projects a required field, not a Blob. Renaming it is refused with EPERM, and unlinking it while the field is required is refused with EPERM. The Str keeps its text and data.orna still names the file.
async fn cwd_mount_required_text_rename_and_unlink_are_refused() {
    let big = ascii_text(70 * 1024);
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    assert_saved(
        &row.save("large", &format!("{{ title: \"{big}\", }}")),
        "large Str save",
    );
    let title = row.row_path.join("title.txt");
    let renamed = std::fs::rename(&title, row.row_path.join("title.md"))
        .expect_err("a large Str .txt is not renamed");
    assert_eq!(renamed.raw_os_error(), Some(EPERM));
    let unlinked =
        std::fs::remove_file(&title).expect_err("a required Str .txt is never unlinked");
    assert_eq!(unlinked.raw_os_error(), Some(EPERM));
    assert_eq!(std::fs::read(&title).unwrap(), big.as_bytes(), "the Str keeps its text");
    let document = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(document.contains("./title.txt"), "data.orna still names the .txt: {document}");
    row.unmount();
}

#[tokio::test]
/// profiles/vfs-1.md:110-111 and ORNA-VFS-013: unlinking the `.txt` of an optional large Str commits `null` for the field and removes the sibling file.
async fn cwd_mount_optional_text_unlink_commits_null() {
    let Some(mut row) = mount_music_row_with_source(
        "pub table Music(id: Str) { title: Str?, }\n",
        "{ title: \"before\", }\n",
    )
    .await
    else {
        return;
    };
    assert_saved(
        &row.save("large", &format!("{{ title: \"{}\", }}", ascii_text(70 * 1024))),
        "large optional Str save",
    );
    let title = row.row_path.join("title.txt");
    std::fs::remove_file(&title).expect("unlinking an optional large Str is accepted");
    assert!(!title.exists(), "the unlinked optional Str has no file");
    let document = std::fs::read_to_string(row.row_path.join("data.orna")).unwrap();
    assert!(document.contains("title: null"), "data.orna commits the optional Str as null: {document}");
    row.unmount();
}
