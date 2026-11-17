//! Console commands that read or present the session: `:type`, `:inspect`,
//! `:members`, `:table`, `:history`, `:timing`, `:copy`, `:doc`, `:source`,
//! and `:clear`.
//!
//! `repl.rs` dispatches the session, watch, and help commands itself. Every
//! other `:name` reaches [`run`], which either runs the command or refuses it
//! with a located error that names the command. A command this console does
//! not implement yet is refused by name, never silently ignored.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write};
use std::ops::Range;

use orna_check::Type;
use orna_evaluator::{RelationDisplay, ReplValue};
use orna_project_repl::{AdmittedReplSession, ReplDetail};

use crate::repl::{AnsiColor, ReplFailure, write_repl_failure, write_repl_outcome, write_styled};
use crate::repl_copy;
use crate::repl_doc;
use crate::repl_inspect::inspect_full;

/// The most submitted inputs `:history` keeps.
const MAX_HISTORY: usize = 256;

/// Every command name the console knows, in the order `:help` lists them.
const COMMANDS: [&str; 19] = [
    "help", "at", "watch", "unwatch", "quit", "type", "inspect", "members", "table", "history",
    "timing", "clear", "doc", "source", "open", "plan", "trace", "copy", "display",
];

/// Commands the console recognises but does not implement yet. Each is
/// refused by name; none is silently accepted.
const UNSUPPORTED: [&str; 4] = ["open", "plan", "trace", "display"];

/// The code every console refusal carries.
const COMMAND_CODE: &str = "ORNA-REPL-COMMAND";

/// Console state that outlives one submission: the input history and the
/// `:timing` switch.
#[derive(Default)]
pub(crate) struct ConsoleState {
    /// Submitted inputs, oldest first, each with its submission number.
    history: VecDeque<(usize, String)>,
    /// Submissions made so far, which numbers the next history entry.
    submitted: usize,
    timing: bool,
}

impl ConsoleState {
    /// Records one submitted input. Blank input is not history.
    pub(crate) fn record(&mut self, source: &str) {
        let line = source.trim();
        if line.is_empty() {
            return;
        }
        self.submitted += 1;
        if self.history.len() == MAX_HISTORY {
            self.history.pop_front();
        }
        self.history
            .push_back((self.submitted, line.replace('\n', " ")));
    }

    /// Whether `:timing on` is in effect.
    pub(crate) fn timing(&self) -> bool {
        self.timing
    }
}

/// A command the console refuses: the located failure, and the text its span
/// points into.
struct Refusal<'a> {
    failure: ReplFailure,
    source: &'a str,
}

/// `Ok(None)` when the command ran; `Ok(Some(_))` when it was refused. The
/// outer `io::Result` is a failed write.
type Outcome<'a> = io::Result<Option<Refusal<'a>>>;

/// One parsed `:name ARG` command, borrowed from the submitted source.
struct Cmd<'a> {
    source: &'a str,
    name: &'a str,
    name_span: Range<usize>,
    arg: &'a str,
}

/// Runs one console command. `source` begins with `:` after leading
/// whitespace; the caller routes only such input here.
pub(crate) fn run<W: Write>(
    source: &str,
    session: &AdmittedReplSession,
    console: &mut ConsoleState,
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    match execute(source, session, console, writer, color_enabled)? {
        None => Ok(()),
        Some(refusal) => {
            write_repl_failure(writer, &refusal.failure, refusal.source, color_enabled)
        }
    }
}

