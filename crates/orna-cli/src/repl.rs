use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::io::{self, BufRead, Write};
use std::time::Instant;

use crate::repl_commands::{ConsoleState, run as run_console_command};
use crate::repl_inspect::{DISPLAY, render};
use orna_evaluator::{RelationDisplay, RelationWindow, ReplValue};
use orna_live::live_presentation::{LivePresentationUpdate, ResyncRequest, WatchPresentation};
use orna_project_repl::{AdmittedReplSession, ReplDetail, ReplError, ReplOutcome};
use orna_protocol::{Envelope, Limits, Message, PresentNode};
use orna_value::{CanonicalSnapshot, Raw, canonical_uuid_text};

const MAX_INPUT_BYTES: usize = 65_536;
const REPL_HELP: &str = "commands: :help [name], :at CWD|HEAD|ref, :watch expression, :unwatch [id], :type expression, :inspect expression, :members expression, :table expression, :history [query], :timing on|off, :clear, :quit";

type ReplSubmissionHandler<'a> =
    dyn FnMut(&mut AdmittedReplSession, &str) -> Result<ReplOutcome, ReplFailure> + 'a;

#[derive(Clone, Copy)]
pub(super) enum AnsiColor {
    Green,
    Yellow,
    Red,
    Cyan,
    Dim,
}

impl AnsiColor {
    const fn sequence(self) -> &'static [u8] {
        match self {
            Self::Green => b"\x1b[32m",
            Self::Yellow => b"\x1b[33m",
            Self::Red => b"\x1b[31m",
            Self::Cyan => b"\x1b[36m",
            Self::Dim => b"\x1b[2m",
        }
    }
}

pub(super) fn write_styled<W: io::Write>(
    writer: &mut W,
    color: AnsiColor,
    text: &[u8],
    enabled: bool,
) -> io::Result<()> {
    if enabled {
        writer.write_all(color.sequence())?;
    }
    writer.write_all(text)?;
    if enabled {
        writer.write_all(b"\x1b[0m")?;
    }
    Ok(())
}

pub(super) fn write_styled_display<W: io::Write, T: std::fmt::Display>(
    writer: &mut W,
    color: AnsiColor,
    value: T,
    enabled: bool,
) -> io::Result<()> {
    if enabled {
        writer.write_all(color.sequence())?;
    }
    write!(writer, "{value}")?;
    if enabled {
        writer.write_all(b"\x1b[0m")?;
    }
    Ok(())
}

pub(crate) enum ReadSubmission {
    Eof,
    TooLong,
    InvalidUtf8,
    Source(String),
}

/// A repository-neutral selector for a replacement REPL snapshot context.
///
/// `Ref` deliberately retains only the user-supplied reference spelling. The
/// adapter that implements [`SnapshotSessionLoader`] is responsible for
/// resolving it read-only and for constructing a fully admitted replacement
/// session from that immutable snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotTarget {
    Cwd,
    Head,
    Ref(String),
}

/// Prepares a complete replacement session for a read-only `:at` selection.
///
/// The loader must not mutate repository HEAD, the index, the worktree, or
/// refs. It must return an *owned*, fully admitted session only after source
/// loading and admission succeed. `run_with_loader_and_submitter` leaves the
/// current session unchanged for malformed selectors and loader errors, then
/// replaces the whole session on success; the replacement therefore cannot
/// retain the previous snapshot's REPL overlay, `$_`, or `$?`.
///
/// The CLI integration adapter belongs in `main.rs`: it should map `Cwd` to
/// the current project loader and `Head`/`Ref` to read-only committed snapshot
/// resolution and `ProjectLoader::load_committed_snapshot`.
pub trait SnapshotSessionLoader {
    type Error;

    fn load_snapshot(&self, target: SnapshotTarget) -> Result<AdmittedReplSession, Self::Error>;
}

/// A host-owned subscription source yielding complete encoded live frames.
/// The host sends the Watch request and supplies its negotiated presentation;
/// the REPL neither opens a connection nor encodes protocol messages. The
/// session is the environment the watch expression resolves in, the same one
/// the console evaluates in (Orna-1.1.0 17-repl).
pub trait WatchFrameSource {
    type Error;

    fn start_watch(
        &mut self,
        source: &str,
        session: &AdmittedReplSession,
    ) -> Result<WatchPresentation, Self::Error>;
    fn next_frame(&mut self, watch: [u8; 16]) -> Result<Option<Vec<u8>>, Self::Error>;
    fn request_resync(
        &mut self,
        request: ResyncRequest,
        session: &AdmittedReplSession,
    ) -> Result<(), Self::Error>;
    fn close_watch(&mut self, watch: [u8; 16]) -> Result<(), Self::Error>;
    /// Re-evaluates the subscription once at a prompt boundary and retains a
    /// frame when the observation changed. A source without a scheduler does
    /// nothing.
    fn poll(&mut self, watch: [u8; 16], session: &AdmittedReplSession) -> Result<(), Self::Error>;
}

/// Observable state for the currently subscribed REPL watch.
#[derive(Default)]
pub struct WatchCommandState {
    presentation: Option<WatchPresentation>,
}

/// Accessors that only `tests/watch_command.rs` reads. It includes this module
/// by path, so the binary's own test build never calls them.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "read by tests/watch_command.rs, which includes this module by path"
)]
impl WatchCommandState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn published_revision(&self) -> Option<u64> {
        self.presentation
            .as_ref()?
            .published()
            .map(orna_live::live_presentation::PublishedPresentation::revision)
    }

    pub fn awaiting_snapshot(&self) -> bool {
        self.presentation
            .as_ref()
            .is_some_and(WatchPresentation::awaiting_snapshot)
    }

    pub fn begin_resubscription(&mut self) {
        if let Some(presentation) = &mut self.presentation {
            presentation.begin_resubscription();
        }
    }
}

impl WatchCommandState {
    pub fn watch(&self) -> Option<[u8; 16]> {
        self.presentation.as_ref().map(WatchPresentation::watch)
    }

    /// Remains available until the host has successfully sent the resync.
    pub fn take_resync_request(&self) -> Option<ResyncRequest> {
        self.presentation.as_ref()?.take_resync_request()
    }

    pub fn acknowledge_resync_request(&mut self, request: ResyncRequest) {
        if let Some(presentation) = &mut self.presentation {
            presentation.acknowledge_resync_request(request);
        }
    }

