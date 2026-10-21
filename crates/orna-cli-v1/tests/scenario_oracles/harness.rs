//! Product-command harness for the scenario oracles: the pinned oracle file,
//! one fresh `orna init` database per scenario, a bounded process runner, and
//! the pass/gap verdicts the runner reports.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// Byte-identical copy of the Orna 1.1.0 release `tests/scenarios.json`.
const SCENARIOS_JSON: &str = include_str!("../fixtures/reference/tests/scenarios.json");
/// The release `SHA256SUMS` entry for `tests/scenarios.json`.
const SCENARIOS_SHA256: &str = "93b8edb858bc0a674cd0256c6350bd7286062eb3e2a01accc964e135fdc24ff1";
/// Upper bound for one product command; a debug-build REPL insert takes a
/// few seconds, so this only stops a hung process.
const COMMAND_LIMIT: Duration = Duration::from_secs(240);
/// Concurrent scenarios: each one starts several product processes.
const MAX_WORKERS: usize = 4;

/// One oracle as published: its identity and title. The given/when/then text
/// stays in the vendored file.
pub struct Oracle {
    pub title: String,
}

/// Every scenario in the vendored release file, keyed by scenario ID, after
/// checking that the copy is still the release file.
static ORACLES: LazyLock<BTreeMap<String, Oracle>> = LazyLock::new(|| {
    assert_eq!(
        hex(&Sha256::digest(SCENARIOS_JSON.as_bytes())),
        SCENARIOS_SHA256,
        "the vendored scenario oracles no longer match the release file"
    );
    let document: serde_json::Value =
        serde_json::from_str(SCENARIOS_JSON).expect("the release oracle file is JSON");
    let scenarios = document["scenarios"]
        .as_array()
        .expect("the release oracle file lists scenarios");
    assert_eq!(
        document["count"].as_u64(),
        Some(scenarios.len() as u64),
        "the release oracle file counts its scenarios"
    );
    let mut oracles = BTreeMap::new();
    for scenario in scenarios {
        let id = scenario["id"].as_str().expect("an oracle has an ID");
        let title = scenario["title"].as_str().expect("an oracle has a title");
        let previous = oracles.insert(
            id.to_owned(),
            Oracle {
                title: title.to_owned(),
            },
        );
        assert!(previous.is_none(), "oracle {id} is published twice");
    }
    oracles
});

pub fn oracles() -> &'static BTreeMap<String, Oracle> {
    &ORACLES
}

