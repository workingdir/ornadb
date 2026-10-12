use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_sys_v1::{
    AdmissionError, AdmissionRequest, Argument, ArgumentMap, ExecutionBoundary,
    FunctionDescriptor, FunctionId, FunctionIdentity, InvocationContext, InvocationExecutor,
    InvocationMode, InvocationResult, RevisionId, RuntimeId, RuntimeSupervisor, SnapshotId,
    TransactionMode, TypeId, TypeWitness, TypedValue,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SYSTEM_REFERENCE: &str =
    include_str!("fixtures/reference/source/15-system.md");
const RESERVED_SYS: &str =
    include_str!("fixtures/reference/examples/invalid/reserved-sys.orna");
const LEGACY_SYS_RUNTIME: &str = include_str!(
    "fixtures/reference/examples/invalid/legacy-sys-runtime.orna"
);
const LEGACY_SYS_STORAGE_CALL: &str = include_str!(
    "fixtures/reference/examples/invalid/legacy-sys-storage-call.orna"
);
const MUTATE_SYS_COMMIT: &str =
    include_str!("fixtures/reference/examples/invalid/mutate-sys-commit.orna");
const TYPED_INVOKE_VALID: &str =
    include_str!("fixtures/typed-invoke-valid.orna");
const TYPED_INVOKE_MISMATCH: &str =
    include_str!("fixtures/typed-invoke-mismatch.orna");
const ERASED_INVOKE_MISSING_WITNESS: &str = include_str!(
    "fixtures/erased-invoke-missing-witness.orna"
);
const TYPED_START_VALID: &str =
    include_str!("fixtures/typed-start-valid.orna");
const TYPED_START_MISMATCH: &str =
    include_str!("fixtures/typed-start-mismatch.orna");
const ERASED_START_VALID: &str =
    include_str!("fixtures/erased-start-valid.orna");
const ERASED_INVOKE_VALID: &str =
    include_str!("fixtures/erased-invoke-valid.orna");

#[derive(Clone, Copy)]
enum Expected {
    Accepted,
    Rejected,
    Diagnostic(&'static str),
}

struct Case {
    id: &'static str,
    source_line: usize,
    boundary: &'static str,
    fixture: &'static str,
    source: &'static str,
    expected: Expected,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            id: "ORNA-SYS-003",
            source_line: 22,
            boundary: "typecheck rejects reserved root namespace declaration",
            fixture: "examples/invalid/reserved-sys.orna",
            source: RESERVED_SYS,
            expected: Expected::Rejected,
        },
        Case {
            id: "ORNA-SYS-005",
            source_line: 26,
            boundary: "resolve emits the required retired-member diagnostic",
            fixture: "examples/invalid/legacy-sys-runtime.orna",
            source: LEGACY_SYS_RUNTIME,
            expected: Expected::Diagnostic("ORNA100-E-SYS-RUNTIME"),
        },
        Case {
            id: "ORNA-SYS-120",
            source_line: 250,
            boundary: "typecheck rejects the storage grouping namespace as a callable selector",
            fixture: "examples/invalid/legacy-sys-storage-call.orna",
            source: LEGACY_SYS_STORAGE_CALL,
            expected: Expected::Rejected,
        },
        Case {
            id: "ORNA-SYS-046",
            source_line: 156,
            boundary: "the failed semantic result retains its stable diagnostic code",
            fixture: "examples/invalid/legacy-sys-runtime.orna",
            source: LEGACY_SYS_RUNTIME,
            expected: Expected::Diagnostic("ORNA100-E-SYS-RUNTIME"),
        },
        Case {
            id: "ORNA-SYS-077",
            source_line: 277,
            boundary: "typed reflective invocation is admitted through semantic analysis",
            fixture: "crates/orna-conformance-v1/tests/fixtures/typed-invoke-valid.orna",
            source: TYPED_INVOKE_VALID,
            expected: Expected::Accepted,
        },
        Case {
            id: "ORNA-SYS-131",
            source_line: 295,
            boundary: "ArgumentMap preserves exact types, canonical order, and rejects duplicate names",
            fixture: "crates/orna-conformance-v1/tests/fixtures/typed-invoke-valid.orna",
            source: TYPED_INVOKE_VALID,
            expected: Expected::Accepted,
        },
        Case {
            id: "ORNA-SYS-084",
            source_line: 315,
            boundary: "typecheck rejects direct mutation of a portable system relation",
            fixture: "examples/invalid/mutate-sys-commit.orna",
            source: MUTATE_SYS_COMMIT,
            expected: Expected::Rejected,
        },
        Case {
            id: "ORNA-SYS-130",
            source_line: 293,
            boundary: "typecheck rejects missing typed result witness instead of inferring an erased value fallback",
            fixture: "crates/orna-conformance-v1/tests/fixtures/erased-invoke-missing-witness.orna",
            source: ERASED_INVOKE_MISSING_WITNESS,
            expected: Expected::Rejected,
        },
        Case {
            id: "ORNA-SYS-132",
            source_line: 297,
            boundary: "typecheck rejects the mismatch and executor counters stay zero for typed invoke/start",
            fixture: "crates/orna-conformance-v1/tests/fixtures/typed-invoke-mismatch.orna",
            source: TYPED_INVOKE_MISMATCH,
            expected: Expected::Rejected,
        },
        Case {
            id: "ORNA-SYS-140",
            source_line: 371,
            boundary: "typecheck covers erased and typed invoke/start overload fixtures",
            fixture: "crates/orna-conformance-v1/tests/fixtures/typed-start-valid.orna",
            source: TYPED_START_VALID,
            expected: Expected::Accepted,
        },
    ]
}