    /// Retain the authoritative presentation when the same watch is
    /// re-subscribed, including its complete-snapshot barrier.
    fn install(&mut self, presentation: WatchPresentation) -> Option<[u8; 16]> {
        if self
            .presentation
            .as_ref()
            .is_some_and(|current| current.watch() == presentation.watch())
        {
            return None;
        }
        self.presentation
            .replace(presentation)
            .map(|previous| previous.watch())
    }

    fn close(&mut self) -> Option<[u8; 16]> {
        self.presentation
            .take()
            .map(|presentation| presentation.watch())
    }
}

struct WatchBinding<'a, S: ?Sized> {
    source: &'a mut S,
    state: &'a mut WatchCommandState,
}

/// The most watches one REPL host keeps. The console owns one subscription at
/// a time; a host that fails to close one must not grow without bound.
const MAX_REPL_WATCHES: usize = 64;

/// Why the production REPL watch host could not serve a `:watch` request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplWatchError {
    /// The host has no repository identity to pin an observation to.
    Unavailable,
    /// The source was not a read-only, externally effect-free expression.
    Rejected,
    /// The observation could not be encoded within the protocol limits.
    FrameRejected,
}

/// One watch the host observes, independent of its client presentation.
struct ObservedWatch {
    source: String,
    present: PresentNode,
    revision: u64,
    pending: VecDeque<Vec<u8>>,
}

/// The production `:watch` host for `orna repl`.
///
/// The watch expression is evaluated in the REPL session, the environment the
/// console uses, and only a read-only, externally effect-free expression is
/// admitted (30-protocol "eval and watch sources"). Observations are delivered
/// as complete Present snapshots through the same live presentation machinery
/// page sessions use (ORNA-REPL-006). This build has no dependency scheduler,
/// so the host re-evaluates once per prompt and on every resync, and a changed
/// tree advances the watch revision; a complete snapshot is the universal
/// fallback ORNA-WIRE-001 requires.
pub struct ReplWatchHost {
    identity: Option<([u8; 16], [u8; 16])>,
    limits: Limits,
    watches: BTreeMap<[u8; 16], ObservedWatch>,
}

impl ReplWatchHost {
    /// Builds the host for a session's database and repository identities.
    /// Without them the console stays usable but `:watch` is refused, because
    /// a presentation update must name the runtime it observes.
    #[must_use]
    pub fn new(identity: Option<([u8; 16], [u8; 16])>, limits: Limits) -> Self {
        Self {
            identity,
            limits,
            watches: BTreeMap::new(),
        }
    }
}

/// Encodes one complete Present snapshot for `watch`.
fn watch_snapshot(
    identity: Option<([u8; 16], [u8; 16])>,
    limits: Limits,
    watch: [u8; 16],
    observed: &ObservedWatch,
) -> Result<Vec<u8>, ReplWatchError> {
    let (database, repository) = identity.ok_or(ReplWatchError::Unavailable)?;
    let mut request = [0; 16];
    getrandom::fill(&mut request).map_err(|_| ReplWatchError::Unavailable)?;
    let snapshot = CanonicalSnapshot::cwd(database, repository, 0.into())
        .map_err(|_| ReplWatchError::FrameRejected)?;
    Envelope {
        request: Some(request),
        watch: Some(watch),
        message: Message::Snapshot {
            revision: observed.revision,
            present: observed.present.clone(),
            snapshot,
        },
        extensions: BTreeMap::new(),
    }
    .encode(limits)
    .map_err(|_| ReplWatchError::FrameRejected)
}

/// Evaluates one observation and queues a complete snapshot when the visible
/// tree changed, or when `force` requires the current tree regardless.
fn observe_watch(
    identity: Option<([u8; 16], [u8; 16])>,
    limits: Limits,
    watch: [u8; 16],
    observed: &mut ObservedWatch,
    session: &AdmittedReplSession,
    force: bool,
) -> Result<(), ReplWatchError> {
    let value = session
        .preview(&observed.source)
        .map_err(|_| ReplWatchError::Rejected)?;
    let present = PresentNode::from_value(value).map_err(|_| ReplWatchError::Rejected)?;
    if present == observed.present {
        if !force {
            return Ok(());
        }
    } else {
        observed.present = present;
        observed.revision = observed
            .revision
            .checked_add(1)
            .ok_or(ReplWatchError::FrameRejected)?;
    }
    let frame = watch_snapshot(identity, limits, watch, observed)?;
    observed.pending.push_back(frame);
    Ok(())
}

impl WatchFrameSource for ReplWatchHost {
    type Error = ReplWatchError;

    fn start_watch(
        &mut self,
        source: &str,
        session: &AdmittedReplSession,
    ) -> Result<WatchPresentation, Self::Error> {
        let identity = self.identity.ok_or(ReplWatchError::Unavailable)?;
        if self.watches.len() >= MAX_REPL_WATCHES {
            return Err(ReplWatchError::Rejected);
        }
        let value = session
            .preview(source)
            .map_err(|_| ReplWatchError::Rejected)?;
        let present = PresentNode::from_value(value).map_err(|_| ReplWatchError::Rejected)?;
        let mut watch = [0; 16];
        getrandom::fill(&mut watch).map_err(|_| ReplWatchError::Unavailable)?;
        if self.watches.contains_key(&watch) {
            return Err(ReplWatchError::Unavailable);
        }
        let mut observed = ObservedWatch {
            source: source.to_owned(),
            present,
            revision: 0,
            pending: VecDeque::new(),
        };
        let frame = watch_snapshot(Some(identity), self.limits, watch, &observed)?;
        observed.pending.push_back(frame);
        self.watches.insert(watch, observed);
        WatchPresentation::new(watch, self.limits).map_err(|_| ReplWatchError::FrameRejected)
    }

    fn next_frame(&mut self, watch: [u8; 16]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self
            .watches
            .get_mut(&watch)
            .and_then(|observed| observed.pending.pop_front()))
    }

    fn request_resync(
        &mut self,
        request: ResyncRequest,
        session: &AdmittedReplSession,
    ) -> Result<(), Self::Error> {
        let watch = request.watch();
        let identity = self.identity;
        let limits = self.limits;
        let Some(observed) = self.watches.get_mut(&watch) else {
            return Err(ReplWatchError::Unavailable);
        };
        observe_watch(identity, limits, watch, observed, session, true)
    }

    fn close_watch(&mut self, watch: [u8; 16]) -> Result<(), Self::Error> {
        self.watches.remove(&watch);
        Ok(())
    }

    fn poll(&mut self, watch: [u8; 16], session: &AdmittedReplSession) -> Result<(), Self::Error> {
        let identity = self.identity;
        let limits = self.limits;
        let Some(observed) = self.watches.get_mut(&watch) else {
            return Ok(());
        };
        observe_watch(identity, limits, watch, observed, session, false)
    }
}

