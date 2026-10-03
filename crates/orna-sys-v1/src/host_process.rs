//! Explicit, allowlisted native process execution.

use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use orna_sys_macros::sys_host_operation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessProviderError {
    Denied,
    InvalidRequest,
    TimedOut,
    OutputLimit,
    Unavailable,
}

impl ProcessProviderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Denied => "sys.host.process.denied",
            Self::InvalidRequest => "sys.host.process.invalid_request",
            Self::TimedOut => "sys.host.process.timeout",
            Self::OutputLimit => "sys.host.process.output_limit",
            Self::Unavailable => "sys.host.process.unavailable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcessGrant {
    executable: PathBuf,
    working_directory_root: PathBuf,
    environment_names: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRunOutput {
    pub exit_status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Native process capability restricted to exact executables, directory roots,
/// child environment names, timeouts, and combined output bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessProvider {
    grants: Vec<ProcessGrant>,
    maximum_timeout: Duration,
    maximum_output_bytes: usize,
}

impl ProcessProvider {
    /// Creates an empty process allowlist with host-selected resource limits.
    pub fn new(
        maximum_timeout: Duration,
        maximum_output_bytes: usize,
    ) -> Result<Self, ProcessProviderError> {
        if maximum_timeout.is_zero() || maximum_output_bytes == 0 {
            return Err(ProcessProviderError::InvalidRequest);
        }
        Ok(Self {
            grants: Vec::new(),
            maximum_timeout,
            maximum_output_bytes,
        })
    }

    /// Adds one exact executable grant rooted at an existing directory.
    ///
    /// The working directory supplied at call time must resolve below this
    /// root. Child variables are always cleared and only the named variables
    /// can be supplied to the child.
    pub fn allow_command(
        &mut self,
        executable: impl AsRef<Path>,
        working_directory_root: impl AsRef<Path>,
        environment_names: impl IntoIterator<Item = String>,
    ) -> Result<(), ProcessProviderError> {
        let executable = executable
            .as_ref()
            .canonicalize()
            .map_err(|_| ProcessProviderError::InvalidRequest)?;
        let working_directory_root = working_directory_root
            .as_ref()
            .canonicalize()
            .map_err(|_| ProcessProviderError::InvalidRequest)?;
        if !executable.is_file() || !working_directory_root.is_dir() {
            return Err(ProcessProviderError::InvalidRequest);
        }
        let mut allowed_environment = BTreeSet::new();
        for name in environment_names {
            validate_environment_name(&name)?;
            if !allowed_environment.insert(name) {
                return Err(ProcessProviderError::InvalidRequest);
            }
        }
        let grant = ProcessGrant {
            executable,
            working_directory_root,
            environment_names: allowed_environment,
        };
        if self.grants.contains(&grant) {
            return Err(ProcessProviderError::InvalidRequest);
        }
        self.grants.push(grant);
        self.grants.sort_by(|left, right| {
            left.executable.cmp(&right.executable).then_with(|| {
                left.working_directory_root
                    .cmp(&right.working_directory_root)
            })
        });
        Ok(())
    }

