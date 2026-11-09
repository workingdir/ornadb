use super::*;
use crate::format3::format3_repository_for_table;
use orna_runtime::{FaultPoint, RecoveryDisposition, RuntimeError};

const MUSIC_SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";
const INSERT_LONDON: &str = r#"main.Music.insert({ id: "london", title: "Live in London" })"#;

struct FailAt(FaultPoint);

impl FaultInjector for FailAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
}

#[test]
fn committed_repl_insert_finishes_its_request_row() {
    let (_directory, repository, _relation) = format3_repository_for_table(MUSIC_SOURCE, "Music");
    let ReplRuntime {
        repository,
        mut session,
        transactions,
    } = repl_runtime(Some(repository)).expect("repl runtime");
    let mut transactions = transactions.expect("repl transactions");
    assert!(
        run_repl_input(
            repository.as_ref(),
            Some(&mut transactions),
            &mut session,
            INSERT_LONDON,
        )
        .is_ok(),
        "the REPL insert commits"
    );
    let state = &transactions.writer.as_ref().expect("runtime writer").state;
    let running = transactions
        .runtime
        .block_on(state.running_requests())
        .expect("running requests");
    assert!(
        running.is_empty(),
        "a committed REPL insert leaves no running request"
    );
}

#[test]
fn sys_dispatch_observes_request_row_already_running() {
    let (directory, repository, _relation) = format3_repository_for_table(MUSIC_SOURCE, "Music");
    let ReplRuntime {
        repository,
        mut session,
        transactions,
    } = repl_runtime(Some(repository)).expect("repl runtime");
    let mut transactions = transactions.expect("repl transactions");
    FIRST_SYS_DISPATCH_RUNNING.with(|first| first.set(None));
    std::fs::write(directory.path().join("probe.bin"), b"probe").expect("probe file");
    let input = format!(
        "sys.blob.capture_file({:?}, \"probe.bin\", 65536)",
        directory.path().to_string_lossy().as_ref()
    );
    run_repl_input(
        repository.as_ref(),
        Some(&mut transactions),
        &mut session,
        &input,
    )
    .expect("the blob capture succeeds");
    let running_at_first_dispatch = FIRST_SYS_DISPATCH_RUNNING.with(std::cell::Cell::get);
    assert_eq!(
        running_at_first_dispatch,
        Some(1),
        "the request row is reserved before the first sys binding runs"
    );
}

/// Runs an insert whose commit fails at `point` and asserts the uncertain
/// diagnostic and the still-running request row, then that fenced recovery
/// settles it without proving a rollback.
fn assert_commit_fault_reports_uncertain(point: FaultPoint) {
    let (_directory, repository, _relation) = format3_repository_for_table(MUSIC_SOURCE, "Music");
    let ReplRuntime {
        repository,
        mut session,
        transactions,
    } = repl_runtime(Some(repository)).expect("repl runtime");
    let mut transactions = transactions.expect("repl transactions");
    transactions.faults = Arc::new(FailAt(point));
    let failure = run_repl_input(
        repository.as_ref(),
        Some(&mut transactions),
        &mut session,
        INSERT_LONDON,
    )
    .err()
    .unwrap_or_else(|| panic!("{point:?}: the injected fault fails the REPL request"));
    assert_eq!(failure.code, "ORNA-REPL-EFFECT-UNCERTAIN", "{point:?}");

    let state = &transactions.writer.as_ref().expect("runtime writer").state;
    let running = transactions
        .runtime
        .block_on(state.running_requests())
        .expect("running requests");
    assert_eq!(
        running.len(),
        1,
        "{point:?}: the failed commit leaves its own row running"
    );
    let row = &running[0];
    let lost = transactions
        .runtime
        .block_on(state.request_owner(row.identity, row.fingerprint))
        .expect("request owner")
        .expect("the running row has an owner");
    let fence = transactions
        .runtime
        .block_on(state.recover_abandoned(lost.owner_id, [0x52; 16]))
        .expect("fence the abandoned writer");
    let rollback = TerminalOutcome::new(b"orna.repl.rollback.v1".to_vec()).expect("outcome");
    let uncertain = TerminalOutcome::new(b"orna.repl.uncertain.v1".to_vec()).expect("outcome");
    let recovered = transactions
        .runtime
        .block_on(state.recover_running_request_with_outcomes(
            row.identity,
            row.fingerprint,
            lost,
            fence,
            rollback,
            uncertain,
        ))
        .expect("recover the running request");
    assert_ne!(
        recovered.disposition,
        RecoveryDisposition::RollbackProven,
        "{point:?}: a REPL activation declares ExternalPossible effects, so a rollback is never proven"
    );
    assert_eq!(
        recovered.disposition,
        RecoveryDisposition::ExternalEffectsUncertain,
        "{point:?}"
    );
    let after = transactions
        .runtime
        .block_on(state.running_requests())
        .expect("running requests after recovery");
    assert!(
        after.is_empty(),
        "{point:?}: recovery settles the fenced request row"
    );
}