fn execute<'a, W: Write>(
    source: &'a str,
    session: &AdmittedReplSession,
    console: &mut ConsoleState,
    writer: &mut W,
    color_enabled: bool,
) -> Outcome<'a> {
    let leading = source.len() - source.trim_start().len();
    let body = &source[leading..];
    let after_colon = body.strip_prefix(':').unwrap_or(body);
    let name_len = after_colon
        .find(char::is_whitespace)
        .unwrap_or(after_colon.len());
    let cmd = Cmd {
        source,
        name: &after_colon[..name_len],
        name_span: leading..leading + 1 + name_len,
        arg: after_colon[name_len..].trim(),
    };
    match cmd.name {
        "type" => type_command(&cmd, session, writer, color_enabled),
        "inspect" => inspect_command(&cmd, session, writer, color_enabled),
        "members" => members_command(&cmd, session, writer, color_enabled),
        "table" => table_command(&cmd, session, writer, color_enabled),
        "history" => history_command(&cmd, console, writer),
        "timing" => Ok(timing_command(&cmd, console)),
        "clear" => clear_command(&cmd, writer, color_enabled),
        "doc" => doc_command(&cmd, session, writer, color_enabled),
        "source" => source_command(&cmd, session, writer, color_enabled),
        "copy" => repl_copy::run(cmd.arg, session, writer, color_enabled).map(|()| None),
        name if UNSUPPORTED.contains(&name) => Ok(Some(refuse_name(
            &cmd,
            format!("`:{name}` is not available in this console yet"),
            vec![
                "the console runs :type, :doc, :source, :inspect, :members, :table, :history, :timing, :copy, and :clear"
                    .into(),
                "enter `:help` to see the supported commands".into(),
            ],
        ))),
        // These are dispatched by `repl.rs` before a submission reaches the
        // console; the arms keep a usage error if that routing ever changes.
        "help" => Ok(Some(refuse_name(
            &cmd,
            "usage: :help [name]".into(),
            Vec::new(),
        ))),
        "at" => Ok(Some(refuse_name(
            &cmd,
            "usage: :at CWD|HEAD|ref".into(),
            Vec::new(),
        ))),
        "watch" => Ok(Some(refuse_name(
            &cmd,
            "usage: :watch EXPR".into(),
            Vec::new(),
        ))),
        "unwatch" => Ok(Some(refuse_name(
            &cmd,
            "usage: :unwatch [ID]".into(),
            Vec::new(),
        ))),
        "quit" => Ok(Some(refuse_name(&cmd, "usage: :quit".into(), Vec::new()))),
        _ => Ok(Some(unknown_command(&cmd))),
    }
}

/// `:doc NAME` prints the doc comment lines of a declaration.
fn doc_command<'a, W: Write>(
    cmd: &Cmd<'a>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> Outcome<'a> {
    declaration_command(cmd, session, repl_doc::Part::Docs, writer, color_enabled)
}

/// `:source NAME` prints the source text of a declaration.
fn source_command<'a, W: Write>(
    cmd: &Cmd<'a>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> Outcome<'a> {
    declaration_command(cmd, session, repl_doc::Part::Source, writer, color_enabled)
}

fn declaration_command<'a, W: Write>(
    cmd: &Cmd<'a>,
    session: &AdmittedReplSession,
    part: repl_doc::Part,
    writer: &mut W,
    color_enabled: bool,
) -> Outcome<'a> {
    if cmd.arg.is_empty() {
        return Ok(Some(refuse_name(
            cmd,
            format!("usage: :{} NAME", cmd.name),
            Vec::new(),
        )));
    }
    match repl_doc::find(session.declarations(), cmd.arg) {
        Ok(declaration) => {
            repl_doc::write_part(writer, declaration, part, color_enabled)?;
            Ok(None)
        }
        Err(message) => Ok(Some(refuse_arg_message(cmd, message))),
    }
}

fn type_command<'a, W: Write>(
    cmd: &Cmd<'a>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> Outcome<'a> {
    if cmd.arg.is_empty() {
        return Ok(Some(refuse_name(
            cmd,
            "usage: :type EXPR".into(),
            Vec::new(),
        )));
    }
    let ty = match session.expression_type(cmd.arg) {
        Ok(ty) => ty,
        Err(error) => return Ok(Some(refuse_arg(cmd, ReplFailure::from(error)))),
    };
    write_styled(
        writer,
        AnsiColor::Green,
        ty.to_string().as_bytes(),
        color_enabled,
    )?;
    writer.write_all(b"\n")?;
    Ok(None)
}

