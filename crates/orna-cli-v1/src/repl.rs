use std::fmt::Write as _;
use std::io::{self, BufRead, Write};

use orna_client::live_presentation::{LivePresentationUpdate, ResyncRequest, WatchPresentation};
use orna_conformance_v1::AdmittedReplSession;
use orna_foundation_v1::{CanonicalValue, canonical_uuid_text};
use orna_value_v1::Raw;

const MAX_INPUT_BYTES: usize = 65_536;
const MAX_INSPECT_DEPTH: usize = 4;
const MAX_INSPECT_ITEMS: usize = 16;
const MAX_INSPECT_TEXT: usize = 256;
const REPL_HELP: &str = "commands: :help [name], :at CWD|HEAD|ref, :watch expression, :quit";

enum ReadSubmission {
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
/// loading and admission succeed. `run_with_snapshot_loader` leaves the
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
/// the REPL neither opens a connection nor encodes protocol messages.
pub trait WatchFrameSource {
    type Error;

    fn start_watch(&mut self, source: &str) -> Result<WatchPresentation, Self::Error>;
    fn next_frame(&mut self, watch: [u8; 16]) -> Result<Option<Vec<u8>>, Self::Error>;
}

/// Observable state for the currently subscribed REPL watch.
#[derive(Default)]
pub struct WatchCommandState {
    presentation: Option<WatchPresentation>,
}

impl WatchCommandState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn published_revision(&self) -> Option<u64> {
        self.presentation
            .as_ref()?
            .published()
            .map(|published| published.revision())
    }

    pub fn watch(&self) -> Option<[u8; 16]> {
        self.presentation.as_ref().map(WatchPresentation::watch)
    }

    pub fn awaiting_snapshot(&self) -> bool {
        self.presentation
            .as_ref()
            .is_some_and(WatchPresentation::awaiting_snapshot)
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

    pub fn begin_resubscription(&mut self) {
        if let Some(presentation) = &mut self.presentation {
            presentation.begin_resubscription();
        }
    }

    /// Retain the authoritative presentation when the same watch is
    /// re-subscribed, including its complete-snapshot barrier.
    fn install(&mut self, presentation: WatchPresentation) {
        if self
            .presentation
            .as_ref()
            .is_some_and(|current| current.watch() == presentation.watch())
        {
            return;
        }
        self.presentation = Some(presentation);
    }
}

struct WatchBinding<'a, S: ?Sized> {
    source: &'a mut S,
    state: &'a mut WatchCommandState,
}

#[cfg(not(test))]
fn diagnostic_presentation(
    code: &str,
    condition: &'static str,
    remedy: &'static str,
) -> (&'static str, &'static str) {
    super::diagnostic_documentation(code).map_or((condition, remedy), |documentation| {
        (documentation.title, documentation.help)
    })
}

/// The watch integration test compiles this file without the CLI root module.
/// Keep the shared catalogue wording it exercises available in that context.
#[cfg(test)]
fn diagnostic_presentation(
    code: &str,
    condition: &'static str,
    remedy: &'static str,
) -> (&'static str, &'static str) {
    match code {
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
        "ORNA-REPL-EFFECT" => (
            "REPL preview cannot perform an effect",
            "evaluate a pure expression or invoke the operation through its admitted runtime entry point",
        ),
        "ORNA-REPL-AT" => (
            "snapshot selection failed",
            "choose an existing snapshot and keep the repository available while selecting it",
        ),
        _ => (condition, remedy),
    }
}

fn write_diagnostic<W: Write>(
    writer: &mut W,
    code: &str,
    condition: &'static str,
    remedy: &'static str,
) -> io::Result<()> {
    let (title, help) = diagnostic_presentation(code, condition, remedy);
    writeln!(writer, "error[{code}]: {title}\nhelp: {help}")
}

/// Run one retained, line-oriented admitted REPL session. A malformed or
/// rejected submission reports its redacted evaluator code and leaves the
/// session open.
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
    )
}

/// Run an admitted REPL session with an atomic, loader-backed `:at` command.
///
/// A successful `:at CWD|HEAD|ref` installs the owned candidate returned by
/// `loader`. Failed preparation and malformed selectors report
/// `ORNA-REPL-AT` and retain the prior session unchanged.
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
    )
}

/// Run with a host-owned live watch frame source. The REPL receives complete
/// encoded frames, while the host owns the subscription and resync transport.
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
    )
}

