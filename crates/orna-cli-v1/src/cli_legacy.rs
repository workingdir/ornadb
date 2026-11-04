//! Snapshot admission and legacy (format-1/2) addressing for `orna query` and
//! `orna history`.
//!
//! Both commands read one committed snapshot. Which snapshot it is comes from
//! `--at` or from `HEAD`, and the read is answered either by the native
//! format-3 store or, for a pin whose format context reports a read-only
//! compatibility input, by the versioned legacy reader
//! (`orna_application_v1::read_legacy_table_rows`; ORNA-UPGRADE-001,
//! ORNA-UPGRADE-002, ORNA-ROW-013).
//!
//! Neither path may answer a snapshot project admission refuses. Format
//! admission is the part of project admission that decides whether a snapshot
//! is readable at all (ORNA-META-001, ORNA-META-002, ORNA-UPGRADE-003), and it
//! is defined over the loaded project: the source modules decide which
//! directories are tables, and a format-3 snapshot that tracks a loose row file
//! under one of them is the mixed layout `ORNA-REPO-CONTEXT-004` names. Reading
//! the store first would answer such a snapshot from rows whose row files the
//! snapshot also carries, so both commands admit the pin before they read it.
//!
//! The admission is one project load per command. For a format-3 snapshot that
//! load is the added cost of the read, and it is the price of the rule above:
//! there is no cheaper boundary that decides what project admission admits. A
//! project the loader refuses for a *source* reason (a parse error, an
//! unresolvable import) is not refused here: neither command reads the source,
//! and source admission is what `orna check` and `orna run` report.

use orna_application_v1::{LegacyReadError, LegacyTableAddress};
use orna_project_v1::{ProjectLoadError, ProjectLoader};
use orna_repository_v1::{FormatContextError, GitCommitRef, Repository};

use super::{Cow, Diagnostic, Exit};

/// Report style both commands use for a snapshot whose format admission failed.
const ADMISSION_HELP: &str = "a format-3 snapshot tracks only `.orna/database.orna` and `.orna/store`, and rows change through `orna publish`; commit a snapshot without the named path (`git rm --cached PATH`, then commit), then retry";

/// Admits one pinned snapshot through the project loader's format admission.
///
/// Only the format decision is propagated. A snapshot that tracks a path its
/// recorded format does not admit fails here with its own code
/// (ORNA-REPO-CONTEXT-004 for a mixed layout) and the path in the message,
/// before either command reads a row.
pub(super) fn admit_snapshot(
    repository: &Repository,
    commit: Option<&GitCommitRef>,
) -> Result<(), Diagnostic> {
    let Some(commit) = commit else {
        // An unborn branch has no committed snapshot to admit; the read that
        // follows reports its own missing-snapshot failure.
        return Ok(());
    };
    match ProjectLoader::default().load_committed_snapshot(repository, commit) {
        // Format admission's own diagnosis keeps its code and message: the code
        // is ORNA-REPO-CONTEXT-004 for a mixed layout, and the message names the
        // path that mixes it.
        Err(ProjectLoadError::FormatContext(refusal)) => Err(mixed_formats(&refusal)),
        // A source the loader refuses is not what this read is answered from.
        _ => Ok(()),
    }
}

/// A snapshot this build cannot read as one format: the repository's
/// mixed-format diagnosis, with the path it names and a remedy that is true for
/// a committed snapshot.
fn mixed_formats(refusal: &FormatContextError) -> Diagnostic {
    Diagnostic {
        code: refusal.code(),
        title: Cow::Owned(refusal.to_string()),
        help: ADMISSION_HELP,
        exit: Exit::Target,
        detail: None,
        body: None,
    }
}

/// The table address a relation argument names in a legacy snapshot.
///
/// `RELATION` is a 32-digit hexadecimal relation id, the format-3 spelling both
/// commands document. A legacy snapshot has two committed table addresses and
/// no third: a compact table's storage table id (that same hexadecimal form) and
/// an editable table's key-path directory. So a non-hexadecimal argument is read
/// as that directory, repository-relative, and is validated against the
/// snapshot's own committed row paths rather than against a second name space.
pub(super) fn legacy_address(
    value: &str,
    legacy_help: &'static str,
) -> Result<LegacyTableAddress, Diagnostic> {
    if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let mut bytes = [0_u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .expect("hexadecimal digits were checked above");
        }
        return Ok(LegacyTableAddress::Relation(bytes));
    }
    let mut components = value.split('/');
    let safe = !value.is_empty()
        && !value.starts_with('/')
        && !value.starts_with(".orna")
        && components.all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.contains('\\')
        });
    if !safe {
        return Err(Diagnostic::target_with_detail(
            "E2000",
            "Legacy table path is not a key-path directory",
            legacy_help,
            format!(
                "got {value:?}; a legacy table is addressed by its storage table id or by its key-path directory, such as `contacts/Contact`"
            ),
        ));
    }
    Ok(LegacyTableAddress::TablePath(value.to_owned()))
}

/// The command's own refusal for a legacy read that could not answer.
///
/// The error's own message is the title: it names the table or row unit the
/// snapshot does not hold, and an attached detail renders only with `--debug`.
pub(super) fn legacy_read_error(error: &LegacyReadError, help: &'static str) -> Diagnostic {
    Diagnostic {
        code: "E2000",
        title: Cow::Owned(error.to_string()),
        help,
        exit: Exit::Target,
        detail: None,
        body: None,
    }
}
