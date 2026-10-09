//! Whole-process crash proofs for the shared durable row-transaction boundary.
//!
//! Gate D (`reference/Orna-1.1.0/implementation/ACCEPTANCE.md`) requires fault
//! injection or SIGKILL at each capture/pack install/pin/row commit boundary,
//! followed by a restart that proves no acknowledged row lacks bytes and that
//! no abandoned owner retains write permission.
//!
//! The in-process `FaultPoint` seam cannot stand in for that at the commit
//! barrier. A check answered *before* `tx.commit()` is completed by the
//! transaction unwind, so the rollback it reports is real. A check answered
//! *after* the commit barrier has nothing left to unwind: returning an `Err`
//! there would claim a rollback that never happened. These tests therefore
//! re-execute this test binary and abort the child with SIGABRT exactly at the
//! seam, then reopen the same repository from the parent and assert the durable
//! outcome.
//!
//! The same seam also carries the request side of the boundary: a request whose
//! rows, checkpoint and terminal claim committed together must not be run a
//! second time after the crash that followed the commit.

use std::{
    env,
    ffi::OsString,
    fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    ActivationWork, Component, ConsumerIdentity, FaultInjector, FaultPoint, RequestIdentity,
    RequestState, RunObservationRegistration, RunningTableRequestContinuation, RuntimeError,
    RuntimeIdentity, RuntimeState, RuntimeTableIdentity, TableMutation, TableObjectId,
    TerminalOutcome, run_admitted_table_request_activation,
};
use tempfile::{Builder, TempDir};

/// A repository root module that declares the admitted relation the request
/// boundary commits into. Shared with the admitted-relation transaction proofs.
const REQUEST_SOURCE: &str = include_str!("fixtures/admitted-relation-transactions.orna");
/// Records the bare repository root the row-commit proofs need.
const ROW_SOURCE: &str = include_str!("fixtures/durable_crash_recovery_main.orna");

/// Set by the parent so the re-executed child takes the crashing branch.
const CRASH_SEAM: &str = "ORNA_CRASH_SEAM";
/// Absolute path of the repository the child must crash inside.
const CRASH_REPOSITORY: &str = "ORNA_CRASH_REPOSITORY";

const COMMIT_BARRIER_TEST: &str = "crash_at_the_row_commit_barrier_keeps_the_committed_row";
const PRE_COMMIT_TEST: &str = "crash_before_the_row_commit_barrier_acknowledges_nothing";
const REQUEST_COMMIT_TEST: &str = "crash_at_the_request_commit_barrier_keeps_the_request_terminal";
const REQUEST_PRE_COMMIT_TEST: &str =
    "crash_before_the_request_commit_barrier_abandons_the_request";

const OWNER: [u8; 16] = [4; 16];
const REPLACEMENT: [u8; 16] = [9; 16];

/// Which side of the writer transaction commit barrier the child dies on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Seam {
    /// Inside the still-open transaction: the rows exist only in the
    /// uncommitted transaction, so process death must leave nothing durable.
    BeforeCommit,
    /// After `commit()` returned: the row, checkpoint and capture are durable,
    /// so a crash here must leave an acknowledged row behind.
    AfterCommit,
    /// Inside the still-open transaction of an admitted *request* activation:
    /// no row, checkpoint or terminal claim may survive.
    RequestBeforeCommit,
    /// After the request activation committed: the row, checkpoint and terminal
    /// claim are durable together, so the request must not run a second time.
    RequestAfterCommit,
}

impl Seam {
    const fn tag(self) -> &'static str {
        match self {
            Self::BeforeCommit => "before-commit",
            Self::AfterCommit => "after-commit",
            Self::RequestBeforeCommit => "request-before-commit",
            Self::RequestAfterCommit => "request-after-commit",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "before-commit" => Some(Self::BeforeCommit),
            "after-commit" => Some(Self::AfterCommit),
            "request-before-commit" => Some(Self::RequestBeforeCommit),
            "request-after-commit" => Some(Self::RequestAfterCommit),
            _ => None,
        }
    }

    /// The request activations admit a relation, so they need the repository
    /// root that declares one.
    const fn needs_admitted_fixture(self) -> bool {
        matches!(self, Self::RequestBeforeCommit | Self::RequestAfterCommit)
    }

    /// The point the child aborts on.
    const fn point(self) -> FaultPoint {
        match self {
            Self::BeforeCommit | Self::RequestBeforeCommit => FaultPoint::AfterMutation,
            Self::AfterCommit | Self::RequestAfterCommit => FaultPoint::AfterCommit,
        }
    }
}

