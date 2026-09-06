#![allow(
    dead_code,
    reason = "The bounded binary exposes planning and session seams for a later integration adapter."
)]

mod repl;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, BufReader};
use std::process::ExitCode;

use orna_conformance_v1::{
    AdmittedReplSession, BoundedEvaluator, ProjectEnvironment, ProjectExpectations, ProjectUnit,
    ReplError, RuntimeEvaluator, SourceUnit, StageOutcome,
};
use orna_evaluator_v1::Limits;

const SENSOR_SOURCE_IDENTITY: &str = "example:sensors:v1";
const STD_MATH_LOGICAL_PATH: &str = "std/math.orna";
const STD_MATH_SOURCE: &str = include_str!("stdlib/std/math.orna");

fn standard_sources() -> [(String, String); 1] {
    [(STD_MATH_LOGICAL_PATH.into(), STD_MATH_SOURCE.into())]
}

fn standard_profile() -> orna_semantic_v1::StandardDependencyProfile {
    orna_semantic_v1::StandardDependencyProfile::from_sources(
        "orna.std/v1-pure-math",
        standard_sources(),
    )
    .expect("built-in standard source profile is valid")
}

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
    Repl(Option<String>),
    Init,
    Check,
    Invoke(String),
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
        None => Command::Repl(None),
        Some("repl") => Command::Repl(words.next().map(str::to_owned)),
        Some("init") => Command::Init,
        Some("check") => Command::Check,
        Some("invoke") => Command::Invoke(
            words
                .next()
                .ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`invoke` needs a function target",
                        "supply a reachable zero-argument pure function name after `invoke`",
                    )
                })?
                .to_owned(),
        ),
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
enum SessionState {
    Open,
    Closing,
    Closed,
}
trait SessionAdapter {
    fn is_terminal(&mut self, child: u64) -> Result<bool, Diagnostic>;
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
        if self.state != SessionState::Open {
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
        self.state = SessionState::Closing;
        for child in self.owned.iter().copied() {
            if !adapter.is_terminal(child)? {
                adapter.cancel(child)?;
            }
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

fn local_project_path(endpoint: &Endpoint) -> Result<&str, Diagnostic> {
    match endpoint {
        Endpoint::ManagedLocal => Ok("."),
        Endpoint::Path(path) => Ok(path),
        Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => Err(Diagnostic::target(
            "E2100",
            "project checking requires a local Git worktree",
            "use `check` with the current worktree or `--db PATH check`",
        )),
    }
}

fn load_project(endpoint: &Endpoint) -> Result<orna_project_v1::LoadedProject, Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "project Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    orna_project_v1::ProjectLoader::default()
        .load_with_standard_profile(&repository, Some(standard_profile()))
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "project source could not be loaded",
                "fix the project module graph and source boundaries, then run check again",
            )
        })
}

fn semantic_catalogue(
    project: &orna_project_v1::LoadedProject,
) -> Result<orna_semantic_v1::Catalogue, Diagnostic> {
    let Some(profile) = project.standard_profile() else {
        return Ok(orna_semantic_v1::Catalogue::authoritative_core());
    };
    if project
        .standard_modules()
        .iter()
        .any(|module| !profile.module_digests().contains_key(module))
    {
        return Err(Diagnostic::target(
            "E2101",
            "standard module is outside the pinned bundle",
            "use a standard module from the selected Orna standard profile",
        ));
    }
    orna_semantic_v1::Catalogue::authoritative_core()
        .with_standard_sources(profile, standard_sources())
        .map_err(|_| {
            Diagnostic::unavailable(
                "pinned standard source bundle was rejected",
                "use the standard source bundle selected by the Orna runtime",
            )
        })
}