fn repl_error_documentation(code: &str) -> (&'static str, &'static str) {
    match code {
        "ORNA-REPL-INPUT-LIMIT" => (
            "submission is longer than the REPL input limit",
            "shorten the submission and enter it again",
        ),
        "ORNA-REPL-INPUT-UTF8" => (
            "submission is not valid UTF-8 text",
            "re-enter the submission using valid UTF-8",
        ),
        "ORNA-REPL-COMMAND" => (
            "command is unknown or unavailable in this REPL",
            "enter `:help` to see the supported commands",
        ),
        "ORNA-REPL-AT" => (
            "requested project snapshot could not be loaded",
            "choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`",
        ),
        "ORNA-REPL-EFFECT" => (
            "REPL preview cannot perform an effect",
            "evaluate a pure expression or invoke the operation through its admitted runtime entry point",
        ),
        "ORNA-REPL-WATCH-FRAME" => (
            "live update could not be read",
            "start the watch again with `:watch expression`",
        ),
        "ORNA-REPL-WATCH-SOURCE" => (
            "live update source is unavailable",
            "start the watch again with `:watch expression`",
        ),
        "ORNA-S010-IMPORT" => (
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        ),
        "ORNA-S012-UNRESOLVED" => (
            "name could not be resolved",
            "declare the name or add the matching `use` import before using it",
        ),
        "ORNA-S021-TYPE" => (
            "expression has the wrong type",
            "change the expression or its declared type so the value and requirement agree",
        ),
        _ => (
            "expression could not be evaluated",
            "review the expression and try again",
        ),
    }
}

/// A failed submission: the stable code, plus the evaluator's located detail
/// (message, source span, notes and remedies) when the evaluator has one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReplFailure {
    pub(crate) code: String,
    pub(crate) detail: Option<ReplDetail>,
}

impl ReplFailure {
    pub(crate) fn code(code: &str) -> Self {
        Self {
            code: code.to_owned(),
            detail: None,
        }
    }

    /// A failure under `code` whose detail names what is wrong and the
    /// remedies to apply, without pointing at source: the host raised it, not
    /// the evaluator.
    pub(crate) fn located(code: &str, message: impl Into<String>, help: Vec<String>) -> Self {
        Self {
            code: code.to_owned(),
            detail: Some(ReplDetail {
                code: code.to_owned(),
                message: message.into(),
                span: None,
                notes: Vec::new(),
                help,
            }),
        }
    }
}

impl From<String> for ReplFailure {
    fn from(code: String) -> Self {
        Self { code, detail: None }
    }
}

impl From<ReplError> for ReplFailure {
    fn from(error: ReplError) -> Self {
        Self {
            code: error.code().to_owned(),
            detail: error.detail().cloned(),
        }
    }
}

/// Byte ranges of string literal source, quotes included. Interpolated
/// strings contribute their whole region, so interpolation code between the
/// quotes is covered too. Returns `None` when the source does not lex, so no
/// line can be shown without knowing whether it holds literal text.
fn string_literal_ranges(source: &str) -> Option<Vec<std::ops::Range<usize>>> {
    let tokens = orna_syntax::lex(source).ok()?;
    let mut ranges = Vec::new();
    let mut open = None;
    for token in &tokens {
        match token.kind {
            orna_syntax::TokenKind::String => ranges.push(token.span.start..token.span.end),
            orna_syntax::TokenKind::StringStart => open = Some(token.span.start),
            orna_syntax::TokenKind::StringEnd => {
                if let Some(start) = open.take() {
                    ranges.push(start..token.span.end);
                }
            }
            _ => {}
        }
    }
    Some(ranges)
}

/// Locates a span as `line N, column M`, then the source line with carets
/// under the span. A line overlapping string literal source is withheld,
/// because submitted text may be sensitive; the location and carets remain.
/// Returns `None` when the span is not on character boundaries of `source`.
fn render_span(source: &str, span: &std::ops::Range<usize>) -> Option<String> {
    let start = span.start;
    if start > source.len() || !source.is_char_boundary(start) {
        return None;
    }
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |index| start + index);
    let line_number = source[..start].matches('\n').count() + 1;
    let column = source[line_start..start].chars().count() + 1;
    let end = span.end.clamp(start, line_end);
    let width = source
        .get(start..end)
        .map_or(1, |text| text.chars().count().max(1));
    let gutter = line_number.to_string();
    let pad = " ".repeat(gutter.len());
    let withheld = match string_literal_ranges(source) {
        None => Some("<source withheld: submission does not tokenize>"),
        Some(ranges)
            if ranges
                .iter()
                .any(|range| range.start < line_end && range.end > line_start) =>
        {
            Some("<source withheld: line contains a string literal>")
        }
        Some(_) => None,
    };
    let line = withheld.unwrap_or(&source[line_start..line_end]);
    Some(format!(
        "  --> line {line_number}, column {column}\n{pad} |\n{gutter} | {line}\n{pad} | {}{}",
        " ".repeat(column - 1),
        "^".repeat(width),
    ))
}

/// Renders a failed submission for the person who typed it. The message
/// leads, followed by the location, notes and remedies. A failure without
/// located detail uses the fixed text for its code.
pub(crate) fn render_repl_failure(failure: &ReplFailure, source: &str) -> String {
    let Some(detail) = &failure.detail else {
        let (title, help) = repl_error_documentation(&failure.code);
        return format!("error[{}]: {title}\nhelp: {help}", failure.code);
    };
    let mut text = format!("error[{}]: {}", detail.code, detail.message);
    if let Some(span) = &detail.span
        && let Some(location) = render_span(source, span)
    {
        text.push('\n');
        text.push_str(&location);
    }
    for note in &detail.notes {
        let _ = write!(text, "\nnote: {note}");
    }
    if detail.help.is_empty() {
        let (_, help) = repl_error_documentation(&failure.code);
        let _ = write!(text, "\nhelp: {help}");
    }
    for help in &detail.help {
        let _ = write!(text, "\nhelp: {help}");
    }
    text
}