fn inspect_command<'a, W: Write>(
    cmd: &Cmd<'a>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> Outcome<'a> {
    if cmd.arg.is_empty() {
        return Ok(Some(refuse_name(
            cmd,
            "usage: :inspect EXPR".into(),
            Vec::new(),
        )));
    }
    let outcome = match session.preview_outcome(cmd.arg, RelationDisplay::Window) {
        Ok(outcome) => outcome,
        Err(error) => return Ok(Some(refuse_arg(cmd, ReplFailure::from(error)))),
    };
    match &outcome.value {
        Some(ReplValue::Value(value)) => {
            write_styled(
                writer,
                AnsiColor::Green,
                inspect_full(value).as_bytes(),
                color_enabled,
            )?;
            writer.write_all(b"\n")?;
        }
        _ => write_repl_outcome(writer, &outcome, cmd.arg, color_enabled)?,
    }
    Ok(None)
}

fn members_command<'a, W: Write>(
    cmd: &Cmd<'a>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> Outcome<'a> {
    if cmd.arg.is_empty() {
        return Ok(Some(refuse_name(
            cmd,
            "usage: :members EXPR".into(),
            Vec::new(),
        )));
    }
    let ty = match session.expression_type(cmd.arg) {
        Ok(ty) => ty,
        Err(error) => return Ok(Some(refuse_arg(cmd, ReplFailure::from(error)))),
    };
    let Some(fields) = row_fields(&ty) else {
        return Ok(Some(refuse_arg_message(
            cmd,
            format!("`:members` needs a record or a relation of records, not `{ty}`"),
        )));
    };
    for (name, field) in fields {
        write_styled(writer, AnsiColor::Green, name.as_bytes(), color_enabled)?;
        writer.write_all(b" : ")?;
        write_styled(
            writer,
            AnsiColor::Dim,
            field.to_string().as_bytes(),
            color_enabled,
        )?;
        writer.write_all(b"\n")?;
    }
    Ok(None)
}

/// The fields of a record, or of the record a relation holds.
fn row_fields(ty: &Type) -> Option<&BTreeMap<String, Type>> {
    match ty {
        Type::Record(fields) => Some(fields),
        Type::Relation(element) => match element.as_ref() {
            Type::Record(fields) => Some(fields),
            _ => None,
        },
        _ => None,
    }
}

fn table_command<'a, W: Write>(
    cmd: &Cmd<'a>,
    session: &AdmittedReplSession,
    writer: &mut W,
    color_enabled: bool,
) -> Outcome<'a> {
    if cmd.arg.is_empty() {
        return Ok(Some(refuse_name(
            cmd,
            "usage: :table EXPR".into(),
            Vec::new(),
        )));
    }
    let ty = match session.expression_type(cmd.arg) {
        Ok(ty) => ty,
        Err(error) => return Ok(Some(refuse_arg(cmd, ReplFailure::from(error)))),
    };
    if !matches!(ty, Type::Relation(_)) {
        return Ok(Some(refuse_arg_message(
            cmd,
            format!("`:table` needs a relation, not `{ty}`"),
        )));
    }
    let outcome = match session.preview_outcome(cmd.arg, RelationDisplay::Window) {
        Ok(outcome) => outcome,
        Err(error) => return Ok(Some(refuse_arg(cmd, ReplFailure::from(error)))),
    };
    write_repl_outcome(writer, &outcome, cmd.arg, color_enabled)?;
    Ok(None)
}

/// Prints each retained input with its submission number. An argument
/// filters to inputs containing it.
fn history_command<'a, W: Write>(
    cmd: &Cmd<'a>,
    console: &ConsoleState,
    writer: &mut W,
) -> Outcome<'a> {
    for (number, line) in &console.history {
        if line.contains(cmd.arg) {
            writeln!(writer, "{number:>4}  {line}")?;
        }
    }
    Ok(None)
}

