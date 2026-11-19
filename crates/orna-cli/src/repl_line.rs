//! Reading the REPL console's input lines (spec `17-repl.md`, "Syntax-aware
//! editing").
//!
//! When standard input and standard output are both a terminal, each line is
//! edited in place: the cursor keys move through it, Up and Down recall the
//! session's earlier inputs, and TAB completes a `:command` name. Piped input,
//! and a terminal that reports itself as `dumb`, are read as plain lines, as
//! before. Persistent history, highlighting, and completion beyond command
//! names are not part of this reader.

use std::io::{self, BufRead, Read};

#[cfg(target_os = "linux")]
use std::io::IsTerminal;

/// The input the REPL console reads submissions from.
pub(crate) enum ReplInput {
    /// Lines from a pipe or a file.
    Piped(io::BufReader<io::StdinLock<'static>>),
    /// Lines typed at a terminal, edited in place.
    #[cfg(target_os = "linux")]
    Terminal(terminal::TerminalLines),
}

impl ReplInput {
    /// Standard input, read through the line editor when standard input and
    /// standard output are terminals and `TERM` is not `dumb`.
    pub(crate) fn stdio() -> Self {
        let stdin = io::stdin();
        #[cfg(target_os = "linux")]
        {
            let dumb = matches!(std::env::var("TERM").as_deref(), Ok("dumb"));
            if stdin.is_terminal() && io::stdout().is_terminal() && !dumb {
                return Self::Terminal(terminal::TerminalLines::new(stdin.lock()));
            }
        }
        Self::Piped(io::BufReader::new(stdin.lock()))
    }
}

impl Read for ReplInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Piped(input) => input.read(buf),
            #[cfg(target_os = "linux")]
            Self::Terminal(input) => {
                let available = input.fill_buf()?;
                let count = available.len().min(buf.len());
                buf[..count].copy_from_slice(&available[..count]);
                input.consume(count);
                Ok(count)
            }
        }
    }
}

impl BufRead for ReplInput {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        match self {
            Self::Piped(input) => input.fill_buf(),
            #[cfg(target_os = "linux")]
            Self::Terminal(input) => input.fill_buf(),
        }
    }

    fn consume(&mut self, amount: usize) {
        match self {
            Self::Piped(input) => input.consume(amount),
            #[cfg(target_os = "linux")]
            Self::Terminal(input) => input.consume(amount),
        }
    }
}

#[cfg(target_os = "linux")]
mod terminal {
    use std::collections::VecDeque;
    use std::io::{self, BufRead, Write};

    use rustix::termios::{
        InputModes, LocalModes, OptionalActions, SpecialCodeIndex, Termios, tcgetattr, tcsetattr,
    };

    use crate::repl::MAX_INPUT_BYTES;
    use crate::repl_commands::COMMANDS;

    /// The most submitted inputs the editor recalls.
    const HISTORY_LIMIT: usize = 256;
    /// The most parameter bytes an escape sequence may carry.
    const MAX_PARAMETERS: usize = 16;
    /// Rings the terminal bell, for a key with nothing to do.
    const BELL: u8 = 0x07;
    const ERASE_TO_END: &[u8] = b"\x1b[K";
    const CURSOR_RIGHT: &[u8] = b"\x1b[1C";

    /// What one input byte did to the line being edited.
    #[derive(Debug, PartialEq, Eq)]
    enum Step {
        /// The line is still open.
        Continue,
        /// The line was submitted, with its terminating newline.
        Submit(String),
        /// Ctrl-D on an empty line: the input is over.
        Eof,
    }

    /// Where the parser stands within an escape sequence.
    #[derive(Debug, Default, PartialEq, Eq)]
    enum Escape {
        #[default]
        Ground,
        /// After ESC.
        Introducer,
        /// After ESC `[`, with the parameter bytes read so far.
        Csi(Vec<u8>),
        /// After ESC `O`.
        Ss3,
    }