/// Run with both an atomic snapshot loader and a host-owned live watch.
pub fn run_with_loader_and_watch<
    R: BufRead,
    W: Write,
    L: SnapshotSessionLoader + ?Sized,
    S: WatchFrameSource + ?Sized,
>(
    reader: &mut R,
    writer: &mut W,
    session: &mut AdmittedReplSession,
    loader: &L,
    source: &mut S,
    state: &mut WatchCommandState,
) -> io::Result<()> {
    run_loop(
        reader,
        writer,
        session,
        Some(loader),
        Some(WatchBinding { source, state }),
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
) -> io::Result<()> {
    loop {
        pump_watch_frames(&mut watch, writer)?;
        writer.write_all(b"> ")?;
        writer.flush()?;
        match read_submission(reader)? {
            ReadSubmission::Eof => return Ok(()),
            ReadSubmission::TooLong => write_diagnostic(
                writer,
                "ORNA-REPL-INPUT-LIMIT",
                "Orna input exceeds the REPL limit",
                "split it into smaller submissions",
            )?,
            ReadSubmission::InvalidUtf8 => write_diagnostic(
                writer,
                "ORNA-REPL-INPUT-UTF8",
                "REPL input is not valid UTF-8",
                "re-enter the Orna source or command using valid UTF-8 text",
            )?,
            ReadSubmission::Source(source) => {
                let command = source.trim();
                if command == ":quit" {
                    return Ok(());
                }
                if let Some(help) = parse_help_command(command) {
                    match help {
                        Ok(text) => writeln!(writer, "{text}")?,
                        Err(()) => write_diagnostic(
                            writer,
                            "ORNA-REPL-COMMAND",
                            "REPL command is unknown or has invalid arguments",
                            "use :help to see supported commands and argument forms",
                        )?,
                    }
                } else if let Some(target) = parse_snapshot_target(command) {
                    let Some(loader) = loader else {
                        write_diagnostic(
                            writer,
                            "ORNA-REPL-COMMAND",
                            "snapshot selection is unavailable in this REPL session",
                            "use a project-backed REPL session to select CWD, HEAD, or a reference",
                        )?;
                        continue;
                    };
                    match target.and_then(|target| loader.load_snapshot(target).map_err(|_| ())) {
                        Ok(candidate) => *session = candidate,
                        Err(()) => write_diagnostic(
                            writer,
                            "ORNA-REPL-AT",
                            "snapshot selection failed",
                            "choose an existing snapshot and keep the repository available",
                        )?,
                    }
                } else if let Some(rest) = parse_watch_source(command) {
                    dispatch_watch(rest, &mut watch, writer)?;
                } else if source.trim_start().starts_with(':') {
                    write_diagnostic(
                        writer,
                        "ORNA-REPL-COMMAND",
                        "REPL command is unknown or has invalid arguments",
                        "use :help to see supported commands and argument forms",
                    )?;
                } else {
                    match session.submit(&source) {
                        Ok(Some(value)) => writeln!(writer, "{}", inspect(&value))?,
                        Ok(None) => {}
                        Err(error) => write_diagnostic(
                            writer,
                            error.code(),
                            "Orna submission was rejected",
                            "revise the expression or declaration, then submit it again",
                        )?,
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
    writer: &mut W,
) -> io::Result<()> {
    let Some(binding) = watch.as_mut() else {
        write_diagnostic(
            writer,
            "ORNA-REPL-COMMAND",
            "live watch is unavailable in this REPL session",
            "evaluate an Orna expression for a one-time preview instead",
        )?;
        return Ok(());
    };
    match binding.source.start_watch(source) {
        Ok(presentation) => {
            binding.state.install(presentation);
            drain_available_frames(binding, writer)?;
        }
        Err(_) => write_diagnostic(
            writer,
            "ORNA-REPL-COMMAND",
            "live watch could not be started for that Orna expression",
            "revise the Orna expression and try :watch again",
        )?,
    }
    Ok(())
}

/// Applies one frame to the owning presentation and prints its outcome.
fn accept_frame<W: Write>(
    state: &mut WatchCommandState,
    frame: Vec<u8>,
    writer: &mut W,
) -> io::Result<()> {
    let Some(presentation) = state.presentation.as_mut() else {
        return Ok(());
    };
    match presentation.receive_encoded(&frame) {
        Ok(LivePresentationUpdate::SnapshotInstalled) => {
            if let Some(published) = presentation.published() {
                writeln!(
                    writer,
                    "watch: snapshot installed (rev {})",
                    published.revision()
                )?;
            }
        }
        Ok(LivePresentationUpdate::DeltaApplied) => {
            if let Some(published) = presentation.published() {
                writeln!(
                    writer,
                    "watch: delta applied (rev {})",
                    published.revision()
                )?;
            }
        }
        Ok(LivePresentationUpdate::ResyncRequired) => {
            writeln!(writer, "watch: resync required")?;
        }
        Err(_) => write_diagnostic(
            writer,
            "ORNA-REPL-WATCH-FRAME",
            "live watch update could not be applied",
            "restart the watch with :watch and the same Orna expression",
        )?,
    }
    Ok(())
}

/// Drains every immediately available frame; `Ok(None)` keeps the REPL
/// interactive instead of blocking the line loop.
fn drain_available_frames<W: Write, S: WatchFrameSource + ?Sized>(
    binding: &mut WatchBinding<'_, S>,
    writer: &mut W,
) -> io::Result<()> {
    let Some(watch) = binding.state.watch() else {
        return Ok(());
    };
    loop {
        match binding.source.next_frame(watch) {
            Ok(Some(frame)) => accept_frame(binding.state, frame, writer)?,
            Ok(None) => return Ok(()),
            Err(_) => {
                write_diagnostic(
                    writer,
                    "ORNA-REPL-WATCH-SOURCE",
                    "live watch update is unavailable",
                    "restart the watch with :watch and the same Orna expression",
                )?;
                return Ok(());
            }
        }
    }
}

fn pump_watch_frames<W: Write, S: WatchFrameSource + ?Sized>(
    watch: &mut Option<WatchBinding<'_, S>>,
    writer: &mut W,
) -> io::Result<()> {
    if let Some(binding) = watch.as_mut() {
        drain_available_frames(binding, writer)?;
    }
    Ok(())
}

fn parse_help_command(command: &str) -> Option<Result<&'static str, ()>> {
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

fn parse_snapshot_target(command: &str) -> Option<Result<SnapshotTarget, ()>> {
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
fn parse_watch_source(command: &str) -> Option<&str> {
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

fn read_submission<R: BufRead>(reader: &mut R) -> io::Result<ReadSubmission> {
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

pub fn inspect(value: &CanonicalValue) -> String {
    let (text, ty) = inspect_raw(value.raw(), 0);
    format!("{text} : {ty}")
}

fn inspect_raw(raw: &Raw, depth: usize) -> (String, &'static str) {
    if depth >= MAX_INSPECT_DEPTH {
        return ("…".into(), "Value");
    }
    match raw {
        Raw::Null => ("null".into(), "Null"),
        Raw::Bool(value) => (value.to_string(), "Bool"),
        Raw::Int(value) => (truncate(&value.to_string()), "Int"),
        Raw::Float(bits) => (format_float(*bits), "Float"),
        Raw::Bytes(bytes) => (inspect_bytes(bytes), "Bytes"),
        Raw::Text(value) => (format!("\"{}\"", escape(value)), "Str"),
        Raw::Array(values) => (inspect_sequence(values, depth), "Array"),
        Raw::Map(entries) => (inspect_map(entries, depth), "Map"),
        Raw::Tag(0, _) => ("<redacted>".into(), "Secret"),
        // OVB tag 37 is the closed 1.0 UUID representation. Keep its
        // structural fallback human-readable without turning presentation
        // into an alternate persistence encoding.
        Raw::Tag(37, value) => inspect_uuid(value),
        Raw::Tag(tag, value) => {
            let (text, _) = inspect_raw(value, depth + 1);
            (format!("Tag<{tag}>({text})"), "Tagged")
        }
    }
}

fn inspect_uuid(value: &Raw) -> (String, &'static str) {
    let Raw::Bytes(bytes) = value else {
        return ("Tag<37>(<invalid>)".into(), "Tagged");
    };
    let Ok(bytes) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        return ("Tag<37>(<invalid>)".into(), "Tagged");
    };
    (canonical_uuid_text(bytes), "Uuid")
}

fn inspect_sequence(values: &[Raw], depth: usize) -> String {
    let mut items = values
        .iter()
        .take(MAX_INSPECT_ITEMS)
        .map(|value| inspect_raw(value, depth + 1).0)
        .collect::<Vec<_>>();
    if values.len() > MAX_INSPECT_ITEMS {
        items.push("…".into());
    }
    format!("[{}]", items.join(", "))
}

fn inspect_map(entries: &[(Raw, Raw)], depth: usize) -> String {
    let mut items = entries
        .iter()
        .take(MAX_INSPECT_ITEMS)
        .map(|(key, value)| {
            let key = inspect_raw(key, depth + 1).0;
            let value = inspect_raw(value, depth + 1).0;
            format!("{key}: {value}")
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_INSPECT_ITEMS {
        items.push("…".into());
    }
    format!("{{{}}}", items.join(", "))
}

fn inspect_bytes(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes.iter().take(MAX_INSPECT_TEXT / 2) {
        write!(&mut text, "{byte:02x}").expect("writing to String is infallible");
    }
    if bytes.len() > MAX_INSPECT_TEXT / 2 {
        text.push('…');
    }
    format!("0x{text}")
}

fn format_float(bits: u64) -> String {
    let value = f64::from_bits(bits);
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value.is_sign_negative() {
            "-Infinity".into()
        } else {
            "Infinity".into()
        }
    } else {
        value.to_string()
    }
}

fn escape(value: &str) -> String {
    truncate(&value.escape_default().to_string())
}

fn truncate(value: &str) -> String {
    let mut end = value.len().min(MAX_INSPECT_TEXT);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = value[..end].to_owned();
    if end < value.len() {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_evaluator_v1::Limits;
    use std::{cell::Cell, io::BufReader};

    #[test]
    fn inspect_is_bounded_and_redacts_protected_values() {
        let value = CanonicalValue::protected();
        assert_eq!(inspect(&value), "<redacted> : Secret");
        assert_eq!(
            truncate(&"x".repeat(MAX_INSPECT_TEXT + 1)),
            format!("{}…", "x".repeat(MAX_INSPECT_TEXT))
        );
    }

    #[test]
    fn inspect_renders_canonical_uuid_text_without_affecting_ovb() {
        let value = CanonicalValue::uuid([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]);
        assert_eq!(
            inspect(&value),
            "00112233-4455-6677-8899-aabbccddeeff : Uuid"
        );
        assert_eq!(
            value.encode().expect("canonical UUID OVB"),
            vec![
                0xd8, 0x25, 0x50, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
                0xbb, 0xcc, 0xdd, 0xee, 0xff
            ]
        );
    }

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
                "> {REPL_HELP}\n> :at CWD|HEAD|ref\n> error[ORNA-REPL-COMMAND]: REPL command is unknown or has invalid arguments\nhelp: use :help to see supported commands and argument forms\n> 2 : Int\n> "
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
            "> error[ORNA-REPL-AT]: snapshot selection failed\nhelp: choose an existing snapshot and keep the repository available while selecting it\n> error[ORNA-REPL-AT]: snapshot selection failed\nhelp: choose an existing snapshot and keep the repository available while selecting it\n> error[ORNA-REPL-AT]: snapshot selection failed\nhelp: choose an existing snapshot and keep the repository available while selecting it\n> "
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
            "> > error[ORNA-REPL-AT]: snapshot selection failed\nhelp: choose an existing snapshot and keep the repository available while selecting it\n> 42 : Int\n> "
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
            "> error[ORNA-REPL-INPUT-LIMIT]: Orna input exceeds the REPL limit\nhelp: split it into smaller submissions\n> 1 : Int\n> "
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
        assert!(output.starts_with("> 2 : Int\n> error[ORNA-S021-TYPE]: expression has the wrong type\nhelp: change the expression or its declared type so the value and requirement agree\n"));
        assert!(output.ends_with("> 2 : Int\n> "));
    }

    #[test]
    fn status_binding_reports_a_redacted_failure_then_a_success() {
        let mut input = b"let secret_name: Int = \"private\";\n$?\n40 + 2\n$?\n:quit\n".as_slice();
        let mut output = Vec::new();
        let mut session = AdmittedReplSession::new(Limits::default());
        run(&mut input, &mut output, &mut session).expect("REPL runs");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> error[ORNA-S021-TYPE]: expression has the wrong type\nhelp: change the expression or its declared type so the value and requirement agree\n> {\"code\": \"ORNA-S021-TYPE\", \"message\": \"<redacted>\", \"redacted\": true, \"severity\": \"error\"} : Map\n> 42 : Int\n> null : Null\n> "
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
            "> > 2 : Int\n> error[ORNA-REPL-EFFECT]: REPL preview cannot perform an effect\nhelp: evaluate a pure expression or invoke the operation through its admitted runtime entry point\n> 2 : Int\n> "
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
            "> 2 : Int\n> error[ORNA-REPL-INPUT-UTF8]: REPL input is not valid UTF-8\nhelp: re-enter the Orna source or command using valid UTF-8 text\n> 2 : Int\n> "
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
}
