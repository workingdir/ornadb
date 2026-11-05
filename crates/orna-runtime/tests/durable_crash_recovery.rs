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

use std::{
    env,
    ffi::OsString,
    fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use orna_repository::Repository;
use orna_runtime::{
    ActivationEffectReport, FaultInjector, FaultPoint, RecoveryDisposition, RequestIdentity,
    RequestOwner, RequestState, RuntimeError, RuntimeIdentity, RuntimeState, TableMutation,
    TerminalOutcome, WriterLease,
};
use tempfile::{Builder, TempDir};

/// Set by the parent so the re-executed child takes the crashing branch.
const CRASH_SEAM: &str = "ORNA_CRASH_SEAM";
/// Absolute path of the repository the child must crash inside.
const CRASH_REPOSITORY: &str = "ORNA_CRASH_REPOSITORY";
/// Path the child creates for the one external effect it performs.
const CRASH_EFFECT_RECEIPT: &str = "ORNA_CRASH_EFFECT_RECEIPT";

const COMMIT_BARRIER_TEST: &str = "crash_at_the_row_commit_barrier_keeps_the_committed_row";
const PRE_COMMIT_TEST: &str = "crash_before_the_row_commit_barrier_acknowledges_nothing";
const EXTERNAL_EFFECT_TEST: &str = "crash_after_an_external_effect_recovery_reports_uncertainty";

const OWNER: [u8; 16] = [4; 16];
const REPLACEMENT: [u8; 16] = [9; 16];
/// A different owner tried through the replacement's own lock-holding handle.
const THIRD_OWNER: [u8; 16] = [12; 16];

/// Which side of the writer transaction commit barrier the child dies on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Seam {
    /// Inside the still-open transaction: the rows exist only in the
    /// uncommitted transaction, so process death must leave nothing durable.
    BeforeCommit,
    /// After `commit()` returned: the row, checkpoint and capture are durable,
    /// so a crash here must leave an acknowledged row behind.
    AfterCommit,
    /// Inside a request activation's table transaction, after the statement
    /// runs, with the activation's external effect already performed. Process
    /// death rolls the transaction back without any durable rollback receipt,
    /// so recovery cannot claim the external effect was undone.
    ExternalEffect,
}

impl Seam {
    const fn tag(self) -> &'static str {
        match self {
            Self::BeforeCommit => "before-commit",
            Self::AfterCommit => "after-commit",
            Self::ExternalEffect => "external-effect",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "before-commit" => Some(Self::BeforeCommit),
            "after-commit" => Some(Self::AfterCommit),
            "external-effect" => Some(Self::ExternalEffect),
            _ => None,
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
    fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/durable_crash_recovery_main.orna"),
    )
    .expect("write fixture root module");
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
/// requested seam while one table row is being committed.
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
        let context = state.begin_activation().await.expect("capture activation");
        let mutation = TableMutation::insert([7; 16], "records", vec![1], vec![9])
            .expect("valid typed table mutation");
        if seam == Seam::ExternalEffect {
            let (_, capability) = state
                .reserve_request_with_admission(external_request(), EXTERNAL_FINGERPRINT)
                .await
                .expect("reserve the external-effect request");
            let capability = capability.expect("a fresh reservation issues one capability");
            state
                .start_request_with_owner_and_admission(
                    external_request(),
                    EXTERNAL_FINGERPRINT,
                    lease,
                    capability,
                )
                .await
                .expect("start the external-effect request");
            perform_external_effect();
            state
                .commit_table_request_activation(
                    lease,
                    external_request(),
                    EXTERNAL_FINGERPRINT,
                    &context,
                    &[mutation],
                    [8; 32],
                    TerminalOutcome::new(vec![20]).expect("valid terminal outcome"),
                    &AbortAt(FaultPoint::AfterTableWrite),
                    ActivationEffectReport::ExternalPossible,
                )
                .await
                .expect("the crashing seam never returns");
            unreachable!("the crash seam must abort the child process");
        }
        let point = match seam {
            Seam::BeforeCommit => FaultPoint::AfterMutation,
            Seam::AfterCommit => FaultPoint::AfterCommit,
            Seam::ExternalEffect => unreachable!("handled above"),
        };
        state
            .commit_table_activation(lease, &context, &[mutation], [8; 32], &AbortAt(point))
            .await
            .expect("the crashing seam never returns");
        unreachable!("the crash seam must abort the child process");
    });
}