fn timing_command<'a>(cmd: &Cmd<'a>, console: &mut ConsoleState) -> Option<Refusal<'a>> {
    match cmd.arg {
        "on" => console.timing = true,
        "off" => console.timing = false,
        _ => {
            return Some(refuse_name(cmd, "usage: :timing on|off".into(), Vec::new()));
        }
    }
    None
}

/// Clears the terminal when colour is on. Without colour the console has no
/// terminal control sequences to send, so the command prints nothing.
fn clear_command<'a, W: Write>(cmd: &Cmd<'a>, writer: &mut W, color_enabled: bool) -> Outcome<'a> {
    if !cmd.arg.is_empty() {
        return Ok(Some(refuse_name(cmd, "usage: :clear".into(), Vec::new())));
    }
    if color_enabled {
        writer.write_all(b"\x1b[2J\x1b[H")?;
    }
    Ok(None)
}

fn unknown_command<'a>(cmd: &Cmd<'a>) -> Refusal<'a> {
    let mut help = Vec::new();
    if let Some((_, nearest)) = COMMANDS
        .iter()
        .map(|candidate| (edit_distance(cmd.name, candidate), *candidate))
        .min()
        .filter(|(distance, _)| *distance <= 2)
    {
        help.push(format!("did you mean `:{nearest}`?"));
    }
    help.push("enter `:help` to see the supported commands".into());
    refuse_name(cmd, format!("unknown command `:{}`", cmd.name), help)
}

/// Refuses the command name itself, locating the error at `:name`.
fn refuse_name<'a>(cmd: &Cmd<'a>, message: String, help: Vec<String>) -> Refusal<'a> {
    Refusal {
        failure: located(cmd.name_span.clone(), message, help),
        source: cmd.source,
    }
}

/// Refuses an evaluator failure, whose span is already relative to the
/// argument text.
fn refuse_arg<'a>(cmd: &Cmd<'a>, failure: ReplFailure) -> Refusal<'a> {
    Refusal {
        failure,
        source: cmd.arg,
    }
}

/// Refuses the whole argument with a message the console itself produced.
fn refuse_arg_message<'a>(cmd: &Cmd<'a>, message: String) -> Refusal<'a> {
    refuse_arg(cmd, located(0..cmd.arg.len(), message, Vec::new()))
}

fn located(span: Range<usize>, message: String, help: Vec<String>) -> ReplFailure {
    ReplFailure {
        code: COMMAND_CODE.into(),
        detail: Some(ReplDetail {
            code: COMMAND_CODE.into(),
            message,
            span: Some(span),
            notes: Vec::new(),
            help,
        }),
    }
}