/// Aborts the whole process at exactly one fault point. `std::process::abort`
/// raises SIGABRT, which no `Drop` or transaction unwind can intercept; a
/// child crash therefore cannot be confused with an in-process error return.
struct AbortAt(FaultPoint);

impl FaultInjector for AbortAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            std::process::abort();
        }
        Ok(())
    }
}

fn repository() -> (TempDir, Repository) {
    repository_with_source(ROW_SOURCE)
}

/// Prepares a repository whose root module is `source`. The request boundary
/// needs an admitted relation declared in the checked-in fixture.
fn repository_with_source(source: &str) -> (TempDir, Repository) {
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-durable-crash-")
        .tempdir_in(target)
        .expect("create repository directory");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(directory.path(), &["config", "user.name", "kierandrewett"]);
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    fs::write(directory.path().join("main.orna"), source).expect("write fixture root module");
    git(directory.path(), &["add", "main.orna"]);
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "initial snapshot"],
    );
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [1; 16],
        repository_id: [2; 16],
    }
}

/// The crashing child: open the repository the parent prepared and die at the
/// requested seam while one row commit is being made durable.
fn crash_child_body(seam: Seam, repository_path: &Path) {
    let repository = Repository::discover(repository_path).expect("discover child repository");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build child runtime");
    runtime.block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("open child runtime");
        let lease = state.acquire_lease(OWNER).await.expect("acquire writer");
        if seam.needs_admitted_fixture() {
            crash_child_request(seam, &state, lease).await;
        }
        let context = state.begin_activation().await.expect("capture activation");
        let mutation = TableMutation::insert([7; 16], "records", vec![1], vec![9])
            .expect("valid typed table mutation");
        state
            .commit_table_activation(
                lease,
                &context,
                &[mutation],
                [8; 32],
                &AbortAt(seam.point()),
            )
            .await
            .expect("the crashing seam never returns");
        unreachable!("the crash seam must abort the child process");
    });
}

/// The request side of the same seam: commit one admitted request activation so
/// its row, checkpoint and terminal claim become durable together, then die.
async fn crash_child_request(seam: Seam, state: &RuntimeState, lease: WriterLease) {
    let object_id = TableObjectId::new([0x94; 16]);
    let table = "Note".to_owned();
    let identity = RuntimeTableIdentity::new(table.clone(), object_id).expect("admitted identity");
    let request = RequestIdentity {
        session_id: [30; 16],
        request_id: [31; 16],
    };
    let fingerprint = [32; 32];
    let continuation =
        admitted_request(state, lease, request, fingerprint, &request_consumer()).await;
    let insert = TableMutation::new([0x70; 16], &table, vec![1], Some(b"crash".to_vec()))
        .expect("valid typed table mutation")
        .with_table_object_id(object_id);
    run_admitted_table_request_activation(
        state,
        continuation,
        std::slice::from_ref(&identity),
        &AbortAt(seam.point()),
        TerminalOutcome::new(b"accepted".to_vec()).expect("terminal outcome"),
        move |_snapshot| async move {
            Ok::<_, ()>(ActivationWork::new(vec![insert], [33; 32], ()))
        },
    )
    .await
    .expect("the crashing seam never returns");
    unreachable!("the crash seam must abort the child process");
}

/// The consumer identity the request activations run under.
fn request_consumer() -> ConsumerIdentity {
    ConsumerIdentity {
        principal: Component::new("durable-crash-recovery").expect("component"),
        root: Component::new("main").expect("component"),
        function: Component::new("main").expect("component"),
        binding: Component::new("durable-crash-recovery").expect("component"),
    }
}