    /// The line being edited, the inputs recalled from earlier in the session,
    /// and the parser state of a key sequence split across bytes.
    #[derive(Default)]
    struct LineEditor {
        line: String,
        /// Byte offset of the cursor within `line`, on a character boundary.
        cursor: usize,
        history: VecDeque<String>,
        /// The history index shown while Up and Down are recalling inputs.
        recall: Option<usize>,
        /// The line that was being typed when recall began.
        draft: String,
        /// Bytes of a multi-byte character that is not yet complete.
        pending: Vec<u8>,
        escape: Escape,
    }

    impl LineEditor {
        /// Applies one input byte, appending what the terminal must show.
        fn feed(&mut self, byte: u8, out: &mut Vec<u8>) -> Step {
            match std::mem::take(&mut self.escape) {
                Escape::Ground => {}
                Escape::Introducer => {
                    self.escape = match byte {
                        b'[' => Escape::Csi(Vec::new()),
                        b'O' => Escape::Ss3,
                        _ => Escape::Ground,
                    };
                    return Step::Continue;
                }
                Escape::Csi(mut parameters) => {
                    match byte {
                        0x20..=0x3f if parameters.len() < MAX_PARAMETERS => {
                            parameters.push(byte);
                            self.escape = Escape::Csi(parameters);
                        }
                        0x40..=0x7e => self.key(&parameters, byte, out),
                        _ => {}
                    }
                    return Step::Continue;
                }
                Escape::Ss3 => {
                    self.key(&[], byte, out);
                    return Step::Continue;
                }
            }
            // Every byte of a multi-byte character is at least 0x80, so an
            // ASCII byte always ends an incomplete character.
            if byte < 0x80 {
                self.pending.clear();
            }
            match byte {
                b'\r' | b'\n' => return self.submit(out),
                0x03 => {
                    // Ctrl-C abandons the line. The console reads a blank
                    // submission in its place, which changes nothing and is
                    // not recorded.
                    out.extend_from_slice(b"^C\n");
                    self.reset();
                    return Step::Submit("\n".to_owned());
                }
                0x04 => {
                    if self.line.is_empty() {
                        out.push(b'\n');
                        return Step::Eof;
                    }
                    self.delete_forward(out);
                }
                0x1b => self.escape = Escape::Introducer,
                0x01 => self.home(out),
                0x02 => self.left(out),
                0x05 => self.end(out),
                0x06 => self.right(out),
                0x08 | 0x7f => self.delete_back(out),
                b'\t' => self.complete(out),
                0x0b => self.kill_to_end(out),
                0x0e => self.history_down(out),
                0x10 => self.history_up(out),
                0x15 => self.kill_to_start(out),
                0x20..=0x7e => {
                    let mut buffer = [0u8; 4];
                    self.insert(char::from(byte).encode_utf8(&mut buffer), out);
                }
                0x80..=0xff => self.push_utf8(byte, out),
                _ => {}
            }
            Step::Continue
        }

        /// The input ended. A partial line is submitted as it stands; an empty
        /// one is not.
        fn end_of_input(&mut self, out: &mut Vec<u8>) -> Option<String> {
            out.push(b'\n');
            let text = std::mem::take(&mut self.line);
            self.record(&text);
            self.reset();
            (!text.is_empty()).then_some(text)
        }

        fn submit(&mut self, out: &mut Vec<u8>) -> Step {
            out.push(b'\n');
            let mut text = std::mem::take(&mut self.line);
            self.record(&text);
            self.reset();
            text.push('\n');
            Step::Submit(text)
        }

        fn reset(&mut self) {
            self.line.clear();
            self.cursor = 0;
            self.recall = None;
            self.draft.clear();
            self.pending.clear();
        }

        /// Keeps `text` as the newest recalled input. Blank input, and input
        /// equal to the newest one, are not kept.
        fn record(&mut self, text: &str) {
            if text.trim().is_empty() || self.history.back().is_some_and(|last| last == text) {
                return;
            }
            if self.history.len() == HISTORY_LIMIT {
                self.history.pop_front();
            }
            self.history.push_back(text.to_owned());
        }

