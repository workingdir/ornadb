//! Gate D (`reference/Orna-1.1.0/implementation/ACCEPTANCE.md`): recovery of an
//! uncertain external effect never fabricates a proven rollback.
//!
//! An external effect recorded before a writer transaction is killed has an
//! outcome outside the database that the runtime cannot observe, so recovery
//! must report it as `ExternalEffectsUncertain`. Only a controlled transaction
//! whose rollback was observed in-process before the kill may be reported as
//! `RollbackProven`.
//!
//! Like `durable_crash_recovery.rs`, these tests re-execute this test binary and
//! abort the child with SIGABRT exactly at the request-level commit seam, then
//! reopen the same repository from the parent and assert the durable outcome.

use std::{
    env,
    fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    ActivationEffectReport, ControlledEffectToken, DeclaredEffectSet, FaultInjector, FaultPoint,
    RecoveryDisposition,
    RequestIdentity, RequestOwner, RequestState, RuntimeError, RuntimeIdentity, RuntimeState,
    TableMutation, TerminalOutcome,
};
use tempfile::{Builder, TempDir};

/// Selects the crashing branch in the re-executed child.
const SCENARIO: &str = "ORNA_UNCERTAIN_SCENARIO";
/// Absolute path of the repository the child must crash inside.
const SCENARIO_REPOSITORY: &str = "ORNA_UNCERTAIN_REPOSITORY";

const EXTERNAL_EFFECT_TEST: &str =
    "external_effect_at_the_kill_recovers_as_uncertain_never_rollback";
const CONTROLLED_ROLLBACK_TEST: &str =
    "controlled_rollback_before_the_kill_is_the_only_proven_rollback";

const OWNER: [u8; 16] = [4; 16];
const REPLACEMENT: [u8; 16] = [9; 16];
const FINGERPRINT: [u8; 32] = [6; 32];
/// SIGABRT is the signal `std::process::abort` raises on Linux.
const SIGABRT: i32 = 6;
const ROLLBACK_PROVEN: &str = "rollback-proven";
const EFFECTS_UNCERTAIN: &str = "external-effects-uncertain";
const UNREPORTED_ROLLBACK_TEST: &str =
    "explicit_external_possible_rollback_before_the_kill_is_never_proven";

/// Which request-level crash the re-executed child performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scenario {
    /// An external effect was recorded, then the process dies inside the open
    /// writer transaction at the table-write seam.
    ExternalEffectAtKill,
    /// A controlled transaction was rolled back in-process, then the process
    /// dies after the rollback was recorded.
    ControlledRollbackThenKill,
    /// An activation declared possible external effects, its table write was
    /// rolled back in-process, then the process dies. The rollback is observed
    /// but cannot be proven controlled, so recovery must stay uncertain.
    UnreportedRollbackThenKill,
}

impl Scenario {
    const fn tag(self) -> &'static str {
        match self {
            Self::ExternalEffectAtKill => "external-effect-at-kill",
            Self::ControlledRollbackThenKill => "controlled-rollback-then-kill",
            Self::UnreportedRollbackThenKill => "unreported-rollback-then-kill",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "external-effect-at-kill" => Some(Self::ExternalEffectAtKill),
            "controlled-rollback-then-kill" => Some(Self::ControlledRollbackThenKill),
            "unreported-rollback-then-kill" => Some(Self::UnreportedRollbackThenKill),
            _ => None,
        }
    }
}

/// Aborts the whole process at one fault point. No `Drop` or transaction unwind
/// runs, so the uncommitted writer transaction is left as a real crash leaves it.
struct AbortAt(FaultPoint);

impl FaultInjector for AbortAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            std::process::abort();
        }
        Ok(())
    }
}

/// Returns `Err` at one point, so the transaction unwinds through the runtime's
/// own rollback path before the child dies.
struct RefuseAt(FaultPoint);

impl FaultInjector for RefuseAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            return Err(RuntimeError::FaultInjected(point));
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
        .prefix("orna-runtime-uncertain-effect-")
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

fn request() -> RequestIdentity {
    RequestIdentity {
        session_id: [5; 16],
        request_id: [6; 16],
    }
}

fn outcome(label: &str) -> TerminalOutcome {
    TerminalOutcome::new(label.as_bytes().to_vec()).expect("bounded terminal outcome")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime")
}