fn analyze(source: &str) -> orna_semantic_v1::Analysis {
    analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    )
}

#[test]
fn ten_sys_gap_rows_have_fixture_backed_semantic_boundary_evidence() {
    let rows = cases();
    assert_eq!(rows.len(), 10);
    let mut ids = std::collections::BTreeSet::new();

    for row in &rows {
        assert!(ids.insert(row.id), "duplicate requirement row {}", row.id);
        let source_anchor = format!("**{}**", row.id);
        assert!(
            SYSTEM_REFERENCE
                .lines()
                .nth(row.source_line - 1)
                .is_some_and(|line| line.contains(&source_anchor)),
            "{} must be specified at source/15-system.md:{}",
            row.id,
            row.source_line
        );
        assert!(row.fixture.ends_with(".orna"));
        assert!(!row.source.trim().is_empty(), "{} fixture is empty", row.id);

        let analysis = analyze(row.source);
        match row.expected {
            Expected::Accepted => assert!(
                analysis.is_ok(),
                "{} {} failed: {:?}",
                row.id,
                row.boundary,
                analysis.diagnostics
            ),
            Expected::Rejected => assert!(
                !analysis.is_ok(),
                "{} {} unexpectedly passed",
                row.id,
                row.boundary
            ),
            Expected::Diagnostic(code) => assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code() == code),
                "{} expected {code}: {:?}",
                row.id,
                analysis.diagnostics
            ),
        }

        match row.id {
            "ORNA-SYS-131" => argument_map_contract_is_type_preserving_and_canonical(),
            "ORNA-SYS-132" => result_witness_mismatch_fails_before_executor_effects(),
            _ => {}
        }

        let observed = match row.expected {
            Expected::Accepted => "typecheck=Passed",
            Expected::Rejected => "expected-negative-test=Passed; typecheck=Failed-as-specified",
            Expected::Diagnostic(_) => "expected-diagnostic-test=Passed; resolve=Failed-as-specified",
        };
        println!(
            "{} source/15-system.md:{} Specified=Yes Exists=Yes Passed=Yes boundary={} fixture={} observed={}; full_requirement=Partial",
            row.id, row.source_line, row.boundary, row.fixture, observed
        );
    }

    // ORNA-SYS-140's overload obligation has four distinct forms. The
    // current row above records the typed-start pass; these additional real
    // fixtures ensure the remaining forms are at the same typecheck boundary.
    for (fixture, source, expected) in [
        (
            "crates/orna-conformance-v1/tests/fixtures/erased-invoke-valid.orna",
            ERASED_INVOKE_VALID,
            true,
        ),
        (
            "crates/orna-conformance-v1/tests/fixtures/typed-invoke-mismatch.orna",
            TYPED_INVOKE_MISMATCH,
            false,
        ),
        (
            "crates/orna-conformance-v1/tests/fixtures/erased-start-valid.orna",
            ERASED_START_VALID,
            true,
        ),
        (
            "crates/orna-conformance-v1/tests/fixtures/typed-start-mismatch.orna",
            TYPED_START_MISMATCH,
            false,
        ),
    ] {
        assert_eq!(analyze(source).is_ok(), expected, "{fixture}");
        println!(
            "ORNA-SYS-140 source/15-system.md:371 Specified=Yes Exists=Yes Passed=Yes boundary=typecheck fixture={fixture} observed={}; full_requirement=Partial",
            if expected { "Passed" } else { "expected-failure" }
        );
    }
}