/// The current 1.1.0 obligations (§39 "1.1 binary and source-hosted
/// profiles"): the final oracles and the 1.1 behavioural oracles.
pub fn is_release_obligation(id: &str) -> bool {
    id.starts_with("FINAL-") || id.starts_with("V110-")
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// Why a scenario step did not hold, with the product output that shows it.
#[derive(Debug)]
pub struct Failure(pub String);

/// An executor's result: every then-clause it checks held.
pub type Check = Result<(), Failure>;
/// A gap probe's result: `Ok(evidence)` while the blocker still reproduces;
/// `Err` once it does not.
pub type Probe = Result<String, Failure>;

/// Fails the enclosing executor or probe with a formatted reason.
macro_rules! ensure {
    ($condition:expr, $($reason:tt)+) => {
        if !$condition {
            return Err($crate::harness::Failure(format!($($reason)+)));
        }
    };
}
pub(crate) use ensure;

/// How a registered scenario is expected to behave today.
pub enum Expect {
    /// The executor drives the scenario end to end and must pass.
    Pass(fn(&mut Db) -> Check),
    /// A known gap. `bead` tracks the work that unblocks the scenario;
    /// `blocker` says what is missing; `probe` reproduces that blocker with
    /// product commands. When the probe stops reproducing it, the runner
    /// fails until someone writes the executor or records the next blocker.
    Gap {
        bead: &'static str,
        blocker: &'static str,
        probe: fn(&mut Db) -> Probe,
    },
}

pub struct Entry {
    pub id: &'static str,
    pub expect: Expect,
    /// The scenario mounts a VFS view. Without FUSE it is reported as
    /// unavailable, never passed; `ORNA_REQUIRE_FUSE=1` makes that a failure.
    pub requires_fuse: bool,
}

impl Entry {
    pub fn pass(id: &'static str, executor: fn(&mut Db) -> Check) -> Self {
        Self {
            id,
            expect: Expect::Pass(executor),
            requires_fuse: false,
        }
    }

    pub fn gap(
        id: &'static str,
        bead: &'static str,
        blocker: &'static str,
        probe: fn(&mut Db) -> Probe,
    ) -> Self {
        Self {
            id,
            expect: Expect::Gap {
                bead,
                blocker,
                probe,
            },
            requires_fuse: false,
        }
    }

    pub fn with_fuse(mut self) -> Self {
        self.requires_fuse = true;
        self
    }
}

pub enum Verdict {
    Pass,
    Gap(String),
    Unavailable(String),
    Fail(String),
    Stale(String),
}

impl Verdict {
    fn is_failure(&self) -> bool {
        matches!(self, Verdict::Fail(_) | Verdict::Stale(_))
    }
}

/// Whether VFS scenarios can mount, under the suite's FUSE policy:
/// `ORNA_SKIP_FUSE_TEST=1` opts out, and `ORNA_REQUIRE_FUSE=1` turns missing
/// prerequisites into a failure instead of an unavailable report.
fn fuse_unavailable() -> Option<String> {
    static REASON: LazyLock<Option<String>> = LazyLock::new(|| {
        let flag = |name: &str| std::env::var_os(name).is_some_and(|value| value == "1");
        let required = flag("ORNA_REQUIRE_FUSE");
        let reason = if flag("ORNA_SKIP_FUSE_TEST") {
            Some("ORNA_SKIP_FUSE_TEST=1 opts out of FUSE scenarios".to_owned())
        } else if let Err(error) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fuse")
        {
            Some(format!("/dev/fuse is unavailable: {error}"))
        } else if !["fusermount3", "fusermount"]
            .iter()
            .any(|helper| Command::new(helper).arg("-h").output().is_ok())
        {
            Some("neither fusermount3 nor fusermount is executable".to_owned())
        } else {
            None
        };
        assert!(
            !(required && reason.is_some()),
            "ORNA_REQUIRE_FUSE=1 but FUSE scenarios cannot run: {reason:?}"
        );
        reason
    });
    REASON.clone()
}

/// Runs one scenario in its own fresh database.
pub fn run_entry(entry: &Entry) -> Verdict {
    if entry.requires_fuse {
        if let Some(reason) = fuse_unavailable() {
            return Verdict::Unavailable(reason);
        }
    }
    let mut db = match Db::init() {
        Ok(db) => db,
        Err(Failure(reason)) => return Verdict::Fail(format!("orna init: {reason}")),
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| match &entry.expect {
        Expect::Pass(executor) => executor(&mut db).map(|()| String::new()),
        Expect::Gap { probe, .. } => probe(&mut db),
    }));
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(panic) => Err(Failure(format!(
            "the harness panicked: {}",
            panic_text(&panic)
        ))),
    };
    match (&entry.expect, outcome) {
        (Expect::Pass(_), Ok(_)) => Verdict::Pass,
        (Expect::Pass(_), Err(Failure(reason))) => {
            Verdict::Fail(format!("{reason}\n{}", db.transcript))
        }
        (Expect::Gap { .. }, Ok(evidence)) => Verdict::Gap(evidence),
        (Expect::Gap { .. }, Err(Failure(reason))) => Verdict::Stale(format!(
            "the blocker no longer reproduces: {reason}\n{}",
            db.transcript
        )),
    }
}

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
        .unwrap_or_else(|| "non-text panic".to_owned())
}

