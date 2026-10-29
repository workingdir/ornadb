#![cfg(target_os = "linux")]
//! Kernel-limit proof for profiles/vfs-1.md:138, which names `EAGAIN` as the
//! stale expected-version errno. The `orna edit` xattr commit path cannot
//! deliver that errno to the client: on Linux 6.19.10 an `EAGAIN` reply to a
//! `setxattr` reaches the client as `EOPNOTSUPP` (95), so `orna edit` would
//! report E2010 ("outcome unknown") instead of E2000. `ESTALE` (116) passes
//! through the kernel unchanged, and the production daemon uses it for the
//! stale refusal (fuse_mount.rs, the `EditError::Stale` arm of the xattr
//! commit). Tracked by ornadb-eufo.1.1; the spec amendment is ornadb-eufo.1.2.
//!
//! The test mounts a test-only FUSE daemon that replies to `setxattr` with a
//! fixed errno, so the client errno is traceable to that reply. It changes no
//! production code. The assertions describe observed kernel behaviour: a
//! kernel that delivers `EAGAIN` unchanged fails the first test, which is the
//! signal to re-examine the spec amendment, not to weaken this test.

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

#[test]
fn eagain_reply_to_setxattr_surfaces_to_client_as_eopnotsupp() {
    if !require_fuse_or_skip() {
        return;
    }
    let (errno, calls) = client_errno_for_daemon_reply(Errno::from_i32(libc::EAGAIN));
    assert!(
        calls >= 1,
        "the client errno must come from the daemon reply; the daemon received {calls} setxattr calls",
    );
    assert_eq!(
        errno,
        libc::EOPNOTSUPP,
        "EAGAIN (profiles/vfs-1.md:138, stale expected version) reaches the client as EOPNOTSUPP (95) on this kernel, so the setxattr commit path cannot carry EAGAIN",
    );
}

#[test]
fn estale_reply_to_setxattr_passes_through_to_client() {
    if !require_fuse_or_skip() {
        return;
    }
    let (errno, calls) = client_errno_for_daemon_reply(Errno::ESTALE);
    assert!(
        calls >= 1,
        "the client errno must come from the daemon reply; the daemon received {calls} setxattr calls",
    );
    assert_eq!(
        errno,
        libc::ESTALE,
        "ESTALE (116) is the stale signal that reaches the client unchanged on the setxattr path",
    );
}