/// Performs the one external effect the child is allowed to make. The receipt
/// is created exclusively, so a second execution fails instead of passing
/// quietly, and its contents say the action ran once.
fn perform_external_effect() {
    use std::io::Write;

    let receipt = PathBuf::from(env::var_os(CRASH_EFFECT_RECEIPT).expect("effect receipt path"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&receipt)
        .expect("perform the external effect once");
    file.write_all(b"external-effect\n")
        .expect("record the external effect");
}

/// The request whose activation reports a possible external effect.
const fn external_request() -> RequestIdentity {
    RequestIdentity {
        session_id: [5; 16],
        request_id: [6; 16],
    }
}

const EXTERNAL_FINGERPRINT: [u8; 32] = [7; 32];

/// Runs this test binary again, crashing the child at `seam`.
///
/// The child dies by SIGABRT on purpose, so the kernel would write a core dump
/// into the test's working directory on every run. On Linux the child is a
/// `/bin/sh` that disables core dumps for itself and then `exec`s this same
/// test binary, so the limit survives into the crashing process without unsafe
/// code or an extra dependency.
fn run_crashing_child(
    seam: Seam,
    repository_path: &Path,
    test_name: &str,
    effect_receipt: Option<&Path>,
) -> ExitStatus {
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
    if let Some(receipt) = effect_receipt {
        command.env(CRASH_EFFECT_RECEIPT, receipt.as_os_str());
    }
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
    let status = run_crashing_child(
        Seam::AfterCommit,
        directory.path(),
        COMMIT_BARRIER_TEST,
        None,
    );
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

        let replacement =
            assert_only_the_lock_holder_fences_the_crashed_owner(&state, &repository).await;

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
    let status = run_crashing_child(Seam::BeforeCommit, directory.path(), PRE_COMMIT_TEST, None);
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

        assert_only_the_lock_holder_fences_the_crashed_owner(&state, &repository).await;
    });
}

