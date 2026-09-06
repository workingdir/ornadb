#![allow(
    dead_code,
    reason = "The bounded binary exposes planning and session seams for a later integration adapter."
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::process::ExitCode;

const REPL_TARGET: &str = "std.cli.repl";
const SENSOR_SOURCE_IDENTITY: &str = "example:sensors:v1";

#[allow(
    dead_code,
    reason = "The bounded CLI records the complete specified exit-status space."
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Exit {
    Success = 0,
    Target = 1,
    Usage = 2,
    Connection = 3,
    Authorisation = 4,
    Presentation = 5,
    Cancelled = 6,
    Protocol = 7,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Diagnostic {
    code: &'static str,
    title: &'static str,
    help: &'static str,
    exit: Exit,
}
impl Diagnostic {
    const fn usage(code: &'static str, title: &'static str, help: &'static str) -> Self {
        Self {
            code,
            title,
            help,
            exit: Exit::Usage,
        }
    }
    const fn target(code: &'static str, title: &'static str, help: &'static str) -> Self {
        Self {
            code,
            title,
            help,
            exit: Exit::Target,
        }
    }
    const fn unavailable(title: &'static str, help: &'static str) -> Self {
        Self::target("E2000", title, help)
    }
}
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "error[{}]: {}\nhelp: {}",
            self.code, self.title, self.help
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Endpoint {
    ManagedLocal,
    Path(String),
    UnixSocket(String),
    RemoteTls(String),
}
impl Endpoint {
    fn parse(value: &str) -> Result<Self, Diagnostic> {
        if value.is_empty() {
            return Err(Diagnostic::usage(
                "E1004",
                "database endpoint is empty",
                "provide a local path, socket, or secure Orna URI",
            ));
        }
        if value.contains('@') || value.contains('#') || value.contains('?') {
            return Err(Diagnostic::usage(
                "E1005",
                "database endpoint contains unsupported credentials or URI parts",
                "use transport authentication and omit credentials, fragments, and query parameters",
            ));
        }
        if let Some(path) = value.strip_prefix("orna+unix://") {
            return (!path.is_empty())
                .then(|| Self::UnixSocket(path.to_owned()))
                .ok_or_else(|| {
                    Diagnostic::usage(
                        "E1004",
                        "database endpoint is empty",
                        "provide a socket path",
                    )
                });
        }
        if let Some(rest) = value.strip_prefix("orna://") {
            let mut parts = rest.splitn(2, '/');
            let authority = parts.next().unwrap_or_default();
            let name = parts.next().unwrap_or_default();
            if authority.is_empty() || name.is_empty() {
                return Err(Diagnostic::usage(
                    "E1004",
                    "database URI needs an authority and database name",
                    "use orna://HOST/DATABASE or orna://local/INSTANCE",
                ));
            }
            return Ok(if authority == "local" {
                Self::Path(name.to_owned())
            } else {
                Self::RemoteTls(value.to_owned())
            });
        }
        Ok(Self::Path(value.to_owned()))
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Invocation {
    Seed,
    Exercise,
    SensorsIngest,
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Repl,
    Init,
    Check,
    Run(Invocation),
    Help,
    Version,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Parsed {
    endpoint: Endpoint,
    command: Command,
}
fn parse_cli(arguments: &[String]) -> Result<Parsed, Diagnostic> {
    let mut endpoint = Endpoint::ManagedLocal;
    let mut words = arguments.iter().map(String::as_str).peekable();
    while matches!(words.peek(), Some(&"--db")) {
        words.next();
        endpoint = Endpoint::parse(words.next().ok_or_else(|| {
            Diagnostic::usage(
                "E1001",
                "option `--db` needs a value",
                "supply an endpoint after `--db`",
            )
        })?)?;
    }
    let command = match words.next() {
        None | Some("repl") => Command::Repl,
        Some("init") => Command::Init,
        Some("check") => Command::Check,
        Some("--help" | "-h" | "help") => Command::Help,
        Some("--version" | "-V") => Command::Version,
        Some("run") => match words.next() {
            Some("seed") => Command::Run(Invocation::Seed),
            Some("exercise") => Command::Run(Invocation::Exercise),
            Some("sensors.ingest") => Command::Run(Invocation::SensorsIngest),
            Some(_) => {
                return Err(Diagnostic::usage(
                    "E1002",
                    "reference invocation is not supported by this bounded slice",
                    "use `run seed`, `run exercise`, or `run sensors.ingest`",
                ));
            }
            None => {
                return Err(Diagnostic::usage(
                    "E1001",
                    "`run` needs a reference invocation",
                    "use `run seed`, `run exercise`, or `run sensors.ingest`",
                ));
            }
        },
        Some(_) => {
            return Err(Diagnostic::usage(
                "E1002",
                "command is not supported by this bounded slice",
                "use `repl`, `init`, `run`, `--help`, or `--version`",
            ));
        }
    };
    if words.next().is_some() {
        return Err(Diagnostic::usage(
            "E1003",
            "command has unexpected arguments",
            "this bounded slice accepts no extra arguments",
        ));
    }
    Ok(Parsed { endpoint, command })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplInput {
    Quit,
    Expression,
}
fn parse_repl_input(input: &str) -> Result<ReplInput, Diagnostic> {
    match input.trim() {
        ":quit" => Ok(ReplInput::Quit),
        text if text.starts_with(':') => Err(Diagnostic::usage(
            "E1101",
            "REPL console command is not supported by this bounded slice",
            "use `:quit` or submit an Orna expression for the session adapter",
        )),
        _ => Ok(ReplInput::Expression),
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionState {
    Open,
    Closed,
}
trait SessionAdapter {
    fn cancel(&mut self, child: u64) -> Result<(), Diagnostic>;
    fn drain_terminal(&mut self) -> Result<(), Diagnostic>;
    fn close_transport(&mut self) -> Result<(), Diagnostic>;
}
struct Session {
    state: SessionState,
    owned: BTreeSet<u64>,
}
impl Session {
    fn new() -> Self {
        Self {
            state: SessionState::Open,
            owned: BTreeSet::new(),
        }
    }
    fn own(&mut self, child: u64) -> Result<(), Diagnostic> {
        if self.state == SessionState::Closed {
            return Err(Diagnostic::target(
                "E1201",
                "cannot add an invocation to a closed session",
                "start a new root session before creating child actions",
            ));
        }
        self.owned.insert(child);
        Ok(())
    }
    fn close<A: SessionAdapter>(&mut self, adapter: &mut A) -> Result<bool, Diagnostic> {
        if self.state == SessionState::Closed {
            return Ok(false);
        }
        for child in &self.owned {
            adapter.cancel(*child)?;
        }
        adapter.drain_terminal()?;
        adapter.close_transport()?;
        self.owned.clear();
        self.state = SessionState::Closed;
        Ok(true)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
struct ReferenceState {
    initialized: bool,
    books: BTreeSet<&'static str>,
    loans: BTreeMap<&'static str, &'static str>,
    stock: BTreeMap<(&'static str, &'static str), i32>,
    readings: BTreeMap<(&'static str, u8), &'static str>,
    checkpoint: u8,
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum Step {
    Init,
    Seed,
    Exercise,
    Sensors,
    Noop(&'static str),
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Plan {
    steps: Vec<Step>,
}
fn seeded(s: &ReferenceState) -> bool {
    s.books == BTreeSet::from(["book-1", "book-2"])
        && s.stock == BTreeMap::from([(("north", "pencil"), 12), (("south", "pencil"), 4)])
}
fn exercised(s: &ReferenceState) -> bool {
    s.loans == BTreeMap::from([("book-1", "reader-1")])
        && s.stock == BTreeMap::from([(("north", "pencil"), 9), (("south", "pencil"), 7)])
}
fn seed(s: &mut ReferenceState) -> Result<(), Diagnostic> {
    if !s.initialized {
        return Err(Diagnostic::target(
            "E2002",
            "reference database is not initialized",
            "initialize it before invoking the reference seed",
        ));
    }
    if !s.books.is_empty() || !s.stock.is_empty() {
        return Err(Diagnostic::target(
            "E2003",
            "reference seed would create duplicate keys",
            "use a fresh database or workflow planning after confirming the seed postcondition",
        ));
    }
    s.books.extend(["book-1", "book-2"]);
    s.stock.insert(("north", "pencil"), 12);
    s.stock.insert(("south", "pencil"), 4);
    Ok(())
}
fn exercise(s: &mut ReferenceState) -> Result<(), Diagnostic> {
    if !seeded(s) {
        return Err(Diagnostic::target(
            "E2004",
            "reference exercise requires the exact seeded state",
            "seed an empty initialized reference database before exercising it",
        ));
    }
    if !s.loans.is_empty() {
        return Err(Diagnostic::target(
            "E2005",
            "reference exercise would create a duplicate loan key",
            "use workflow planning after confirming the exercise postcondition",
        ));
    }
    s.stock.insert(("north", "pencil"), 9);
    s.stock.insert(("south", "pencil"), 7);
    s.loans.insert("book-1", "reader-1");
    Ok(())
}
fn sensors(s: &mut ReferenceState) -> Result<(), Diagnostic> {
    if !s.initialized {
        return Err(Diagnostic::target(
            "E2002",
            "reference database is not initialized",
            "initialize it before invoking sensor ingestion",
        ));
    }
    for (sensor, sequence, value) in [
        ("greenhouse", 0, "18.25"),
        ("greenhouse", 1, "18.50"),
        ("greenhouse", 2, "18.75"),
    ]
    .into_iter()
    .skip(usize::from(s.checkpoint))
    {
        if s.readings.insert((sensor, sequence), value).is_some() {
            return Err(Diagnostic::target(
                "E2009",
                "sensor ingestion would create a duplicate reading key",
                "restore the matching checkpoint or use a fresh reference database",
            ));
        }
        s.checkpoint = sequence + 1;
    }
    Ok(())
}
fn plan(s: &ReferenceState) -> Result<Plan, Diagnostic> {
    let mut p = s.clone();
    let mut steps = Vec::new();
    if p.initialized {
        steps.push(Step::Noop("already initialized"));
    } else {
        p.initialized = true;
        steps.push(Step::Init);
    }
    if seeded(&p) || exercised(&p) {
        steps.push(Step::Noop("seed postcondition holds"));
    } else {
        seed(&mut p)?;
        steps.push(Step::Seed);
    }
    if exercised(&p) {
        steps.push(Step::Noop("exercise postcondition holds"));
    } else {
        exercise(&mut p)?;
        steps.push(Step::Exercise);
    }
    if p.checkpoint == 3 && p.readings.len() == 3 {
        steps.push(Step::Noop("sensor consumer at exhaustion"));
    } else {
        sensors(&mut p)?;
        steps.push(Step::Sensors);
    }
    Ok(Plan { steps })
}
fn apply(s: &mut ReferenceState, p: &Plan) -> Result<(), Diagnostic> {
    let mut candidate = s.clone();
    for step in &p.steps {
        match step {
            Step::Init => {
                if candidate.initialized {
                    return Err(Diagnostic::target(
                        "E2001",
                        "reference database is already initialized",
                        "use workflow planning to make initialization idempotent",
                    ));
                }
                candidate.initialized = true;
            }
            Step::Seed => seed(&mut candidate)?,
            Step::Exercise => exercise(&mut candidate)?,
            Step::Sensors => sensors(&mut candidate)?,
            Step::Noop(_) => {}
        }
    }
    *s = candidate;
    Ok(())
}

fn check_project(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let path = match endpoint {
        Endpoint::ManagedLocal => ".",
        Endpoint::Path(path) => path,
        Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => {
            return Err(Diagnostic::target(
                "E2100",
                "project checking requires a local Git worktree",
                "use `check` with the current worktree or `--db PATH check`",
            ));
        }
    };
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "project Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let project = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "project source could not be loaded",
                "fix the project module graph and source boundaries, then run check again",
            )
        })?;
    let analysis = orna_semantic_v1::analyze_with_catalogue(
        project.modules(),
        &orna_semantic_v1::Catalogue::authoritative_core(),
    );
    if analysis.is_ok() {
        println!("project valid");
        Ok(())
    } else {
        Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run check again",
        ))
    }
}

fn execute(parsed: &Parsed) -> Result<(), Diagnostic> {
    match parsed.command {
        Command::Help => {
            println!(
                "orna-cli-v1 [--db ENDPOINT] [repl|init|check|run seed|run exercise|run sensors.ingest]"
            );
            Ok(())
        }
        Command::Version => {
            println!("orna-cli-v1 0.1.0");
            Ok(())
        }
        Command::Init => Err(Diagnostic::unavailable(
            "database initialization is not available",
            "use an Orna runtime with repository initialization enabled",
        )),
        Command::Check => check_project(&parsed.endpoint),
        Command::Repl => Err(Diagnostic::unavailable(
            "the REPL runtime is not available",
            "use an Orna runtime with source execution enabled",
        )),
        Command::Run(_) => Err(Diagnostic::unavailable(
            "reference invocation execution is not available",
            "use an Orna runtime with source execution enabled",
        )),
    }
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let parsed = match parse_cli(&args) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(error.exit as u8);
        }
    };
    match execute(&parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit as u8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parsing_is_bounded_and_diagnostic_is_stable() {
        let parsed = parse_cli(&[
            "--db".into(),
            "orna://host/reference".into(),
            "run".into(),
            "seed".into(),
        ])
        .expect("parses");
        assert_eq!(parsed.command, Command::Run(Invocation::Seed));
        assert_eq!(
            parsed.endpoint,
            Endpoint::RemoteTls("orna://host/reference".into())
        );
        assert_eq!(
            parse_cli(&["check".into()]).unwrap().command,
            Command::Check
        );
        let error = parse_cli(&["serve".into()]).expect_err("unsupported");
        assert_eq!((error.code, error.exit), ("E1002", Exit::Usage));
    }
    #[test]
    fn check_loads_reachable_project_sources_and_ignores_unreachable_modules() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use library; pub fn run() {}",
        )
        .expect("main source");
        std::fs::write(directory.path().join("library.orna"), "pub fn seed() {}").expect("library");
        std::fs::write(directory.path().join("unused.orna"), "not a module").expect("unused");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        assert_eq!(check_project(&endpoint), Ok(()));
    }
    #[test]
    fn check_rejects_semantic_errors_with_a_stable_cli_diagnostic() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "pub fn run(): Int = true;",
        )
        .expect("main source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let error = check_project(&endpoint).expect_err("semantic error");
        assert_eq!((error.code, error.exit), ("E2101", Exit::Target));
    }
    #[test]
    fn endpoints_reject_secrets() {
        assert_eq!(
            Endpoint::parse("orna://a@host/db")
                .expect_err("credentials")
                .code,
            "E1005"
        );
        assert_eq!(
            Endpoint::parse("orna://host")
                .expect_err("no database")
                .code,
            "E1004"
        );
    }
    #[test]
    fn repl_is_function_backed_and_console_is_bounded() {
        assert_eq!(REPL_TARGET, "std.cli.repl");
        assert_eq!(parse_repl_input(":quit"), Ok(ReplInput::Quit));
        assert_eq!(
            parse_repl_input(":watch x").expect_err("not in slice").code,
            "E1101"
        );
    }
    #[test]
    fn workflow_is_deterministic_and_idempotent() {
        let mut state = ReferenceState::default();
        let first = plan(&state).expect("plan");
        assert_eq!(
            first.steps,
            vec![Step::Init, Step::Seed, Step::Exercise, Step::Sensors]
        );
        apply(&mut state, &first).expect("apply");
        assert_eq!(
            (
                state.checkpoint,
                state.readings.len(),
                state.stock.get(&("north", "pencil"))
            ),
            (3, 3, Some(&9))
        );
        let repeat = plan(&state).expect("replan");
        assert!(
            repeat
                .steps
                .iter()
                .all(|step| matches!(step, Step::Noop(_)))
        );
    }
    #[test]
    fn invalid_workflow_rolls_back_candidate() {
        let mut state = ReferenceState {
            initialized: true,
            ..ReferenceState::default()
        };
        let before = state.clone();
        let invalid = Plan {
            steps: vec![Step::Exercise],
        };
        assert_eq!(
            apply(&mut state, &invalid).expect_err("invalid").code,
            "E2004"
        );
        assert_eq!(state, before);
    }
    #[test]
    fn direct_seed_preserves_reference_duplicate_failure() {
        let mut state = ReferenceState {
            initialized: true,
            ..ReferenceState::default()
        };
        seed(&mut state).expect("first seed");
        assert_eq!(seed(&mut state).expect_err("duplicate").code, "E2003");
    }
    #[derive(Default)]
    struct Recording {
        calls: Vec<String>,
    }
    impl SessionAdapter for Recording {
        fn cancel(&mut self, child: u64) -> Result<(), Diagnostic> {
            self.calls.push(format!("cancel:{child}"));
            Ok(())
        }
        fn drain_terminal(&mut self) -> Result<(), Diagnostic> {
            self.calls.push("drain".into());
            Ok(())
        }
        fn close_transport(&mut self) -> Result<(), Diagnostic> {
            self.calls.push("close".into());
            Ok(())
        }
    }
    #[test]
    fn close_cancels_only_owned_children_then_drains_and_is_idempotent() {
        let mut session = Session::new();
        session.own(9).expect("open");
        session.own(2).expect("open");
        let mut adapter = Recording::default();
        assert_eq!(session.close(&mut adapter), Ok(true));
        assert_eq!(adapter.calls, ["cancel:2", "cancel:9", "drain", "close"]);
        assert_eq!(session.close(&mut adapter), Ok(false));
        assert_eq!(adapter.calls.len(), 4);
    }
    #[test]
    fn identity_is_declared_for_adapter_integration() {
        assert_eq!(SENSOR_SOURCE_IDENTITY, "example:sensors:v1");
    }
    #[test]
    fn planned_commands_fail_closed_instead_of_claiming_success() {
        let parsed = parse_cli(&["init".into()]).expect("parses");
        let error = execute(&parsed).expect_err("not implemented");
        assert_eq!((error.code, error.exit), ("E2000", Exit::Target));
    }
}
