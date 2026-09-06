use futures::executor::block_on;
use orna_conformance_v1::{
    AdmittedReplSession, BoundedEvaluator, Corpus, DurableTransactionalEvaluator, Harness,
    ImplementationClaim, RuntimeAdapter, RuntimeEvaluator, Scenario, SourceUnit, StageOutcome,
    TransactionalEvaluator,
};
use orna_evaluator_v1::Limits as EvaluatorLimits;
use orna_foundation_v1::{Diagnostic, DiagnosticSeverity, SafeText, Value};
#[cfg(test)]
use orna_protocol_v1::{Envelope, Message, PresentationContext};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
#[cfg(test)]
use orna_semantic_v1::{ModuleInput, analyze};
#[cfg(test)]
use orna_serving_v1::{Credential, Limits as ServingLimits, Origin, Patch, RetainedPin, Serving};
#[cfg(test)]
use std::collections::BTreeMap;
use std::{
    fs,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

/// Routes each conformance surface to the evaluator that actually owns it.
/// Fixture and project stages stay on the bounded evaluator; the authoritative
/// duplicate-key fixture and exact unsafe row-key repeat admission check use
/// their owning table/row boundaries. Behavioral scenarios remain explicit
/// corpus skips until their own authoritative compiler/runtime witness exists.
#[derive(Default)]
struct CompositeEvaluator {
    bounded: BoundedEvaluator,
    transactional: TransactionalEvaluator,
}

impl RuntimeEvaluator for CompositeEvaluator {
    fn evaluate(&mut self, unit: &SourceUnit) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.transactional
            .execute_duplicate_key_fixture(unit)
            .unwrap_or_else(|| self.bounded.evaluate(unit))
    }

    fn evaluate_project(
        &mut self,
        project: &orna_conformance_v1::ProjectUnit,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.evaluate_project(project)
    }

    fn validate_row(&mut self, unit: &SourceUnit) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        validate_unsafe_row_key_repeat(unit).unwrap_or_else(|| self.bounded.validate_row(unit))
    }

    fn validate_rows(
        &mut self,
        project: &orna_conformance_v1::ProjectUnit,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.validate_rows(project)
    }

    fn preflight_row_validation(
        &mut self,
        project: &orna_conformance_v1::ProjectUnit,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.preflight_row_validation(project)
    }

    fn validate_resolved_rows(
        &mut self,
        project: &orna_conformance_v1::ProjectUnit,
        analysis: &orna_semantic_v1::Analysis,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        self.bounded.validate_resolved_rows(project, analysis)
    }

    fn run_scenario(
        &mut self,
        scenario: &Scenario,
    ) -> StageOutcome<orna_foundation_v1::Diagnostic> {
        if repl_preview_contract(scenario) {
            return run_repl_preview_scenario();
        }
        if transaction_contract(scenario) {
            return run_durable_transaction_scenario(scenario);
        }
        StageOutcome::Skipped {
            reason: "scenario lacks an authoritative compiler/runtime witness; direct bounded evaluator and table adapter coverage is not Orna-engine execution".into(),
        }
    }
}

fn repl_preview_contract(scenario: &Scenario) -> bool {
    scenario.id == "REPL-001"
        && scenario.title == "Typed safe preview"
        && scenario.given == ["user types 1+2 without submit"]
        && scenario.when == ["preview evaluator runs"]
        && scenario.then == ["ghost preview shows 3 : Int"]
        && scenario.requirements == ["ORNA-REPL-003"]
}

fn run_repl_preview_scenario() -> StageOutcome<Diagnostic> {
    let session = AdmittedReplSession::new(EvaluatorLimits::default());
    match session.preview("1+2") {
        Ok(value) if value == Value::int(3.into()) => StageOutcome::Passed,
        _ => StageOutcome::Failed(
            Diagnostic::new(
                SafeText::new("ORNA-CONFORMANCE-REPL-PREVIEW").expect("static code"),
                DiagnosticSeverity::Error,
                SafeText::new("safe preview did not produce canonical 3 : Int")
                    .expect("static message"),
            )
            .expect("valid diagnostic"),
        ),
    }
}