/// Writes rendered error text with the header line in red and the rest plain.
pub(crate) fn write_rendered_error<W: Write>(
    writer: &mut W,
    text: &str,
    color_enabled: bool,
) -> io::Result<()> {
    let (header, rest) = text.split_once('\n').unwrap_or((text, ""));
    write_styled(writer, AnsiColor::Red, header.as_bytes(), color_enabled)?;
    if !rest.is_empty() {
        writer.write_all(b"\n")?;
        writer.write_all(rest.as_bytes())?;
    }
    writer.write_all(b"\n")
}

pub(crate) fn write_repl_failure<W: Write>(
    writer: &mut W,
    failure: &ReplFailure,
    source: &str,
    color_enabled: bool,
) -> io::Result<()> {
    write_rendered_error(writer, &render_repl_failure(failure, source), color_enabled)
}

pub(crate) fn write_repl_error<W: Write>(
    writer: &mut W,
    code: &str,
    color_enabled: bool,
) -> io::Result<()> {
    write_repl_failure(writer, &ReplFailure::code(code), "", color_enabled)
}

/// Writes one submission's outcome: a value as `BODY : TYPE` with its static
/// type, a relation as its bounded row window. An input without a value
/// prints nothing.
pub(crate) fn write_repl_outcome<W: Write>(
    writer: &mut W,
    outcome: &ReplOutcome,
    source: &str,
    color_enabled: bool,
) -> io::Result<()> {
    let ty = outcome.ty.as_ref().map(ToString::to_string);
    match &outcome.value {
        None => Ok(()),
        Some(ReplValue::Value(value)) => {
            let (body, runtime) = render(value.raw(), 0, DISPLAY);
            write_result_line(
                writer,
                &body,
                ty.as_deref().unwrap_or(runtime),
                color_enabled,
            )
        }
        Some(ReplValue::Relation(window)) => {
            write_relation_window(writer, window, source, ty.as_deref(), color_enabled)
        }
    }
}

/// Writes `BODY : LABEL`: the result green, the type dim.
pub(crate) fn write_result_line<W: Write>(
    writer: &mut W,
    body: &str,
    label: &str,
    color_enabled: bool,
) -> io::Result<()> {
    write_styled(writer, AnsiColor::Green, body.as_bytes(), color_enabled)?;
    writer.write_all(b" : ")?;
    write_styled(writer, AnsiColor::Dim, label.as_bytes(), color_enabled)?;
    writer.write_all(b"\n")
}

/// Writes a relation window as its header, `SOURCE : Relation<T> · N rows · CWD`,
/// then a table of its rows with one column per record field.
fn write_relation_window<W: Write>(
    writer: &mut W,
    window: &RelationWindow,
    source: &str,
    ty: Option<&str>,
    color_enabled: bool,
) -> io::Result<()> {
    let count = window.rows.len();
    let extent = if window.has_more {
        format!("first {count} rows")
    } else {
        format!("{count} rows")
    };
    write_styled(
        writer,
        AnsiColor::Green,
        source_label(source).as_bytes(),
        color_enabled,
    )?;
    writer.write_all(b" : ")?;
    let label = format!("{} · {extent} · CWD", ty.unwrap_or("Relation"));
    write_styled(writer, AnsiColor::Dim, label.as_bytes(), color_enabled)?;
    writer.write_all(b"\n")?;

    let rows: Vec<Vec<(String, String)>> =
        window.rows.iter().map(|row| row_cells(row.raw())).collect();
    let mut columns: Vec<String> = Vec::new();
    for (name, _) in rows.iter().flatten() {
        if !columns.contains(name) {
            columns.push(name.clone());
        }
    }
    if columns.is_empty() {
        return Ok(());
    }
    let grid: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| {
                    row.iter()
                        .find(|(name, _)| name == column)
                        .map_or_else(String::new, |(_, cell)| cell.clone())
                })
                .collect()
        })
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            grid.iter()
                .map(|row| row[index].chars().count())
                .chain(std::iter::once(column.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    write_table_row(writer, &columns, &widths, true, color_enabled)?;
    for row in &grid {
        write_table_row(writer, row, &widths, false, color_enabled)?;
    }
    Ok(())
}

/// Writes one table row with each cell padded to its column width; the
/// header row is dim.
fn write_table_row<W: Write>(
    writer: &mut W,
    cells: &[String],
    widths: &[usize],
    header: bool,
    color_enabled: bool,
) -> io::Result<()> {
    let mut line = String::new();
    for (index, (cell, width)) in cells.iter().zip(widths).enumerate() {
        if index > 0 {
            line.push_str("  ");
        }
        line.push_str(cell);
        if index + 1 < cells.len() {
            line.push_str(&" ".repeat(width.saturating_sub(cell.chars().count())));
        }
    }
    if header {
        write_styled(writer, AnsiColor::Dim, line.as_bytes(), color_enabled)?;
    } else {
        writer.write_all(line.as_bytes())?;
    }
    writer.write_all(b"\n")
}

/// The `(column, cell)` pairs of one relation row: a record's fields in
/// order, or one `value` cell for a row that is not a record.
fn row_cells(raw: &Raw) -> Vec<(String, String)> {
    match raw {
        Raw::Map(entries) => entries
            .iter()
            .map(|(key, value)| (column_name(key), render(value, 1, DISPLAY).0))
            .collect(),
        other => vec![("value".to_owned(), render(other, 1, DISPLAY).0)],
    }
}

fn column_name(key: &Raw) -> String {
    match key {
        Raw::Text(text) => text.clone(),
        other => render(other, 1, DISPLAY).0,
    }
}

/// The relation's source text as a single header line.
fn source_label(source: &str) -> String {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Writes one `RESULT : TYPE` rendering: the result green, the type dim.
pub(crate) fn write_rendered_value<W: Write>(
    writer: &mut W,
    rendered: &str,
    color_enabled: bool,
) -> io::Result<()> {
    let (result, ty) = rendered.rsplit_once(" : ").unwrap_or((rendered, ""));
    write_styled(writer, AnsiColor::Green, result.as_bytes(), color_enabled)?;
    if !ty.is_empty() {
        writer.write_all(b" : ")?;
        write_styled(writer, AnsiColor::Dim, ty.as_bytes(), color_enabled)?;
    }
    writer.write_all(b"\n")
}

/// Run one retained, line-oriented admitted REPL session. A malformed or
/// rejected submission reports its redacted evaluator code and leaves the
/// session open. The binary always runs with a snapshot loader and a
/// submitter; this bare form exists for the tests of the line loop.
#[cfg(test)]
pub fn run<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    session: &mut AdmittedReplSession,
) -> io::Result<()> {
    run_loop(
        reader,
        writer,
        session,
        None::<&dyn SnapshotSessionLoader<Error = ()>>,
        None::<WatchBinding<'_, dyn WatchFrameSource<Error = ()>>>,
        false,
        None,
    )
}

/// Run an admitted REPL session with an atomic, loader-backed `:at` command.
///
/// A successful `:at CWD|HEAD|ref` installs the owned candidate returned by
/// `loader`. Failed preparation and malformed selectors report
/// `ORNA-REPL-AT` and retain the prior session unchanged.
#[cfg(test)]
pub fn run_with_snapshot_loader<R: BufRead, W: Write, L: SnapshotSessionLoader + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    session: &mut AdmittedReplSession,
    loader: &L,
) -> io::Result<()> {
    run_loop(
        reader,
        writer,
        session,
        Some(loader),
        None::<WatchBinding<'_, dyn WatchFrameSource<Error = ()>>>,
        false,
        None,
    )
}

