//! The row projection failures of a mount, as `mount status` reports them
//! (ORNA-VFS-005). The daemon serves them through the `user.orna.row-diagnostics`
//! xattr; this module parses that list and names what could not be read.

use orna_repository::MountStatus;
#[cfg(target_os = "linux")]
use rustix::io::Errno;

#[cfg(target_os = "linux")]
use super::mount_listing_failure;

/// One failed row or table projection of a mount, as `mount status` reports it: the
/// mount path of its node, the exact `sys.*` code when the failure names one, and
/// the failure message (ORNA-VFS-005).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RowDiagnosticEntry {
    pub(super) path: String,
    pub(super) code: Option<String>,
    pub(super) message: String,
}

/// The row projection failures of a mount, and how many the daemon left out of its
/// bounded list. `error` is why they could not be listed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RowDiagnosticListing {
    pub(super) entries: Vec<RowDiagnosticEntry>,
    pub(super) omitted: u64,
    pub(super) error: Option<String>,
}

/// Parses the `path<TAB>code<TAB>message` lines of a row diagnostic list. The
/// `\tomitted\tN` line counts the failures the daemon did not list.
#[cfg(target_os = "linux")]
pub(super) fn parse_row_diagnostics(list: &str) -> RowDiagnosticListing {
    let mut listing = RowDiagnosticListing::default();
    for line in list.lines().filter(|line| !line.is_empty()) {
        if let Some(count) = line.strip_prefix("\tomitted\t") {
            listing.omitted += count.parse::<u64>().unwrap_or(0);
            continue;
        }
        let fields: Vec<&str> = line.splitn(3, '\t').collect();
        listing.entries.push(match fields[..] {
            [path, code, message] => RowDiagnosticEntry {
                path: path.to_owned(),
                code: (!code.is_empty()).then(|| code.to_owned()),
                message: message.to_owned(),
            },
            _ => RowDiagnosticEntry {
                path: line.to_owned(),
                code: None,
                message: "unreadable row diagnostic".to_owned(),
            },
        });
    }
    listing
}

/// The row projection failures of a mount. Every mount serves them, read-only or
/// writable, so they are listed for each one.
#[cfg(target_os = "linux")]
pub(super) fn status_row_diagnostics(status: &MountStatus) -> RowDiagnosticListing {
    match orna_fuse::row_diagnostic_list(status.mountpoint()) {
        Ok(list) => parse_row_diagnostics(&list),
        Err(Errno::NODATA) => RowDiagnosticListing {
            error: Some(
                "this mount's daemon predates row diagnostics; row projection failures are not reported"
                    .to_owned(),
            ),
            ..RowDiagnosticListing::default()
        },
        Err(error) => RowDiagnosticListing {
            error: Some(mount_listing_failure(error, "row diagnostics")),
            ..RowDiagnosticListing::default()
        },
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn status_row_diagnostics(_status: &MountStatus) -> RowDiagnosticListing {
    RowDiagnosticListing {
        error: Some("live row diagnostics require Linux FUSE".to_owned()),
        ..RowDiagnosticListing::default()
    }
}