/// Admits one observed request through the durable live admission boundary, as
/// the admitted-relation transaction proofs do.
async fn admitted_request(
    state: &RuntimeState,
    writer: WriterLease,
    identity: RequestIdentity,
    fingerprint: [u8; 32],
    consumer: &ConsumerIdentity,
) -> RunningTableRequestContinuation {
    let (_, capability) = state
        .reserve_request_with_admission(identity, fingerprint)
        .await
        .expect("reserve request");
    let capability = capability.expect("fresh owner-bound capability");
    state
        .begin_observed_request_with_admission(
            RunObservationRegistration {
                request: identity,
                consumer_identity: consumer.clone(),
                function: "main".into(),
                source_identity: Some("test:durable-crash-recovery:v1".into()),
                invocation_id: identity.request_id,
            },
            fingerprint,
            writer,
            capability,
        )
        .await
        .expect("admit observed request");
    state
        .continue_running_table_request(identity, fingerprint, writer)
        .await
        .expect("continue running request")
}

/// Runs this test binary again, crashing the child at `seam`.
///
/// The child dies by SIGABRT on purpose, so the kernel would write a core dump
/// into the test's working directory on every run. On Linux the child is a
/// `/bin/sh` that disables core dumps for itself and then `exec`s this same
/// test binary, so the limit survives into the crashing process without unsafe
/// code or an extra dependency.
fn run_crashing_child(seam: Seam, repository_path: &Path, test_name: &str) -> ExitStatus {
    let binary = env::current_exe().expect("resolve test binary");
    let mut command = if cfg!(target_os = "linux") {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("ulimit -c 0 2>/dev/null; exec \"$0\" \"$@\"")
            .arg(binary);
        command
    } else {
        Command::new(binary)
    };
    command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CRASH_SEAM, OsString::from(seam.tag()))
        .env(CRASH_REPOSITORY, repository_path.as_os_str());
    command.status().expect("run crashing child")
}

fn child_seam() -> Option<Seam> {
    env::var(CRASH_SEAM)
        .ok()
        .and_then(|value| Seam::parse(&value))
}

fn child_repository() -> PathBuf {
    PathBuf::from(env::var_os(CRASH_REPOSITORY).expect("child repository path"))
}

#[test]
fn crash_at_the_row_commit_barrier_keeps_the_committed_row() {
    if let Some(seam) = child_seam() {
        crash_child_body(seam, &child_repository());
        return;
    }

    let (directory, repository) = repository();
    let status = run_crashing_child(Seam::AfterCommit, directory.path(), COMMIT_BARRIER_TEST);
    assert_eq!(
        status.signal(),
        Some(libc_sigabrt()),
        "the child must die by signal at the commit barrier, not return an error"
    );

    // Reopening runs the full recovery validation: the committed generation,
    // its checkpoint digest and the CWD capture must agree.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build parent runtime");
    runtime.block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("a committed generation reopens as valid recovery state");
        assert_eq!(
            state.committed_table_row("records", &[1]).await.unwrap(),
            Some(vec![9]),
            "the row committed before the crash stays acknowledged"
        );
        assert_eq!(
            state
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("committed generation retains its checkpoint")
                .generation,
            1
        );
        assert_eq!(state.pending().await.unwrap().len(), 1);

        // The crashed owner is still recorded, and its write permission is
        // only released through the explicit compare-and-swap takeover.
        assert_eq!(
            state.acquire_lease(REPLACEMENT).await,
            Err(RuntimeError::LeaseHeld)
        );
        let replacement = state
            .recover_abandoned(OWNER, REPLACEMENT)
            .await
            .expect("the abandoned owner is recoverable by compare-and-swap");
        assert_eq!(replacement.owner_id, REPLACEMENT);
        assert_eq!(replacement.epoch, 2, "takeover advances the writer epoch");
        assert_eq!(
            state.acquire_lease(OWNER).await,
            Err(RuntimeError::LeaseHeld)
        );

        // The replacement writer can commit a further row on top of the
        // surviving one, so the crash consumed no later tail.
        let context = state.begin_activation().await.unwrap();
        let next = TableMutation::insert([10; 16], "records", vec![2], vec![11]).unwrap();
        state
            .commit_table_activation(replacement, &context, &[next], [12; 32], &NoAbort)
            .await
            .expect("the recovered owner commits");
        assert_eq!(
            state.committed_table_row("records", &[2]).await.unwrap(),
            Some(vec![11])
        );
        assert_eq!(
            state.committed_table_row("records", &[1]).await.unwrap(),
            Some(vec![9])
        );
    });
}