/// Records an external effect, starts the table transaction and dies at the
/// table-write seam, before the transaction can commit or roll back.
fn external_effect_child(repository_path: &Path) {
    let repository = Repository::discover(repository_path).expect("discover child repository");
    runtime().block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("open child runtime");
        let owner = state.acquire_lease(OWNER).await.expect("acquire writer");
        let (_, capability) = state
            .reserve_request_with_admission(request(), FINGERPRINT)
            .await
            .expect("reserve request");
        let capability = capability.expect("fresh owner-bound capability");
        state
            .start_request_with_owner_and_admission(request(), FINGERPRINT, owner, capability)
            .await
            .expect("start owned request");
        state
            .record_external_effect(request(), FINGERPRINT, owner)
            .await
            .expect("record external effect");
        let context = state.begin_activation().await.expect("capture activation");
        let mutation = TableMutation::insert([7; 16], "records", vec![1], vec![9])
            .expect("valid typed table mutation");
        let _ = state
            .commit_table_request_activation(
                owner,
                request(),
                FINGERPRINT,
                &context,
                &[mutation],
                [8; 32],
                outcome(EFFECTS_UNCERTAIN),
                &AbortAt(FaultPoint::AfterTableWrite),
                ActivationEffectReport::declare(DeclaredEffectSet::None),
            )
            .await;
        unreachable!("the table-write seam must abort the child process");
    });
}

/// Records a controlled transaction whose table write is refused in-process.
/// The runtime rolls the write back and records the rollback proof before the
/// process dies, so this is the only crash whose rollback may be proven.
fn controlled_rollback_child(repository_path: &Path) {
    let repository = Repository::discover(repository_path).expect("discover child repository");
    runtime().block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("open child runtime");
        let owner = state.acquire_lease(OWNER).await.expect("acquire writer");
        let (_, capability) = state
            .reserve_request_with_admission(request(), FINGERPRINT)
            .await
            .expect("reserve request");
        let capability = capability.expect("fresh owner-bound capability");
        state
            .start_request_with_owner_and_admission(request(), FINGERPRINT, owner, capability)
            .await
            .expect("start owned request");
        state
            .record_controlled_transaction(
                request(),
                FINGERPRINT,
                owner,
                &ControlledEffectToken::from_declaration(DeclaredEffectSet::None)
                    .expect("a None declaration yields the controlled token"),
            )
            .await
            .expect("record controlled transaction");
        let context = state.begin_activation().await.expect("capture activation");
        let mutation = TableMutation::insert([7; 16], "records", vec![1], vec![9])
            .expect("valid typed table mutation");
        let result = state
            .commit_table_request_activation(
                owner,
                request(),
                FINGERPRINT,
                &context,
                &[mutation],
                [8; 32],
                outcome(ROLLBACK_PROVEN),
                &RefuseAt(FaultPoint::AfterTableWrite),
                ActivationEffectReport::declare(DeclaredEffectSet::None),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(RuntimeError::FaultInjected(FaultPoint::AfterTableWrite))
            ),
            "the refused table write must surface as an in-process rollback"
        );
        std::process::abort();
    });
}

/// Commits an activation that declares possible external effects and whose
/// table write is refused in-process. The runtime rolls the write back, but
/// without a controlled declaration it records no rollback proof, so the
/// process dies with an unprovable rollback on record.
fn unreported_rollback_child(repository_path: &Path) {
    let repository = Repository::discover(repository_path).expect("discover child repository");
    runtime().block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("open child runtime");
        let owner = state.acquire_lease(OWNER).await.expect("acquire writer");
        let (_, capability) = state
            .reserve_request_with_admission(request(), FINGERPRINT)
            .await
            .expect("reserve request");
        let capability = capability.expect("fresh owner-bound capability");
        state
            .start_request_with_owner_and_admission(request(), FINGERPRINT, owner, capability)
            .await
            .expect("start owned request");
        let context = state.begin_activation().await.expect("capture activation");
        let mutation = TableMutation::insert([7; 16], "records", vec![1], vec![9])
            .expect("valid typed table mutation");
        let result = state
            .commit_table_request_activation(
                owner,
                request(),
                FINGERPRINT,
                &context,
                &[mutation],
                [8; 32],
                outcome(ROLLBACK_PROVEN),
                &RefuseAt(FaultPoint::AfterTableWrite),
                ActivationEffectReport::declare(DeclaredEffectSet::ExternalPossible),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(RuntimeError::FaultInjected(FaultPoint::AfterTableWrite))
            ),
            "the refused table write must surface as an in-process rollback"
        );
        std::process::abort();
    });
}