#[test]
fn repl_commit_fault_reports_uncertain_and_keeps_its_request_row_running() {
    for point in [
        FaultPoint::BeforeTableWrite,
        FaultPoint::AfterTableWrite,
        FaultPoint::BeforeTerminalClaim,
        FaultPoint::AfterTerminalClaim,
    ] {
        assert_commit_fault_reports_uncertain(point);
    }
}

#[test]
fn repl_pure_evaluation_failure_keeps_its_evaluator_code() {
    let (_directory, repository, _relation) = format3_repository_for_table(MUSIC_SOURCE, "Music");
    let ReplRuntime {
        repository,
        mut session,
        transactions,
    } = repl_runtime(Some(repository)).expect("repl runtime");
    let mut transactions = transactions.expect("repl transactions");
    let failure = run_repl_input(
        repository.as_ref(),
        Some(&mut transactions),
        &mut session,
        "1 / 0",
    )
    .expect_err("the division by zero fails");
    assert_eq!(
        failure.code, "ORNA-EVAL-DIVIDE-BY-ZERO",
        "a failure with no external effect keeps the evaluator code"
    );
}

#[test]
fn repl_effect_uncertain_failure_keeps_the_evaluation_cause() {
    let failure = repl_evaluation_failure(ReplEvaluationFailure {
        error: ApplicationError::Evaluation("ORNA-EVAL-DIVIDE-BY-ZERO".to_owned()),
        external_effects_possible: true,
    });
    assert_eq!(failure.code, REPL_EFFECT_UNCERTAIN);
    let detail = failure
        .detail
        .expect("an uncertain outcome carries its cause in the detail");
    assert_eq!(detail.code, REPL_EFFECT_UNCERTAIN);
    assert!(
        detail.message.contains("ORNA-EVAL-DIVIDE-BY-ZERO"),
        "the detail names the evaluation cause: {}",
        detail.message
    );
}

#[test]
fn repl_effect_free_failure_keeps_its_own_code_without_detail() {
    let failure = repl_evaluation_failure(ReplEvaluationFailure {
        error: ApplicationError::Evaluation("ORNA-EVAL-DIVIDE-BY-ZERO".to_owned()),
        external_effects_possible: false,
    });
    assert_eq!(failure.code, "ORNA-EVAL-DIVIDE-BY-ZERO");
    assert!(failure.detail.is_none());
}

#[test]
fn repl_effectful_failure_before_any_external_binding_leaves_no_running_row() {
    // `n` is an Int, so `1 / 0` type-checks and the insert stages as an
    // effectful activation that fails during evaluation, before any binding.
    let (_directory, repository, _relation) =
        format3_repository_for_table("pub table Counter(id: Str) { n: Int, }\n", "Counter");
    let ReplRuntime {
        repository,
        mut session,
        transactions,
    } = repl_runtime(Some(repository)).expect("repl runtime");
    let mut transactions = transactions.expect("repl transactions");
    let failure = run_repl_input(
        repository.as_ref(),
        Some(&mut transactions),
        &mut session,
        r#"main.Counter.insert({ id: "zero", n: 1 / 0 })"#,
    )
    .expect_err("the division by zero fails before any write");
    assert_ne!(
        failure.code, "ORNA-REPL-EFFECT-UNCERTAIN",
        "no external binding ran, so the failure is not uncertain"
    );
    let state = transactions
        .writer_state()
        .expect("the effectful input opened the runtime state");
    let running = transactions
        .runtime
        .block_on(state.running_requests())
        .expect("running requests");
    assert!(
        running.is_empty(),
        "a failure before any external effect cancels its reserved row"
    );
}

#[test]
fn repl_enum_keyed_assertion_write_is_refused_as_repl_effect() {
    // The candidate seam accepts only scalar primary-key components and
    // checks every table of an asserting project, so an enum key anywhere in
    // the project refuses the write before it is published (ornadb-gq4g).
    // The refused write's reserved row is cancelled; it is never admitted.
    let (_directory, repository, _relation) = format3_repository_for_table(
        "pub enum Level {\n    low,\n    high,\n}\n\
             pub table Task(level: Level) {\n    title: Str,\n    \
             assert every(row => row.title != \"\");\n}\n",
        "Task",
    );
    let ReplRuntime {
        repository,
        mut session,
        transactions,
    } = repl_runtime(Some(repository)).expect("repl runtime");
    let mut transactions = transactions.expect("repl transactions");
    let failure = run_repl_input(
        repository.as_ref(),
        Some(&mut transactions),
        &mut session,
        r#"main.Task.insert({ level: main.Level.low, title: "Ship" })"#,
    )
    .expect_err("the enum-keyed write is refused before it is published");
    assert_eq!(failure.code, "ORNA-REPL-EFFECT");
    let state = transactions
        .writer_state()
        .expect("the effectful input opened the runtime state");
    let running = transactions
        .runtime
        .block_on(state.running_requests())
        .expect("running requests");
    assert!(
        running.is_empty(),
        "a refused candidate cancels its reserved row"
    );
}