/// Runs every entry on a small worker pool and returns verdicts in entry
/// order.
pub fn run_entries(entries: &[Entry]) -> Vec<Verdict> {
    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_WORKERS)
        .min(entries.len().max(1));
    let mut verdicts = thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(entry) = entries.get(index) else {
                            break done;
                        };
                        done.push((index, run_entry(entry)));
                    }
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("a scenario worker finished"))
            .collect::<Vec<_>>()
    });
    verdicts.sort_by_key(|(index, _)| *index);
    verdicts.into_iter().map(|(_, verdict)| verdict).collect()
}

/// Prints one line per scenario and fails when any executor failed or any
/// gap no longer reproduces its blocker.
pub fn report(entries: &[Entry], verdicts: &[Verdict]) {
    let oracles = oracles();
    let mut text = String::new();
    let mut failures = String::new();
    let (mut passed, mut gaps, mut unavailable) = (0, 0, 0);
    for (entry, verdict) in entries.iter().zip(verdicts) {
        let title = oracles
            .get(entry.id)
            .map_or("<not in the release oracle file>", |oracle| &oracle.title);
        let line = match (verdict, &entry.expect) {
            (Verdict::Pass, _) => {
                passed += 1;
                format!("PASS   {:<22} {title}", entry.id)
            }
            (Verdict::Gap(evidence), Expect::Gap { bead, blocker, .. }) => {
                gaps += 1;
                format!(
                    "GAP    {:<22} {title}\n         blocked by {bead}: {blocker}\n         observed: {evidence}",
                    entry.id
                )
            }
            (Verdict::Unavailable(reason), _) => {
                unavailable += 1;
                format!(
                    "UNAVAIL {:<21} {title}\n         not run: {reason}",
                    entry.id
                )
            }
            (Verdict::Fail(reason), _) => format!("FAIL   {:<22} {title}\n{reason}", entry.id),
            (Verdict::Stale(reason), Expect::Gap { bead, .. }) => format!(
                "STALE  {:<22} {title}\n         listed as blocked by {bead}, but {reason}",
                entry.id
            ),
            (Verdict::Gap(_) | Verdict::Stale(_), Expect::Pass(_)) => {
                unreachable!("an executor cannot produce a gap verdict")
            }
        };
        if verdict.is_failure() {
            let _ = writeln!(failures, "{line}");
        }
        let _ = writeln!(text, "{line}");
    }
    let _ = writeln!(
        text,
        "{} scenarios: {passed} pass, {gaps} known gaps, {unavailable} not run without FUSE, {} failing",
        entries.len(),
        entries.len() - passed - gaps - unavailable
    );
    println!("{text}");
    assert!(
        failures.is_empty(),
        "scenario oracle failures (FAIL: an executor no longer passes; STALE: a listed \
         gap's blocker no longer reproduces, so write the scenario's executor or record \
         its next blocker):\n{failures}"
    );
}

/// The output and exit status of one product or Git command.
pub struct Run {
    pub command: String,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Standard output exactly as written, for binary Git objects.
    pub raw_stdout: Vec<u8>,
}

impl Run {
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }

    /// Standard output followed by standard error.
    pub fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// The first `error[...]` line, or the first output line.
    pub fn diagnostic(&self) -> String {
        let output = self.output();
        output
            .lines()
            .map(|line| line.trim_start_matches("> ").trim())
            .find(|line| line.starts_with("error["))
            .or_else(|| output.lines().map(str::trim).find(|line| !line.is_empty()))
            .unwrap_or("<no output>")
            .to_owned()
    }

    fn describe(&self) -> String {
        let status = self.code.map_or_else(
            || "no exit status".to_owned(),
            |code| format!("exit {code}"),
        );
        format!("`{}` ({status}): {}", self.command, excerpt(&self.output()))
    }

    /// The command succeeded.
    pub fn ok(self) -> Result<Run, Failure> {
        if self.succeeded() {
            Ok(self)
        } else {
            Err(Failure(format!("expected success: {}", self.describe())))
        }
    }
}