        /// Applies the key named by an escape sequence. `parameters` are the
        /// bytes between `[` and the final byte, and are empty for `ESC O`.
        fn key(&mut self, parameters: &[u8], final_byte: u8, out: &mut Vec<u8>) {
            match (parameters, final_byte) {
                (_, b'A') => self.history_up(out),
                (_, b'B') => self.history_down(out),
                (_, b'C') => self.right(out),
                (_, b'D') => self.left(out),
                (_, b'H') | (b"1" | b"7", b'~') => self.home(out),
                (_, b'F') | (b"4" | b"8", b'~') => self.end(out),
                (b"3", b'~') => self.delete_forward(out),
                _ => {}
            }
        }

        /// Inserts `text` at the cursor and echoes it, with the text after it
        /// rewritten and the cursor returned to its place.
        fn insert(&mut self, text: &str, out: &mut Vec<u8>) {
            // The newline that submits the line counts against the input limit.
            if self.line.len() + text.len() + 1 > MAX_INPUT_BYTES {
                out.push(BELL);
                return;
            }
            self.recall = None;
            self.line.insert_str(self.cursor, text);
            self.cursor += text.len();
            out.extend_from_slice(text.as_bytes());
            let tail = &self.line[self.cursor..];
            out.extend_from_slice(tail.as_bytes());
            move_left(out, tail.chars().count());
        }

