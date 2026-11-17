//! Per-node projection diagnostics (ORNA-VFS-005). A row or table whose projection fails is
//! recorded until a later load projects it. The records are served through the root
//! `XATTR_ROW_DIAGNOSTICS` xattr, so `orna mount status` can report the exact failure.

use std::collections::BTreeMap;

/// The most failing nodes recorded at once. A failure beyond this is counted, not stored.
const MAX_RECORDS: usize = 256;
/// The longest message kept, in bytes. A longer message is cut at a character boundary.
const MAX_MESSAGE_BYTES: usize = 512;
/// The byte budget of the rendered value, so the kernel returns it in one read.
const BYTE_BUDGET: usize = 32 * 1024;
/// Room for the final `\tomitted\tN` line (its count is at most 20 digits).
const OMITTED_RESERVE: usize = 32;

/// One failed projection: the mount path of its node, the exact `sys.*` code when the
/// failure names one, and the failure message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowDiagnostic {
    pub(crate) path: String,
    pub(crate) code: Option<&'static str>,
    pub(crate) message: String,
}

/// The failed projections of a mount, keyed by node inode.
#[derive(Debug, Default)]
pub(crate) struct RowDiagnostics {
    records: BTreeMap<u64, RowDiagnostic>,
    /// Failures that could not be stored because the records were full.
    dropped: u64,
}

impl RowDiagnostics {
    /// Records the failure of `inode`, replacing any earlier failure of it.
    pub(crate) fn record(
        &mut self,
        inode: u64,
        path: String,
        code: Option<&'static str>,
        message: &str,
    ) {
        if !self.records.contains_key(&inode) && self.records.len() >= MAX_RECORDS {
            self.dropped += 1;
            return;
        }
        self.records.insert(
            inode,
            RowDiagnostic {
                path,
                code,
                message: bounded_message(message),
            },
        );
    }

    /// Forgets the failure of `inode` once its node projects.
    pub(crate) fn clear(&mut self, inode: u64) {
        self.records.remove(&inode);
    }

    /// Renders the records whose node `is_live` accepts as sorted
    /// `path<TAB>code<TAB>message` lines, one per record, with an empty code when the
    /// failure names none. Lines that do not fit the byte budget are not listed; they
    /// are counted on one final `\tomitted\tN` line, which has an empty first field
    /// that no path can have. `N` also counts the failures that were never stored.
    pub(crate) fn render(&self, is_live: impl Fn(u64) -> bool) -> String {
        let mut lines: Vec<String> = self
            .records
            .iter()
            .filter(|(inode, _)| is_live(**inode))
            .map(|(_, record)| {
                format!(
                    "{}\t{}\t{}",
                    line_field(&record.path),
                    record.code.unwrap_or(""),
                    line_field(&record.message)
                )
            })
            .collect();
        lines.sort();
        let total = lines.len();
        let mut kept = Vec::with_capacity(total);
        let mut bytes = 0_usize;
        for text in lines {
            if bytes + text.len() + 1 + OMITTED_RESERVE > BYTE_BUDGET {
                break;
            }
            bytes += text.len() + 1;
            kept.push(text);
        }
        let omitted = u64::try_from(total - kept.len())
            .unwrap_or(u64::MAX)
            .saturating_add(self.dropped);
        if omitted > 0 {
            kept.push(format!("\tomitted\t{omitted}"));
        }
        kept.join("\n")
    }
}

/// Cuts a message to `MAX_MESSAGE_BYTES` at a character boundary.
fn bounded_message(message: &str) -> String {
    let mut end = message.len().min(MAX_MESSAGE_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

/// Replaces the separators of the line format, so a field cannot split a line.
fn line_field(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_node_is_recorded_with_its_path_code_and_message() {
        let mut records = RowDiagnostics::default();
        records.record(
            7,
            "Music/one".to_owned(),
            None,
            "FUSE row projection failed",
        );
        records.record(
            8,
            "Music/two".to_owned(),
            Some("sys.vfs.stale_edit"),
            "stale",
        );
        assert_eq!(
            records.render(|_| true),
            "Music/one\t\tFUSE row projection failed\nMusic/two\tsys.vfs.stale_edit\tstale"
        );
    }

    #[test]
    fn a_later_successful_projection_clears_the_record() {
        let mut records = RowDiagnostics::default();
        records.record(7, "Music/one".to_owned(), None, "deferred");
        records.clear(7);
        assert_eq!(records.render(|_| true), "");
    }

    #[test]
    fn a_record_replaces_the_earlier_failure_of_its_node() {
        let mut records = RowDiagnostics::default();
        records.record(7, "Music/one".to_owned(), None, "deferred");
        records.record(7, "Music/one".to_owned(), None, "fatal");
        assert_eq!(records.render(|_| true), "Music/one\t\tfatal");
    }

    #[test]
    fn nodes_that_left_the_mount_are_not_reported() {
        let mut records = RowDiagnostics::default();
        records.record(7, "Music/gone".to_owned(), None, "fatal");
        assert_eq!(records.render(|inode| inode != 7), "");
    }

    #[test]
    fn separators_in_fields_and_long_messages_stay_on_one_bounded_line() {
        let mut records = RowDiagnostics::default();
        records.record(
            7,
            "Mu\tsic\none".to_owned(),
            None,
            &format!("bad\r\n{}", "x".repeat(MAX_MESSAGE_BYTES * 2)),
        );
        let rendered = records.render(|_| true);
        assert_eq!(rendered.lines().count(), 1, "{rendered}");
        let fields: Vec<&str> = rendered.split('\t').collect();
        assert_eq!(fields.len(), 3, "{rendered}");
        assert_eq!(fields[0], "Mu sic one");
        assert_eq!(fields[2].len(), MAX_MESSAGE_BYTES);
    }

    #[test]
    fn full_records_count_the_failures_they_could_not_store() {
        let mut records = RowDiagnostics::default();
        for inode in 0..MAX_RECORDS as u64 + 3 {
            records.record(inode, format!("Music/{inode}"), None, "fatal");
        }
        let rendered = records.render(|_| true);
        assert!(rendered.ends_with("\n\tomitted\t3"), "{rendered}");
        assert_eq!(rendered.lines().count(), MAX_RECORDS + 1);
    }

    #[test]
    fn lines_beyond_the_byte_budget_are_omitted_and_counted() {
        let mut records = RowDiagnostics::default();
        for inode in 0..100_u64 {
            records.record(inode, format!("Music/{inode:03}"), None, &"m".repeat(500));
        }
        let rendered = records.render(|_| true);
        let last = rendered.lines().last().expect("a line");
        let omitted: u64 = last
            .strip_prefix("\tomitted\t")
            .expect("omitted line")
            .parse()
            .expect("count");
        assert!(omitted > 0, "{rendered}");
        assert!(rendered.len() <= BYTE_BUDGET, "{}", rendered.len());
        assert_eq!(rendered.lines().count() as u64, 100 - omitted + 1);
    }
}