fn excerpt(text: &str) -> String {
    const LIMIT: usize = 1500;
    let text = text.trim_end();
    if text.len() <= LIMIT {
        return text.to_owned();
    }
    let mut end = LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// A database created by `orna init` in a private directory, with private
/// `HOME`/`XDG_*` directories and a test commit identity.
pub struct Db {
    directory: TempDir,
    root: PathBuf,
    home: PathBuf,
    transcript: String,
}

impl Db {
    pub fn init() -> Result<Db, Failure> {
        Self::create(None)
    }

    /// `orna init` inside a Git repository created with `--object-format`.
    pub fn init_with_object_format(format: &str) -> Result<Db, Failure> {
        Self::create(Some(format))
    }

    fn create(object_format: Option<&str>) -> Result<Db, Failure> {
        let directory =
            TempDir::new().map_err(|error| Failure(format!("temporary directory: {error}")))?;
        let home = directory.path().join("home");
        fs::create_dir(&home).map_err(|error| Failure(format!("home directory: {error}")))?;
        let root = directory.path().join("db");
        let mut db = Db {
            root: root.clone(),
            home,
            directory,
            transcript: String::new(),
        };
        let parent = db.directory.path().to_path_buf();
        let root_text = path_text(&root)?;
        if let Some(format) = object_format {
            let format = format!("--object-format={format}");
            db.run_in(&parent, "git", &["init", "-q", &format, &root_text], None)?
                .ok()?;
        }
        db.run_in(&parent, "orna", &["init", &root_text], None)?
            .ok()?;
        for (name, value) in [
            ("user.name", "Orna Scenario"),
            ("user.email", "orna-scenario@example.invalid"),
            ("commit.gpgsign", "false"),
        ] {
            db.git(&["config", name, value])?.ok()?;
        }
        Ok(db)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every command this database ran, with its output.
    pub fn transcript(&self) -> &str {
        &self.transcript
    }

    /// Runs `orna ARGS` with `editor` as `EDITOR` and `VISUAL`.
    pub fn orna_with_editor(&mut self, editor: &Path, arguments: &[&str]) -> Result<Run, Failure> {
        let label = format!("orna {} (EDITOR={})", arguments.join(" "), editor.display());
        let mut command = self.command("orna");
        command
            .args(arguments)
            .env("EDITOR", editor)
            .env("VISUAL", editor)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let run = execute(command, label, None)?;
        let _ = writeln!(
            self.transcript,
            "$ {}\n{}",
            run.command,
            indent(&excerpt(&run.output()))
        );
        Ok(run)
    }

    /// A scratch directory beside the database, outside its worktree.
    pub fn scratch(&self, name: &str) -> Result<PathBuf, Failure> {
        let path = self.directory.path().join(name);
        fs::create_dir_all(&path).map_err(|error| Failure(format!("{name}: {error}")))?;
        Ok(path)
    }

    pub fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> Result<(), Failure> {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| Failure(format!("{relative}: {error}")))?;
        }
        fs::write(&path, contents).map_err(|error| Failure(format!("write {relative}: {error}")))
    }

    pub fn read(&self, relative: &str) -> Result<Vec<u8>, Failure> {
        fs::read(self.root.join(relative))
            .map_err(|error| Failure(format!("read {relative}: {error}")))
    }

    /// Runs `orna ARGS` in the database root.
    pub fn orna(&mut self, arguments: &[&str]) -> Result<Run, Failure> {
        let root = self.root.clone();
        self.run_in(&root, "orna", arguments, None)
    }

    /// Runs `git ARGS` in the database root.
    pub fn git(&mut self, arguments: &[&str]) -> Result<Run, Failure> {
        let root = self.root.clone();
        self.run_in(&root, "git", arguments, None)
    }

    /// The database root as text, for paths written into Orna source.
    pub fn root_text(&self) -> Result<String, Failure> {
        path_text(&self.root)
    }

    /// One REPL session that submits `lines` and then `:quit`.
    pub fn repl(&mut self, lines: &[&str]) -> Result<Run, Failure> {
        let root = self.root.clone();
        let root_text = path_text(&root)?;
        let mut input = lines.join("\n");
        input.push_str("\n:quit\n");
        self.run_in(&root, "orna", &["--db", &root_text, "repl"], Some(&input))
    }

    /// Submits one effectful REPL expression in its own session and requires
    /// the REPL to accept it.
    pub fn repl_effect(&mut self, expression: &str) -> Result<Run, Failure> {
        let run = self.repl(&[expression])?;
        ensure!(
            run.succeeded() && !run.output().contains("error["),
            "the REPL rejected `{expression}`: {}",
            run.describe()
        );
        Ok(run)
    }

    /// Writes `main.orna`, stages it with Git and commits it with
    /// `orna commit`, returning each admitted table's relation ID.
    pub fn declare(&mut self, source: &str) -> Result<BTreeMap<String, String>, Failure> {
        self.write("main.orna", source)?;
        self.git(&["add", "main.orna", ".orna/database.orna"])?
            .ok()?;
        let committed = self
            .orna(&["commit", "-m", "Declare the scenario schema"])?
            .ok()?;
        Ok(committed
            .stdout
            .lines()
            .filter_map(|line| line.strip_prefix("admitted table "))
            .filter_map(|admitted| admitted.split_once(" as relation "))
            .map(|(table, relation)| (table.to_owned(), relation.trim().to_owned()))
            .collect())
    }

    /// The relation ID `orna commit` reported for `table`.
    pub fn relation(tables: &BTreeMap<String, String>, table: &str) -> Result<String, Failure> {
        tables.get(table).cloned().ok_or_else(|| {
            Failure(format!(
                "orna commit did not admit table {table}: {tables:?}"
            ))
        })
    }

    pub fn publish(&mut self) -> Result<Run, Failure> {
        self.orna(&["publish", "--message", "Publish the scenario rows"])
    }

    /// The runtime mutation count `orna status --short` reports.
    pub fn runtime_mutations(&mut self) -> Result<u64, Failure> {
        let status = self.orna(&["status", "--short"])?.ok()?;
        status
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("RM "))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|count| count.parse().ok())
            .ok_or_else(|| {
                Failure(format!(
                    "no runtime summary in status: {}",
                    status.describe()
                ))
            })
    }

    /// `orna query RELATION ARGS --format json`.
    pub fn query_json(
        &mut self,
        relation: &str,
        arguments: &[&str],
    ) -> Result<serde_json::Value, Failure> {
        let mut command = vec!["query", relation];
        command.extend_from_slice(arguments);
        command.extend_from_slice(&["--format", "json"]);
        let listed = self.orna(&command)?.ok()?;
        serde_json::from_str(&listed.stdout).map_err(|error| {
            Failure(format!(
                "orna query printed no JSON ({error}): {}",
                listed.describe()
            ))
        })
    }

    /// Git output of a command that must succeed.
    pub fn git_text(&mut self, arguments: &[&str]) -> Result<String, Failure> {
        Ok(self.git(arguments)?.ok()?.stdout)
    }

    /// Starts `orna serve` on a free loopback port and waits for it to
    /// announce the address.
    pub fn serve(&mut self) -> Result<Server, Failure> {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map_err(|error| Failure(format!("free port: {error}")))?
            .port();
        let mut command = self.command("orna");
        command
            .args(["serve", "--port", &port.to_string()])
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command
            .spawn()
            .map_err(|error| Failure(format!("orna serve did not start: {error}")))?;
        let mut server = Server { child, port };
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = server.child.try_wait() {
                let mut output = String::new();
                if let Some(mut stderr) = server.child.stderr.take() {
                    let _ = stderr.read_to_string(&mut output);
                }
                return Err(Failure(format!(
                    "orna serve exited ({status}): {}",
                    excerpt(&output)
                )));
            }
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                let _ = writeln!(self.transcript, "$ orna serve --port {port}");
                return Ok(server);
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(Failure(
            "orna serve did not listen within 60 seconds".to_owned(),
        ))
    }

    /// The database UUID `orna init` recorded in `.orna/database.orna`.
    pub fn database_id(&self) -> Result<String, Failure> {
        let metadata = String::from_utf8(self.read(".orna/database.orna")?)
            .map_err(|_| Failure("the database metadata is not UTF-8".to_owned()))?;
        metadata
            .split('"')
            .find(|field| {
                field.len() == 36
                    && field.chars().filter(|character| *character == '-').count() == 4
            })
            .map(str::to_owned)
            .ok_or_else(|| Failure(format!("no database UUID in {metadata}")))
    }

    /// Starts `orna ARGS` in the database root with extra environment and
    /// returns the running child for the caller to wait for or kill.
    pub fn spawn_orna(
        &mut self,
        arguments: &[&str],
        environment: &[(&str, &str)],
    ) -> Result<Child, Failure> {
        let mut command = self.command("orna");
        command
            .args(arguments)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in environment {
            command.env(name, value);
        }
        let _ = writeln!(
            self.transcript,
            "$ orna {} (background)",
            arguments.join(" ")
        );
        command.spawn().map_err(|error| {
            Failure(format!(
                "`orna {}` did not start: {error}",
                arguments.join(" ")
            ))
        })
    }

    /// Mounts the database at a fresh directory beside it: `["--at", "HEAD"]`
    /// for a snapshot view, `["--cwd"]` for the workspace view. The view is
    /// released when the guard drops.
    pub fn mount(&mut self, name: &str, mode: &[&str]) -> Result<Mount, Failure> {
        let point = self.scratch(name)?;
        let point_text = path_text(&point)?;
        let mut command = self.command("orna");
        command
            .arg("mount")
            .arg(&point_text)
            .args(mode)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command
            .spawn()
            .map_err(|error| Failure(format!("orna mount did not start: {error}")))?;
        let mut unmount = self.command("orna");
        unmount
            .args(["unmount", &point_text])
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut mount = Mount {
            point,
            child,
            unmount,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !mounted(&mount.point) {
            if let Ok(Some(status)) = mount.child.try_wait() {
                return Err(Failure(format!(
                    "orna mount {point_text} {} exited ({status}) before the view attached",
                    mode.join(" ")
                )));
            }
            if Instant::now() >= deadline {
                return Err(Failure(format!(
                    "orna mount {point_text} {} did not attach within 60 seconds",
                    mode.join(" ")
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = writeln!(
            self.transcript,
            "$ orna mount {point_text} {}",
            mode.join(" ")
        );
        Ok(mount)
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(match program {
            "orna" => env!("CARGO_BIN_EXE_orna-cli-v1"),
            other => other,
        });
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("LC_ALL", "C");
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
            "GIT_CONFIG_COUNT",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn run_in(
        &mut self,
        directory: &Path,
        program: &str,
        arguments: &[&str],
        input: Option<&str>,
    ) -> Result<Run, Failure> {
        let label = format!("{program} {}", arguments.join(" "));
        let mut command = self.command(program);
        command
            .args(arguments)
            .current_dir(directory)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let run = execute(command, label, input)?;
        let _ = writeln!(
            self.transcript,
            "$ {}\n{}",
            run.command,
            indent(&excerpt(&run.output()))
        );
        Ok(run)
    }
}

fn indent(text: &str) -> String {
    text.lines().map(|line| format!("    {line}\n")).collect()
}

fn path_text(path: &Path) -> Result<String, Failure> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Failure(format!("non-UTF-8 test path {}", path.display())))
}

/// Runs a prepared command to completion, killing it after `COMMAND_LIMIT`.
fn execute(mut command: Command, label: String, input: Option<&str>) -> Result<Run, Failure> {
    let mut child = command
        .spawn()
        .map_err(|error| Failure(format!("`{label}` did not start: {error}")))?;
    let writer = input.map(|input| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input = input.to_owned();
        thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        })
    });
    let stdout = drain(child.stdout.take().expect("piped stdout"));
    let stderr = drain(child.stderr.take().expect("piped stderr"));
    let deadline = Instant::now() + COMMAND_LIMIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let raw_stdout = stdout.join().unwrap_or_default();
    let mut stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    if status.is_none() {
        let _ = write!(
            stderr,
            "\n<killed after {} seconds>",
            COMMAND_LIMIT.as_secs()
        );
    }
    Ok(Run {
        command: label,
        code: status.and_then(|status| status.code()),
        stdout: String::from_utf8_lossy(&raw_stdout).into_owned(),
        stderr,
        raw_stdout,
    })
}

fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// An attached `orna mount` view, released when dropped.
pub struct Mount {
    pub point: PathBuf,
    child: Child,
    unmount: Command,
}

impl Drop for Mount {
    fn drop(&mut self) {
        let _ = self.unmount.output();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if mounted(&self.point) {
            for helper in ["fusermount3", "fusermount"] {
                if Command::new(helper)
                    .arg("-u")
                    .arg(&self.point)
                    .output()
                    .is_ok_and(|output| output.status.success())
                {
                    break;
                }
            }
        }
    }
}

/// Whether `/proc/self/mountinfo` lists `point` as a mount point.
fn mounted(point: &Path) -> bool {
    let Ok(canonical) = fs::canonicalize(point) else {
        return false;
    };
    let Some(canonical) = canonical.to_str() else {
        return false;
    };
    fs::read_to_string("/proc/self/mountinfo").is_ok_and(|info| {
        info.lines()
            .any(|line| line.split(' ').nth(4) == Some(canonical))
    })
}

/// A running `orna serve`, stopped when dropped.
pub struct Server {
    child: Child,
    port: u16,
}

/// One HTTP/1.1 response.
pub struct Response {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

impl Response {
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl Server {
    /// Sends one request with `Connection: close` and reads the response.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Response, Failure> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))
            .map_err(|error| Failure(format!("connect to orna serve: {error}")))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .map_err(|error| Failure(format!("socket timeout: {error}")))?;
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.port,
            body.len()
        );
        for (name, value) in headers {
            let _ = write!(request, "{name}: {value}\r\n");
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(body))
            .map_err(|error| Failure(format!("send {method} {path}: {error}")))?;
        let mut bytes = Vec::new();
        stream
            .read_to_end(&mut bytes)
            .map_err(|error| Failure(format!("read {method} {path}: {error}")))?;
        let split = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| Failure(format!("{method} {path}: response has no header end")))?;
        let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| Failure(format!("{method} {path}: no status line in {head:?}")))?;
        Ok(Response {
            status,
            head,
            body: bytes[split + 4..].to_vec(),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The blocker reproduces when `orna ARGS` is not a command at all.
pub fn unknown_command(db: &mut Db, arguments: &[&str]) -> Probe {
    let run = db.orna(arguments)?;
    ensure!(
        run.code == Some(2) && run.output().contains("unknown Orna command"),
        "`orna {}` is now a command: {}",
        arguments.join(" "),
        run.describe()
    );
    Ok(format!(
        "orna {}: {}",
        arguments.join(" "),
        run.diagnostic()
    ))
}

/// The blocker reproduces when the project REPL answers `lines` with the
/// diagnostic code `code`.
pub fn repl_refuses(db: &mut Db, lines: &[&str], code: &str) -> Probe {
    let run = db.repl(lines)?;
    let needle = format!("error[{code}]");
    ensure!(
        run.output().contains(&needle),
        "the REPL no longer answers `{}` with {needle}: {}",
        lines.join("; "),
        run.describe()
    );
    Ok(format!("REPL `{}`: {}", lines.join("; "), run.diagnostic()))
}

/// The blocker reproduces when `orna check` rejects `source` with `code`.
pub fn check_refuses(db: &mut Db, source: &str, code: &str) -> Probe {
    db.write("main.orna", source)?;
    let run = db.orna(&["check"])?;
    ensure!(
        !run.succeeded() && run.output().contains(code),
        "orna check no longer rejects the source with {code}: {}",
        run.describe()
    );
    Ok(format!("orna check: {}", one_line(&run.output(), code)))
}

/// The first output line containing `needle`.
pub fn one_line(output: &str, needle: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .find(|line| line.contains(needle))
        .unwrap_or_default()
        .to_owned()
}
