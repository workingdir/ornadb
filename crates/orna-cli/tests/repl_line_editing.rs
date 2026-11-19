//! Line editing in `orna repl` on a terminal (spec `17-repl.md`, "Syntax-aware
//! editing"): the console reads each line through its own editor, so Tab
//! completes a `:command` name and Up recalls an earlier submission. Persistent
//! history and the other editing aids the spec lists are not covered here.
//!
//! The REPL edits lines only when its standard streams are a terminal, so each
//! session runs under `script(1)` (util-linux), which gives the REPL a
//! pseudo-terminal and relays the test's typed keys and the REPL's output.

#![cfg(target_os = "linux")]

#[path = "support/product.rs"]
mod product;

use std::{
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

use product::*;

const BOOK_LIBRARY: &str = "pub table Book(id: Str) {\n    title: Str,\n}\n";
const PROMPT: &str = "> ";
const RESULT: &str = "42 : Int";
const DEADLINE: Duration = Duration::from_mins(1);

/// A committed project whose REPL sessions can evaluate expressions.
fn committed_library(directory: &tempfile::TempDir) -> PathBuf {
    let root = initialized_database(directory, BOOK_LIBRARY);
    commit_source(&root, "book library");
    root
}

/// `text` as one single-quoted shell word.
fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The terminal output the test has read so far.
#[derive(Default)]
struct ScreenState {
    bytes: Vec<u8>,
    /// Set once the REPL has exited and its output has been read to the end.
    closed: bool,
}

impl ScreenState {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    fn count(&self, needle: &str) -> usize {
        self.text().matches(needle).count()
    }
}

#[derive(Default)]
struct Screen {
    state: Mutex<ScreenState>,
    changed: Condvar,
}

/// Copies the terminal output into `screen` until the terminal closes.
fn relay(mut output: impl Read, screen: &Screen) {
    let mut chunk = [0u8; 4096];
    loop {
        match output.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                let mut state = screen.state.lock().expect("screen lock");
                state.bytes.extend_from_slice(&chunk[..count]);
                screen.changed.notify_all();
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let mut state = screen.state.lock().expect("screen lock");
    state.closed = true;
    screen.changed.notify_all();
}

/// An `orna repl` running on a pseudo-terminal. The test types a line only
/// after the REPL has answered the one before, so each key sequence reaches
/// the editor as one line. Dropping the session kills the process.
struct TerminalSession {
    child: Child,
    typed: ChildStdin,
    screen: Arc<Screen>,
}

impl TerminalSession {
    fn start(root: &Path) -> Self {
        let binary = env!("CARGO_BIN_EXE_orna");
        let db = root.to_str().expect("UTF-8 test path");
        let repl = format!(
            "exec {} --color never --db {} repl",
            shell_word(binary),
            shell_word(db),
        );
        let mut command = Command::new("script");
        isolate_git(&mut command);
        let mut child = command
            .args(["-q", "-e", "-c", repl.as_str(), "/dev/null"])
            .env("TERM", "xterm-kitty")
            .env_remove("TMUX")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start `script` to give the REPL a pseudo-terminal");
        let typed = child.stdin.take().expect("script stdin");
        let relayed = child.stdout.take().expect("script stdout");
        let screen = Arc::new(Screen::default());
        let sink = Arc::clone(&screen);
        thread::spawn(move || relay(relayed, &sink));
        Self {
            child,
            typed,
            screen,
        }
    }

    /// Types `keys` as the user's keystrokes.
    fn type_keys(&mut self, keys: &str) {
        self.typed
            .write_all(keys.as_bytes())
            .expect("type into the REPL terminal");
        self.typed.flush().expect("flush the REPL terminal input");
    }

    /// Waits until the terminal output satisfies `done`. Fails with the output
    /// so far if the terminal closes first or the deadline passes.
    fn wait_until(&self, what: &str, done: impl Fn(&ScreenState) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        let mut state = self.screen.state.lock().expect("screen lock");
        while !done(&state) {
            assert!(
                !state.closed,
                "the REPL closed its terminal before {what}:\n{}",
                state.text()
            );
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for {what}:\n{}",
                state.text()
            );
            state = self
                .screen
                .changed
                .wait_timeout(state, remaining)
                .expect("screen wait")
                .0;
        }
    }

    /// Waits for the REPL to exit and its terminal to close, so all of its
    /// output has been read, and returns its exit status.
    fn finish(&mut self) -> ExitStatus {
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll the REPL process") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "the REPL did not exit:\n{}",
                self.printed()
            );
            thread::sleep(Duration::from_millis(10));
        };
        self.wait_until("the terminal closes", |state| state.closed);
        status
    }

    /// Everything the REPL has printed so far.
    fn printed(&self) -> String {
        self.screen.state.lock().expect("screen lock").text()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // The outcome of a test is already decided by the time the session is
        // dropped, and a process that has exited has nothing left to kill.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn tab_completes_a_command_name_at_the_prompt() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = committed_library(&directory);
    let mut session = TerminalSession::start(&root);

    session.wait_until("the first prompt", |state| state.count(PROMPT) >= 1);
    // `:qu` matches only `:quit`, so Tab completes it and the submission
    // ends the session. Without completion the REPL would keep waiting.
    session.type_keys(":qu\t\r");
    let status = session.finish();

    let printed = session.printed();
    assert!(status.success(), "exit {status:?}:\n{printed}");
    assert!(printed.contains(":quit"), "{printed}");
    assert!(!printed.contains("unknown command"), "{printed}");
    assert!(!printed.contains("error["), "{printed}");
}

#[test]
fn up_recalls_the_previous_submission_and_runs_it_again() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = committed_library(&directory);
    let mut session = TerminalSession::start(&root);

    session.wait_until("the first prompt", |state| state.count(PROMPT) >= 1);
    session.type_keys("40 + 2\r");
    session.wait_until("the first result and the next prompt", |state| {
        state.count(RESULT) >= 1 && state.count(PROMPT) >= 2
    });
    session.type_keys("\x1b[A\r");
    session.wait_until("the recalled submission's result", |state| {
        state.count(RESULT) >= 2
    });
    session.type_keys(":quit\r");
    let status = session.finish();

    let printed = session.printed();
    assert!(status.success(), "exit {status:?}:\n{printed}");
    assert!(!printed.contains("error["), "{printed}");
}