    #[sys_host_operation(
        r###"{"name":"std.io.process.run","version":{"major":1,"minor":0},"signature":"fn std.io.process.run(executable: Str, arguments: [Str], working_directory: Str, environment: [(Str, Str)], input: Blob?, timeout: Duration?, max_output_bytes: Int): (Int?, Blob, Blob)","effects":["invoke"],"preconditions":["executable resolves to an exact host-allowlisted binary","working_directory resolves below an allowlisted root","child environment names are explicitly allowlisted","timeout and combined output are within provider limits"],"failures":["sys.host.process.denied","sys.host.process.invalid_request","sys.host.process.timeout","sys.host.process.output_limit","sys.host.process.unavailable"],"role":"host.std.io.process@1.0","provider":"orna.sys.host.process.v1","implementation":"run"}"###
    )]
    pub fn run(
        &self,
        executable: &str,
        arguments: &[String],
        working_directory: &str,
        environment: &[(String, String)],
        input: Option<&[u8]>,
        timeout: Option<Duration>,
        max_output_bytes: usize,
    ) -> Result<ProcessRunOutput, ProcessProviderError> {
        if executable.contains('\0')
            || working_directory.contains('\0')
            || arguments.iter().any(|argument| argument.contains('\0'))
            || environment
                .iter()
                .any(|(name, value)| name.contains('\0') || value.contains('\0'))
            || max_output_bytes == 0
            || max_output_bytes > self.maximum_output_bytes
        {
            return Err(ProcessProviderError::InvalidRequest);
        }
        if timeout.is_some_and(|timeout| timeout > self.maximum_timeout) {
            return Err(ProcessProviderError::Denied);
        }
        let executable = Path::new(executable)
            .canonicalize()
            .map_err(|_| ProcessProviderError::Denied)?;
        let working_directory = Path::new(working_directory)
            .canonicalize()
            .map_err(|_| ProcessProviderError::Denied)?;
        let grant = self
            .grants
            .iter()
            .find(|grant| {
                grant.executable == executable
                    && working_directory.starts_with(&grant.working_directory_root)
            })
            .ok_or(ProcessProviderError::Denied)?;
        let mut child_environment = BTreeSet::new();
        for (name, _) in environment {
            validate_environment_name(name)?;
            if !grant.environment_names.contains(name) || !child_environment.insert(name) {
                return Err(ProcessProviderError::Denied);
            }
        }

        let mut command = Command::new(executable);
        command
            .args(arguments)
            .current_dir(working_directory)
            .env_clear()
            .envs(environment.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // An omitted API timeout still inherits the host-selected cap. This
        // keeps every admitted process bounded even when callers use `null`.
        let effective_timeout = timeout.unwrap_or(self.maximum_timeout);
        let deadline = Instant::now()
            .checked_add(effective_timeout)
            .ok_or(ProcessProviderError::InvalidRequest)?;
        let mut child = command
            .spawn()
            .map_err(|_| ProcessProviderError::Unavailable)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(ProcessProviderError::Unavailable)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(ProcessProviderError::Unavailable)?;
        let stdin = child
            .stdin
            .take()
            .ok_or(ProcessProviderError::Unavailable)?;
        let capture = Arc::new(Mutex::new(Capture::default()));
        let abort = Arc::new(AtomicBool::new(false));
        let stdout_thread = capture_pipe(
            stdout,
            true,
            capture.clone(),
            abort.clone(),
            max_output_bytes,
        );
        let stderr_thread = capture_pipe(
            stderr,
            false,
            capture.clone(),
            abort.clone(),
            max_output_bytes,
        );
        let input_thread = if let Some(input) = input {
            let input = input.to_vec();
            let mut stdin = stdin;
            Some(thread::spawn(move || {
                stdin.write_all(&input).map_err(|_| ())
            }))
        } else {
            drop(stdin);
            None
        };
        let status = loop {
            if abort.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                join_reader(stdout_thread);
                join_reader(stderr_thread);
                if let Some(writer) = input_thread {
                    let _ = writer.join();
                }
                let capture = capture
                    .lock()
                    .expect("process capture mutex is not poisoned");
                return Err(if capture.output_limit_exceeded {
                    ProcessProviderError::OutputLimit
                } else {
                    ProcessProviderError::Unavailable
                });
            }
            let status = match child.try_wait() {
                Ok(status) => status,
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    join_reader(stdout_thread);
                    join_reader(stderr_thread);
                    if let Some(writer) = input_thread {
                        let _ = writer.join();
                    }
                    return Err(ProcessProviderError::Unavailable);
                }
            };
            if let Some(status) = status {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                join_reader(stdout_thread);
                join_reader(stderr_thread);
                if let Some(writer) = input_thread {
                    let _ = writer.join();
                }
                return Err(ProcessProviderError::TimedOut);
            }
            thread::sleep(Duration::from_millis(2));
        };

        join_reader(stdout_thread);
        join_reader(stderr_thread);
        if let Some(writer) = input_thread {
            if !matches!(writer.join(), Ok(Ok(()))) {
                return Err(ProcessProviderError::Unavailable);
            }
        }
        let mut capture = capture
            .lock()
            .expect("process capture mutex is not poisoned");
        if capture.output_limit_exceeded {
            return Err(ProcessProviderError::OutputLimit);
        }
        if capture.read_failed {
            return Err(ProcessProviderError::Unavailable);
        }
        Ok(ProcessRunOutput {
            exit_status: status.code(),
            stdout: std::mem::take(&mut capture.stdout),
            stderr: std::mem::take(&mut capture.stderr),
        })
    }
}

#[derive(Default)]
struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    total: usize,
    output_limit_exceeded: bool,
    read_failed: bool,
}

fn capture_pipe<R: Read + Send + 'static>(
    mut reader: R,
    is_stdout: bool,
    capture: Arc<Mutex<Capture>>,
    abort: Arc<AtomicBool>,
    maximum_output_bytes: usize,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return,
                Ok(count) => {
                    let mut capture = capture
                        .lock()
                        .expect("process capture mutex is not poisoned");
                    let remaining = maximum_output_bytes.saturating_sub(capture.total);
                    let accepted = count.min(remaining);
                    let bytes = &buffer[..accepted];
                    if is_stdout {
                        capture.stdout.extend_from_slice(bytes);
                    } else {
                        capture.stderr.extend_from_slice(bytes);
                    }
                    capture.total += accepted;
                    if accepted < count {
                        capture.output_limit_exceeded = true;
                        abort.store(true, Ordering::Release);
                        return;
                    }
                }
                Err(_) => {
                    capture
                        .lock()
                        .expect("process capture mutex is not poisoned")
                        .read_failed = true;
                    abort.store(true, Ordering::Release);
                    return;
                }
            }
        }
    })
}

fn join_reader(reader: thread::JoinHandle<()>) {
    let _ = reader.join();
}

fn validate_environment_name(name: &str) -> Result<(), ProcessProviderError> {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return Err(ProcessProviderError::InvalidRequest);
    };
    if !(first.is_ascii_uppercase() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ProcessProviderError::InvalidRequest);
    }
    Ok(())
}