fn child_scenario() -> Option<Scenario> {
    env::var(SCENARIO)
        .ok()
        .and_then(|value| Scenario::parse(&value))
}

fn child_repository() -> PathBuf {
    PathBuf::from(env::var_os(SCENARIO_REPOSITORY).expect("child repository path"))
}

/// Runs this test binary again, crashing the child at `scenario`.
///
/// The child dies by SIGABRT on purpose. On Linux the child is a `/bin/sh` that
/// disables core dumps for itself and then `exec`s this test binary, so the
/// limit survives into the crashing process without unsafe code.
fn run_crashing_child(
    scenario: Scenario,
    repository_path: &Path,
    test_name: &str,
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
        .env(SCENARIO, scenario.tag())
        .env(SCENARIO_REPOSITORY, repository_path.as_os_str());
    command.status().expect("run crashing child")
}

#[test]
fn external_effect_at_the_kill_recovers_as_uncertain_never_rollback() {
    if child_scenario() == Some(Scenario::ExternalEffectAtKill) {
        external_effect_child(&child_repository());
    }

    let (directory, repository) = repository();
    let status = run_crashing_child(
        Scenario::ExternalEffectAtKill,
        directory.path(),
        EXTERNAL_EFFECT_TEST,
    );
    assert_eq!(
        status.signal(),
        Some(SIGABRT),
        "the child must die by signal inside the open writer transaction"
    );

    runtime().block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("a killed external effect reopens as valid recovery state");
        assert_eq!(
            state.committed_table_row("records", &[1]).await.unwrap(),
            None,
            "a row whose transaction never committed is not acknowledged"
        );

        // The started request and its recorded effect survive the kill; the
        // owner is still recorded and no disposition has been decided yet.
        let before = state
            .request_status(request(), FINGERPRINT)
            .await
            .unwrap()
            .expect("the started request survives the kill");
        assert_eq!(before.state, RequestState::Running);
        assert_eq!(before.terminal_outcome, None);
        assert_eq!(
            state.request_owner(request(), FINGERPRINT).await.unwrap(),
            Some(RequestOwner {
                owner_id: OWNER,
                epoch: 1,
            })
        );

        let fence = state
            .recover_abandoned(OWNER, REPLACEMENT)
            .await
            .expect("the abandoned owner is recoverable by compare-and-swap");
        assert_eq!(fence.epoch, 2, "takeover advances the writer epoch");
        let recovered = state
            .recover_running_request_with_outcomes(
                request(),
                FINGERPRINT,
                RequestOwner {
                    owner_id: OWNER,
                    epoch: 1,
                },
                fence,
                outcome(ROLLBACK_PROVEN),
                outcome(EFFECTS_UNCERTAIN),
            )
            .await
            .expect("the uncertain external effect recovers from durable evidence");
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain,
            "an external effect with unknown outcome is never a proven rollback"
        );
        assert_eq!(recovered.status.state, RequestState::Orphaned);
        assert_eq!(
            recovered.status.terminal_outcome,
            Some(outcome(EFFECTS_UNCERTAIN))
        );
        drop(state);

        let reopened = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("the recovered request reopens as valid recovery state");
        assert_eq!(
            reopened
                .request_recovery_disposition(request(), FINGERPRINT)
                .await
                .unwrap(),
            Some(RecoveryDisposition::ExternalEffectsUncertain)
        );
        assert_eq!(
            reopened.request_owner(request(), FINGERPRINT).await.unwrap(),
            None,
            "recovery releases the abandoned owner"
        );
        let (reserved, capability) = reopened
            .reserve_request_with_admission(request(), FINGERPRINT)
            .await
            .expect("the orphaned request stays addressable");
        assert_eq!(reserved.state, RequestState::Orphaned);
        assert_eq!(
            reserved.terminal_outcome,
            Some(outcome(EFFECTS_UNCERTAIN))
        );
        assert!(
            capability.is_none(),
            "an orphaned request never reissues admission"
        );
    });
}

