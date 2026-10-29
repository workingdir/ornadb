//! `orna mount DIR --at SELECTOR`, `orna mount DIR --cwd`, status and unmount.
//!
//! An `--at` view resolves its selector exactly once and stays read-only. `--cwd`
//! mounts the current committed workspace and sends document and sibling-temp
//! saves through validated durable-row admission. Both modes record the mounted
//! snapshot in the repository runtime, and `unmount` removes exactly that record.

use std::path::Path;
#[cfg(target_os = "linux")]
use std::{fs, os::unix::ffi::{OsStrExt, OsStringExt}};

use super::*;
use orna_repository_v1::{MountStatus, MountView, Repository};

fn mount_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run mount DIR --cwd or mount DIR --at SELECTOR inside an initialized repository",
        detail.into(),
    )
}

fn open_repository(endpoint: &Endpoint) -> Result<Repository, Diagnostic> {
    let path = local_project_path(endpoint)?;
    Repository::discover(path)
        .map_err(|error| mount_error("Repository could not be opened", format!("{error:?}")))
}

fn stdout_error() -> Diagnostic {
    mount_error("Mount output is unavailable", "stdout could not be written")
}

#[cfg(target_os = "linux")]
pub(super) fn run_mount(
    endpoint: &Endpoint,
    mountpoint: &Path,
    selector: &str,
) -> Result<(), Diagnostic> {
    let repository = open_repository(endpoint)?;
    release_dead_mount(&repository, mountpoint)?;
    let view = MountView::attach(&repository, mountpoint, selector).map_err(attach_refusal)?;
    let status = match view.verify_record() {
        Ok(status) => status,
        Err(error) => {
            let _ = view.detach();
            return Err(mount_error("Mount record could not be verified", error.to_string()));
        }
    };
    let signal_handler = match MountSignalHandler::new() {
        Ok(handler) => handler,
        Err(error) => {
            let _ = view.detach();
            return Err(mount_error("Mount signal handling could not be started", error));
        }
    };
    let source = format!("orna-{}", status.view_hex());
    let session = match crate::fuse_mount::spawn_read_only(
        &repository, view.commit(), view.context(), status.mountpoint(), source,
    ) {
        Ok(session) => session,
        Err(error) => {
            drop(signal_handler);
            let _ = view.detach();
            return Err(mount_error("Mount was refused", error));
        }
    };
    serve_mount(view, &status, signal_handler, session)
}

/// Refuses an attach, naming the remedy for a view left by a mount that is no
/// longer running.
#[cfg(target_os = "linux")]
fn attach_refusal(error: orna_repository_v1::MountError) -> Diagnostic {
    match error {
        orna_repository_v1::MountError::MountpointAlreadyAttached => Diagnostic::target_with_detail(
            "E2000",
            "Mount was refused",
            "a view is already registered at this mountpoint; run unmount DIR to release it when no mount process serves it",
            error.to_string(),
        ),
        error => mount_error("Mount was refused", error.to_string()),
    }
}

/// Whether a mount found at a mountpoint is a dead Orna FUSE mount that may be
/// released: it is an Orna FUSE mount and its mountpoint answers ENOTCONN.
#[cfg(target_os = "linux")]
fn is_releasable_dead_mount(identity: &MountedFuseIdentity, disconnected: bool) -> bool {
    identity.filesystem.starts_with("fuse") && is_orna_mount_source(&identity.source) && disconnected
}

/// Whether the mount now found at the path is still the mount that was judged
/// dead. A different mount at the same path is a replacement and must not be
/// released.
#[cfg(target_os = "linux")]
fn dead_mount_still_same(
    judged: &MountedFuseIdentity,
    current: Option<&MountedFuseIdentity>,
) -> bool {
    current == Some(judged)
}

/// Re-checks the judged dead mount, then releases it only when the re-check
/// still matches. Returns whether the release ran.
#[cfg(target_os = "linux")]
fn release_judged_dead_mount<E>(
    judged: &MountedFuseIdentity,
    current: impl FnOnce() -> Result<Option<MountedFuseIdentity>, E>,
    release: impl FnOnce() -> Result<(), E>,
) -> Result<bool, E> {
    if !dead_mount_still_same(judged, current()?.as_ref()) {
        return Ok(false);
    }
    release()?;
    Ok(true)
}

/// Releases a dead Orna FUSE mount that a killed mount process left at
/// `mountpoint` (W15-05, ORNA-VFS-021): its kernel mount answers ENOTCONN and
/// nothing serves it. The dead mount is unmounted and its view record, when it
/// belongs to this repository, is detached, so a new mount attaches without a
/// manual `fusermount -u`. Accepted rows and fsynced scratch live in the
/// repository runtime and are served again by the new mount.
#[cfg(target_os = "linux")]
fn release_dead_mount(repository: &Repository, mountpoint: &Path) -> Result<(), Diagnostic> {
    let mountpoint = canonical_mountpoint_or_original(mountpoint);
    let Some(identity) = mounted_mount_for_path(&mountpoint)? else {
        return Ok(());
    };
    let disconnected = matches!(
        &fs::symlink_metadata(&mountpoint),
        Err(error) if error.kind() == std::io::ErrorKind::NotConnected
    );
    if !is_releasable_dead_mount(&identity, disconnected) {
        return Ok(());
    }
    // The registry lock excludes Orna attach and detach while the identity is
    // re-checked and released, so a view attached since the first read is kept.
    let released = repository
        .with_mount_registry_locked(|| {
            release_judged_dead_mount(
                &identity,
                || mounted_mount_for_path(&mountpoint),
                || {
                    crate::fuse_mount::unmount_dead_mountpoint(&mountpoint)
                        .map_err(|error| mount_error("Dead Orna mount could not be released", error))
                },
            )
        })
        .map_err(|error| mount_error("Mount registry could not be locked", error.to_string()))??;
    if !released {
        let _ = writeln!(
            io::stderr().lock(),
            "orna: {} was replaced by another mount; leaving it attached",
            mountpoint.display()
        );
        return Ok(());
    }
    if let Some(view_id) = identity.source.strip_prefix("orna-") {
        match repository.open_mount_view_by_id(view_id) {
            Ok(view) => {
                let status = view.verify_record().map_err(|error| {
                    mount_error("Mount view could not be verified", error.to_string())
                })?;
                if status.mountpoint() == mountpoint {
                    detach_view_with_retry(repository, &status, view)?;
                }
            }
            Err(orna_repository_v1::MountError::NotAttached) => {}
            Err(error) => {
                return Err(mount_error("Mount view could not be opened", error.to_string()));
            }
        }
    }
    let _ = writeln!(
        io::stderr().lock(),
        "released the dead Orna mount at {} left by a stopped mount process",
        mountpoint.display()
    );
    Ok(())
}

/// How a served mount ended.
#[cfg(target_os = "linux")]
#[derive(Debug, Eq, PartialEq)]
enum MountEnd {
    /// `orna unmount` or a signal ended the session with nothing unresolved.
    Clean,
    /// A signal stopped the session holding unresolved drafts, which the mount
    /// retained and reported instead of discarding them.
    DraftsRetained,
    /// The registered view disappeared, so the mount stopped serving.
    ViewDetached,
}

/// Serves an attached view's FUSE session until it ends, then releases the
/// view. `orna unmount` can detach the view while the FUSE mount is starting,
/// so the record is checked once the mount exists and again while it serves:
/// no mount keeps serving without its registered view (W15-04).
#[cfg(target_os = "linux")]
fn serve_mount(
    view: MountView,
    status: &MountStatus,
    signal_handler: MountSignalHandler,
    session: fuser::BackgroundSession,
) -> Result<(), Diagnostic> {
    if let Err(error) = view.verify_record() {
        let cleanup = session.umount_and_join();
        drop(signal_handler);
        let _ = view.detach();
        let detail = match cleanup {
            Ok(()) => error.to_string(),
            Err(cleanup_error) => format!("{error}; FUSE cleanup also failed: {cleanup_error}"),
        };
        return Err(mount_error(
            "Mount start-up was cancelled because its view was unmounted",
            detail,
        ));
    }
    if writeln!(io::stdout().lock(), "{}", human_row(status)).is_err() {
        let cleanup = session.umount_and_join();
        drop(signal_handler);
        if let Err(error) = cleanup {
            return Err(mount_error("FUSE cleanup failed", error.to_string()));
        }
        view.detach()
            .map_err(|error| mount_error("Mount view could not be detached", error.to_string()))?;
        return Err(stdout_error());
    }
    let source = format!("orna-{}", status.view_hex());
    // `orna unmount` releases the record only after the FUSE mount is gone, so
    // a missing record while this mount is still listed means it lost its view.
    let registered = || {
        !matches!(
            view.verify_record(),
            Err(orna_repository_v1::MountError::NotAttached)
        ) || !orna_mount_is_listed(status.mountpoint(), &source)
    };
    let end = signal_handler
        .join(session, status.mountpoint(), &source, &registered)
        .map_err(|error| mount_error("Mount session ended unexpectedly", error))?;
    view.detach()
        .map_err(|error| mount_error("Mount view could not be detached", error.to_string()))?;
    match end {
        MountEnd::Clean => Ok(()),
        MountEnd::DraftsRetained => Err(Diagnostic::target_with_detail(
            "E2000",
            "Mount stopped with unresolved drafts retained",
            "the drafts listed above were not saved: reapply them from the listed recovery records, or from the scratch files the next mount restores",
            "a termination signal stopped the mount while drafts were unresolved",
        )),
        MountEnd::ViewDetached => Err(mount_error(
            "Mount stopped because its view was unmounted",
            "the registered view disappeared while the mount was serving",
        )),
    }
}