fn argument_map_contract_is_type_preserving_and_canonical() {
    let arguments = ArgumentMap::new([
        Argument {
            name: "zeta".into(),
            value: TypedValue::public(TypeId::new("Int"), [1]),
        },
        Argument {
            name: "alpha".into(),
            value: TypedValue::public(TypeId::new("Str"), [1]),
        },
    ])
    .expect("distinct names form an argument map");
    let entries = arguments.entries().collect::<Vec<_>>();
    assert_eq!(entries.iter().map(|(name, _)| *name).collect::<Vec<_>>(), ["alpha", "zeta"]);
    assert_eq!(entries[0].1.static_type(), &TypeId::new("Str"));
    assert_eq!(entries[1].1.static_type(), &TypeId::new("Int"));
    assert!(matches!(
        ArgumentMap::new([
            Argument {
                name: "same".into(),
                value: TypedValue::public(TypeId::new("Int"), [1]),
            },
            Argument {
                name: "same".into(),
                value: TypedValue::public(TypeId::new("Str"), [1]),
            },
        ]),
        Err(AdmissionError::ArgumentType {
            detail: orna_sys_v1::ArgumentTypeDetail::Duplicate,
            ..
        })
    ));
}

struct CountingExecutor(Arc<AtomicUsize>);

impl InvocationExecutor for CountingExecutor {
    fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
        self.0.fetch_add(1, Ordering::SeqCst);
        InvocationResult::Success(TypedValue::public(TypeId::new("Contact"), []))
    }
}

fn incompatible_typed_request(mode: InvocationMode) -> AdmissionRequest<()> {
    AdmissionRequest {
        function: FunctionDescriptor {
            identity: FunctionIdentity {
                function: FunctionId::new("contact"),
                revision: RevisionId::from_bytes([0x61; 32]),
                snapshot: SnapshotId::new("s1"),
            },
            parameters: Vec::new(),
            result_type: TypeId::new("Contact"),
            visible: true,
            callable: true,
            generics_resolved: true,
        },
        arguments: ArgumentMap::default(),
        explicit_snapshot: None,
        mode,
        transaction: match mode {
            InvocationMode::Invoke => TransactionMode::Inherit,
            InvocationMode::Start => TransactionMode::Separate,
        },
        witness: TypeWitness::new(TypeId::new("Invoice")),
        idempotency_key: None,
        context: InvocationContext::default(),
    }
}

fn result_witness_mismatch_fails_before_executor_effects() {
    let supervisor = RuntimeSupervisor::new(RuntimeId::new("runtime"));
    let invoke_calls = Arc::new(AtomicUsize::new(0));
    let mut invoke = CountingExecutor(Arc::clone(&invoke_calls));
    assert_eq!(
        supervisor.run(incompatible_typed_request(InvocationMode::Invoke), &mut invoke),
        Err(AdmissionError::ReturnType)
    );
    assert_eq!(invoke_calls.load(Ordering::SeqCst), 0);

    let start_calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        supervisor.start(
            incompatible_typed_request(InvocationMode::Start),
            CountingExecutor(Arc::clone(&start_calls)),
        ),
        Err(AdmissionError::ReturnType)
    );
    assert_eq!(start_calls.load(Ordering::SeqCst), 0);
}