/// Run an admitted REPL with a host-provided submission adapter and the
/// production `:watch` host. The adapter is discarded after successful
/// snapshot replacement because its transaction authority is bound to the
/// initial committed source snapshot.
pub fn run_with_loader_and_submitter<
    R: BufRead,
    W: Write,
    L: SnapshotSessionLoader + ?Sized,
    S: FnMut(&mut AdmittedReplSession, &str) -> Result<ReplOutcome, ReplFailure>,
    F: WatchFrameSource + ?Sized,
>(
    reader: &mut R,
    writer: &mut W,
    session: &mut AdmittedReplSession,
    loader: &L,
    submitter: &mut S,
    watch: (&mut F, &mut WatchCommandState),
    color_enabled: bool,
) -> io::Result<()> {
    let submitter: &mut ReplSubmissionHandler<'_> = submitter;
    let (source, state) = watch;
    run_loop(
        reader,
        writer,
        session,
        Some(loader),
        Some(WatchBinding { source, state }),
        color_enabled,
        Some(submitter),
    )
}

/// Run with a host-owned live watch frame source. The REPL receives complete
/// encoded frames, while the host owns the subscription and resync transport.
/// The production host is [`ReplWatchHost`]; this entry point lets tests drive
/// the watch command with a scripted source.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "driven by tests/watch_command.rs, which includes this module by path"
)]
pub fn run_with_watch<R: BufRead, W: Write, S: WatchFrameSource + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    session: &mut AdmittedReplSession,
    source: &mut S,
    state: &mut WatchCommandState,
) -> io::Result<()> {
    run_loop(
        reader,
        writer,
        session,
        None::<&dyn SnapshotSessionLoader<Error = ()>>,
        Some(WatchBinding { source, state }),
        false,
        None,
    )
}

fn run_loop<
    R: BufRead,
    W: Write,
    L: SnapshotSessionLoader + ?Sized,
    S: WatchFrameSource + ?Sized,
>(
    reader: &mut R,
    writer: &mut W,
    session: &mut AdmittedReplSession,
    loader: Option<&L>,
    mut watch: Option<WatchBinding<'_, S>>,
    color_enabled: bool,
    mut submitter: Option<&mut ReplSubmissionHandler<'_>>,
) -> io::Result<()> {
    let mut console = ConsoleState::default();
    loop {
        pump_watch_frames(&mut watch, session, writer, color_enabled)?;
        write_styled(writer, AnsiColor::Dim, b"> ", color_enabled)?;
        writer.flush()?;
        match read_submission(reader)? {
            ReadSubmission::Eof => {
                close_session_watch(&mut watch, writer, color_enabled)?;
                return Ok(());
            }
            ReadSubmission::TooLong => {
                write_repl_error(writer, "ORNA-REPL-INPUT-LIMIT", color_enabled)?;
            }
            ReadSubmission::InvalidUtf8 => {
                write_repl_error(writer, "ORNA-REPL-INPUT-UTF8", color_enabled)?;
            }
            ReadSubmission::Source(source) => {
                let command = source.trim();
                if command == ":quit" {
                    close_session_watch(&mut watch, writer, color_enabled)?;
                    return Ok(());
                }
                if let Some(help) = parse_help_command(command) {
                    match help {
                        Ok(text) => {
                            write_styled(writer, AnsiColor::Cyan, text.as_bytes(), color_enabled)?;
                        }
                        Err(()) => {
                            write_repl_error(writer, "ORNA-REPL-COMMAND", color_enabled)?;
                        }
                    }
                    if help.is_ok() {
                        writer.write_all(b"\n")?;
                    }
                } else if let Some(target) = parse_snapshot_target(command) {
                    let Some(loader) = loader else {
                        write_repl_error(writer, "ORNA-REPL-COMMAND", color_enabled)?;
                        continue;
                    };
                    match target.and_then(|target| loader.load_snapshot(target).map_err(|_| ())) {
                        Ok(candidate) => {
                            *session = candidate;
                            submitter = None;
                        }
                        Err(()) => write_repl_error(writer, "ORNA-REPL-AT", color_enabled)?,
                    }
                } else if let Some(rest) = parse_watch_source(command) {
                    dispatch_watch(rest, &mut watch, session, writer, color_enabled)?;
                } else if let Some(rest) = parse_unwatch_command(command) {
                    dispatch_unwatch(rest, &mut watch, writer, color_enabled)?;
                } else if source.trim_start().starts_with(':') {
                    run_console_command(&source, session, &mut console, writer, color_enabled)?;
                } else {
                    let started = console.timing().then(Instant::now);
                    let result = match submitter.as_deref_mut() {
                        Some(submit) => submit(session, &source),
                        None => session
                            .submit_outcome(&source, RelationDisplay::Window)
                            .map_err(ReplFailure::from),
                    };
                    match result {
                        Ok(outcome) => {
                            write_repl_outcome(writer, &outcome, &source, color_enabled)?;
                        }
                        Err(failure) => {
                            write_repl_failure(writer, &failure, &source, color_enabled)?;
                        }
                    }
                    console.record(&source);
                    if let Some(started) = started {
                        writeln!(writer, "time: {:.3?}", started.elapsed())?;
                    }
                }
            }
        }
    }
}
/// Starts (or rebinds) the host watch, then drains available frames.
fn dispatch_watch<W: Write, S: WatchFrameSource + ?Sized>(
    source: &str,
    watch: &mut Option<WatchBinding<'_, S>>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    let Some(binding) = watch.as_mut() else {
        write_repl_error(writer, "ORNA-REPL-COMMAND", color_enabled)?;
        return Ok(());
    };
    match binding.source.start_watch(source, session) {
        Ok(presentation) => {
            if let Some(previous) = binding.state.install(presentation)
                && binding.source.close_watch(previous).is_err()
            {
                write_repl_error(writer, "ORNA-REPL-WATCH-SOURCE", color_enabled)?;
            }
            drain_available_frames(binding, session, writer, color_enabled)?;
        }
        Err(_) => write_repl_error(writer, "ORNA-REPL-COMMAND", color_enabled)?,
    }
    Ok(())
}