/// Whether the Orna FUSE mount with `source` is still listed at `mountpoint`.
#[cfg(target_os = "linux")]
fn orna_mount_is_listed(mountpoint: &Path, source: &str) -> bool {
    mounted_mount_for_path(mountpoint)
        .is_ok_and(|identity| identity.is_some_and(|identity| identity.source == source))
}

#[cfg(not(target_os = "linux"))]
pub(super) fn run_mount(
    _endpoint: &Endpoint,
    _mountpoint: &Path,
    _selector: &str,
) -> Result<(), Diagnostic> {
    Err(mount_error("Workspace mounts are unavailable", "mount DIR --at requires Linux FUSE support"))
}

#[cfg(target_os = "linux")]
pub(super) fn run_mount_cwd(endpoint: &Endpoint, mountpoint: &Path) -> Result<(), Diagnostic> {
    let repository = open_repository(endpoint)?;
    let commit = repository
        .head()
        .map_err(|error| mount_error("Workspace HEAD could not be read", format!("{error:?}")))?
        .ok_or_else(|| {
            mount_error(
                "Workspace has no committed HEAD",
                "publish a commit before mounting",
            )
        })?;
    release_dead_mount(&repository, mountpoint)?;
    let view = MountView::attach_writable(&repository, mountpoint, commit.as_str())
        .map_err(attach_refusal)?;
    let status = match view.verify_record() {
        Ok(status) => status,
        Err(error) => {
            let _ = view.detach();
            return Err(mount_error(
                "Mount record could not be verified",
                error.to_string(),
            ));
        }
    };
    let signal_handler = match MountSignalHandler::new() {
        Ok(handler) => handler,
        Err(error) => {
            let _ = view.detach();
            return Err(mount_error(
                "Mount signal handling could not be started",
                error,
            ));
        }
    };
    let source = format!("orna-{}", status.view_hex());
    let session = match crate::fuse_mount::spawn_editor(
        &repository,
        &commit,
        view.context(),
        status.mountpoint(),
        source,
        view.row_baselines(),
    ) {
        Ok(session) => session,
        Err(error) => {
            drop(signal_handler);
            let _ = view.detach();
            return Err(mount_error("Mount was refused", error));
        }
    };
    serve_mount(view, &status, signal_handler, session)
}

#[cfg(target_os = "linux")]
fn edit_refusal(error: crate::fuse_mount::EditError) -> Diagnostic {
    match error {
        crate::fuse_mount::EditError::Stale(detail) => Diagnostic::target_with_detail(
            "E2000",
            "Edit is stale against the mounted baseline",
            "run mount DIR --cwd to read the current row, then reapply the retained draft",
            detail,
        ),
        crate::fuse_mount::EditError::Failed(detail) => mount_error("Edit was not accepted", detail),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn run_edit(endpoint: &Endpoint, path: &str, row: bool) -> Result<(), Diagnostic> {
    let repository = open_repository(endpoint)?;
    let path = Path::new(path);
    if path.is_absolute() {
        let (mountpoint, relative) = mounted_edit_target(&repository, path, row)?;
        return edit_via_owner(&repository, &mountpoint, &relative, row);
    }
    // The writer lease is held by a live editor mount, so a second in-process
    // writer would be refused (ORNA-TURSO-005). Route through that owner instead.
    let mut writable = writable_repository_mounts(&repository)?;
    if writable.len() > 1 {
        return Err(mount_error(
            "Edit was not accepted",
            "more than one writable Orna mount serves this repository; unmount all but one",
        ));
    }
    if let Some(mount) = writable.pop() {
        return edit_via_owner(&repository, &mount.mountpoint, path, row);
    }
    let context = repository
        .open_format_context_at_selector("HEAD")
        .map_err(|error| {
            mount_error(
                "Edit was not accepted",
                format!("workspace snapshot could not be pinned: {error:?}"),
            )
        })?;
    crate::fuse_mount::edit_path(
        &repository,
        context.commit(),
        &context,
        path.to_str().unwrap_or_default(),
        row,
        None,
    )
    .map_err(edit_refusal)
}

/// Edits `relative` through the live editor mount at `mountpoint`: the CLI
/// seeds the editor draft from the mounted bytes, then the owner commits the
/// candidate with its own writer lease (ORNA-VFS-015).
#[cfg(target_os = "linux")]
fn edit_via_owner(
    repository: &Repository,
    mountpoint: &Path,
    relative: &Path,
    row: bool,
) -> Result<(), Diagnostic> {
    let initial = fs::read(mountpoint.join(relative)).map_err(|error| {
        mount_error(
            "Edit was not accepted",
            format!("mounted entry could not be read: {error}"),
        )
    })?;
    let draft = crate::fuse_mount::run_editor_on_draft(repository.runtime_paths().root(), &initial)
        .map_err(edit_refusal)?;
    let mut request = vec![if row { b'1' } else { b'0' }, 0];
    request.extend_from_slice(relative.as_os_str().as_bytes());
    request.push(0);
    request.extend_from_slice(draft.as_os_str().as_bytes());
    // The digest names the bytes the editor was seeded from, so the owner refuses a draft whose
    // mounted view moved after this read (41-vfs.md:75-77).
    request.push(0);
    request.extend_from_slice(crate::fuse_mount::edit_view_digest(&initial).as_bytes());
    match rustix::fs::setxattr(
        mountpoint,
        "user.orna.edit-commit",
        &request,
        rustix::fs::XattrFlags::empty(),
    ) {
        Ok(()) => Ok(()),
        Err(error) => settle_owner_reply(Err(error), &draft),
    }
}

/// Settles the owner's reply to `user.orna.edit-commit`.
///
/// The owner answers a refused request with a definite errno (`EBUSY`,
/// `ESTALE`, `EIO`, `EINVAL`, `ENODATA`) and retains the draft. Any other
/// errno means the reply never arrived, so the owner may have committed before
/// it ended. The retained draft is the request identity: the owner removes it
/// only after an accepted commit (ORNA-ADMIN-003). A missing draft therefore
/// means the edit was accepted; a present draft leaves the outcome unknown.
#[cfg(target_os = "linux")]
fn settle_owner_reply(
    reply: Result<(), rustix::io::Errno>,
    draft: &Path,
) -> Result<(), Diagnostic> {
    let error = match reply {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    if error == rustix::io::Errno::STALE {
        return Err(edit_refusal(crate::fuse_mount::EditError::Stale(format!(
            "the mounted baseline is stale against the durable row; nothing was written; draft retained at {}",
            draft.display()
        ))));
    }
    let definite_refusal = [
        rustix::io::Errno::BUSY,
        rustix::io::Errno::IO,
        rustix::io::Errno::INVAL,
        rustix::io::Errno::NODATA,
    ]
    .contains(&error);
    if definite_refusal {
        return Err(mount_error(
            "Edit was not accepted",
            format!(
                "mounted workspace refused the edit ({error}); draft retained at {}",
                draft.display()
            ),
        ));
    }
    if !draft.exists() {
        return Ok(());
    }
    Err(Diagnostic::target_with_detail(
        "E2010",
        "Edit outcome is unknown",
        "run `orna status` to inspect the durable row before retrying the retained draft",
        format!(
            "the mount connection ended before the owner replied ({error}); the draft at {} is still present, so the edit may or may not have been accepted",
            draft.display()
        ),
    ))
}

/// One Orna FUSE mount from `/proc/self/mountinfo`.
#[cfg(target_os = "linux")]
struct OrnaFuseMount {
    mountpoint: std::path::PathBuf,
    read_only: bool,
    view_id: String,
}

#[cfg(target_os = "linux")]
fn orna_fuse_mounts(mountinfo: &str) -> Vec<OrnaFuseMount> {
    let mut mounts = Vec::new();
    for line in mountinfo.lines() {
        let Some((mount_fields, filesystem_fields)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mountpoint) = mount_fields.split_whitespace().nth(4) else {
            continue;
        };
        let mut filesystem = filesystem_fields.split_whitespace();
        if !filesystem
            .next()
            .is_some_and(|filesystem| filesystem.starts_with("fuse"))
        {
            continue;
        }
        let Some(source) = filesystem.next() else {
            continue;
        };
        let Some(view_id) = source
            .strip_prefix("orna-")
            .filter(|_| is_orna_mount_source(source))
        else {
            continue;
        };
        let read_only = mount_fields
            .split_whitespace()
            .nth(5)
            .is_some_and(|options| options.split(',').any(|option| option == "ro"));
        mounts.push(OrnaFuseMount {
            mountpoint: std::path::PathBuf::from(std::ffi::OsString::from_vec(
                unescape_mountinfo_path(mountpoint),
            )),
            read_only,
            view_id: view_id.to_owned(),
        });
    }
    mounts
}

#[cfg(target_os = "linux")]
fn read_orna_fuse_mounts() -> Result<Vec<OrnaFuseMount>, Diagnostic> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| mount_error("Mount table could not be read", error.to_string()))?;
    Ok(orna_fuse_mounts(&mountinfo))
}

/// Writable Orna mounts whose view is registered to `repository`.
#[cfg(target_os = "linux")]
fn writable_repository_mounts(repository: &Repository) -> Result<Vec<OrnaFuseMount>, Diagnostic> {
    let views: Vec<String> = repository
        .mount_status()
        .map_err(|error| mount_error("Mount status could not be read", error.to_string()))?
        .iter()
        .map(MountStatus::view_hex)
        .collect();
    Ok(read_orna_fuse_mounts()?
        .into_iter()
        .filter(|mount| !mount.read_only && views.contains(&mount.view_id))
        .collect())
}

/// Resolves an absolute edit path to the registered mount containing it and
/// the mount-relative path to edit.
#[cfg(target_os = "linux")]
fn mounted_edit_target(
    repository: &Repository,
    path: &Path,
    row: bool,
) -> Result<(std::path::PathBuf, std::path::PathBuf), Diagnostic> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(mount_error(
            "Edit was not accepted",
            "mounted edit path may not contain parent traversal",
        ));
    }
    let outside = || {
        mount_error(
            "Edit was not accepted",
            "absolute edit path must name an entry inside an active Orna workspace mount",
        )
    };
    let mut matched: Option<(usize, OrnaFuseMount, std::path::PathBuf)> = None;
    for mount in read_orna_fuse_mounts()? {
        let Ok(relative) = path.strip_prefix(&mount.mountpoint) else {
            continue;
        };
        let depth = mount.mountpoint.components().count();
        if matched
            .as_ref()
            .is_none_or(|(current, _, _)| depth > *current)
        {
            let relative = relative.to_path_buf();
            matched = Some((depth, mount, relative));
        }
    }
    let (_, mount, relative) = matched.ok_or_else(outside)?;
    if mount.read_only {
        return Err(mount_error(
            "Edit was not accepted",
            "the selected workspace mount is read-only",
        ));
    }
    if relative.as_os_str().is_empty() {
        return Err(outside());
    }
    let serves = repository
        .mount_status()
        .map_err(|error| mount_error("Mount status could not be read", error.to_string()))?
        .iter()
        .any(|status| status.view_hex() == mount.view_id);
    if !serves {
        return Err(mount_error(
            "Edit was not accepted",
            "the selected workspace mount belongs to another repository",
        ));
    }
    let relative = if row {
        relative.join("data.orna")
    } else {
        relative
    };
    Ok((mount.mountpoint, relative))
}