fn check_project(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = semantic_catalogue(&project)?;
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
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

fn execution_project(project: &orna_project_v1::LoadedProject) -> ProjectUnit {
    let mut modules: Vec<SourceUnit> = project
        .modules()
        .iter()
        .zip(project.identities())
        .map(|(module, identity)| SourceUnit {
            fixture_id: "cli-project".into(),
            source_id: identity.logical_path().into(),
            parse_as: "module_unit".into(),
            source: module.source.clone(),
        })
        .collect();
    if project.has_standard_imports() {
        modules.push(SourceUnit {
            fixture_id: "cli-standard".into(),
            source_id: STD_MATH_LOGICAL_PATH.into(),
            parse_as: "module_unit".into(),
            source: STD_MATH_SOURCE.into(),
        });
    }
    ProjectUnit {
        fixture_id: "cli-project".into(),
        project_id: "cli-project".into(),
        environment_id: None,
        modules,
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
        },
    }
}

fn run_pure_invocation(endpoint: &Endpoint, target: &str) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = semantic_catalogue(&project)?;
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run check again",
        ));
    }

    let mut evaluator = BoundedEvaluator::default();
    match evaluator.evaluate_project(&execution_project(&project)) {
        StageOutcome::Passed => {}
        StageOutcome::Failed(_) | StageOutcome::Skipped { .. } => {
            return Err(Diagnostic::unavailable(
                "one-shot invocation requires an executable function-only project",
                "use reachable modules containing only pure functions; tables, effects, and streams require the integrated runtime",
            ));
        }
    }
    match evaluator.invoke(target) {
        StageOutcome::Passed => {
            println!("invocation completed");
            Ok(())
        }
        StageOutcome::Failed(_) | StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "requested invocation is not available",
            "define a reachable zero-argument pure function with the requested name",
        )),
    }
}

fn repl_session(endpoint: &Endpoint) -> Result<AdmittedReplSession, Diagnostic> {
    let project_context = match endpoint {
        Endpoint::ManagedLocal => orna_repository_v1::Repository::discover(".").is_ok(),
        Endpoint::Path(_) | Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => true,
    };
    if project_context {
        let project = load_project(endpoint)?;
        return AdmittedReplSession::from_loaded_project(
            &project,
            standard_sources(),
            Limits::default(),
        )
        .map_err(|error| repl_session_error(&error));
    }
    Ok(AdmittedReplSession::new(Limits::default()))
}

fn repl_session_error(error: &ReplError) -> Diagnostic {
    if error.code().starts_with("ORNA-S") || error.code() == "ORNA-REPL-SEMANTIC" {
        Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error before starting a project REPL session",
        )
    } else {
        Diagnostic::target(
            "E2200",
            "project REPL session is not available",
            "use a project supported by the configured REPL runtime",
        )
    }
}

fn run_repl_submission<W: std::io::Write>(
    session: &mut AdmittedReplSession,
    source: &str,
    writer: &mut W,
) -> Result<(), Diagnostic> {
    if source.trim() == ":quit" {
        return Ok(());
    }
    match session.submit(source) {
        Ok(Some(value)) => {
            writeln!(writer, "{}", repl::inspect(&value)).map_err(|_| {
                Diagnostic::target(
                    "E2200",
                    "REPL console I/O failed",
                    "check the terminal input and output streams, then start a new session",
                )
            })?;
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => {
            writeln!(writer, "error[{}]", error.code()).map_err(|_| {
                Diagnostic::target(
                    "E2200",
                    "REPL console I/O failed",
                    "check the terminal input and output streams, then start a new session",
                )
            })?;
            Err(Diagnostic::unavailable(
                "REPL submission failed",
                "use source supported by the current evaluator session",
            ))
        }
    }
}

fn run_repl(endpoint: &Endpoint, expression: Option<&str>) -> Result<(), Diagnostic> {
    let mut session = repl_session(endpoint)?;
    if let Some(source) = expression {
        return run_repl_submission(&mut session, source, &mut io::stdout().lock());
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    repl::run(
        &mut BufReader::new(stdin.lock()),
        &mut stdout.lock(),
        &mut session,
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2200",
            "REPL console I/O failed",
            "check the terminal input and output streams, then start a new session",
        )
    })
}