#[test]
fn controlled_rollback_before_the_kill_is_the_only_proven_rollback() {
    if child_scenario() == Some(Scenario::ControlledRollbackThenKill) {
        controlled_rollback_child(&child_repository());
    }

    let (directory, repository) = repository();
    let status = run_crashing_child(
        Scenario::ControlledRollbackThenKill,
        directory.path(),
        CONTROLLED_ROLLBACK_TEST,
    );
    assert_eq!(
        status.signal(),
        Some(SIGABRT),
        "the child must die by signal after its in-process rollback"
    );

    runtime().block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("a rolled-back request reopens as valid recovery state");
        assert_eq!(
            state.committed_table_row("records", &[1]).await.unwrap(),
            None,
            "the rolled-back table write is not acknowledged"
        );

        let fence = state
            .recover_abandoned(OWNER, REPLACEMENT)
            .await
            .expect("the abandoned owner is recoverable by compare-and-swap");
        let recovered = state
            .recover_running_request_with_outcomes(
                request(),
                FINGERPRINT,
                RequestOwner {
                    owner_id: OWNER,
                    epoch: 1,
                },
                fence,
                outcome(ROLLBACK_PROVEN),
                outcome(EFFECTS_UNCERTAIN),
            )
            .await
            .expect("the recorded rollback proof recovers from durable evidence");
        assert_eq!(recovered.disposition, RecoveryDisposition::RollbackProven);
        assert_eq!(recovered.status.state, RequestState::Orphaned);
        assert_eq!(
            recovered.status.terminal_outcome,
            Some(outcome(ROLLBACK_PROVEN))
        );
        drop(state);

        let reopened = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("the recovered request reopens as valid recovery state");
        assert_eq!(
            reopened
                .request_recovery_disposition(request(), FINGERPRINT)
                .await
                .unwrap(),
            Some(RecoveryDisposition::RollbackProven)
        );
        let reopened_status = reopened
            .request_status(request(), FINGERPRINT)
            .await
            .unwrap()
            .expect("the recovered request stays addressable");
        assert_eq!(
            reopened_status.terminal_outcome,
            Some(outcome(ROLLBACK_PROVEN))
        );
        assert_eq!(
            reopened.committed_table_row("records", &[1]).await.unwrap(),
            None
        );
    });
}

#[test]
fn explicit_external_possible_rollback_before_the_kill_is_never_proven() {
    if child_scenario() == Some(Scenario::UnreportedRollbackThenKill) {
        unreported_rollback_child(&child_repository());
    }

    let (directory, repository) = repository();
    let status = run_crashing_child(
        Scenario::UnreportedRollbackThenKill,
        directory.path(),
        UNREPORTED_ROLLBACK_TEST,
    );
    assert_eq!(
        status.signal(),
        Some(SIGABRT),
        "the child must die by signal after its in-process rollback"
    );

    runtime().block_on(async {
        let state = RuntimeState::open(&repository, identity(), [3; 32])
            .await
            .expect("an unreported rollback reopens as valid recovery state");
        assert_eq!(
            state.committed_table_row("records", &[1]).await.unwrap(),
            None,
            "the rolled-back table write is not acknowledged"
        );

        let fence = state
            .recover_abandoned(OWNER, REPLACEMENT)
            .await
            .expect("the abandoned owner is recoverable by compare-and-swap");
        let recovered = state
            .recover_running_request_with_outcomes(
                request(),
                FINGERPRINT,
                RequestOwner {
                    owner_id: OWNER,
                    epoch: 1,
                },
                fence,
                outcome(ROLLBACK_PROVEN),
                outcome(EFFECTS_UNCERTAIN),
            )
            .await
            .expect("the unreported rollback recovers from durable evidence");
        assert_ne!(
            recovered.disposition,
            RecoveryDisposition::RollbackProven,
            "an activation that did not declare controlled effects is never a proven rollback"
        );
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain
        );
        assert_eq!(recovered.status.state, RequestState::Orphaned);
        assert_eq!(
            recovered.status.terminal_outcome,
            Some(outcome(EFFECTS_UNCERTAIN))
        );
    });
}
