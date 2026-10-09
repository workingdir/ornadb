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

use super::*;
use orna_repository_v1::{MountStatus, MountView, Repository};

fn mount_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna mount DIR --at SELECTOR` inside an initialized repository",
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

pub(super) fn run_unmount(endpoint: &Endpoint, mountpoint: &Path) -> Result<(), Diagnostic> {
    let repository = open_repository(endpoint)?;
    let status = repository
        .unmount_view(mountpoint)
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