fn execute(parsed: &Parsed) -> Result<(), Diagnostic> {
    match parsed.command {
        Command::Help => {
            println!(
                "orna-cli-v1 [--db ENDPOINT] [repl [EXPRESSION]|init|check|invoke TARGET|run seed|run exercise|run sensors.ingest]"
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
        Command::Invoke(ref target) => run_pure_invocation(&parsed.endpoint, target),
        Command::Repl(ref expression) => run_repl(&parsed.endpoint, expression.as_deref()),
        Command::Run(Invocation::Seed) => run_pure_invocation(&parsed.endpoint, "seed"),
        Command::Run(Invocation::Exercise | Invocation::SensorsIngest) => {
            Err(Diagnostic::unavailable(
                "reference invocation execution is not available",
                "use an Orna runtime with source execution enabled",
            ))
        }
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
        assert_eq!(
            parse_cli(&["invoke".into(), "library.value".into()])
                .expect("parses")
                .command,
            Command::Invoke("library.value".into())
        );
        assert_eq!(
            parse_cli(&["repl".into(), "1 + 2".into()])
                .expect("parses")
                .command,
            Command::Repl(Some("1 + 2".into()))
        );
        assert_eq!(
            parse_cli(&["invoke".into()])
                .expect_err("target is required")
                .code,
            "E1001"
        );
        let error = parse_cli(&["serve".into()]).expect_err("unsupported");
        assert_eq!((error.code, error.exit), ("E1002", Exit::Usage));
    }

    #[test]
    fn project_repl_factory_retains_imports_in_a_scripted_session() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use library; pub fn run(): Int = library.twice(21);",
        )
        .expect("main source");
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn twice(value: Int): Int = value + value; fn hidden(value: Int): Int = value;",
        )
        .expect("library source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let mut session = repl_session(&endpoint).expect("project session");
        let mut input =
            b"use library;\nlet n: Int = 21;\nlibrary.hidden(n)\nlibrary.twice(n)\n:quit\n"
                .as_slice();
        let mut output = Vec::new();
        repl::run(&mut input, &mut output, &mut session).expect("scripted session");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> > > error[ORNA-S012-UNRESOLVED]\n> 42 : Int\n> "
        );
    }

    #[test]
    fn single_expression_repl_reports_visible_success_failure_and_quit() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let mut output = Vec::new();
        assert_eq!(
            run_repl_submission(&mut session, "1 + 2", &mut output),
            Ok(())
        );
        let error = run_repl_submission(&mut session, "missing()", &mut output)
            .expect_err("failure is reported");
        assert_eq!(error.code, "E2000");
        assert_eq!(
            run_repl_submission(&mut session, ":quit", &mut output),
            Ok(())
        );
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.starts_with("3 : Int\nerror[ORNA-S012-UNRESOLVED]"));
        assert_eq!(output.matches('\n').count(), 2);
    }

    struct BrokenWriter;

    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("writer failed"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn single_expression_repl_converts_writer_failure_to_console_diagnostic() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let error =
            run_repl_submission(&mut session, "1", &mut BrokenWriter).expect_err("writer failure");
        assert_eq!((error.code, error.exit), ("E2200", Exit::Target));
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
    fn authoritative_reference_project_checks_but_init_and_seed_fail_closed() {
        let directory = tempfile::tempdir().expect("temporary reference project");
        let reference = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../reference/Orna-1.0.0/examples/reference");
        for name in [
            "main.orna",
            "library.orna",
            "warehouse.orna",
            "sensors.orna",
            "values.orna",
        ] {
            std::fs::copy(reference.join(name), directory.path().join(name))
                .expect("reference source");
        }
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["add", "."])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        let endpoint = Endpoint::Path(directory.path().display().to_string());
        assert_eq!(
            execute(&Parsed {
                endpoint: endpoint.clone(),
                command: Command::Check,
            }),
            Ok(())
        );
        for command in [
            Command::Init,
            Command::Run(Invocation::Seed),
            Command::Invoke("seed".into()),
        ] {
            let error = execute(&Parsed {
                endpoint: endpoint.clone(),
                command,
            })
            .expect_err("effectful reference workflow is outside the bounded CLI slice");
            assert_eq!((error.code, error.exit), ("E2000", Exit::Target));
        }
    }

    #[test]
    fn run_seed_executes_a_reachable_standard_free_project() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use library; pub fn seed(): Int = library.value();",
        )
        .expect("main source");
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn value(): Int = 42;",
        )
        .expect("library source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let parsed = Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: Command::Run(Invocation::Seed),
        };
        assert_eq!(execute(&parsed), Ok(()));
    }

    #[test]
    fn invoke_executes_a_qualified_reachable_pure_function() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use library; pub fn seed(): Int = library.value();",
        )
        .expect("main source");
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn value(): Int = 42;",
        )
        .expect("library source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let parsed = Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: Command::Invoke("library.value".into()),
        };
        assert_eq!(execute(&parsed), Ok(()));
    }

    #[test]
    fn check_accepts_the_pinned_pure_math_standard_bundle() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use std.math; pub fn run(): Int = std.math.increment(41);",
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
        assert_eq!(check_project(&endpoint), Ok(()));
    }

    #[test]
    fn check_rejects_a_standard_module_outside_the_pinned_bundle() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use std.cli; pub fn run() {}",
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
        let error = check_project(&endpoint).expect_err("unbundled standard module");
        assert_eq!((error.code, error.exit), ("E2101", Exit::Target));
    }

    #[test]
    fn run_seed_executes_a_pinned_standard_function() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use std.math; pub fn seed(): Bool = std.math.is_zero(std.math.decrement(1));",
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

        let parsed = Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: Command::Run(Invocation::Seed),
        };
        assert_eq!(execute(&parsed), Ok(()));
    }

    #[test]
    fn run_seed_executes_composed_source_defined_standard_math() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use std.math; pub fn seed(): Int = std.math.clamp(std.math.max(2, 9), 3, 7);",
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

        let parsed = Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: Command::Run(Invocation::Seed),
        };
        assert_eq!(execute(&parsed), Ok(()));
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
        terminal: BTreeSet<u64>,
        fail_drain: bool,
    }
    impl SessionAdapter for Recording {
        fn is_terminal(&mut self, child: u64) -> Result<bool, Diagnostic> {
            self.calls.push(format!("terminal:{child}"));
            Ok(self.terminal.contains(&child))
        }
        fn cancel(&mut self, child: u64) -> Result<(), Diagnostic> {
            self.calls.push(format!("cancel:{child}"));
            Ok(())
        }
        fn drain_terminal(&mut self) -> Result<(), Diagnostic> {
            self.calls.push("drain".into());
            if self.fail_drain {
                return Err(Diagnostic::target(
                    "E1202",
                    "session cleanup is incomplete",
                    "retry close after child cleanup completes",
                ));
            }
            Ok(())
        }
        fn close_transport(&mut self) -> Result<(), Diagnostic> {
            self.calls.push("close".into());
            Ok(())
        }
    }
    #[test]
    fn close_cancels_only_owned_unfinished_children_then_drains_and_is_idempotent() {
        let mut session = Session::new();
        session.own(9).expect("open");
        session.own(2).expect("open");
        let mut adapter = Recording {
            terminal: BTreeSet::from([9]),
            ..Recording::default()
        };
        assert_eq!(session.close(&mut adapter), Ok(true));
        assert_eq!(
            adapter.calls,
            ["terminal:2", "cancel:2", "terminal:9", "drain", "close"]
        );
        assert_eq!(session.close(&mut adapter), Ok(false));
        assert_eq!(adapter.calls.len(), 5);
    }
    #[test]
    fn failed_close_keeps_the_session_closed_to_new_children_until_cleanup_retries() {
        let mut session = Session::new();
        session.own(2).expect("open");
        let mut adapter = Recording {
            fail_drain: true,
            ..Recording::default()
        };

        assert_eq!(
            session.close(&mut adapter).expect_err("drain failure").code,
            "E1202"
        );
        assert_eq!(session.own(3).expect_err("admission sealed").code, "E1201");

        adapter.fail_drain = false;
        assert_eq!(session.close(&mut adapter), Ok(true));
        assert_eq!(
            adapter.calls,
            [
                "terminal:2",
                "cancel:2",
                "drain",
                "terminal:2",
                "cancel:2",
                "drain",
                "close"
            ]
        );
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