/// Closes the console's watch. `:unwatch` alone closes the active watch; an id
/// closes it only when it names that watch.
fn dispatch_unwatch<W: Write, S: WatchFrameSource + ?Sized>(
    id: &str,
    watch: &mut Option<WatchBinding<'_, S>>,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    let active = watch
        .as_ref()
        .and_then(|binding| binding.state.presentation.as_ref())
        .map(WatchPresentation::watch);
    let names_active = match active {
        None => false,
        Some(_) if id.is_empty() => true,
        Some(active) => canonical_uuid_text(active) == id,
    };
    if !names_active {
        return write_repl_error(writer, "ORNA-REPL-COMMAND", color_enabled);
    }
    close_session_watch(watch, writer, color_enabled)
}

/// The argument of `:unwatch [id]`, or `None` for any other command.
fn parse_unwatch_command(command: &str) -> Option<&str> {
    let rest = command.strip_prefix(":unwatch")?;
    if rest.is_empty() {
        return Some("");
    }
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}

/// Cancels the watch owned by this session when the REPL closes.
fn close_session_watch<W: Write, S: WatchFrameSource + ?Sized>(
    watch: &mut Option<WatchBinding<'_, S>>,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    let Some(binding) = watch.as_mut() else {
        return Ok(());
    };
    if let Some(watch) = binding.state.close()
        && binding.source.close_watch(watch).is_err()
    {
        write_repl_error(writer, "ORNA-REPL-WATCH-SOURCE", color_enabled)?;
    }
    Ok(())
}

/// Applies one frame to the owning presentation and prints its outcome.
fn accept_frame<W: Write>(
    state: &mut WatchCommandState,
    frame: &[u8],
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    let Some(presentation) = state.presentation.as_mut() else {
        return Ok(());
    };
    match presentation.receive_encoded(frame) {
        Ok(LivePresentationUpdate::SnapshotInstalled) => write_watch_observation(
            writer,
            presentation,
            b"watch: snapshot installed",
            color_enabled,
        ),
        Ok(LivePresentationUpdate::DeltaApplied) => {
            write_watch_observation(writer, presentation, b"watch: delta applied", color_enabled)
        }
        Ok(LivePresentationUpdate::ResyncRequired) => {
            write_styled(
                writer,
                AnsiColor::Yellow,
                b"watch: resync required\n",
                color_enabled,
            )?;
            Ok(())
        }
        Err(_) => write_repl_error(writer, "ORNA-REPL-WATCH-FRAME", color_enabled),
    }
}

/// Reports an installed observation with its revision. A complete snapshot and
/// an applied delta both publish a new visible revision of the watched tree.
fn write_watch_observation<W: Write>(
    writer: &mut W,
    presentation: &WatchPresentation,
    label: &[u8],
    color_enabled: bool,
) -> io::Result<()> {
    let Some(published) = presentation.published() else {
        return Ok(());
    };
    write_styled(writer, AnsiColor::Green, label, color_enabled)?;
    writer.write_all(b" (rev ")?;
    write_styled_display(writer, AnsiColor::Dim, published.revision(), color_enabled)?;
    writer.write_all(b")\n")
}

/// Drains every immediately available frame; `Ok(None)` keeps the REPL
/// interactive instead of blocking the line loop.
fn drain_available_frames<W: Write, S: WatchFrameSource + ?Sized>(
    binding: &mut WatchBinding<'_, S>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    if !send_pending_resync(binding, session, writer, color_enabled)? {
        return Ok(());
    }
    let Some(watch) = binding.state.watch() else {
        return Ok(());
    };
    loop {
        match binding.source.next_frame(watch) {
            Ok(Some(frame)) => {
                accept_frame(binding.state, &frame, writer, color_enabled)?;
                if !send_pending_resync(binding, session, writer, color_enabled)? {
                    return Ok(());
                }
            }
            Ok(None) => return Ok(()),
            Err(_) => {
                write_repl_error(writer, "ORNA-REPL-WATCH-SOURCE", color_enabled)?;
                return Ok(());
            }
        }
    }
}

/// Sends the presentation's pending resync through its owning host source.
/// A failed send remains pending and is retried at the next loop boundary.
fn send_pending_resync<W: Write, S: WatchFrameSource + ?Sized>(
    binding: &mut WatchBinding<'_, S>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<bool> {
    let Some(request) = binding.state.take_resync_request() else {
        return Ok(true);
    };
    if binding.source.request_resync(request, session).is_ok() {
        binding.state.acknowledge_resync_request(request);
        Ok(true)
    } else {
        write_repl_error(writer, "ORNA-REPL-WATCH-SOURCE", color_enabled)?;
        Ok(false)
    }
}

/// Re-evaluates the watch once at a prompt boundary, then drains every frame
/// it queued.
fn pump_watch_frames<W: Write, S: WatchFrameSource + ?Sized>(
    watch: &mut Option<WatchBinding<'_, S>>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    if let Some(binding) = watch.as_mut() {
        if let Some(id) = binding.state.watch()
            && binding.source.poll(id, session).is_err()
        {
            write_repl_error(writer, "ORNA-REPL-WATCH-SOURCE", color_enabled)?;
        }
        drain_available_frames(binding, session, writer, color_enabled)?;
    }
    Ok(())
}

pub(crate) fn parse_help_command(command: &str) -> Option<Result<&'static str, ()>> {
    let mut words = command.split_ascii_whitespace();
    if words.next() != Some(":help") {
        return None;
    }
    let topic = words.next();
    if words.next().is_some() {
        return Some(Err(()));
    }
    Some(match topic {
        None => Ok(REPL_HELP),
        Some("help") => Ok(":help [name]"),
        Some("at") => Ok(":at CWD|HEAD|ref"),
        Some("quit") => Ok(":quit"),
        Some(_) => Err(()),
    })
}

