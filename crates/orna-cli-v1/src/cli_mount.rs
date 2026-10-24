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
    let view =
        MountView::attach(&repository, mountpoint, commit.as_str()).map_err(attach_refusal)?;
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
        let (edit_path, view) = mounted_edit_view(&repository, path, row)?;
        let edit_path = edit_path.to_str().ok_or_else(|| {
            mount_error("Edit was not accepted", "edit path is not valid UTF-8")
        })?;
        return crate::fuse_mount::edit_path(
            &repository,
            view.commit(),
            view.context(),
            edit_path,
            row,
            Some(view.row_baselines()),
        )
        .map_err(edit_refusal);
    }
    let context = repository
        .open_format_context_at_selector("HEAD")
        .map_err(|error| mount_error("Edit was not accepted", format!("workspace snapshot could not be pinned: {error:?}")))?;
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

#[cfg(target_os = "linux")]
fn mounted_edit_view(
    repository: &Repository,
    path: &Path,
    row: bool,
) -> Result<(std::path::PathBuf, MountView), Diagnostic> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(mount_error(
            "Edit was not accepted",
            "mounted edit path may not contain parent traversal",
        ));
    }
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| mount_error("Mount table could not be read", error.to_string()))?;
    let mut matched: Option<(usize, std::path::PathBuf, bool, String)> = None;
    for line in mountinfo.lines() {
        let Some((mount_fields, filesystem_fields)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mountpoint) = mount_fields.split_whitespace().nth(4) else {
            continue;
        };
        let mut filesystem = filesystem_fields.split_whitespace();
        if !filesystem.next().is_some_and(|filesystem| filesystem.starts_with("fuse")) {
            continue;
        }
        let Some(source) = filesystem.next() else {
            continue;
        };
        let Some(view_id) = source.strip_prefix("orna-").filter(|_| is_orna_mount_source(source)) else {
            continue;
        };
        let read_only = mount_fields
            .split_whitespace()
            .nth(5)
            .is_some_and(|options| options.split(',').any(|option| option == "ro"));
        let mountpoint = std::path::PathBuf::from(std::ffi::OsString::from_vec(
            unescape_mountinfo_path(mountpoint),
        ));
        let Ok(relative) = path.strip_prefix(&mountpoint) else {
            continue;
        };
        let depth = mountpoint.components().count();
        if matched
            .as_ref()
            .is_none_or(|(current_depth, _, _, _)| depth > *current_depth)
        {
            matched = Some((depth, relative.to_path_buf(), read_only, view_id.to_owned()));
        }
    }
    let (_, relative, read_only, view_id) = matched.ok_or_else(|| {
        mount_error(
            "Edit was not accepted",
            "absolute edit path must name an entry inside an active Orna workspace mount",
        )
    })?;
    if read_only {
        return Err(mount_error(
            "Edit was not accepted",
            "the selected workspace mount is read-only",
        ));
    }
    if relative.as_os_str().is_empty() {
        return Err(mount_error(
            "Edit was not accepted",
            "absolute edit path must name an entry inside an active Orna workspace mount",
        ));
    }
    let view = repository
        .open_mount_view_by_id(&view_id)
        .map_err(|error| mount_error("Mounted workspace view could not be reopened", error.to_string()))?;
    Ok((
        if row {
            relative.join("data.orna")
        } else {
            relative
        },
        view,
    ))
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

/// The join loop re-checks the registered view every this many 25 ms ticks.
#[cfg(target_os = "linux")]
const VIEW_CHECK_TICKS: u32 = 8;

/// Stop cause when the registered view disappears under a serving mount.
#[cfg(target_os = "linux")]
const VIEW_DETACHED: &str = "its view was unmounted";

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
                        match crate::fuse_mount::retain_drafts_for_shutdown(&mountpoint) {
                            Ok(true) => end = MountEnd::DraftsRetained,
                            Ok(false) => {}
                            Err(error) => retention_error = Some(error),
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
                        crate::fuse_mount::unmount_mountpoint(&mountpoint)
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
            for line in list.lines() {
                let (path, state) = line.split_once('\t').unwrap_or((line, "unresolved"));
                let _ = writeln!(stderr, "unresolved draft: {path}: {state}");
            }
        }
        Err(error) => {
            let _ = writeln!(stderr, "unresolved drafts could not be listed: {error}");
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
        }
    }
    Ok(())
}

fn human_row(status: &MountStatus) -> String {
    format!(
        "{} snapshot {} format {} legacy {} read-only",
        status.mountpoint().display(),
        status.snapshot_hex(),
        status.repository_format_number(),
        status.is_legacy_format(),
    )
}

fn status_json(status: &MountStatus) -> String {
    mount_row_json(
        status.mountpoint(),
        &status.snapshot_hex(),
        &status.view_hex(),
        status.repository_format_number(),
        status.is_legacy_format(),
    )
}

/// One status object with the documented keys. The mountpoint is JSON-escaped
/// so a path containing a quote, backslash or control character cannot produce
/// invalid output.
fn mount_row_json(
    mountpoint: &Path,
    snapshot_hex: &str,
    view_hex: &str,
    format: u8,
    legacy: bool,
) -> String {
    format!(
        "{{\"mountpoint\":\"{}\",\"snapshot\":\"{}\",\"view\":\"{}\",\"format\":{},\"legacy\":{},\"read_only\":true}}",
        escape_json(&mountpoint.to_string_lossy()),
        snapshot_hex,
        view_hex,
        format,
        legacy,
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

#[cfg(test)]
mod tests {
    use super::{escape_json, mount_row_json};
    use std::path::Path;

    #[test]
    fn status_json_has_exactly_the_documented_keys() {
        let object = mount_row_json(Path::new("/mnt/old"), "aa", "bb", 1, true);
        assert_eq!(
            object,
            "{\"mountpoint\":\"/mnt/old\",\"snapshot\":\"aa\",\"view\":\"bb\",\"format\":1,\"legacy\":true,\"read_only\":true}"
        );
    }

    #[test]
    fn status_json_escapes_a_hostile_mountpoint() {
        let object = mount_row_json(Path::new("/mnt/a\"b\\c"), "aa", "bb", 3, false);
        assert!(object.contains("\\\""), "quote is escaped: {object}");
        assert!(object.contains("\\\\"), "backslash is escaped: {object}");
        assert!(
            !object.contains("a\"b"),
            "raw quote must not appear: {object}"
        );
    }

    #[test]
    fn escape_json_leaves_ordinary_paths_unchanged() {
        assert_eq!(escape_json("/mnt/library"), "/mnt/library");
        assert_eq!(escape_json("tab\there"), "tab\\there");
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