#[test]
fn crash_before_the_row_commit_barrier_acknowledges_nothing() {
    if let Some(seam) = child_seam() {
        crash_child_body(seam, &child_repository());
        return;
    }

    let (directory, repository) = repository();
    let status = run_crashing_child(Seam::BeforeCommit, directory.path(), PRE_COMMIT_TEST);
    assert_eq!(
        status.signal(),
        Some(libc_sigabrt()),
        "the child must die by signal inside the open transaction"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build parent runtime");
    runtime.block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("an uncommitted crash reopens as valid recovery state");
        assert_eq!(
            state.committed_table_row("records", &[1]).await.unwrap(),
            None,
            "a row whose transaction never committed is not acknowledged"
        );
        assert!(state.latest_checkpoint().await.unwrap().is_none());
        assert!(
            state.pending().await.unwrap().is_empty(),
            "the uncommitted tail leaves no durable mutation record"
        );

        // The crashed owner still holds the fence until an explicit takeover.
        assert_eq!(
            state.acquire_lease(REPLACEMENT).await,
            Err(RuntimeError::LeaseHeld)
        );
        let replacement = state
            .recover_abandoned(OWNER, REPLACEMENT)
            .await
            .expect("the abandoned owner is recoverable by compare-and-swap");
        assert_eq!(replacement.epoch, 2);
    });
}

/// A request activation commits its row, checkpoint and terminal claim in one
/// write transaction. Dying immediately after that commit must leave the
/// request terminal, so a retry cannot run the same action twice.
#[test]
fn crash_at_the_request_commit_barrier_keeps_the_request_terminal() {
    if let Some(seam) = child_seam() {
        crash_child_body(seam, &child_repository());
        return;
    }

    let (directory, repository) = repository_with_source(REQUEST_SOURCE);
    let status = run_crashing_child(
        Seam::RequestAfterCommit,
        directory.path(),
        REQUEST_COMMIT_TEST,
    );
    assert_eq!(
        status.signal(),
        Some(libc_sigabrt()),
        "the child must die by signal at the request commit barrier"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build parent runtime");
    runtime.block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("a committed request reopens as valid recovery state");
        let object_id = TableObjectId::new([0x94; 16]);
        let table = "Note".to_owned();
        let identity = RuntimeTableIdentity::new(table.clone(), object_id).expect("identity");
        assert_eq!(
            state
                .begin_admitted_table_activation(std::slice::from_ref(&identity))
                .await
                .expect("admit live relation")
                .table_rows()[&table],
            vec![(vec![1], b"crash".to_vec())],
            "the row committed with the terminal claim stays acknowledged"
        );
        let request = RequestIdentity {
            session_id: [30; 16],
            request_id: [31; 16],
        };
        let status = state
            .request_status(request, [32; 32])
            .await
            .expect("read request status")
            .expect("request retained");
        assert_eq!(
            status.state,
            RequestState::Completed,
            "a crash after the commit must not leave the request re-runnable"
        );

        // The terminal claim is what refuses the repeat: continuing the same
        // request requires it to still be Running under the current owner.
        let replacement = state
            .recover_abandoned(OWNER, REPLACEMENT)
            .await
            .expect("recover the abandoned writer");
        assert_eq!(
            state
                .continue_running_table_request(request, [32; 32], replacement)
                .await,
            Err(RuntimeError::RequestStateConflict),
            "the committed request must not be continued a second time"
        );
    });
}

