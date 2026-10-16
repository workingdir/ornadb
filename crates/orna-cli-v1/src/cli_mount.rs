//! `orna mount DIR --at SELECTOR`, `orna mount status [--json]` and
//! `orna unmount DIR`: the VFS-1 read-only view surface.
//!
//! An `--at` view resolves its selector exactly once and is always read-only
//! (profiles/vfs-1.md). Attaching records the view in the repository runtime,
//! so `mount status` can report a format-1/2 migration reader beside a
//! format-3 workspace view, and every view keeps its own snapshot identity and
//! record. Read-only means this command never writes to the attached snapshot
//! or to the workspace; the only mutation is the mount record, and `unmount`
//! removes exactly that record.

use std::path::Path;
#[cfg(target_os = "linux")]
use std::{fs, os::unix::ffi::OsStrExt, process::Command};

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
        .ok_or_else(|| mount_error("Workspace has no committed HEAD", "publish a commit before mounting"))?;
    let view = MountView::attach(&repository, mountpoint, commit.as_str())
        .map_err(|error| mount_error("Mount was refused", error.to_string()))?;
    let status = match view.verify_record() {
        Ok(status) => status,
        Err(error) => {
            let _ = view.detach();
            return Err(mount_error("Mount record could not be verified", error.to_string()));
        }
    };
    let source = format!("orna-{}", status.view_hex());
    let session = match crate::fuse_mount::spawn_read_only(
        &repository,
        &commit,
        view.context(),
        status.mountpoint(),
        source,
    ) {
        Ok(session) => session,
        Err(error) => {
            let _ = view.detach();
            return Err(mount_error("Mount was refused", error));
        }
    };
    if writeln!(io::stdout().lock(), "{}", human_row(&status)).is_err() {
        let _ = session.umount_and_join();
        let _ = view.detach();
        return Err(stdout_error());
    }
    session
        .join()
        .map_err(|error| mount_error("Mount session ended unexpectedly", error.to_string()))
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
    let mountpoint = fs::canonicalize(mountpoint)
        .map_err(|error| mount_error("Mountpoint could not be resolved", error.to_string()))?;
    #[cfg(not(target_os = "linux"))]
    let mountpoint = mountpoint.to_path_buf();

    #[cfg(target_os = "linux")]
    {
        let statuses = repository
            .mount_status()
            .map_err(|error| mount_error("Mount status could not be read", error.to_string()))?;
        if let Some(view) = statuses.iter().find(|view| view.mountpoint() == mountpoint) {
            if let Some((filesystem, source)) = mounted_source_for_path(&mountpoint)? {
                let expected = format!("orna-{}", view.view_hex());
                if !filesystem.starts_with("fuse") || source != expected {
                    return Err(mount_error(
                        "Unmount was refused",
                        format!("mountpoint belongs to {filesystem} {source}, not {expected}"),
                    ));
                }
                let output = Command::new("fusermount3")
                    .arg("-u")
                    .arg("--")
                    .arg(&mountpoint)
                    .output()
                    .map_err(|error| mount_error("FUSE unmount failed", error.to_string()))?;
                if !output.status.success() {
                    return Err(mount_error(
                        "FUSE unmount failed",
                        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    ));
                }
            }
        }
    }

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