/// Levenshtein distance between two command names, by characters.
fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (index, left_char) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = index + 1;
        for (column, right_char) in right.iter().enumerate() {
            let up = row[column + 1];
            let substitution = diagonal + usize::from(left_char != *right_char);
            row[column + 1] = substitution.min(row[column] + 1).min(up + 1);
            diagonal = up;
        }
    }
    row[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_evaluator::Limits;

    fn run_source(source: &str, console: &mut ConsoleState) -> String {
        let session = AdmittedReplSession::new(Limits::default());
        let mut out = Vec::new();
        run(source, &session, console, &mut out, false).expect("in-memory write");
        String::from_utf8(out).expect("utf8 output")
    }

    #[test]
    fn unknown_command_names_itself_and_suggests_nearest() {
        let mut console = ConsoleState::default();
        let text = run_source(":tpye 1 + 1", &mut console);
        assert!(text.contains("unknown command `:tpye`"), "{text}");
        assert!(text.contains("did you mean `:type`?"), "{text}");
        assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
    }

    #[test]
    fn unknown_command_without_near_match_gives_no_suggestion() {
        let mut console = ConsoleState::default();
        let text = run_source(":zzzzzzzz", &mut console);
        assert!(text.contains("unknown command `:zzzzzzzz`"), "{text}");
        assert!(!text.contains("did you mean"), "{text}");
    }

    #[test]
    fn unsupported_command_is_refused_by_name_not_ignored() {
        let mut console = ConsoleState::default();
        for name in UNSUPPORTED {
            let text = run_source(&format!(":{name}"), &mut console);
            assert!(
                text.contains(&format!("`:{name}` is not available in this console yet")),
                "{text}"
            );
        }
    }

    #[test]
    fn type_prints_static_type_of_expression() {
        let mut console = ConsoleState::default();
        assert_eq!(run_source(":type 1 + 1", &mut console), "Int\n");
    }

    #[test]
    fn type_without_expression_is_usage_error() {
        let mut console = ConsoleState::default();
        let text = run_source(":type", &mut console);
        assert!(text.contains("usage: :type EXPR"), "{text}");
    }

    #[test]
    fn members_refuses_non_record_with_located_message() {
        let mut console = ConsoleState::default();
        let text = run_source(":members 1", &mut console);
        assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
        assert!(
            text.contains("needs a record or a relation of records, not `Int`"),
            "{text}"
        );
    }

    #[test]
    fn table_refuses_non_relation_with_located_message() {
        let mut console = ConsoleState::default();
        let text = run_source(":table 1 + 1", &mut console);
        assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
        assert!(
            text.contains("`:table` needs a relation, not `Int`"),
            "{text}"
        );
    }

    #[test]
    fn history_lists_numbered_inputs_and_filters() {
        let mut console = ConsoleState::default();
        console.record("1 + 1");
        console.record("  2 + 2 ");
        console.record("   ");
        console.record("let x = 3");
        assert_eq!(
            run_source(":history", &mut console),
            "   1  1 + 1\n   2  2 + 2\n   3  let x = 3\n"
        );
        assert_eq!(
            run_source(":history let", &mut console),
            "   3  let x = 3\n"
        );
    }

    #[test]
    fn history_keeps_only_most_recent_entries_with_stable_numbers() {
        let mut console = ConsoleState::default();
        for index in 0..(MAX_HISTORY + 2) {
            console.record(&format!("{index}"));
        }
        let text = run_source(":history", &mut console);
        assert!(text.starts_with("   3  2\n"), "{text}");
        assert!(text.ends_with(&format!("{:>4}  {}\n", MAX_HISTORY + 2, MAX_HISTORY + 1)));
    }

    #[test]
    fn timing_switches_and_rejects_other_arguments() {
        let mut console = ConsoleState::default();
        assert!(!console.timing());
        assert_eq!(run_source(":timing on", &mut console), "");
        assert!(console.timing());
        assert_eq!(run_source(":timing off", &mut console), "");
        assert!(!console.timing());
        let text = run_source(":timing maybe", &mut console);
        assert!(text.contains("usage: :timing on|off"), "{text}");
    }

    #[test]
    fn clear_emits_terminal_sequence_only_with_colour() {
        let session = AdmittedReplSession::new(Limits::default());
        let mut console = ConsoleState::default();
        let mut plain = Vec::new();
        run(":clear", &session, &mut console, &mut plain, false).expect("write");
        assert!(plain.is_empty());
        let mut coloured = Vec::new();
        run(":clear", &session, &mut console, &mut coloured, true).expect("write");
        assert_eq!(coloured, b"\x1b[2J\x1b[H");
    }

    #[test]
    fn edit_distance_counts_single_character_edits() {
        assert_eq!(edit_distance("type", "tpye"), 2);
        assert_eq!(edit_distance("type", "type"), 0);
        assert_eq!(edit_distance("", "at"), 2);
        assert_eq!(edit_distance("inspec", "inspect"), 1);
    }
}