/// The other side of the request barrier: dying inside the still-open
/// transaction must abandon the request without a row or a terminal claim, and
/// must not fabricate a rollback it never performed.
#[test]
fn crash_before_the_request_commit_barrier_abandons_the_request() {
    if let Some(seam) = child_seam() {
        crash_child_body(seam, &child_repository());
        return;
    }

    let (directory, repository) = repository_with_source(REQUEST_SOURCE);
    let status = run_crashing_child(
        Seam::RequestBeforeCommit,
        directory.path(),
        REQUEST_PRE_COMMIT_TEST,
    );
    assert_eq!(
        status.signal(),
        Some(libc_sigabrt()),
        "the child must die by signal inside the request transaction"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build parent runtime");
    runtime.block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("an uncommitted request crash reopens as valid recovery state");
        let object_id = TableObjectId::new([0x94; 16]);
        let table = "Note".to_owned();
        let identity = RuntimeTableIdentity::new(table.clone(), object_id).expect("identity");
        assert!(
            state
                .begin_admitted_table_activation(std::slice::from_ref(&identity))
                .await
                .expect("admit live relation")
                .table_rows()[&table]
                .is_empty(),
            "a row whose transaction never committed is not acknowledged"
        );
        let request = RequestIdentity {
            session_id: [30; 16],
            request_id: [31; 16],
        };
        let status = state
            .request_status(request, [32; 32])
            .await
            .expect("read request status")
            .expect("request retained");
        assert_eq!(status.state, RequestState::Running);
        assert_eq!(status.terminal_outcome, None);
        assert_eq!(
            state.latest_checkpoint().await.expect("read checkpoint"),
            None
        );
        assert!(
            state.pending().await.expect("pending tail").is_empty(),
            "the uncommitted tail leaves no durable mutation record"
        );
    });
}

/// The non-crashing injector used by the parent's follow-up commits.
#[derive(Debug, Default)]
struct NoAbort;

impl FaultInjector for NoAbort {
    fn check(&self, _: FaultPoint) -> Result<(), RuntimeError> {
        Ok(())
    }
}

/// Returns `Err` at one point instead of aborting, without unwinding anything.
#[derive(Debug)]
struct RefuseAt(FaultPoint);

impl FaultInjector for RefuseAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            return Err(RuntimeError::FaultInjected(point));
        }
        Ok(())
    }
}

/// Pins the semantics of the new seam: `AfterCommit` runs once the row is
/// durable, so it is a crash probe rather than a rollback point. Every
/// pre-commit point answers before the barrier and is therefore completed by
/// the transaction unwind; `AfterCommit` must leave the committed row visible
/// even when the injector refuses.
#[tokio::test]
async fn the_post_commit_seam_is_not_a_rollback_point() {
    let (_directory, repository) = repository();
    let state = RuntimeState::open(&repository, identity(), [3; 32])
        .await
        .expect("open runtime");
    let writer = state.acquire_lease(OWNER).await.expect("acquire writer");

    for (key, point) in [
        (1_u8, FaultPoint::AfterMutation),
        (2_u8, FaultPoint::AfterCommit),
    ] {
        let context = state.begin_activation().await.expect("capture activation");
        let mutation = TableMutation::insert([20 + key; 16], "records", vec![key], vec![key])
            .expect("valid typed table mutation");
        let result = state
            .commit_table_activation(
                writer,
                &context,
                &[mutation],
                [30 + key; 32],
                &RefuseAt(point),
            )
            .await;
        assert_eq!(result, Err(RuntimeError::FaultInjected(point)));
        let expected = if point == FaultPoint::AfterCommit {
            Some(vec![key])
        } else {
            None
        };
        assert_eq!(
            state.committed_table_row("records", &[key]).await.unwrap(),
            expected,
            "{point:?} answered on the wrong side of the commit barrier"
        );
    }
}

/// SIGABRT is the signal `std::process::abort` raises on Linux.
fn libc_sigabrt() -> i32 {
    6
}