pub(crate) fn parse_snapshot_target(command: &str) -> Option<Result<SnapshotTarget, ()>> {
    let mut words = command.split_ascii_whitespace();
    if words.next() != Some(":at") {
        return None;
    }
    let Some(selector) = words.next() else {
        return Some(Err(()));
    };
    if words.next().is_some() {
        return Some(Err(()));
    }
    if selector.chars().any(char::is_control) {
        return Some(Err(()));
    }
    Some(Ok(match selector {
        "CWD" => SnapshotTarget::Cwd,
        "HEAD" => SnapshotTarget::Head,
        reference => SnapshotTarget::Ref(reference.into()),
    }))
}

/// Splits a `:watch` console command into its required source expression.
pub(crate) fn parse_watch_source(command: &str) -> Option<&str> {
    let mut words = command.split_ascii_whitespace();
    if words.next() != Some(":watch") {
        return None;
    }
    let rest = command
        .split_once(":watch")
        .map_or("", |(_, rest)| rest)
        .trim();
    if rest.is_empty() {
        return None;
    }
    Some(rest)
}

pub(crate) fn read_submission<R: BufRead>(reader: &mut R) -> io::Result<ReadSubmission> {
    let mut source = Vec::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(if source.is_empty() {
                ReadSubmission::Eof
            } else {
                decode_submission(source)
            });
        }
        let take = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| position + 1);
        if source.len().saturating_add(take) > MAX_INPUT_BYTES {
            let complete = bytes.get(take - 1) == Some(&b'\n');
            reader.consume(take);
            if !complete {
                drain_line(reader)?;
            }
            return Ok(ReadSubmission::TooLong);
        }
        let complete = bytes.get(take - 1) == Some(&b'\n');
        source.extend_from_slice(&bytes[..take]);
        reader.consume(take);
        if complete {
            return Ok(decode_submission(source));
        }
    }
}

fn decode_submission(source: Vec<u8>) -> ReadSubmission {
    String::from_utf8(source).map_or(ReadSubmission::InvalidUtf8, ReadSubmission::Source)
}