#[cfg(not(target_os = "linux"))]
pub(super) fn run_edit(_endpoint: &Endpoint, _path: &str, _row: bool) -> Result<(), Diagnostic> {
    Err(mount_error("Workspace editing is unavailable", "orna edit requires Linux VFS support"))
}

/// Grace period for a termination-signal unmount. A non-lazy unmount fails
/// with EBUSY while a reference into the mount is in flight; those references
/// are transient, so the unmount is retried within this window.
#[cfg(target_os = "linux")]
const SIGNAL_UNMOUNT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(target_os = "linux")]
const SIGNAL_UNMOUNT_RETRY: std::time::Duration = std::time::Duration::from_millis(20);

/// Minimum gap between repeated draft-retention failure lines while a stop waits.
#[cfg(target_os = "linux")]
const RETENTION_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// The join loop re-checks the registered view every this many 25 ms ticks.
#[cfg(target_os = "linux")]
const VIEW_CHECK_TICKS: u32 = 8;

/// Stop cause when the registered view disappears under a serving mount.
#[cfg(target_os = "linux")]
const VIEW_DETACHED: &str = "its view was unmounted";

/// Unmounts a quiesced live mount, retrying while the kernel reports it busy.
///
/// A file closed just before unmount can still be held by its pending FUSE
/// release, so `fusermount -u` may fail with EBUSY for a short moment. The
/// mount is quiesced and kept attached meanwhile; a retry only repeats the
/// same unmount request.
#[cfg(target_os = "linux")]
fn unmount_with_busy_retry(mountpoint: &Path) -> Result<(), String> {
    let deadline = std::time::Instant::now() + SIGNAL_UNMOUNT_GRACE;
    loop {
        let error = match crate::fuse_mount::unmount_mountpoint(mountpoint) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if std::time::Instant::now() >= deadline {
            return Err(error);
        }
        std::thread::sleep(SIGNAL_UNMOUNT_RETRY);
    }
}

/// Unmounts a live FUSE session after a termination signal.
///
/// The session keeps serving requests while this retries, so a transient holder
/// can finish and release the mount. A mount still busy after the grace period
/// is reported as a failure, and the caller falls back to a lazy detach.
#[cfg(target_os = "linux")]
async fn unmount_after_signal(
    session: &fuser::BackgroundSession,
    mountpoint: &Path,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + SIGNAL_UNMOUNT_GRACE;
    loop {
        let error = match crate::fuse_mount::unmount_mountpoint(mountpoint) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if session.guard.is_finished() || std::time::Instant::now() >= deadline {
            return Err(error);
        }
        tokio::time::sleep(SIGNAL_UNMOUNT_RETRY).await;
    }
}

