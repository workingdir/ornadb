//! Drives `orna-cli-v1 --db <dir> repl` as a child process for REPL tests.
//!
//! Scripted sessions write each line to stdin and then `:quit`; long-lived
//! sessions use [`ReplProcess`] so a test can submit lines, inspect output and
//! keep a second process running. Every wait is bounded by [`TIMEOUT`].

use std::{
    io::Write as _,
    path::Path,
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(120);

/// Captured result of a finished REPL process.
pub struct ReplRun {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// A running `orna-cli-v1 --db <dir> repl` process with reader threads.
pub struct ReplProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
}

fn capture<R: std::io::Read + Send + 'static>(mut pipe: R) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).expect("read REPL pipe");
        bytes
    })
}

impl ReplProcess {
    /// Starts `repl` with `directory` as the `--db` project.
    pub fn start(directory: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
            .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Orna REPL process");
        let stdin = child.stdin.take();
        let stdout = capture(child.stdout.take().expect("REPL stdout"));
        let stderr = capture(child.stderr.take().expect("REPL stderr"));
        Self {
            child,
            stdin,
            stdout,
            stderr,
        }
    }

    /// Writes one submission line; the helper appends the newline.
    pub fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("REPL stdin is open");
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .expect("write REPL submission");
        stdin.flush().expect("flush REPL submission");
    }

    /// Closes stdin so the REPL sees end of input.
    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// Closes stdin, waits up to the timeout and returns captured output.
    /// A process that does not exit in time is killed and the test fails.
    pub fn finish(mut self) -> ReplRun {
        self.close_stdin();
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll REPL process") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("Orna REPL did not exit within {TIMEOUT:?}");
            }
            thread::sleep(Duration::from_millis(10));
        };
        ReplRun {
            status,
            stdout: String::from_utf8_lossy(&self.stdout.join().expect("stdout reader"))
                .into_owned(),
            stderr: String::from_utf8_lossy(&self.stderr.join().expect("stderr reader"))
                .into_owned(),
        }
    }
}

/// Runs a scripted session: each line is submitted, then `:quit`.
pub fn run_lines(directory: &Path, lines: &[&str]) -> ReplRun {
    let mut process = ReplProcess::start(directory);
    for line in lines {
        process.send_line(line);
    }
    process.send_line(":quit");
    process.finish()
}

/// Runs the one-shot form `orna-cli-v1 --db <dir> repl <expression>`.
pub fn run_one_shot(directory: &Path, expression: &str) -> ReplRun {
    let output = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            directory.to_str().expect("UTF-8 path"),
            "repl",
            expression,
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Orna one-shot REPL process");
    ReplRun {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Creates a temporary project with `orna-cli-v1 init` and writes `files`
/// (relative path, contents) into it. The returned directory must outlive the test.
pub fn initialized_project(files: &[(&str, &str)]) -> TempDir {
    let directory = tempfile::tempdir().expect("temporary Orna project");
    let init = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("Orna init process");
    assert_eq!(init.status.code(), Some(0), "{:?}", init.stderr);
    for (path, contents) in files {
        let target = directory.path().join(path);
        std::fs::create_dir_all(target.parent().expect("file has a parent"))
            .expect("project directory");
        std::fs::write(target, contents).expect("project source");
    }
    directory
}