fn drain_line<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(());
        }
        let take = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| position + 1);
        let complete = bytes.get(take - 1) == Some(&b'\n');
        reader.consume(take);
        if complete {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_evaluator::Limits;
    use std::{cell::Cell, io::BufReader};

    #[test]
    fn scripted_loop_admits_typed_declarations() {
        let mut input = b"let answer: Int = 42;\nanswer\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> > 42 : Int\n> "
        );
    }

    #[test]
    fn help_command_is_effect_free_and_lists_supported_console_operations() {
        let mut input = b":help\n:help at\n:help unknown\n1 + 1\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            format!(
                "> {REPL_HELP}\n> :at CWD|HEAD|ref\n> error[ORNA-REPL-COMMAND]: command is unknown or unavailable in this REPL\nhelp: enter `:help` to see the supported commands\n> 2 : Int\n> "
            )
        );
    }

    struct FailingSnapshotLoader;

    impl SnapshotSessionLoader for FailingSnapshotLoader {
        type Error = ();

        fn load_snapshot(
            &self,
            _target: SnapshotTarget,
        ) -> Result<AdmittedReplSession, Self::Error> {
            Err(())
        }
    }

    struct FreshSnapshotLoader;

    impl SnapshotSessionLoader for FreshSnapshotLoader {
        type Error = ();

        fn load_snapshot(
            &self,
            target: SnapshotTarget,
        ) -> Result<AdmittedReplSession, Self::Error> {
            assert_eq!(target, SnapshotTarget::Head);
            Ok(AdmittedReplSession::new(Limits::default()))
        }
    }

    struct CountingSnapshotLoader {
        calls: Cell<usize>,
    }

    impl SnapshotSessionLoader for CountingSnapshotLoader {
        type Error = ();

        fn load_snapshot(
            &self,
            _target: SnapshotTarget,
        ) -> Result<AdmittedReplSession, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Err(())
        }
    }

    #[test]
    fn malformed_snapshot_selection_does_not_call_the_loader() {
        let mut input = b":at\n:at HEAD extra\n:at HEAD\x1b\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        let loader = CountingSnapshotLoader {
            calls: Cell::new(0),
        };
        run_with_snapshot_loader(&mut input, &mut output, &mut session, &loader)
            .expect("REPL runs");
        assert_eq!(loader.calls.get(), 0);
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> error[ORNA-REPL-AT]: requested project snapshot could not be loaded\nhelp: choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`\n> error[ORNA-REPL-AT]: requested project snapshot could not be loaded\nhelp: choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`\n> error[ORNA-REPL-AT]: requested project snapshot could not be loaded\nhelp: choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`\n> "
        );
    }

    #[test]
    fn failed_snapshot_selection_preserves_the_existing_overlay() {
        let mut input = b"let answer: Int = 42;\n:at HEAD\nanswer\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run_with_snapshot_loader(
            &mut input,
            &mut output,
            &mut session,
            &FailingSnapshotLoader,
        )
        .expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> > error[ORNA-REPL-AT]: requested project snapshot could not be loaded\nhelp: choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`\n> 42 : Int\n> "
        );
    }

    #[test]
    fn successful_snapshot_selection_replaces_the_existing_overlay() {
        let mut input = b"let answer: Int = 42;\n:at HEAD\nanswer\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run_with_snapshot_loader(&mut input, &mut output, &mut session, &FreshSnapshotLoader)
            .expect("REPL runs");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(
            !output.contains("42 : Int"),
            "old overlay survived: {output}"
        );
        assert!(
            output.contains("error["),
            "fresh session unexpectedly resolved answer: {output}"
        );
    }

    #[test]
    fn bounded_input_is_discarded_and_the_next_submission_runs() {
        let mut input = vec![b'x'; MAX_INPUT_BYTES + 1];
        input.extend_from_slice(b"\n1\n:quit\n");
        let mut input = input.as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL recovers");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> error[ORNA-REPL-INPUT-LIMIT]: submission is longer than the REPL input limit\nhelp: shorten the submission and enter it again\n> 1 : Int\n> "
        );
    }

    #[test]
    fn unicode_input_may_span_bufread_chunks() {
        let source = "\"é\"\n:quit\n";
        let mut input = BufReader::with_capacity(1, source.as_bytes());
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> \"\\u{e9}\" : Str\n> "
        );
    }

    #[test]
    fn failed_submission_keeps_the_last_successful_result() {
        let mut input = b"1 + 1\nlet mismatch: Int = \"wrong\";\n$_\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(
            output.starts_with("> 2 : Int\n> error[ORNA-S021-TYPE]: "),
            "{output}"
        );
        assert!(output.contains("\nhelp: "), "{output}");
        assert!(output.ends_with("> 2 : Int\n> "), "{output}");
    }

    #[test]
    fn status_binding_reports_a_redacted_failure_then_a_success() {
        let mut input = b"let secret_name: Int = \"private\";\n$?\n40 + 2\n$?\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.starts_with("> error[ORNA-S021-TYPE]: "), "{output}");
        let status_payload = output
            .lines()
            .find(|line| line.starts_with("> {"))
            .expect("status binding prints its redacted payload");
        assert!(!status_payload.contains("private"), "{output}");
        assert!(status_payload.contains("\"redacted\": true"), "{output}");
        assert!(
            output.ends_with("> 42 : Int\n> null : { code: Str, message: Str, redacted: Bool, severity: Str }?\n> "),
            "{output}"
        );
    }

    #[test]
    fn submitted_effect_is_rejected_without_changing_the_last_result() {
        let mut input =
            b"let seed: Int = 2;\nseed\nstd.net.http.get(\"https://example.com\")\n$_\n:quit\n"
                .as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> > 2 : Int\n> error[ORNA-REPL-EFFECT]: this input performs effects: network\nnote: the core REPL session never runs effects\nhelp: evaluate a pure expression here\nhelp: or run the input in a project repository REPL, where effects are committed\n> 2 : Int\n> "
        );
    }

    #[test]
    fn malformed_utf8_is_a_recoverable_submission_error() {
        let mut input = b"2\n\xff\n$_\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL recovers");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> 2 : Int\n> error[ORNA-REPL-INPUT-UTF8]: submission is not valid UTF-8 text\nhelp: re-enter the submission using valid UTF-8\n> 2 : Int\n> "
        );
    }

    struct BrokenWriter;

    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("writer failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_errors_are_propagated() {
        let mut input = b":quit\n".as_slice();
        let mut writer = BrokenWriter;
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(
            run(&mut input, &mut writer, &mut session)
                .expect_err("writer error")
                .kind(),
            io::ErrorKind::Other
        );
    }

    fn located(code: &str, message: &str, span: std::ops::Range<usize>, help: &str) -> ReplFailure {
        ReplFailure {
            code: code.to_owned(),
            detail: Some(ReplDetail {
                code: code.to_owned(),
                message: message.to_owned(),
                span: Some(span),
                notes: Vec::new(),
                help: vec![help.to_owned()],
            }),
        }
    }

    #[test]
    fn unresolved_name_renders_its_line_column_caret_and_remedy() {
        let failure = located(
            "ORNA-S012-UNRESOLVED",
            "name `he` cannot be resolved",
            0..2,
            "declare `he` before using it, for example `let he = <value>`",
        );
        let text = render_repl_failure(&failure, "he\n");
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(
            lines[0],
            "error[ORNA-S012-UNRESOLVED]: name `he` cannot be resolved"
        );
        assert!(lines.contains(&"  --> line 1, column 1"), "{text}");
        assert!(lines.contains(&"1 | he"), "{text}");
        assert!(lines.contains(&"  | ^^"), "{text}");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("help: declare `he` before using it")),
            "{text}"
        );
    }

    #[test]
    fn string_literal_line_is_withheld_from_the_snippet() {
        let source = "let secret_name: Int = \"private\";\n";
        let failure = located(
            "ORNA-S021-TYPE",
            "static types are incompatible",
            0..source.len() - 1,
            "change the expression or its declared type",
        );
        let text = render_repl_failure(&failure, source);

        assert!(text.contains("  --> line 1, column 1"), "{text}");
        assert!(
            text.contains("<source withheld: line contains a string literal>"),
            "{text}"
        );
        assert!(!text.contains("private"), "{text}");
    }

    #[test]
    fn multi_line_string_middle_line_without_a_quote_is_withheld() {
        let source = "let s = \"first\nsecret middle\nlast\";\n";
        let start = source.find("secret").expect("middle line fixture");
        let failure = located(
            "ORNA-S021-TYPE",
            "static types are incompatible",
            start..start + "secret".len(),
            "change the expression or its declared type",
        );
        let text = render_repl_failure(&failure, source);

        assert!(text.contains("  --> line 2, column 1"), "{text}");
        assert!(
            text.contains("<source withheld: line contains a string literal>"),
            "{text}"
        );
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("middle"), "{text}");
    }

    #[test]
    fn end_of_input_error_caret_sits_after_the_last_token() {
        let failure = located(
            "ORNA-PARSE-001",
            "expected namespace path",
            3..3,
            "name the module to import after `use`, for example `use library;`",
        );
        let text = render_repl_failure(&failure, "use\n");
        let lines: Vec<&str> = text.lines().collect();

        assert!(lines.contains(&"  --> line 1, column 4"), "{text}");
        assert!(lines.contains(&"  |    ^"), "{text}");
    }

    #[test]
    fn multi_line_error_reports_the_line_of_its_span() {
        let source = "let a = 1;\nnofn(2)\n";
        let failure = located(
            "ORNA-S012-UNRESOLVED",
            "name `nofn` cannot be resolved",
            11..15,
            "declare `nofn` before using it",
        );
        let text = render_repl_failure(&failure, source);
        let lines: Vec<&str> = text.lines().collect();

        assert!(lines.contains(&"  --> line 2, column 1"), "{text}");
        assert!(lines.contains(&"2 | nofn(2)"), "{text}");
        assert!(lines.contains(&"  | ^^^^"), "{text}");
    }

    #[test]
    fn failure_without_located_detail_renders_its_fixed_remedy_without_a_location() {
        let text = render_repl_failure(&ReplFailure::code("ORNA-EVAL-PARSE"), "use\n");

        assert!(text.starts_with("error[ORNA-EVAL-PARSE]: "), "{text}");
        assert!(!text.contains("-->"), "{text}");
        assert!(text.contains("\nhelp: "), "{text}");
    }
}
