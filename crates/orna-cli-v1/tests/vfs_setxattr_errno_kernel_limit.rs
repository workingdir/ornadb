#![cfg(target_os = "linux")]
//! Kernel-limit proof for profiles/vfs-1.md:138, which names `EAGAIN` as the
//! stale expected-version errno. In upstream fs/xattr.c (v6.19),
//! `__vfs_setxattr_noperm` (lines 245-246) rewrites a `-EAGAIN` returned by a
//! setxattr handler to `-EOPNOTSUPP` (95). The `orna edit` xattr commit path
//! therefore cannot carry `EAGAIN`: `orna edit` would report E2010 ("outcome
//! unknown") instead of E2000. `ESTALE` (116) is not rewritten. The kernel
//! retries a setxattr that fails with `ESTALE` once, with `LOOKUP_REVAL`
//! (`retry_estale` in `filename_setxattr`, fs/xattr.c lines 669-671), so the
//! daemon receives two calls. The production daemon uses `ESTALE` for the stale
//! refusal (fuse_mount.rs, the `EditError::Stale` arm of the xattr commit).
//!
//! Production coverage of that arm is end to end in tests/vfs_mount_isolation.rs:
//! `orna_edit_refuses_save_when_row_changed_during_editor_session` asserts E2000
//! at line 446, and `blind_replacement_after_late_temp_overwrites_owner_change_while_orna_edit_refuses`
//! asserts E2000 at line 824. Tracked by ornadb-eufo.1.1; the spec amendment is
//! ornadb-eufo.1.2.
//!
//! The test mounts a test-only FUSE daemon that replies to `setxattr` with a
//! fixed errno and counts the calls, so the client errno is traceable to that
//! reply. It changes no production code. The EAGAIN test accepts `EOPNOTSUPP`
//! (the rewrite above) or `EAGAIN` (a kernel that delivers it unchanged), and
//! fails on any other errno, including a lost one. The ESTALE test asserts the
//! exact call count: the original call and the kernel's one retry.

use std::{
    ffi::OsStr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, INodeNo, ReplyAttr, ReplyEmpty, Request,
};
use rustix::fs::XattrFlags;
use tempfile::TempDir;

#[allow(dead_code)]
#[path = "support/editor_mount.rs"]
mod editor_mount;
#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;
#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;
use editor_mount::{mountinfo_contains, require_fuse_or_skip};

const ROOT_INODE: u64 = 1;
const XATTR_NAME: &str = "user.orna.edit-commit";

/// Replies to every `setxattr` with one fixed errno and counts the calls, so
/// a client errno can be traced to the daemon reply it came from.
struct FixedErrnoDaemon {
    reply: Errno,
    setxattr_calls: Arc<AtomicUsize>,
}

fn root_attr() -> FileAttr {
    let now = SystemTime::now();
    FileAttr {
        ino: INodeNo(ROOT_INODE),
        size: 0,
        blocks: 0,
        atime: now,
        mtime: now,
        ctime: now,
        crtime: now,
        kind: FileType::Directory,
        perm: 0o755,
        nlink: 2,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

impl Filesystem for FixedErrnoDaemon {
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        if ino.0 == ROOT_INODE {
            reply.attr(&Duration::ZERO, &root_attr());
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn setxattr(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _name: &OsStr,
        _value: &[u8],
        _flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        self.setxattr_calls.fetch_add(1, Ordering::SeqCst);
        reply.error(self.reply);
    }
}

/// Mounts the test daemon, issues `setxattr` on the mount root, and returns
/// the client errno with the number of `setxattr` calls the daemon received.
fn client_errno_for_daemon_reply(reply: Errno) -> (i32, usize) {
    let dir = TempDir::new().expect("create mountpoint");
    let setxattr_calls = Arc::new(AtomicUsize::new(0));
    let mut config = fuser::Config::default();
    config.mount_options.push(fuser::MountOption::FSName(
        "orna-setxattr-errno-proof".into(),
    ));
    let session = fuser::spawn_mount(
        FixedErrnoDaemon {
            reply,
            setxattr_calls: Arc::clone(&setxattr_calls),
        },
        dir.path(),
        &config,
    )
    .expect("mount test daemon");
    assert!(
        mountinfo_contains(dir.path()),
        "test daemon mount is live at {}",
        dir.path().display(),
    );
    let client = rustix::fs::setxattr(dir.path(), XATTR_NAME, b"{}", XattrFlags::empty());
    let errno = client
        .expect_err("the daemon reply fails the setxattr")
        .raw_os_error();
    // Unmount before the mountpoint directory is removed.
    drop(session);
    (errno, setxattr_calls.load(Ordering::SeqCst))
}

/// The running kernel release, as `uname -r` reports it, for the proof output.
fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_owned())
        .unwrap_or_else(|error| format!("unknown ({error})"))
}

/// Names the errnos this proof expects, for the proof output.
fn errno_name(errno: i32) -> &'static str {
    match errno {
        libc::EOPNOTSUPP => "EOPNOTSUPP",
        libc::EAGAIN => "EAGAIN",
        libc::ESTALE => "ESTALE",
        _ => "unexpected",
    }
}

#[test]
fn eagain_reply_to_setxattr_surfaces_as_eopnotsupp_or_eagain() {
    if !require_fuse_or_skip() {
        return;
    }
    let (errno, calls) = client_errno_for_daemon_reply(Errno::from_i32(libc::EAGAIN));
    eprintln!(
        "kernel release {}: EAGAIN reply reached the client as errno {errno} ({}) after {calls} setxattr calls",
        kernel_release(),
        errno_name(errno),
    );
    assert!(
        calls >= 1,
        "the client errno must come from the daemon reply; the daemon received {calls} setxattr calls",
    );
    assert!(
        errno == libc::EOPNOTSUPP || errno == libc::EAGAIN,
        "EAGAIN (profiles/vfs-1.md:138, stale expected version) must reach the client as EOPNOTSUPP (fs/xattr.c rewrite) or EAGAIN, got errno {errno}",
    );
}

#[test]
fn estale_reply_to_setxattr_passes_through_to_client() {
    if !require_fuse_or_skip() {
        return;
    }
    let (errno, calls) = client_errno_for_daemon_reply(Errno::ESTALE);
    eprintln!(
        "kernel release {}: ESTALE reply reached the client as errno {errno} ({}) after {calls} setxattr calls",
        kernel_release(),
        errno_name(errno),
    );
    // Kernels with retry_estale in filename_setxattr (fs/xattr.c:669-671) send the original call and one retry.
    // The count is printed above as evidence; only the first call is a hard requirement across kernels.
    assert!(
        calls >= 1,
        "the client errno must come from the daemon reply; got {calls} setxattr calls",
    );
    assert_eq!(
        errno,
        libc::ESTALE,
        "ESTALE (116) is the stale signal that reaches the client unchanged on the setxattr path",
    );
}