/// Gate D, ORNA-PUB-009 and REQUEST-1 step 5: an activation that may have made
/// an external effect, killed inside its table transaction, is recovered as
/// `orphaned` with `ExternalEffectsUncertain`. No rollback is claimed, no
/// rollback receipt exists to claim it, the retained payload is the uncertain
/// one, and a retry of the same identity is served from the ledger instead of
/// running the external action again.
#[test]
fn crash_after_an_external_effect_recovery_reports_uncertainty() {
    if let Some(seam) = child_seam() {
        crash_child_body(seam, &child_repository());
        return;
    }

    let (directory, repository) = repository();
    let receipt = directory.path().join("external-effect-receipt");
    let status = run_crashing_child(
        Seam::ExternalEffect,
        directory.path(),
        EXTERNAL_EFFECT_TEST,
        Some(&receipt),
    );
    assert_eq!(
        status.signal(),
        Some(libc_sigabrt()),
        "the child must die by signal inside the activation transaction"
    );
    assert_eq!(
        fs::read_to_string(&receipt).expect("the child performed its external effect"),
        "external-effect\n",
        "the child performs the external effect exactly once, before the abort"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build parent runtime");
    runtime.block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("the crashed activation reopens as valid recovery state");
        assert_eq!(
            state.committed_table_row("records", &[1]).await.unwrap(),
            None,
            "the activation's table transaction never committed"
        );
        let running = state
            .request_status(external_request(), EXTERNAL_FINGERPRINT)
            .await
            .unwrap()
            .expect("the started request is recorded");
        assert_eq!(running.state, RequestState::Running);
        assert_eq!(
            running.terminal_outcome, None,
            "no terminal claim survived the crash"
        );
        assert_eq!(
            state
                .request_recovery_disposition(external_request(), EXTERNAL_FINGERPRINT)
                .await
                .unwrap(),
            None,
            "a crash leaves no disposition behind; recovery decides it"
        );

        let lost = state
            .current_lease()
            .await
            .unwrap()
            .expect("the crashed owner is still recorded");
        assert_eq!(lost.owner_id, OWNER);
        // Explicit takeover leaves the Running requests for recovery to settle
        // with their own payloads; acquiring the lease would instead fence them
        // straight into the conservative outcome.
        let fence = state
            .recover_abandoned(OWNER, REPLACEMENT)
            .await
            .expect("the runtime owner lock holder takes over the crashed owner");
        let recovered = state
            .recover_running_request_with_outcomes(
                external_request(),
                EXTERNAL_FINGERPRINT,
                RequestOwner::from(lost),
                fence,
                TerminalOutcome::new(vec![21]).expect("rollback payload"),
                TerminalOutcome::new(vec![22]).expect("uncertain payload"),
            )
            .await
            .expect("recover the running request");
        assert_eq!(
            recovered.status.state,
            RequestState::Orphaned,
            "recovery marks the request orphaned rather than completed"
        );
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain,
            "a possible external effect is reported as unproven, never as a proven rollback"
        );
        assert_eq!(
            recovered.status.terminal_outcome,
            Some(TerminalOutcome::new(vec![22]).expect("uncertain payload")),
            "recovery retains the uncertain payload, not the rollback one"
        );
        assert_eq!(
            state
                .request_recovery_disposition(external_request(), EXTERNAL_FINGERPRINT)
                .await
                .unwrap(),
            Some(RecoveryDisposition::ExternalEffectsUncertain)
        );

        let (replay, capability) = state
            .reserve_request_with_admission(external_request(), EXTERNAL_FINGERPRINT)
            .await
            .expect("a retry of the same identity is served from the ledger");
        assert_eq!(
            replay.state,
            RequestState::Orphaned,
            "the ledger answers the retry with the recovered state"
        );
        assert!(
            capability.is_none(),
            "a recovered request never issues a new admission capability, so the external action cannot run again"
        );
        assert_eq!(
            fs::read_to_string(&receipt).expect("the receipt survives"),
            "external-effect\n",
            "the external action ran exactly once across the crash and the retry"
        );
    });
}

/// ORNA-TURSO-005/006: the crashed owner's lease stays recorded until the
/// runtime owner lock holder takes it over. A participant without the lock
/// cannot release it, the takeover bumps the epoch, and afterwards neither the
/// abandoned owner nor a different owner can take the live replacement's
/// lease, not even through the lock holder.
async fn assert_only_the_lock_holder_fences_the_crashed_owner(
    state: &RuntimeState,
    repository: &Repository,
) -> WriterLease {
    assert!(
        state.holds_runtime_owner_lock(),
        "the crashed child released the runtime owner lock"
    );
    assert_eq!(
        state
            .current_lease()
            .await
            .unwrap()
            .map(|lease| lease.owner_id),
        Some(OWNER),
        "the crashed owner is still recorded"
    );
    let secondary = RuntimeState::open(repository, identity(), [3; 32])
        .await
        .expect("open a secondary participant");
    assert!(!secondary.holds_runtime_owner_lock());
    assert_eq!(
        secondary.acquire_lease(REPLACEMENT).await,
        Err(RuntimeError::LeaseHeld),
        "a participant without the runtime owner lock cannot fence the crashed owner"
    );
    drop(secondary);

    let replacement = state
        .acquire_lease(REPLACEMENT)
        .await
        .expect("the lock holder takes over the crashed owner");
    assert_eq!(replacement.owner_id, REPLACEMENT);
    assert_eq!(replacement.epoch, 2, "takeover advances the writer epoch");
    assert_eq!(
        state.acquire_lease(OWNER).await,
        Err(RuntimeError::LeaseHeld),
        "the abandoned owner must not regain the writer lease"
    );
    assert_eq!(
        state.acquire_lease(THIRD_OWNER).await,
        Err(RuntimeError::LeaseHeld),
        "holding the runtime owner lock must not fence the live replacement"
    );
    replacement
}

/// The non-crashing injector used by the parent's follow-up commit.
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