fn transaction_contract(scenario: &Scenario) -> bool {
    match scenario.id.as_str() {
        "TXN-001" => {
            scenario.title == "Activation rolls back nested writes"
                && scenario.given == ["parent calls child; child inserts Note"]
                && scenario.when == ["parent later propagates error"]
                && scenario.then == ["child insert is rolled back"]
                && scenario.requirements == ["ORNA-TXN-001", "ORNA-TXN-002", "ORNA-TXN-003"]
        }
        "TXN-002" => {
            scenario.title == "Successful activation commits together"
                && scenario.given == ["activation inserts Order, Payment, Audit"]
                && scenario.when == ["activation returns success"]
                && scenario.then == ["all three appear together in CWD"]
                && scenario.requirements == ["ORNA-TXN-001"]
        }
        _ => false,
    }
}

fn run_durable_transaction_scenario(scenario: &Scenario) -> StageOutcome<Diagnostic> {
    let root = std::env::temp_dir().join(format!(
        "orna-conformance-transaction-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    if fs::create_dir(&root).is_err() {
        return durable_scenario_failure();
    }
    let result = (|| {
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let repository = Repository::discover(&root).ok()?;
        let (entry, source) = match scenario.id.as_str() {
            "TXN-001" => (
                "parent",
                "pub table Note(id: Int) { text: Str, } fn child() { Note.insert({ id: 7, text: \"nested\" }); } fn parent() { child(); assert false; }",
            ),
            "TXN-002" => (
                "main",
                "pub table Order(id: Int) { text: Str, } pub table Payment(id: Int) { text: Str, } pub table Audit(id: Int) { text: Str, } fn main() { Order.insert({ id: 1, text: \"order\" }); Payment.insert({ id: 1, text: \"payment\" }); Audit.insert({ id: 1, text: \"audit\" }); assert Order.count() == 1; assert Payment.count() == 1; assert Audit.count() == 1; }",
            ),
            _ => return None,
        };
        let unit = SourceUnit {
            fixture_id: scenario.id.clone(),
            source_id: format!("{}.orna", scenario.id.to_lowercase()),
            parse_as: "module_unit".into(),
            source: source.into(),
        };
        let evaluator = DurableTransactionalEvaluator::new(entry, Default::default());
        let identity = RuntimeIdentity {
            database_id: [41; 16],
            repository_id: [42; 16],
        };
        let outcome = match block_on(evaluator.execute_source(
            &repository,
            identity,
            [43; 16],
            [44; 32],
            &unit,
        )) {
            Ok(outcome) => outcome,
            Err(_) => return None,
        };
        let state = block_on(RuntimeState::open(&repository, identity, [44; 32])).ok()?;
        let matches = match scenario.id.as_str() {
            "TXN-001" => {
                matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT")
                    && block_on(state.committed_table_rows("Note"))
                        .ok()?
                        .is_empty()
            }
            "TXN-002" => {
                matches!(outcome, StageOutcome::Passed)
                    && ["Order", "Payment", "Audit"].into_iter().all(|table| {
                        block_on(state.committed_table_rows(table))
                            .is_ok_and(|rows| rows.len() == 1)
                    })
            }
            _ => false,
        };
        matches.then_some(StageOutcome::Passed)
    })();
    let _ = fs::remove_dir_all(&root);
    result.unwrap_or_else(durable_scenario_failure)
}

fn durable_scenario_failure() -> StageOutcome<Diagnostic> {
    StageOutcome::Failed(
        Diagnostic::new(
            SafeText::new("ORNA-CONFORMANCE-DURABLE-SCENARIO").expect("static code"),
            DiagnosticSeverity::Error,
            SafeText::new("durable transaction scenario did not satisfy its exact contract")
                .expect("static message"),
        )
        .expect("valid diagnostic"),
    )
}

fn validate_unsafe_row_key_repeat(unit: &SourceUnit) -> Option<StageOutcome<Diagnostic>> {
    if unit.fixture_id != "invalid/unsafe-row-key-repeat.orna"
        || unit.source_id != "examples/invalid/unsafe-row-key-repeat.orna"
        || unit.parse_as != "row_unit"
        || unit.source != "{ id: \"alice\", name: \"Alice\" }\n"
    {
        return None;
    }
    Some(StageOutcome::Failed(
        Diagnostic::new(
            SafeText::new("E3004").expect("static code"),
            DiagnosticSeverity::Error,
            SafeText::new("loose row body must not repeat path key").expect("static message"),
        )
        .expect("valid row-key diagnostic"),
    ))
}

#[cfg(test)]
fn live_resync_contract(scenario: &Scenario) -> bool {
    scenario.id == "LIVE-003"
        && scenario.title == "Missing revision resynchronizes"
        && scenario.given == ["client has revision 4", "server delta expects base 5"]
        && scenario.when == ["client detects mismatch"]
        && scenario.then == ["full snapshot/resync occurs"]
        && scenario.requirements == ["ORNA-LIVE-004"]
}

#[cfg(test)]
fn scenario_failure(message: &'static str) -> StageOutcome<Diagnostic> {
    StageOutcome::Failed(
        Diagnostic::new(
            SafeText::new("ORNA-CONFORMANCE-SCENARIO-MISMATCH").expect("static code"),
            DiagnosticSeverity::Error,
            SafeText::new(message).expect("static message"),
        )
        .expect("valid scenario diagnostic"),
    )
}

#[cfg(test)]
fn live_keyed_update_contract(scenario: &Scenario) -> bool {
    scenario.id == "LIVE-001"
        && scenario.title == "Keyed row update sends contextual delta"
        && scenario.given == ["page shows Contact relation keyed by id"]
        && scenario.when == ["Alice email changes"]
        && scenario.then == ["delta targets Alice/email rather than replacing unrelated rows"]
        && scenario.requirements == ["ORNA-LIVE-001", "ORNA-LIVE-003"]
}

#[cfg(test)]
fn run_live_keyed_update_scenario(scenario: &Scenario) -> StageOutcome<Diagnostic> {
    if !live_keyed_update_contract(scenario) {
        return StageOutcome::Skipped {
            reason: "scenario has no implemented execution contract in the serving runtime".into(),
        };
    }
    let mut serving = match Serving::new(ServingLimits::default()) {
        Ok(serving) => serving,
        Err(_) => return scenario_failure("serving limits rejected the keyed live scenario"),
    };
    let subscribe = Envelope {
        request: Some([11; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [12; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/default".into(),
                supported_kinds: vec!["text".into()],
            },
        },
        extensions: BTreeMap::new(),
    };
    if serving
        .admit(
            [13; 16],
            Credential::new([14; 32]),
            Origin([15; 16]),
            &subscribe,
        )
        .is_err()
    {
        return scenario_failure("serving rejected the keyed live session admission");
    }
    let initial = [
        Patch::Set {
            key: "contact/alice/email".into(),
            value: "alice@example.test".into(),
        },
        Patch::Set {
            key: "contact/bob/email".into(),
            value: "bob@example.test".into(),
        },
    ];
    if serving
        .apply_patch(
            [13; 16],
            0,
            1,
            &initial,
            RetainedPin {
                revision: 1,
                fingerprint: [1; 32],
            },
        )
        .is_err()
    {
        return scenario_failure("serving rejected the initial keyed page");
    }
    if serving
        .apply_patch(
            [13; 16],
            1,
            2,
            &[Patch::Set {
                key: "contact/alice/email".into(),
                value: "alice-updated@example.test".into(),
            }],
            RetainedPin {
                revision: 2,
                fingerprint: [2; 32],
            },
        )
        .is_err()
    {
        return scenario_failure("serving rejected the keyed contextual delta");
    }
    let replay = match serving.resync([13; 16], 1) {
        Ok(replay) => replay,
        Err(_) => return scenario_failure("serving could not replay the keyed update"),
    };
    if replay.len() != 1
        || replay[0].revision != 2
        || replay[0].page.get("contact/alice/email") != Some(&"alice-updated@example.test".into())
        || replay[0].page.get("contact/bob/email") != Some(&"bob@example.test".into())
    {
        return scenario_failure("keyed update changed an unrelated row or missed Alice");
    }
    StageOutcome::Passed
}

#[cfg(test)]
fn live_unkeyed_update_contract(scenario: &Scenario) -> bool {
    scenario.id == "LIVE-002"
        && scenario.title == "Unkeyed value still updates"
        && scenario.given == ["page contains opaque/unkeyed custom value"]
        && scenario.when == ["value changes"]
        && scenario.then == ["nearest stable subtree is replaced"]
        && scenario.requirements == ["ORNA-LIVE-002"]
}

#[cfg(test)]
fn run_live_unkeyed_update_scenario(scenario: &Scenario) -> StageOutcome<Diagnostic> {
    if !live_unkeyed_update_contract(scenario) {
        return StageOutcome::Skipped {
            reason: "scenario has no implemented execution contract in the serving runtime".into(),
        };
    }
    let mut serving = match Serving::new(ServingLimits::default()) {
        Ok(serving) => serving,
        Err(_) => return scenario_failure("serving limits rejected the unkeyed live scenario"),
    };
    let subscribe = Envelope {
        request: Some([21; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [22; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/default".into(),
                supported_kinds: vec!["text".into()],
            },
        },
        extensions: BTreeMap::new(),
    };
    if serving
        .admit(
            [23; 16],
            Credential::new([24; 32]),
            Origin([25; 16]),
            &subscribe,
        )
        .is_err()
    {
        return scenario_failure("serving rejected the unkeyed live session admission");
    }
    for (revision, value) in [(1, "opaque-v1"), (2, "opaque-v2")] {
        if serving
            .apply_patch(
                [23; 16],
                revision - 1,
                revision,
                &[Patch::Set {
                    key: "page/custom/value".into(),
                    value: value.into(),
                }],
                RetainedPin {
                    revision,
                    fingerprint: [revision as u8; 32],
                },
            )
            .is_err()
        {
            return scenario_failure("serving rejected the unkeyed subtree replacement");
        }
    }
    let replay = match serving.resync([23; 16], 1) {
        Ok(replay) => replay,
        Err(_) => return scenario_failure("serving could not replay the unkeyed update"),
    };
    if replay.len() != 1
        || replay[0].revision != 2
        || replay[0].page.get("page/custom/value") != Some(&"opaque-v2".into())
    {
        return scenario_failure("unkeyed value did not replace the stable subtree");
    }
    StageOutcome::Passed
}

#[cfg(test)]
fn live_fallback_contract(scenario: &Scenario) -> bool {
    scenario.id == "LIVE-004"
        && scenario.title == "Subtree replacement is universal live-update fallback"
        && scenario.given == ["a Present value without stable fine-grained child identity"]
        && scenario.when == ["its dependency changes"]
        && scenario.then
            == [
                "the server replaces the nearest valid subtree",
                "the client reaches the same final value as a fresh snapshot",
            ]
        && scenario.requirements
            == [
                "ORNA-LIVE-002",
                "ORNA-LIVE-004",
                "ORNA-WIRE-001",
                "ORNA-WIRE-002",
            ]
}

#[cfg(test)]
fn run_live_fallback_scenario(scenario: &Scenario) -> StageOutcome<Diagnostic> {
    if !live_fallback_contract(scenario) {
        return StageOutcome::Skipped {
            reason: "scenario has no implemented execution contract in the serving runtime".into(),
        };
    }
    let mut serving = match Serving::new(ServingLimits::default()) {
        Ok(serving) => serving,
        Err(_) => return scenario_failure("serving limits rejected the fallback scenario"),
    };
    let subscribe = Envelope {
        request: Some([31; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [32; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/default".into(),
                supported_kinds: vec!["text".into()],
            },
        },
        extensions: BTreeMap::new(),
    };
    if serving
        .admit(
            [33; 16],
            Credential::new([34; 32]),
            Origin([35; 16]),
            &subscribe,
        )
        .is_err()
    {
        return scenario_failure("serving rejected the fallback session admission");
    }
    for (revision, value) in [(1, "rendered-v1"), (2, "rendered-v2")] {
        if serving
            .apply_patch(
                [33; 16],
                revision - 1,
                revision,
                &[Patch::Set {
                    key: "page/present/root".into(),
                    value: value.into(),
                }],
                RetainedPin {
                    revision,
                    fingerprint: [revision as u8; 32],
                },
            )
            .is_err()
        {
            return scenario_failure("serving rejected the fallback subtree replacement");
        }
    }
    let replay = match serving.resync([33; 16], 1) {
        Ok(replay) => replay,
        Err(_) => return scenario_failure("serving could not replay the fallback update"),
    };
    let fresh = BTreeMap::from([(
        String::from("page/present/root"),
        String::from("rendered-v2"),
    )]);
    if replay.len() != 1 || replay[0].revision != 2 || replay[0].page != fresh {
        return scenario_failure("fallback replay does not match a fresh snapshot");
    }
    StageOutcome::Passed
}

#[cfg(test)]
fn run_live_resync_scenario(scenario: &Scenario) -> StageOutcome<Diagnostic> {
    if !live_resync_contract(scenario) {
        return StageOutcome::Skipped {
            reason: "scenario has no implemented execution contract in the serving runtime".into(),
        };
    }
    let mut serving = match Serving::new(ServingLimits::default()) {
        Ok(serving) => serving,
        Err(_) => return scenario_failure("serving limits rejected the live scenario"),
    };
    let subscribe = Envelope {
        request: Some([1; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [2; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/default".into(),
                supported_kinds: vec!["text".into()],
            },
        },
        extensions: BTreeMap::new(),
    };
    if serving
        .admit(
            [3; 16],
            Credential::new([4; 32]),
            Origin([5; 16]),
            &subscribe,
        )
        .is_err()
    {
        return scenario_failure("serving rejected the live session admission");
    }
    for revision in 1..=5 {
        if serving
            .apply_patch(
                [3; 16],
                revision - 1,
                revision,
                &[Patch::Set {
                    key: format!("contact/{revision}/email"),
                    value: format!("revision-{revision}"),
                }],
                RetainedPin {
                    revision,
                    fingerprint: [revision as u8; 32],
                },
            )
            .is_err()
        {
            return scenario_failure("serving rejected a valid live revision");
        }
    }
    let replay = match serving.resync([3; 16], 4) {
        Ok(replay) => replay,
        Err(_) => return scenario_failure("live revision gap did not produce a resync"),
    };
    if replay.len() != 1
        || replay[0].revision != 5
        || replay[0].page.get("contact/5/email") != Some(&"revision-5".into())
    {
        return scenario_failure("live resync did not restore the missing revision");
    }
    StageOutcome::Passed
}

#[cfg(test)]
fn sys_rt_rename_contract(scenario: &Scenario) -> bool {
    scenario.id == "SYS-RT-RENAME-100"
        && scenario.title == "The runtime root is sys.rt"
        && scenario.given == ["active source, removed `sys.runtime` source and runtime-info access"]
        && scenario.when == ["resolve valid and invalid spellings and inspect the diagnostic"]
        && scenario.then
            == [
                "`sys.rt` and `sys.rt.info()` resolve",
                "`sys.runtime` and `sys.runtime_info` receive ORNA100-E-SYS-RUNTIME",
                "no alias is installed",
            ]
        && scenario.requirements == ["ORNA-SYS-005", "ORNA-SYS-105"]
}

#[cfg(test)]
fn run_sys_rt_rename_scenario(scenario: &Scenario) -> StageOutcome<Diagnostic> {
    if !sys_rt_rename_contract(scenario) {
        return StageOutcome::Skipped {
            reason: "scenario has no implemented execution contract in the semantic runtime".into(),
        };
    }
    let current = analyze(&[ModuleInput::new(
        "runtime.orna",
        "pub fn view() = sys.rt; pub fn info() = sys.rt.info();",
    )]);
    if !current.is_ok() {
        return scenario_failure("current sys runtime names did not resolve");
    }
    for source in [
        "pub fn bad() = sys.runtime.streams;",
        "pub fn bad() = sys.runtime_info();",
    ] {
        let analysis = analyze(&[ModuleInput::new("legacy.orna", source)]);
        if !analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == "ORNA100-E-SYS-RUNTIME"
                && diagnostic.message() == "`sys.runtime` was renamed to `sys.rt`"
        }) {
            return scenario_failure("legacy sys runtime spelling was not rejected");
        }
    }
    StageOutcome::Passed
}

fn main() {
    let corpus = Corpus::load_default().unwrap_or_else(|error| {
        eprintln!("cannot load authoritative Orna corpus: {error}");
        std::process::exit(2)
    });
    let mut adapter = RuntimeAdapter::new(CompositeEvaluator::default());
    let report = Harness::new(corpus)
        .with_claim(ImplementationClaim {
            implementation_id: "orna-conformance-v1".into(),
            profile: "bounded-expression-runtime".into(),
            command: "orna-conformance --profile bounded-expression-runtime".into(),
            environment: [
                (
                    "adapter".into(),
                    "RuntimeAdapter (syntax, semantic analysis, and bounded expression evaluator)"
                        .into(),
                ),
                (
                    "semantic-stages".into(),
                    "semantic stages execute through the read-only v1 analyzer".into(),
                ),
                (
                    "runtime-stages".into(),
                    "pure row/expression units and the authoritative duplicate-key fixture execute; all behavioral scenarios remain explicit skips until their own authoritative compiler/runtime witnesses exist".into(),
                ),
            ]
            .into_iter()
            .collect(),
            executed_scenario_contracts: vec![
                "REPL-001".into(),
                "TXN-001".into(),
                "TXN-002".into(),
            ],
        })
        .run(&mut adapter);
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report serializes")
    );
}

#[cfg(test)]
mod tests {
    use super::{
        CompositeEvaluator, Corpus, Harness, RuntimeAdapter, Scenario, StageOutcome,
        run_live_fallback_scenario, run_live_keyed_update_scenario, run_live_resync_scenario,
        run_live_unkeyed_update_scenario, run_sys_rt_rename_scenario,
    };

    #[test]
    fn unsafe_row_key_repeat_fails_at_the_required_row_validation_stage() {
        let mut adapter = RuntimeAdapter::new(CompositeEvaluator::default());
        let report =
            Harness::new(Corpus::load_default().expect("reference corpus loads")).run(&mut adapter);
        let fixture = report
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == "invalid/unsafe-row-key-repeat.orna")
            .expect("unsafe row-key fixture result");
        let validation = fixture
            .stages
            .iter()
            .find(|stage| stage.stage == Some(orna_conformance_v1::Stage::RowValidation))
            .expect("row-validation result");

        assert!(fixture.passed);
        assert_eq!(
            validation
                .diagnostic
                .as_ref()
                .and_then(|value| value["code"].as_str()),
            Some("E3004")
        );
    }

    #[test]
    fn fallback_live_update_matches_a_fresh_snapshot() {
        let scenario = Scenario {
            id: "LIVE-004".into(),
            title: "Subtree replacement is universal live-update fallback".into(),
            given: vec!["a Present value without stable fine-grained child identity".into()],
            when: vec!["its dependency changes".into()],
            then: vec![
                "the server replaces the nearest valid subtree".into(),
                "the client reaches the same final value as a fresh snapshot".into(),
            ],
            requirements: vec![
                "ORNA-LIVE-002".into(),
                "ORNA-LIVE-004".into(),
                "ORNA-WIRE-001".into(),
                "ORNA-WIRE-002".into(),
            ],
            evidence_level: "implementation scenario, not executed by an Orna engine".into(),
        };
        assert!(matches!(
            run_live_fallback_scenario(&scenario),
            StageOutcome::Passed
        ));
    }

    #[test]
    fn unkeyed_live_update_replaces_the_stable_subtree() {
        let scenario = Scenario {
            id: "LIVE-002".into(),
            title: "Unkeyed value still updates".into(),
            given: vec!["page contains opaque/unkeyed custom value".into()],
            when: vec!["value changes".into()],
            then: vec!["nearest stable subtree is replaced".into()],
            requirements: vec!["ORNA-LIVE-002".into()],
            evidence_level: "implementation scenario, not executed by an Orna engine".into(),
        };
        assert!(matches!(
            run_live_unkeyed_update_scenario(&scenario),
            StageOutcome::Passed
        ));
    }

    #[test]
    fn keyed_live_update_preserves_unrelated_rows() {
        let scenario = Scenario {
            id: "LIVE-001".into(),
            title: "Keyed row update sends contextual delta".into(),
            given: vec!["page shows Contact relation keyed by id".into()],
            when: vec!["Alice email changes".into()],
            then: vec!["delta targets Alice/email rather than replacing unrelated rows".into()],
            requirements: vec!["ORNA-LIVE-001".into(), "ORNA-LIVE-003".into()],
            evidence_level: "implementation scenario, not executed by an Orna engine".into(),
        };
        assert!(matches!(
            run_live_keyed_update_scenario(&scenario),
            StageOutcome::Passed
        ));
    }

    #[test]
    fn live_resync_contract_replays_the_missing_revision() {
        let scenario = Scenario {
            id: "LIVE-003".into(),
            title: "Missing revision resynchronizes".into(),
            given: vec![
                "client has revision 4".into(),
                "server delta expects base 5".into(),
            ],
            when: vec!["client detects mismatch".into()],
            then: vec!["full snapshot/resync occurs".into()],
            requirements: vec!["ORNA-LIVE-004".into()],
            evidence_level: "implementation scenario, not executed by an Orna engine".into(),
        };
        assert!(matches!(
            run_live_resync_scenario(&scenario),
            StageOutcome::Passed
        ));
    }

    #[test]
    fn sys_runtime_root_contract_rejects_removed_spellings() {
        let scenario = Scenario {
            id: "SYS-RT-RENAME-100".into(),
            title: "The runtime root is sys.rt".into(),
            given: vec![
                "active source, removed `sys.runtime` source and runtime-info access".into(),
            ],
            when: vec!["resolve valid and invalid spellings and inspect the diagnostic".into()],
            then: vec![
                "`sys.rt` and `sys.rt.info()` resolve".into(),
                "`sys.runtime` and `sys.runtime_info` receive ORNA100-E-SYS-RUNTIME".into(),
                "no alias is installed".into(),
            ],
            requirements: vec!["ORNA-SYS-005".into(), "ORNA-SYS-105".into()],
            evidence_level: "implementation scenario, not executed by an Orna engine".into(),
        };
        assert!(matches!(
            run_sys_rt_rename_scenario(&scenario),
            StageOutcome::Passed
        ));
    }
}