#[cfg(target_os = "linux")]
struct MountSignalHandler {
    runtime: tokio::runtime::Runtime,
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(target_os = "linux")]
impl MountSignalHandler {
    fn new() -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("signal runtime could not be created: {error}"))?;
        let (interrupt, terminate, hangup) = {
            let _entered = runtime.enter();
            use tokio::signal::unix::{SignalKind, signal};
            (
                signal(SignalKind::interrupt())
                    .map_err(|error| format!("SIGINT handler could not be installed: {error}"))?,
                signal(SignalKind::terminate())
                    .map_err(|error| format!("SIGTERM handler could not be installed: {error}"))?,
                signal(SignalKind::hangup())
                    .map_err(|error| format!("SIGHUP handler could not be installed: {error}"))?,
            )
        };
        Ok(Self {
            runtime,
            interrupt,
            terminate,
            hangup,
        })
    }

    /// Waits for the FUSE session to end. A termination signal, or the loss of
    /// the registered view, stops the mount: unresolved drafts are retained and
    /// reported first (ORNA-VFS-024), then the mount is unmounted.
    fn join(
        self,
        session: fuser::BackgroundSession,
        mountpoint: &Path,
        source: &str,
        registered: &dyn Fn() -> bool,
    ) -> Result<MountEnd, String> {
        let Self {
            runtime,
            mut interrupt,
            mut terminate,
            mut hangup,
        } = self;
        let mountpoint = mountpoint.to_path_buf();
        let source = source.to_owned();
        let mut session = Some(session);
        runtime.block_on(async move {
            let mut ticks = 0_u32;
            let stop = loop {
                if session
                    .as_ref()
                    .is_some_and(|session| session.guard.is_finished())
                {
                    break None;
                }
                ticks = ticks.wrapping_add(1);
                if ticks % VIEW_CHECK_TICKS == 0 && !registered() {
                    break Some(VIEW_DETACHED);
                }
                tokio::select! {
                    Some(()) = interrupt.recv() => break Some("SIGINT"),
                    Some(()) = terminate.recv() => break Some("SIGTERM"),
                    Some(()) = hangup.recv() => break Some("SIGHUP"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
                }
            };

            let mut end = MountEnd::Clean;
            let mut retention_error = None;
            if let Some(signal) = stop {
                let still_mounted = session
                    .as_ref()
                    .is_some_and(|session| !session.guard.is_finished());
                if still_mounted {
                    // Only the live Orna mount can retain its drafts; a mount
                    // that is already gone is ending normally.
                    if orna_mount_is_listed(&mountpoint, &source) {
                        // Stop does not unmount until every unresolved draft is kept
                        // (ORNA-VFS-024). A failed retention resumes the mount and is
                        // retried. The first stop signal starts that wait; a second one
                        // abandons it, and the drafts not yet kept are reported as lost.
                        let mut last_logged: Option<std::time::Instant> = None;
                        let mut abandon: Option<String> = None;
                        loop {
                            match crate::fuse_mount::retain_drafts_for_shutdown(&mountpoint) {
                                Ok(true) => {
                                    end = MountEnd::DraftsRetained;
                                    break;
                                }
                                Ok(false) => break,
                                Err(crate::fuse_mount::DraftRetentionStop::Retention(error)) => {
                                    if last_logged.is_none_or(|at| at.elapsed() >= RETENTION_LOG_INTERVAL) {
                                        let unresolved = crate::fuse_mount::unresolved_draft_list(&mountpoint)
                                            .map_or(0, |list| list.lines().filter(|line| !line.is_empty()).count());
                                        eprintln!(
                                            "{error}; {unresolved} unresolved draft(s) not retained; retrying draft retention before unmount (a second stop signal abandons the wait)"
                                        );
                                        last_logged = Some(std::time::Instant::now());
                                    }
                                    if let Err(resume) = crate::fuse_mount::cancel_unmount(&mountpoint) {
                                        eprintln!("{resume}");
                                    }
                                    let second = tokio::select! {
                                        () = tokio::time::sleep(SIGNAL_UNMOUNT_RETRY) => None,
                                        Some(()) = interrupt.recv() => Some("SIGINT"),
                                        Some(()) = terminate.recv() => Some("SIGTERM"),
                                        Some(()) = hangup.recv() => Some("SIGHUP"),
                                    };
                                    if let Some(second) = second {
                                        abandon = Some(format!("{second} repeated while draft retention was failing"));
                                        break;
                                    }
                                }
                                Err(crate::fuse_mount::DraftRetentionStop::Mount(error)) => {
                                    retention_error = Some(error);
                                    break;
                                }
                            }
                        }
                        if abandon.is_none()
                            && retention_error.is_some()
                            && crate::fuse_mount::has_unresolved_drafts(&mountpoint).unwrap_or(true)
                        {
                            abandon = Some("the mount could not be quiesced to keep its drafts".to_owned());
                        }
                        if let Some(reason) = abandon {
                            let lost = crate::fuse_mount::unresolved_draft_list(&mountpoint)
                                .unwrap_or_else(|error| format!("could not list unresolved drafts: {error}"));
                            eprintln!("stop abandoned ({reason}); these unretained drafts will be lost:");
                            for line in lost.lines().filter(|line| !line.is_empty()) {
                                eprintln!("  {line}");
                            }
                            // The mount is deliberately left up and the session is never joined:
                            // unmount refuses unresolved drafts (ORNA-VFS-024).
                            std::mem::forget(session.take().expect("live FUSE session is retained"));
                            return Err(format!("unretained drafts will be lost: stop abandoned ({reason})"));
                        }
                    }
                    let live = session.as_ref().expect("live FUSE session is retained");
                    let unmount_error = unmount_after_signal(live, &mountpoint)
                        .await
                        .err()
                        .map(|error| format!("FUSE unmount failed after {signal}: {error}"));
                    if let Some(error) = unmount_error
                        && session
                            .as_ref()
                            .is_some_and(|session| !session.guard.is_finished())
                    {
                        let fallback = session
                            .take()
                            .expect("live FUSE session is retained")
                            .umount_and_join();
                        return match fallback {
                            // The graceful unmount failed. Report that failure even though the
                            // lazy detach cleaned up, so a busy mount never exits as success.
                            Ok(()) => Err(error),
                            Err(fallback_error) => Err(format!(
                                "{error}; FUSE session cleanup also failed: {fallback_error}"
                            )),
                        };
                    }
                }
                if signal == VIEW_DETACHED {
                    end = MountEnd::ViewDetached;
                }
            }

            session
                .take()
                .expect("FUSE session is retained until it is joined")
                .join()
                .map_err(|error| format!("FUSE session join failed: {error}"))?;
            match retention_error {
                Some(error) => Err(format!(
                    "unresolved drafts could not be retained before the mount stopped: {error}"
                )),
                None => Ok(end),
            }
        })
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn run_mount_cwd(_endpoint: &Endpoint, _mountpoint: &Path) -> Result<(), Diagnostic> {
    Err(mount_error(
        "Workspace mounts are unavailable",
        "mount DIR --cwd requires Linux FUSE support",
    ))
}

pub(super) fn run_unmount(endpoint: &Endpoint, mountpoint: &Path) -> Result<(), Diagnostic> {
    let repository = open_repository(endpoint)?;
    #[cfg(target_os = "linux")]
    let mountpoint = canonical_mountpoint_or_original(mountpoint);
    #[cfg(not(target_os = "linux"))]
    let mountpoint = mountpoint.to_path_buf();

    #[cfg(target_os = "linux")]
    {
        if let Some(mount_identity) = mounted_mount_for_path(&mountpoint)? {
            let filesystem = mount_identity.filesystem.as_str();
            let source = mount_identity.source.as_str();
            if !filesystem.starts_with("fuse") || !is_orna_mount_source(&source) {
                return Err(mount_error(
                    "Unmount was refused",
                    format!("mountpoint belongs to {filesystem} {source}, not an Orna FUSE mount"),
                ));
            }
            let Some(view_id_hex) = source.strip_prefix("orna-") else {
                return Err(mount_error(
                    "Unmount was refused",
                    "FUSE source has no Orna view identity",
                ));
            };
            let attached_view = match repository.open_mount_view_by_id(view_id_hex) {
                Ok(view) => {
                    let status = view.verify_record().map_err(|error| {
                        mount_error("Mount view could not be verified", error.to_string())
                    })?;
                    if status.mountpoint() != mountpoint {
                        return Err(mount_error(
                            "Unmount was refused",
                            format!(
                                "FUSE source identifies a view attached at {}, not {}",
                                status.mountpoint().display(),
                                mountpoint.display(),
                            ),
                        ));
                    }
                    Some((view, status))
                }
                Err(orna_repository_v1::MountError::NotAttached) => {
                    let statuses = repository.mount_status().map_err(|error| {
                        mount_error("Mount status could not be read", error.to_string())
                    })?;
                    if let Some(other_view) = statuses
                        .iter()
                        .find(|status| status.mountpoint() == mountpoint)
                    {
                        return Err(mount_error(
                            "Unmount was refused",
                            format!(
                                "mountpoint record identifies view {}, not FUSE source {}",
                                other_view.view_hex(),
                                view_id_hex,
                            ),
                        ));
                    }
                    None
                }
                Err(error) => {
                    return Err(mount_error(
                        "Mount view could not be opened",
                        error.to_string(),
                    ));
                }
            };
            let (quiesced, dead) = match crate::fuse_mount::begin_unmount(&mountpoint) {
                Ok(()) => (true, false),
                Err(crate::fuse_mount::UnmountPreparationError::UnresolvedDrafts) => {
                    return Err(unresolved_drafts_refusal(&mountpoint));
                }
                Err(crate::fuse_mount::UnmountPreparationError::ReadOnlyMount) => {
                    let unresolved = crate::fuse_mount::has_unresolved_drafts(&mountpoint)
                        .map_err(|error| mount_error("Mount draft state could not be read", error))?;
                    if unresolved {
                        return Err(unresolved_drafts_refusal(&mountpoint));
                    }
                    (false, false)
                }
                // The FUSE daemon is gone (killed, W15-05): the kernel mount
                // answers ENOTCONN and holds no live drafts. Release it and its
                // view; accepted saves and fsynced scratch are in the runtime.
                Err(crate::fuse_mount::UnmountPreparationError::Disconnected) => (false, true),
                Err(crate::fuse_mount::UnmountPreparationError::Operation(error)) => {
                    return Err(mount_error("Mount draft state could not be quiesced", error));
                }
            };
            match mounted_mount_for_path(&mountpoint)? {
                Some(current) if current == mount_identity => {
                    let unmount_error = if dead {
                        crate::fuse_mount::unmount_dead_mountpoint(&mountpoint)
                    } else {
                        unmount_with_busy_retry(&mountpoint)
                    }
                    .err();
                    let remaining = mounted_mount_for_path(&mountpoint)?;
                    if remaining.as_ref() == Some(&mount_identity) {
                        let detail = unmount_error.unwrap_or_else(|| {
                            "FUSE unmount helper returned successfully but the validated mount remains attached".to_owned()
                        });
                        if quiesced {
                            crate::fuse_mount::cancel_unmount(&mountpoint)
                                .map_err(|resume_error| {
                                    mount_error(
                                        "FUSE unmount failed and the mount could not resume",
                                        format!("{detail}; {resume_error}"),
                                    )
                                })?;
                        }
                        return Err(mount_error("FUSE unmount failed", detail));
                    }
                }
                Some(_) => {
                    return Err(mount_error(
                        "Unmount was refused",
                        "mount identity changed before unmount; the replacement mount was left untouched",
                    ));
                }
                None => {}
            }
            if let Some((attached_view, status)) = attached_view {
                detach_view_with_retry(&repository, &status, attached_view)?;
                let written = if dead {
                    writeln!(
                        io::stdout().lock(),
                        "unmounted {} snapshot {}; its mount process had stopped, accepted saves are kept and fsynced scratch is restored by the next mount",
                        status.mountpoint().display(),
                        status.snapshot_hex(),
                    )
                } else {
                    writeln!(
                        io::stdout().lock(),
                        "unmounted {} snapshot {}",
                        status.mountpoint().display(),
                        status.snapshot_hex(),
                    )
                };
                written.map_err(|_| stdout_error())?;
            } else {
                writeln!(
                    io::stdout().lock(),
                    "unmounted orphan FUSE mount {}",
                    mountpoint.display(),
                )
                .map_err(|_| stdout_error())?;
            }
            return Ok(());
        }

        let statuses = repository
            .mount_status()
            .map_err(|error| mount_error("Mount status could not be read", error.to_string()))?;
        let Some(status) = statuses
            .into_iter()
            .find(|status| status.mountpoint() == mountpoint)
        else {
            writeln!(io::stdout().lock(), "already unmounted {}", mountpoint.display())
                .map_err(|_| stdout_error())?;
            return Ok(());
        };
        let view = repository
            .open_mount_view_by_id(&status.view_hex())
            .map_err(|error| mount_error("Mount view could not be opened", error.to_string()))?;
        if view
            .verify_record()
            .map_err(|error| mount_error("Mount view could not be verified", error.to_string()))?
            != status
        {
            return Err(mount_error(
                "Unmount was refused",
                "mount view changed after its status was read",
            ));
        }
        detach_view_with_retry(&repository, &status, view)?;
        writeln!(
            io::stdout().lock(),
            "unmounted {} snapshot {}",
            status.mountpoint().display(),
            status.snapshot_hex(),
        )
        .map_err(|_| stdout_error())?;
        return Ok(());
    }

    #[cfg(not(target_os = "linux"))]
    {
        let status = repository
            .unmount_view(&mountpoint)
            .map_err(|error| mount_error("Unmount was refused", error.to_string()))?;
        writeln!(
            io::stdout().lock(),
            "unmounted {} snapshot {}",
            status.mountpoint().display(),
            status.snapshot_hex(),
        )
        .map_err(|_| stdout_error())?;
        Ok(())
    }
}

/// Refuses an unmount that would discard unresolved drafts (ORNA-VFS-024),
/// naming each draft, its state and its cause code on stderr first, so the
/// remedy applies to known files (W15-03).
#[cfg(target_os = "linux")]
fn unresolved_drafts_refusal(mountpoint: &Path) -> Diagnostic {
    let mut stderr = io::stderr().lock();
    match crate::fuse_mount::unresolved_draft_list(mountpoint) {
        Ok(list) => {
            let parsed = parse_draft_list(&list);
            for draft in &parsed.drafts {
                let _ = writeln!(stderr, "unresolved draft: {}: {}", draft.path, draft.detail);
            }
            if parsed.omitted > 0 {
                let _ = writeln!(stderr, "unresolved drafts not listed: {}", parsed.omitted);
            }
        }
        Err(error) => {
            let _ = writeln!(stderr, "unresolved drafts could not be listed: {}", draft_listing_failure(error));
        }
    }
    Diagnostic::target_with_detail(
        "E2000",
        "Unmount was refused",
        "unresolved workspace drafts remain (listed above): reopen each listed data.orna, which shows the current row, reapply or drop your change and fsync it; rename or remove each listed scratch file; close editors holding unsynced writes; or stop the mount process to retain every draft in recovery storage",
        "the FUSE mount and its repository view remain attached",
    )
}

#[cfg(target_os = "linux")]
fn detach_view_with_retry(
    repository: &Repository,
    status: &MountStatus,
    view: MountView,
) -> Result<(), Diagnostic> {
    let view_id = status.view_hex();
    let first_error = match view.detach() {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let retry_view = match repository.open_mount_view_by_id(&view_id) {
        Ok(view) => view,
        Err(orna_repository_v1::MountError::NotAttached) => return Ok(()),
        Err(error) => {
            return Err(mount_error(
                "Mount view could not be detached",
                format!("{first_error}; recovery lookup failed: {error}"),
            ));
        }
    };
    let retry_status = retry_view.verify_record().map_err(|error| {
        mount_error(
            "Mount view could not be detached",
            format!("{first_error}; recovery record could not be verified: {error}"),
        )
    })?;
    if &retry_status != status {
        return Err(mount_error(
            "Mount view could not be detached",
            "the registered view changed during post-unmount recovery",
        ));
    }
    match retry_view.detach() {
        Ok(()) => Ok(()),
        Err(second_error) => match repository.open_mount_view_by_id(&view_id) {
            Err(orna_repository_v1::MountError::NotAttached) => Ok(()),
            Err(recovery_error) => Err(mount_error(
                "Mount view could not be detached",
                format!("{first_error}; retry failed: {second_error}; recovery lookup failed: {recovery_error}"),
            )),
            Ok(_) => Err(mount_error(
                "Mount view remains attached after FUSE unmount",
                format!("{first_error}; retry failed: {second_error}; run unmount again to retry view cleanup"),
            )),
        },
    }
}
#[cfg(target_os = "linux")]
fn canonical_mountpoint_or_original(path: &Path) -> std::path::PathBuf {
    match fs::canonicalize(path) {
        Ok(canonical) => return canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
        Err(_) => {}
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    match (path.file_name(), fs::canonicalize(parent)) {
        (Some(name), Ok(parent)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

#[cfg(target_os = "linux")]
fn is_orna_mount_source(source: &str) -> bool {
    let Some(view) = source.strip_prefix("orna-") else {
        return false;
    };
    view.len() == 64
        && view
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct MountedFuseIdentity {
    mount_id: u64,
    filesystem: String,
    source: String,
}

#[cfg(target_os = "linux")]
fn mounted_mount_for_path(path: &Path) -> Result<Option<MountedFuseIdentity>, Diagnostic> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| mount_error("Mount table could not be read", error.to_string()))?;
    Ok(mount_identity_in_mountinfo(&mountinfo, path))
}

/// The identity of the mount at `path` in a `/proc/self/mountinfo` table.
#[cfg(target_os = "linux")]
fn mount_identity_in_mountinfo(mountinfo: &str, path: &Path) -> Option<MountedFuseIdentity> {
    for line in mountinfo.lines() {
        let Some((mount_fields, filesystem_fields)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mountpoint) = mount_fields.split_whitespace().nth(4) else {
            continue;
        };
        let Some(mount_id) = mount_fields
            .split_whitespace()
            .next()
            .and_then(|field| field.parse::<u64>().ok())
        else {
            continue;
        };
        if unescape_mountinfo_path(mountpoint) != path.as_os_str().as_bytes() {
            continue;
        }
        let mut fields = filesystem_fields.split_whitespace();
        let filesystem = fields.next().unwrap_or_default().to_owned();
        let source = fields.next().unwrap_or_default().to_owned();
        return Some(MountedFuseIdentity {
            mount_id,
            filesystem,
            source,
        });
    }
    None
}

#[cfg(target_os = "linux")]
fn unescape_mountinfo_path(field: &str) -> Vec<u8> {
    let bytes = field.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if index + 3 < bytes.len()
            && bytes[index] == 0x5c
            && bytes[index + 1..=index + 3]
                .iter()
                .all(|byte| (b'0'..=b'7').contains(byte))
        {
            let value = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');
            decoded.push(value);
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    decoded
}

pub(super) fn run_mount_status(endpoint: &Endpoint, json: bool) -> Result<(), Diagnostic> {
    let repository = open_repository(endpoint)?;
    let statuses = repository
        .mount_status()
        .map_err(|error| mount_error("Mount status could not be read", error.to_string()))?;
    let mut out = io::stdout().lock();
    if json {
        write!(out, "{{\"mounts\":[").map_err(|_| stdout_error())?;
        for (index, status) in statuses.iter().enumerate() {
            if index > 0 {
                write!(out, ",").map_err(|_| stdout_error())?;
            }
            write!(out, "{}", status_json(status)).map_err(|_| stdout_error())?;
        }
        writeln!(out, "]}}").map_err(|_| stdout_error())?;
    } else if statuses.is_empty() {
        writeln!(out, "no mounts").map_err(|_| stdout_error())?;
    } else {
        for status in &statuses {
            writeln!(out, "{}", human_row(status)).map_err(|_| stdout_error())?;
            let listing = status_drafts(status);
            for draft in &listing.drafts {
                writeln!(
                    out,
                    "  unresolved draft {} code {} draft {}: {}",
                    draft.path,
                    draft.code.as_deref().unwrap_or("-"),
                    draft.draft.as_deref().unwrap_or("-"),
                    draft.detail,
                )
                .map_err(|_| stdout_error())?;
            }
            if listing.omitted > 0 {
                writeln!(
                    out,
                    "  note: {} more unresolved draft(s) not listed; the daemon bounds its draft list",
                    listing.omitted
                )
                .map_err(|_| stdout_error())?;
            }
            if listing.source == Some(DraftSource::List) {
                writeln!(
                    out,
                    "  note: this mount's daemon predates the sys.* code and draft id fields; the code and draft columns are empty"
                )
                .map_err(|_| stdout_error())?;
            }
            if let Some(error) = listing.error {
                writeln!(out, "  drafts could not be listed: {error}").map_err(|_| stdout_error())?;
            }
        }
    }
    Ok(())
}

fn human_row(status: &MountStatus) -> String {
    let access = if status.is_writable() { "writable" } else { "read-only" };
    format!(
        "{} snapshot {} view {} format {} legacy {} {access}",
        status.mountpoint().display(),
        status.snapshot_hex(),
        status.view_hex(),
        status.repository_format_number(),
        status.is_legacy_format(),
    )
}

fn status_json(status: &MountStatus) -> String {
    let listing = status_drafts(status);
    mount_row_json(
        status.mountpoint(),
        &status.snapshot_hex(),
        &status.view_hex(),
        status.repository_format_number(),
        status.is_legacy_format(),
        status.is_writable(),
        &listing,
    )
}

/// One unresolved draft of a writable mount, as `mount status` reports it. `code`
/// is the exact `sys.*` Orna cause and `draft` the retained draft id. Each is
/// `None` when the draft has none: a backing I/O or unsynced-write state that the
/// profile names no code for, or a candidate that recovery storage did not retain.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DraftEntry {
    path: String,
    code: Option<String>,
    draft: Option<String>,
    detail: String,
}

/// The lines of an unresolved draft list, and how many drafts the daemon left out
/// of it because its size budget was full.
#[cfg(target_os = "linux")]
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedDrafts {
    drafts: Vec<DraftEntry>,
    omitted: u64,
}

/// Parses the `path<TAB>code<TAB>draft<TAB>detail` lines of an unresolved draft
/// list. A line with only `path<TAB>detail` comes from a mount that predates the
/// code and draft fields, so both are absent. The `\tomitted\tN` line counts the
/// drafts the daemon did not list; its empty path means it is never a draft.
#[cfg(target_os = "linux")]
fn parse_draft_list(list: &str) -> ParsedDrafts {
    fn non_empty(field: &str) -> Option<String> {
        (!field.is_empty()).then(|| field.to_owned())
    }
    let mut parsed = ParsedDrafts::default();
    for line in list.lines().filter(|line| !line.is_empty()) {
        if let Some(count) = line.strip_prefix("\tomitted\t") {
            parsed.omitted += count.parse::<u64>().unwrap_or(0);
            continue;
        }
        let fields: Vec<&str> = line.splitn(4, '\t').collect();
        parsed.drafts.push(match fields[..] {
            [path, code, draft, detail] => DraftEntry {
                path: path.to_owned(),
                code: non_empty(code),
                draft: non_empty(draft),
                detail: detail.to_owned(),
            },
            [path, detail] => DraftEntry {
                path: path.to_owned(),
                code: None,
                draft: None,
                detail: detail.to_owned(),
            },
            _ => DraftEntry {
                path: line.to_owned(),
                code: None,
                draft: None,
                detail: "unresolved".to_owned(),
            },
        });
    }
    parsed
}

/// The unresolved drafts of a writable mount, with their exact Orna cause codes
/// and retained draft ids, and how they were listed. A writable mount whose drafts
/// cannot be listed reports the reason instead of failing the whole status.
#[derive(Debug, Default)]
struct DraftListing {
    drafts: Vec<DraftEntry>,
    /// Drafts the daemon did not list because its list was full.
    omitted: u64,
    /// The xattr the drafts came from; `None` when they could not be listed.
    source: Option<DraftSource>,
    /// Why the drafts could not be listed.
    error: Option<String>,
}

/// Which draft-list xattr a listing came from. `List` is the two-column fallback of
/// a daemon that predates the code and draft fields, so its codes and draft ids are
/// absent.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DraftSource {
    Fields,
    List,
}

impl DraftSource {
    fn json_name(self) -> &'static str {
        match self {
            Self::Fields => "fields",
            Self::List => "list",
        }
    }
}

#[cfg(target_os = "linux")]
fn status_drafts(status: &MountStatus) -> DraftListing {
    if !status.is_writable() {
        return DraftListing::default();
    }
    draft_listing(
        crate::fuse_mount::unresolved_draft_fields(status.mountpoint()),
        || crate::fuse_mount::unresolved_draft_list(status.mountpoint()),
    )
}

/// Chooses the draft listing from the fields xattr. A daemon that does not serve
/// it answers ENODATA; its two-column list xattr is then the fallback, not a failure.
#[cfg(target_os = "linux")]
fn draft_listing(
    fields: Result<String, rustix::io::Errno>,
    list: impl FnOnce() -> Result<String, rustix::io::Errno>,
) -> DraftListing {
    match fields {
        Ok(fields) => DraftListing::listed(&fields, DraftSource::Fields),
        Err(rustix::io::Errno::NODATA) => match list() {
            Ok(list) => DraftListing::listed(&list, DraftSource::List),
            Err(error) => DraftListing::failed(error),
        },
        Err(error) => DraftListing::failed(error),
    }
}

#[cfg(target_os = "linux")]
impl DraftListing {
    fn listed(list: &str, source: DraftSource) -> Self {
        let parsed = parse_draft_list(list);
        Self {
            drafts: parsed.drafts,
            omitted: parsed.omitted,
            source: Some(source),
            error: None,
        }
    }

    fn failed(error: rustix::io::Errno) -> Self {
        Self {
            error: Some(draft_listing_failure(error)),
            ..Self::default()
        }
    }
}

/// Names a failed draft listing. A writable record whose mountpoint is no longer
/// a live Orna mount (the daemon exited, so the kernel answers ENOTCONN, or the
/// path is gone, ENOENT) is a stale record; any other error is a live failure.
#[cfg(target_os = "linux")]
fn draft_listing_failure(error: rustix::io::Errno) -> String {
    if error == rustix::io::Errno::NOTCONN || error == rustix::io::Errno::NOENT {
        "stale mount record: the mountpoint is not a live Orna mount (its process is gone); run `orna unmount` to clear it".to_owned()
    } else {
        format!("could not list Orna mount drafts: {error}")
    }
}

#[cfg(not(target_os = "linux"))]
fn status_drafts(status: &MountStatus) -> DraftListing {
    if !status.is_writable() {
        return DraftListing::default();
    }
    DraftListing {
        error: Some("live draft listing requires Linux FUSE".to_owned()),
        ..DraftListing::default()
    }
}

/// One status object with the documented keys. The mountpoint is JSON-escaped
/// so a path containing a quote, backslash or control character cannot produce
/// invalid output. `draft_source` names the xattr the drafts came from (`fields`,
/// or `list` for a daemon without the code and draft fields), or is `null` when
/// there is no listing. `drafts_omitted` counts drafts the daemon did not list.
fn mount_row_json(
    mountpoint: &Path,
    snapshot_hex: &str,
    view_hex: &str,
    format: u8,
    legacy: bool,
    writable: bool,
    listing: &DraftListing,
) -> String {
    let drafts = listing
        .drafts
        .iter()
        .map(|draft| {
            format!(
                "{{\"path\":\"{}\",\"code\":{},\"draft\":{},\"detail\":\"{}\"}}",
                escape_json(&draft.path),
                optional_json(draft.code.as_deref()),
                optional_json(draft.draft.as_deref()),
                escape_json(&draft.detail)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let draft_source = match listing.source {
        Some(source) => format!("\"{}\"", source.json_name()),
        None => "null".to_owned(),
    };
    let draft_error = optional_json(listing.error.as_deref());
    format!(
        "{{\"mountpoint\":\"{}\",\"snapshot\":\"{}\",\"view\":\"{}\",\"format\":{},\"legacy\":{},\"writable\":{},\"read_only\":{},\"draft_source\":{},\"drafts_truncated\":{},\"drafts_omitted\":{},\"unresolved_drafts\":[{}],\"draft_error\":{}}}",
        escape_json(&mountpoint.to_string_lossy()),
        snapshot_hex,
        view_hex,
        format,
        legacy,
        writable,
        !writable,
        draft_source,
        listing.omitted > 0,
        listing.omitted,
        drafts,
        draft_error,
    )
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control < ' ' => {
                escaped.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// JSON for an optional string: the escaped string, or `null`.
fn optional_json(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", escape_json(value)),
        None => "null".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{escape_json, mount_row_json, DraftEntry, DraftListing, DraftSource};
    use std::path::Path;

    #[test]
    fn status_json_has_exactly_the_documented_keys() {
        let object = mount_row_json(
            Path::new("/mnt/old"),
            "aa",
            "bb",
            1,
            true,
            false,
            &DraftListing::default(),
        );
        assert_eq!(
            object,
            "{\"mountpoint\":\"/mnt/old\",\"snapshot\":\"aa\",\"view\":\"bb\",\"format\":1,\"legacy\":true,\"writable\":false,\"read_only\":true,\"draft_source\":null,\"drafts_truncated\":false,\"drafts_omitted\":0,\"unresolved_drafts\":[],\"draft_error\":null}"
        );
    }

    #[test]
    fn mount_row_json_formats_writable_and_draft_fields() {
        let listing = DraftListing {
            drafts: vec![DraftEntry {
                path: "Music/london/data.orna".to_owned(),
                code: Some("sys.vfs.stale_edit".to_owned()),
                draft: Some("d1".to_owned()),
                detail: "not accepted (sys.vfs.stale_edit); the file shows the current row; candidate retained as draft d1".to_owned(),
            }],
            source: Some(DraftSource::Fields),
            ..DraftListing::default()
        };
        let object =
            mount_row_json(Path::new("/mnt/library"), "aa", "bb", 3, false, true, &listing);
        assert!(
            object.contains("\"writable\":true,\"read_only\":false,\"draft_source\":\"fields\",\"drafts_truncated\":false,\"drafts_omitted\":0"),
            "{object}"
        );
        assert!(
            object.contains("\"unresolved_drafts\":[{\"path\":\"Music/london/data.orna\",\"code\":\"sys.vfs.stale_edit\",\"draft\":\"d1\",\"detail\":\"not accepted (sys.vfs.stale_edit); the file shows the current row; candidate retained as draft d1\"}]"),
            "{object}"
        );
        assert!(object.contains("\"draft_error\":null"), "{object}");
    }

    #[test]
    fn list_fallback_and_omitted_drafts_are_flagged_in_json() {
        let listing = DraftListing {
            drafts: vec![DraftEntry {
                path: "scratch.tmp".to_owned(),
                code: None,
                draft: None,
                detail: "scratch file not renamed".to_owned(),
            }],
            omitted: 3,
            source: Some(DraftSource::List),
            error: None,
        };
        let object =
            mount_row_json(Path::new("/mnt/library"), "aa", "bb", 3, false, true, &listing);
        assert!(
            object.contains("\"draft_source\":\"list\",\"drafts_truncated\":true,\"drafts_omitted\":3"),
            "{object}"
        );
        assert!(
            object.contains("{\"path\":\"scratch.tmp\",\"code\":null,\"draft\":null,\"detail\":\"scratch file not renamed\"}"),
            "{object}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn draft_list_lines_carry_the_code_and_draft_id() {
        use super::parse_draft_list;

        let list = "Music/london/data.orna\tsys.vfs.stale_edit\td1\tnot accepted (sys.vfs.stale_edit); candidate retained as draft d1\nscratch.tmp\t\t\tscratch file not renamed\n\tomitted\t2";
        let parsed = parse_draft_list(list);
        assert_eq!(parsed.omitted, 2);
        assert_eq!(parsed.drafts.len(), 2, "the omitted marker is not a draft");
        assert_eq!(parsed.drafts[0].path, "Music/london/data.orna");
        assert_eq!(parsed.drafts[0].code.as_deref(), Some("sys.vfs.stale_edit"));
        assert_eq!(parsed.drafts[0].draft.as_deref(), Some("d1"));
        assert_eq!(
            parsed.drafts[0].detail,
            "not accepted (sys.vfs.stale_edit); candidate retained as draft d1"
        );
        assert_eq!(parsed.drafts[1].path, "scratch.tmp");
        assert_eq!(parsed.drafts[1].code, None);
        assert_eq!(parsed.drafts[1].draft, None);
        assert_eq!(parsed.drafts[1].detail, "scratch file not renamed");
        assert_eq!(parse_draft_list("x\t\t\tdetail").omitted, 0);

        // The unmount-refusal list reports `path<TAB>detail`, with no code or draft fields.
        let legacy =
            parse_draft_list("Music/london/data.orna\tnot accepted (sys.vfs.stale_edit)").drafts;
        assert_eq!(legacy[0].path, "Music/london/data.orna");
        assert_eq!(legacy[0].code, None);
        assert_eq!(legacy[0].draft, None);
        assert_eq!(legacy[0].detail, "not accepted (sys.vfs.stale_edit)");
    }

    #[test]
    fn status_json_escapes_a_hostile_mountpoint() {
        let object = mount_row_json(
            Path::new("/mnt/a\"b\\c"),
            "aa",
            "bb",
            3,
            false,
            false,
            &DraftListing::default(),
        );
        assert!(object.contains("\\\""), "quote is escaped: {object}");
        assert!(object.contains("\\\\"), "backslash is escaped: {object}");
        assert!(
            !object.contains("a\"b"),
            "raw quote must not appear: {object}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_writable_record_is_reported_distinctly_from_a_live_draft_failure() {
        use super::draft_listing_failure;
        use rustix::io::Errno;

        let stale = draft_listing_failure(Errno::NOTCONN);
        let missing = draft_listing_failure(Errno::NOENT);
        let live = draft_listing_failure(Errno::NODATA);
        assert!(stale.starts_with("stale mount record"), "{stale}");
        assert_eq!(stale, missing);
        assert!(live.starts_with("could not list Orna mount drafts"), "{live}");
        assert!(!live.contains("stale"), "{live}");

        let stale_listing = DraftListing {
            error: Some(stale.clone()),
            ..DraftListing::default()
        };
        let stale_json =
            mount_row_json(Path::new("/mnt/gone"), "aa", "bb", 3, false, true, &stale_listing);
        assert!(stale_json.contains("\"draft_error\":\"stale mount record"), "{stale_json}");
        assert!(stale_json.contains("\"writable\":true"), "{stale_json}");
        let live_listing = DraftListing {
            error: Some(live.clone()),
            ..DraftListing::default()
        };
        let live_json =
            mount_row_json(Path::new("/mnt/gone"), "aa", "bb", 3, false, true, &live_listing);
        assert!(live_json.contains("could not list Orna mount drafts"), "{live_json}");
        assert!(!live_json.contains("stale"), "{live_json}");
    }

    #[test]
    fn escape_json_leaves_ordinary_paths_unchanged() {
        assert_eq!(escape_json("/mnt/library"), "/mnt/library");
        assert_eq!(escape_json("tab\there"), "tab\\there");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn draft_listing_falls_back_to_the_list_xattr_when_fields_are_unserved() {
        use super::{draft_listing, DraftSource};
        use rustix::io::Errno;

        let fallback = draft_listing(Err(Errno::NODATA), || {
            Ok("Music/london/data.orna\tstale edit\n".to_owned())
        });
        assert_eq!(fallback.source, Some(DraftSource::List), "an older daemon's list is the fallback");
        assert_eq!(fallback.error, None);
        assert_eq!(fallback.drafts.len(), 1);
        assert_eq!(fallback.drafts[0].code, None);
        assert_eq!(fallback.drafts[0].draft, None);
        assert_eq!(fallback.drafts[0].detail, "stale edit");

        let fields = draft_listing(
            Ok("Music/london/data.orna\tsys.vfs.stale_edit\tdraft-1\tstale edit\n".to_owned()),
            || panic!("the fields xattr is read without the fallback"),
        );
        assert_eq!(fields.source, Some(DraftSource::Fields));
        assert_eq!(fields.drafts[0].code.as_deref(), Some("sys.vfs.stale_edit"));
        assert_eq!(fields.drafts[0].draft.as_deref(), Some("draft-1"));

        let gone = draft_listing(Err(Errno::NODATA), || Err(Errno::NOTCONN));
        assert_eq!(gone.source, None);
        assert!(gone.error.as_deref().is_some_and(|error| error.contains("stale mount record")));
    }

    #[cfg(target_os = "linux")]
    mod owner_reply {
        use super::super::settle_owner_reply;
        use rustix::io::Errno;
        use std::fs;
        use std::path::{Path, PathBuf};

        fn retained_draft(directory: &Path) -> PathBuf {
            let draft = directory.join("orna-edit-1.draft");
            fs::write(&draft, b"candidate\n").unwrap();
            draft
        }

        #[test]
        fn accepted_reply_is_success() {
            let directory = tempfile::tempdir().unwrap();
            let draft = retained_draft(directory.path());
            assert!(settle_owner_reply(Ok(()), &draft).is_ok());
        }

        #[test]
        fn lost_reply_with_draft_retained_is_an_unknown_outcome() {
            let directory = tempfile::tempdir().unwrap();
            let draft = retained_draft(directory.path());
            let diagnostic = settle_owner_reply(Err(Errno::NOTCONN), &draft)
                .expect_err("a retained draft leaves the outcome unknown");
            assert_eq!(diagnostic.code, "E2010");
            assert_eq!(&*diagnostic.title, "Edit outcome is unknown");
            assert!(diagnostic.help.contains("orna status"), "{}", diagnostic.help);
            let detail = diagnostic.detail.unwrap_or_default();
            assert!(detail.contains(&draft.display().to_string()), "{detail}");
        }

        #[test]
        fn lost_reply_after_an_accepted_commit_is_success() {
            // The owner removes the draft when a commit is accepted, before it
            // replies (admit_edit_result). A lost reply then finds no draft.
            let directory = tempfile::tempdir().unwrap();
            let draft = retained_draft(directory.path());
            fs::remove_file(&draft).unwrap();
            assert!(settle_owner_reply(Err(Errno::NOTCONN), &draft).is_ok());
        }

        #[test]
        fn definite_refusals_are_never_reclassified_as_unknown() {
            let directory = tempfile::tempdir().unwrap();
            let draft = retained_draft(directory.path());
            for present in [true, false] {
                if !present {
                    fs::remove_file(&draft).unwrap();
                }
                for errno in [Errno::BUSY, Errno::IO, Errno::INVAL, Errno::NODATA] {
                    let diagnostic = settle_owner_reply(Err(errno), &draft)
                        .expect_err("a definite refusal is an error");
                    assert_eq!(diagnostic.code, "E2000", "{errno}");
                    assert_eq!(&*diagnostic.title, "Edit was not accepted", "{errno}");
                }
            }
        }

        #[test]
        fn stale_reply_is_a_stale_refusal() {
            let directory = tempfile::tempdir().unwrap();
            let draft = retained_draft(directory.path());
            let diagnostic = settle_owner_reply(Err(Errno::STALE), &draft)
                .expect_err("a stale baseline is refused");
            assert_eq!(diagnostic.code, "E2000");
            assert_eq!(&*diagnostic.title, "Edit is stale against the mounted baseline");
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod mountinfo_identity_tests {
    use super::{
        MountedFuseIdentity, dead_mount_still_same, is_releasable_dead_mount,
        mount_identity_in_mountinfo, release_judged_dead_mount,
    };
    use std::path::Path;

    const VIEW: &str = "orna-0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn release_recheck_sees_a_live_mount_that_replaced_the_dead_one() {
        let path = Path::new("/mnt/orna");
        let dead = format!("41 35 0:45 / /mnt/orna rw,nosuid,nodev - fuse.orna-cli {VIEW} rw\n");
        let live = format!("52 35 0:50 / /mnt/orna rw,nosuid,nodev - fuse.orna-cli {VIEW} rw\n");
        let identity = mount_identity_in_mountinfo(&dead, path).expect("dead mount is listed");
        assert_eq!(
            identity,
            MountedFuseIdentity {
                mount_id: 41,
                filesystem: "fuse.orna-cli".to_owned(),
                source: VIEW.to_owned(),
            }
        );
        assert_eq!(mount_identity_in_mountinfo(&dead, path), Some(identity.clone()));
        assert_ne!(mount_identity_in_mountinfo(&live, path), Some(identity));
    }

    #[test]
    fn unrelated_or_absent_mountpoints_have_no_identity() {
        let path = Path::new("/mnt/orna");
        let other = "36 35 98:0 /other /mnt/other rw - ext4 /dev/sda1 rw\n";
        assert_eq!(mount_identity_in_mountinfo(other, path), None);
        assert_eq!(mount_identity_in_mountinfo("", path), None);
    }

    #[test]
    fn replaced_mount_at_the_judged_path_is_not_the_dead_mount() {
        let path = Path::new("/mnt/orna");
        let dead = format!("41 35 0:45 / /mnt/orna rw,nosuid,nodev - fuse.orna-cli {VIEW} rw\n");
        let live = format!("52 35 0:50 / /mnt/orna rw,nosuid,nodev - fuse.orna-cli {VIEW} rw\n");
        let judged = mount_identity_in_mountinfo(&dead, path).expect("dead mount is listed");
        let replacement = mount_identity_in_mountinfo(&live, path);
        assert!(!dead_mount_still_same(&judged, replacement.as_ref()));
        assert!(!dead_mount_still_same(&judged, None));
        assert!(dead_mount_still_same(&judged, Some(&judged)));
    }

    #[test]
    fn only_disconnected_orna_fuse_mounts_are_releasable() {
        let fuse_orna = MountedFuseIdentity {
            mount_id: 41,
            filesystem: "fuse.orna-cli".to_owned(),
            source: VIEW.to_owned(),
        };
        assert!(is_releasable_dead_mount(&fuse_orna, true));
        assert!(!is_releasable_dead_mount(&fuse_orna, false));
        let foreign_source = MountedFuseIdentity {
            source: "/dev/sda1".to_owned(),
            ..fuse_orna.clone()
        };
        assert!(!is_releasable_dead_mount(&foreign_source, true));
        let not_fuse = MountedFuseIdentity {
            filesystem: "ext4".to_owned(),
            ..fuse_orna
        };
        assert!(!is_releasable_dead_mount(&not_fuse, true));
    }

    #[test]
    fn release_runs_only_when_the_recheck_still_matches() {
        let judged = MountedFuseIdentity {
            mount_id: 41,
            filesystem: "fuse.orna-cli".to_owned(),
            source: VIEW.to_owned(),
        };
        let replacement = MountedFuseIdentity {
            mount_id: 52,
            ..judged.clone()
        };
        let mut released = false;
        let outcome = release_judged_dead_mount::<String>(
            &judged,
            || Ok(Some(replacement.clone())),
            || {
                released = true;
                Ok(())
            },
        );
        assert_eq!(outcome, Ok(false));
        assert!(!released, "a replacement mount must never be released");

        let mut released = false;
        let outcome = release_judged_dead_mount::<String>(
            &judged,
            || Ok(Some(judged.clone())),
            || {
                released = true;
                Ok(())
            },
        );
        assert_eq!(outcome, Ok(true));
        assert!(released, "the same dead mount must be released");
    }
}