        fn push_utf8(&mut self, byte: u8, out: &mut Vec<u8>) {
            self.pending.push(byte);
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    let text = text.to_owned();
                    self.pending.clear();
                    self.insert(&text, out);
                }
                Err(error) if error.error_len().is_none() && self.pending.len() < 4 => {}
                Err(_) => self.pending.clear(),
            }
        }

        fn delete_back(&mut self, out: &mut Vec<u8>) {
            let Some(previous) = self.line[..self.cursor].chars().next_back() else {
                return;
            };
            self.recall = None;
            self.cursor -= previous.len_utf8();
            self.line.remove(self.cursor);
            move_left(out, 1);
            self.redraw_tail(out);
        }

        fn delete_forward(&mut self, out: &mut Vec<u8>) {
            if self.cursor < self.line.len() {
                self.recall = None;
                self.line.remove(self.cursor);
                self.redraw_tail(out);
            }
        }

        /// Writes the text after the cursor over what was there, erases the
        /// cells a shorter text leaves behind, and returns the cursor to where
        /// it was.
        fn redraw_tail(&self, out: &mut Vec<u8>) {
            let tail = &self.line[self.cursor..];
            out.extend_from_slice(tail.as_bytes());
            out.extend_from_slice(ERASE_TO_END);
            move_left(out, tail.chars().count());
        }

        fn left(&mut self, out: &mut Vec<u8>) {
            if let Some(previous) = self.line[..self.cursor].chars().next_back() {
                self.cursor -= previous.len_utf8();
                move_left(out, 1);
            }
        }

        fn right(&mut self, out: &mut Vec<u8>) {
            if let Some(next) = self.line[self.cursor..].chars().next() {
                self.cursor += next.len_utf8();
                out.extend_from_slice(CURSOR_RIGHT);
            }
        }

        fn home(&mut self, out: &mut Vec<u8>) {
            move_left(out, self.line[..self.cursor].chars().count());
            self.cursor = 0;
        }

        fn end(&mut self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.line.as_bytes()[self.cursor..]);
            self.cursor = self.line.len();
        }

        fn kill_to_end(&mut self, out: &mut Vec<u8>) {
            if self.cursor < self.line.len() {
                self.recall = None;
                self.line.truncate(self.cursor);
                out.extend_from_slice(ERASE_TO_END);
            }
        }

        fn kill_to_start(&mut self, out: &mut Vec<u8>) {
            if self.cursor > 0 {
                self.recall = None;
                move_left(out, self.line[..self.cursor].chars().count());
                self.line.replace_range(..self.cursor, "");
                self.cursor = 0;
                self.redraw_tail(out);
            }
        }

        fn history_up(&mut self, out: &mut Vec<u8>) {
            let index = if let Some(current) = self.recall {
                let Some(earlier) = current.checked_sub(1) else {
                    return;
                };
                earlier
            } else {
                let Some(newest) = self.history.len().checked_sub(1) else {
                    return;
                };
                self.draft = self.line.clone();
                newest
            };
            self.recall = Some(index);
            let text = self.history[index].clone();
            self.replace_line(text, out);
        }

        fn history_down(&mut self, out: &mut Vec<u8>) {
            let Some(current) = self.recall else {
                return;
            };
            if let Some(next) = self.history.get(current + 1).cloned() {
                self.recall = Some(current + 1);
                self.replace_line(next, out);
            } else {
                self.recall = None;
                let draft = std::mem::take(&mut self.draft);
                self.replace_line(draft, out);
            }
        }

        /// Replaces the whole line with `text`, leaving the cursor at its end.
        fn replace_line(&mut self, text: String, out: &mut Vec<u8>) {
            move_left(out, self.line[..self.cursor].chars().count());
            out.extend_from_slice(text.as_bytes());
            out.extend_from_slice(ERASE_TO_END);
            self.cursor = text.len();
            self.line = text;
        }

        fn complete(&mut self, out: &mut Vec<u8>) {
            match self.completion() {
                Some(text) => self.insert(&text, out),
                None => out.push(BELL),
            }
        }

        /// The text TAB inserts at the cursor, when it completes anything. The
        /// cursor must sit in a `:command` name, followed by nothing or by
        /// whitespace. The text is the part every matching command name shares,
        /// followed by a space when exactly one name matches and nothing follows.
        fn completion(&self) -> Option<String> {
            let head = &self.line[..self.cursor];
            let tail = &self.line[self.cursor..];
            let prefix = head.strip_prefix(':')?;
            if prefix.contains(char::is_whitespace)
                || !(tail.is_empty() || tail.starts_with(char::is_whitespace))
            {
                return None;
            }
            let names: Vec<&str> = COMMANDS
                .iter()
                .copied()
                .filter(|name| name.starts_with(prefix))
                .collect();
            let (first, rest) = names.split_first()?;
            let shared = rest
                .iter()
                .fold(*first, |shared, name| longest_common_prefix(shared, name));
            let space = if names.len() == 1 && tail.is_empty() {
                " "
            } else {
                ""
            };
            let text = format!("{}{space}", &shared[prefix.len()..]);
            (!text.is_empty()).then_some(text)
        }
    }

    /// The longest leading run of characters `left` and `right` share.
    fn longest_common_prefix<'a>(left: &'a str, right: &str) -> &'a str {
        let shared: usize = left
            .chars()
            .zip(right.chars())
            .take_while(|(a, b)| a == b)
            .map(|(character, _)| character.len_utf8())
            .sum();
        &left[..shared]
    }

    /// Moves the cursor `columns` to the left.
    fn move_left(out: &mut Vec<u8>, columns: usize) {
        if columns > 0 {
            out.extend_from_slice(format!("\x1b[{columns}D").as_bytes());
        }
    }

    /// Lines typed at a terminal, edited in place. A line is read with the
    /// terminal in a mode where each byte arrives as it is typed, and the
    /// terminal's own mode is restored before the line is handed on, so the
    /// evaluation of the line runs with ordinary terminal behaviour.
    pub(crate) struct TerminalLines {
        input: io::StdinLock<'static>,
        editor: LineEditor,
        /// The submitted line, until the reader has consumed it.
        ready: Vec<u8>,
        /// How much of `ready` the reader has consumed.
        position: usize,
        /// Whether the terminal input has ended, after which nothing is read.
        ended: bool,
    }

    impl TerminalLines {
        pub(crate) fn new(input: io::StdinLock<'static>) -> Self {
            Self {
                input,
                editor: LineEditor::default(),
                ready: Vec::new(),
                position: 0,
                ended: false,
            }
        }

        pub(crate) fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.position >= self.ready.len() {
                self.ready.clear();
                self.position = 0;
                if !self.ended {
                    self.read_line()?;
                }
            }
            Ok(&self.ready[self.position..])
        }

        pub(crate) fn consume(&mut self, amount: usize) {
            self.position = self.position.saturating_add(amount).min(self.ready.len());
        }

        /// Reads one line into `ready`. When the input ends before a line is
        /// submitted, `ready` stays empty unless a partial line was typed.
        fn read_line(&mut self) -> io::Result<()> {
            let _mode = RawMode::enter()?;
            let mut echo = Vec::new();
            loop {
                let chunk = self.input.fill_buf()?;
                if chunk.is_empty() {
                    self.ended = true;
                    if let Some(text) = self.editor.end_of_input(&mut echo) {
                        self.ready = text.into_bytes();
                    }
                    return show(&echo);
                }
                let mut consumed = 0;
                let mut submitted = None;
                for &byte in chunk {
                    consumed += 1;
                    match self.editor.feed(byte, &mut echo) {
                        Step::Continue => {}
                        Step::Submit(text) => {
                            submitted = Some(text);
                            break;
                        }
                        Step::Eof => {
                            self.ended = true;
                            break;
                        }
                    }
                }
                self.input.consume(consumed);
                show(&echo)?;
                echo.clear();
                if let Some(text) = submitted {
                    self.ready = text.into_bytes();
                    return Ok(());
                }
                if self.ended {
                    return Ok(());
                }
            }
        }
    }

    /// Writes the echo of typed input to the terminal.
    fn show(bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    }

    /// The terminal's mode while a line is typed: bytes arrive as they are
    /// typed, neither echoed nor interpreted by the line discipline, so the
    /// editor owns the line. The saved mode is restored on drop.
    struct RawMode {
        saved: Termios,
    }

    impl RawMode {
        fn enter() -> io::Result<Self> {
            let stdin = io::stdin();
            let saved = tcgetattr(&stdin)?;
            let mut raw = saved.clone();
            raw.input_modes.remove(InputModes::IXON);
            raw.local_modes.remove(
                LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG | LocalModes::IEXTEN,
            );
            raw.special_codes[SpecialCodeIndex::VMIN] = 1;
            raw.special_codes[SpecialCodeIndex::VTIME] = 0;
            tcsetattr(&stdin, OptionalActions::Drain, &raw)?;
            Ok(Self { saved })
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // Nothing is left to report a failed restore to, so it is not checked.
            let _ = tcsetattr(io::stdin(), OptionalActions::Drain, &self.saved);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{BELL, LineEditor, MAX_INPUT_BYTES, Step, longest_common_prefix};

        /// Types `bytes` in order, returning the echo and every step that is
        /// not `Continue`.
        fn type_keys(editor: &mut LineEditor, bytes: &[u8]) -> (String, Vec<Step>) {
            let mut echo = Vec::new();
            let mut steps = Vec::new();
            for &byte in bytes {
                match editor.feed(byte, &mut echo) {
                    Step::Continue => {}
                    step => steps.push(step),
                }
            }
            (String::from_utf8(echo).expect("the echo is UTF-8"), steps)
        }

        #[test]
        fn enter_submits_the_line_with_its_newline_and_records_it() {
            let mut editor = LineEditor::default();
            let (echo, steps) = type_keys(&mut editor, b"1 + 1\r");
            assert_eq!(echo, "1 + 1\n");
            assert_eq!(steps, [Step::Submit("1 + 1\n".to_owned())]);
            assert_eq!(editor.history, ["1 + 1"]);
        }

        #[test]
        fn up_recalls_earlier_input_and_down_restores_the_draft() {
            let mut editor = LineEditor::default();
            type_keys(&mut editor, b"first\r");
            type_keys(&mut editor, b"dra");
            type_keys(&mut editor, b"\x1b[A");
            assert_eq!(editor.line, "first");
            type_keys(&mut editor, b"\x1b[B");
            assert_eq!(editor.line, "dra");
            assert_eq!(editor.cursor, "dra".len());
        }

        #[test]
        fn cursor_keys_insert_and_delete_in_place() {
            let mut editor = LineEditor::default();
            type_keys(&mut editor, b"ac");
            type_keys(&mut editor, b"\x1b[D");
            type_keys(&mut editor, b"b");
            assert_eq!(editor.line, "abc");
            type_keys(&mut editor, b"\x08");
            assert_eq!(editor.line, "ac");
            type_keys(&mut editor, b"\x1b[H\x1b[3~");
            assert_eq!(editor.line, "c");
            type_keys(&mut editor, b"\x05x");
            assert_eq!(editor.line, "cx");
        }

        #[test]
        fn tab_completes_a_unique_command_name_and_spaces_it() {
            let mut editor = LineEditor::default();
            let (echo, steps) = type_keys(&mut editor, b":he\t");
            assert_eq!(editor.line, ":help ");
            assert_eq!(editor.cursor, ":help ".len());
            assert_eq!(echo, ":help ");
            assert!(steps.is_empty());
        }

        #[test]
        fn tab_fills_a_unique_name_before_trailing_text() {
            let mut editor = LineEditor::default();
            type_keys(&mut editor, b":ti x\x1b[D\x1b[D\t");
            assert_eq!(editor.line, ":timing x");
            assert_eq!(editor.cursor, ":timing".len());
        }

        #[test]
        fn tab_beeps_when_the_matching_names_share_nothing_more() {
            let mut ambiguous = LineEditor::default();
            let (echo, _) = type_keys(&mut ambiguous, b":h\t");
            assert_eq!(ambiguous.line, ":h");
            assert_eq!(echo, ":h\u{7}");

            let mut bare = LineEditor::default();
            let (echo, _) = type_keys(&mut bare, b":\t");
            assert_eq!(bare.line, ":");
            assert_eq!(echo, ":\u{7}");
        }

        #[test]
        fn tab_outside_a_command_name_beeps_and_changes_nothing() {
            let mut editor = LineEditor::default();
            let (echo, _) = type_keys(&mut editor, b"1 + he\t");
            assert_eq!(editor.line, "1 + he");
            assert_eq!(echo, "1 + he\u{7}");
        }

        #[test]
        fn shared_prefix_stops_at_the_first_difference() {
            assert_eq!(longest_common_prefix("history", "help"), "h");
            assert_eq!(longest_common_prefix("timing", "table"), "t");
            assert_eq!(longest_common_prefix("open", "open"), "open");
        }

        #[test]
        fn ctrl_c_abandons_the_line_without_recording_it() {
            let mut editor = LineEditor::default();
            let (echo, steps) = type_keys(&mut editor, b"9 +\x03");
            assert_eq!(echo, "9 +^C\n");
            assert_eq!(steps, [Step::Submit("\n".to_owned())]);
            assert!(editor.line.is_empty());
            assert!(editor.history.is_empty());
        }

        #[test]
        fn ctrl_d_ends_input_on_an_empty_line_and_deletes_otherwise() {
            let mut editor = LineEditor::default();
            type_keys(&mut editor, b"ab\x1b[D\x04");
            assert_eq!(editor.line, "a");
            let (_, steps) = type_keys(&mut editor, b"\x7f\x04");
            assert_eq!(steps, [Step::Eof]);
        }

        #[test]
        fn multi_byte_characters_are_inserted_whole() {
            let mut editor = LineEditor::default();
            type_keys(&mut editor, "é".as_bytes());
            assert_eq!(editor.line, "é");
            type_keys(&mut editor, b"x");
            assert_eq!(editor.line, "éx");
            type_keys(&mut editor, b"\x7f");
            assert_eq!(editor.line, "é");
        }

        #[test]
        fn an_interrupted_multi_byte_character_is_dropped() {
            let mut editor = LineEditor::default();
            type_keys(&mut editor, &[0xc3, b'a']);
            assert_eq!(editor.line, "a");
        }

        #[test]
        fn a_line_stops_growing_before_the_input_limit() {
            let mut editor = LineEditor::default();
            let (echo, _) = type_keys(&mut editor, "a".repeat(MAX_INPUT_BYTES).as_bytes());
            // The submitting newline needs one of the bytes the limit allows.
            assert_eq!(editor.line.len(), MAX_INPUT_BYTES - 1);
            assert!(echo.ends_with(char::from(BELL)));
        }

        #[test]
        fn history_keeps_no_blank_inputs_and_no_consecutive_repeats() {
            let mut editor = LineEditor::default();
            type_keys(&mut editor, b"1\r");
            type_keys(&mut editor, b"\r");
            type_keys(&mut editor, b"1\r");
            type_keys(&mut editor, b"2\r");
            assert_eq!(editor.history, ["1", "2"]);
        }
    }
}
