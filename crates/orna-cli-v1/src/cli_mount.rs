//! `orna mount DIR --at SELECTOR`, `orna mount DIR --cwd`, status and unmount.
//!
//! An `--at` view resolves its selector exactly once and stays read-only. `--cwd`
//! mounts the current committed workspace and sends document and sibling-temp
//! saves through validated durable-row admission. Both modes record the mounted
//! snapshot in the repository runtime, and `unmount` removes exactly that record.

use std::path::Path;
#[cfg(target_os = "linux")]
use std::{fs, os::unix::ffi::OsStrExt};

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

pub(super) fn run_mount(
    endpoint: &Endpoint,
    mountpoint: &Path,
    selector: &str,
) -> Result<(), Diagnostic> {
    let repository = open_repository(endpoint)?;
    let view = MountView::attach(&repository, mountpoint, selector)
        .map_err(|error| mount_error("Mount was refused", error.to_string()))?;
    // The durable record is re-read before reporting success: an attach that
    // cannot be reported is not an attached view.
    let status = view
        .verify_record()
        .map_err(|error| mount_error("Mount record could not be verified", error.to_string()))?;
    writeln!(io::stdout().lock(), "{}", human_row(&status)).map_err(|_| stdout_error())?;
    Ok(())
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
    let view = MountView::attach(&repository, mountpoint, commit.as_str())
        .map_err(|error| mount_error("Mount was refused", error.to_string()))?;
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
    ) {
        Ok(session) => session,
        Err(error) => {
            drop(signal_handler);
            let _ = view.detach();
            return Err(mount_error("Mount was refused", error));
        }
    };
    if writeln!(io::stdout().lock(), "{}", human_row(&status)).is_err() {
        let session_cleanup = session.umount_and_join();
        drop(signal_handler);
        if let Err(error) = session_cleanup {
            return Err(mount_error("FUSE cleanup failed", error.to_string()));
        }
        view.detach()
            .map_err(|error| mount_error("Mount view could not be detached", error.to_string()))?;
        return Err(stdout_error());
    }
    signal_handler
        .join(session, status.mountpoint())
        .map_err(|error| mount_error("Mount session ended unexpectedly", error))?;
    view.detach()
        .map_err(|error| mount_error("Mount view could not be detached", error.to_string()))
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

    fn join(self, session: fuser::BackgroundSession, mountpoint: &Path) -> Result<(), String> {
        let Self {
            runtime,
            mut interrupt,
            mut terminate,
            mut hangup,
        } = self;
        let mountpoint = mountpoint.to_path_buf();
        let mut session = Some(session);
        runtime.block_on(async move {
            let received_signal = loop {
                if session
                    .as_ref()
                    .is_some_and(|session| session.guard.is_finished())
                {
                    break None;
                }
                tokio::select! {
                    Some(()) = interrupt.recv() => break Some("SIGINT"),
                    Some(()) = terminate.recv() => break Some("SIGTERM"),
                    Some(()) = hangup.recv() => break Some("SIGHUP"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
                }
            };

            if let Some(signal) = received_signal {
                let still_mounted = session
                    .as_ref()
                    .is_some_and(|session| !session.guard.is_finished());
                if still_mounted {
                    let unmount_error = crate::fuse_mount::unmount_mountpoint(&mountpoint)
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
                            Ok(()) => Err(error),
                            Err(fallback_error) => Err(format!(
                                "{error}; FUSE session cleanup also failed: {fallback_error}"
                            )),
                        };
                    }
                }
            }

            session
                .take()
                .expect("FUSE session is retained until it is joined")
                .join()
                .map_err(|error| format!("FUSE session join failed: {error}"))
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
        if let Some((filesystem, source)) = mounted_source_for_path(&mountpoint)? {
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
            crate::fuse_mount::unmount_mountpoint(&mountpoint)
                .map_err(|error| mount_error("FUSE unmount failed", error))?;
            if let Some((attached_view, status)) = attached_view {
                attached_view.detach().map_err(|error| {
                    mount_error("Mount view could not be detached", error.to_string())
                })?;
                writeln!(
                    io::stdout().lock(),
                    "unmounted {} snapshot {}",
                    status.mountpoint().display(),
                    status.snapshot_hex(),
                )
                .map_err(|_| stdout_error())?;
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
        let status = statuses
            .into_iter()
            .find(|status| status.mountpoint() == mountpoint)
            .ok_or_else(|| mount_error("Unmount was refused", "no mount view is attached there"))?;
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
        view.detach()
            .map_err(|error| mount_error("Mount view could not be detached", error.to_string()))?;
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
fn mounted_source_for_path(path: &Path) -> Result<Option<(String, String)>, Diagnostic> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| mount_error("Mount table could not be read", error.to_string()))?;
    for line in mountinfo.lines() {
        let Some((mount_fields, filesystem_fields)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mountpoint) = mount_fields.split_whitespace().nth(4) else {
            continue;
        };
        if unescape_mountinfo_path(mountpoint) != path.as_os_str().as_bytes() {
            continue;
        }
        let mut fields = filesystem_fields.split_whitespace();
        let filesystem = fields.next().unwrap_or_default().to_owned();
        let source = fields.next().unwrap_or_default().to_owned();
        return Ok(Some((filesystem, source)));
    }
    Ok(None)
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
