use orna_conformance_v1::{
    BoundedEvaluator, ConformanceAdapter, Corpus, EvidenceClass, EvidenceStatus, Harness,
    ProjectEnvironment, ProjectExpectations, ProjectUnit, RuntimeAdapter, RuntimeEvaluator,
    Scenario, SemanticAdapter, SourceUnit, StageOutcome, TransactionalEvaluator,
};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::{Diagnostic, OvbRaw, Value};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RequestIdentity, RequestState, RunObservationStatus, RuntimeIdentity, RuntimeState,
    StreamObservationStatus, StreamRunControl, TableMutation,
};
use orna_semantic_v1::{Catalogue, ModuleInput, Namespace, Type, analyze_with_catalogue};
use orna_stream_v1::{DiagnosticClass, DiagnosticCode};
use sha2::Digest;
use std::{
    collections::BTreeMap,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use tempfile::TempDir;

struct CancelAtCheck {
    checks: AtomicUsize,
    cancel_on_check: usize,
}

impl CancelAtCheck {
    fn new(cancel_on_check: usize) -> Self {
        Self {
            checks: AtomicUsize::new(0),
            cancel_on_check,
        }
    }
}

impl StreamRunControl for CancelAtCheck {
    fn cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_on_check
    }

    fn acquire_admission(&self) -> bool {
        true
    }

    fn release_admission(&self) {}
}

fn cancellation_project() -> ProjectUnit {
    ProjectUnit {
        fixture_id: "stream-cancellation".into(),
        project_id: "stream-cancellation".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "stream-cancellation".into(),
            source_id: "stream-cancellation/sensors.orna".into(),
            parse_as: "module_unit".into(),
            source: r#"
                pub table Reading(id: Int) { value: Int, }
                pub fn input() = Stream.from_list([1, 2], source_identity: "example:cancellation");
                pub fn ingest() { input() | for_each(value => {
                    Reading.insert({ id: value, value: value });
                }); }
            "#
            .into(),
        }],
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
            negative_cases: Vec::new(),
        },
    }
}

fn initialized_repository(temp: &TempDir) -> Repository {
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.email", "test@example.invalid"][..],
        &["config", "user.name", "conformance test"][..],
    ] {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(temp.path())
                .status()
                .expect("git command")
                .success()
        );
    }
    Repository::discover(temp.path()).expect("repository")
}

#[test]
fn transactional_fixture_preserves_order_reference_types_without_runtime_claims() {
    let report = Harness::new(Corpus::load_default().expect("reference corpus loads"))
        .run(&mut SemanticAdapter::default());
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/transactional-scope.orna")
        .expect("transactional fixture exists");
    assert!(fixture.passed, "{:?}", fixture.stages);
    assert!(
        fixture
            .stages
            .iter()
            .any(|stage| stage.status == EvidenceStatus::Skipped)
    );
}

#[test]
fn semantic_mail_fixture_distinguishes_stored_email_from_provider_messages() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/unbounded-stream.orna")
        .expect("mail fixture exists");
    assert!(fixture.passed, "{:?}", fixture.stages);
    // This verifies the frozen static contract, not connector execution.
    assert!(
        fixture
            .stages
            .iter()
            .any(|stage| stage.status == EvidenceStatus::Skipped)
    );
}

#[test]
fn semantic_adapter_executes_the_v1_analyzer_with_logical_fixture_names() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "valid/minimal-root.orna")
        .expect("minimal fixture");
    // The analyzer really runs against the authoritative core catalogue.
    assert_eq!(fixture.stages[1].status, EvidenceStatus::Passed);
    assert_eq!(fixture.stages[2].status, EvidenceStatus::Passed);
    assert!(!report.semantic_evidence.is_empty());
    assert!(
        report
            .scenarios
            .iter()
            .all(|scenario| scenario.status == EvidenceStatus::Skipped)
    );
    assert!(report.scenarios.iter().all(|scenario| {
        scenario.detail.contains("runtime-v1") && !scenario.detail.contains("/home/")
    }));
}

#[test]
fn admitted_source_relation_pages_traverse_decimal_keys() {
    fn source(body: &str) -> SourceUnit {
        SourceUnit {
            fixture_id: "decimal-cursor-source".into(),
            source_id: "decimal-cursor-source.orna".into(),
            parse_as: "module_unit".into(),
            source: format!(
                r#"
                    pub table Reading(value: Decimal) {{ label: Str, }}
                    fn main() {{ {body} }}
                "#
            ),
        }
    }

    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let inserted = evaluator.execute_source(&source(
        r#"
            Reading.insert({ value: 2.0, label: "two" });
            Reading.insert({ value: 0.1, label: "one-tenth" });
            Reading.insert({ value: 1.0, label: "one" });
            Reading.insert({ value: 0.01, label: "one-hundredth" });
        "#
    ));
    assert!(
        matches!(inserted, StageOutcome::Passed),
        "source insertion failed: {inserted:?}"
    );

    // Relation evaluation is admitted source execution. Decimal-key pages
    // must resume at the canonical numeric successor without replaying or
    // skipping a row.
    let paged = evaluator.execute_source(&source(
        r#"
            assert (Reading | count) == 4;
            assert (Reading | take(1) | count) == 1;
            assert (Reading | take(3) | count) == 3;
            assert (Reading | drop(3) | count) == 1;
            assert (Reading | drop(4) | count) == 0;
        "#
    ));
    assert!(
        matches!(paged, StageOutcome::Passed),
        "Decimal page assertions failed: {paged:?}"
    );
}

#[test]
fn semantic_project_resolution_uses_project_relative_module_names() {
    let mut adapter = SemanticAdapter::default();
    let project = ProjectUnit {
        fixture_id: "project".into(),
        project_id: "examples/reference".into(),
        environment_id: None,
        modules: vec![
            SourceUnit {
                fixture_id: "project".into(),
                source_id: "examples/reference/main.orna".into(),
                parse_as: "module_unit".into(),
                source: "use library;".into(),
            },
            SourceUnit {
                fixture_id: "project".into(),
                source_id: "examples/reference/library.orna".into(),
                parse_as: "module_unit".into(),
                source: "pub fn pick(value: Int): Int = value;".into(),
            },
        ],
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
            negative_cases: Vec::new(),
        },
    };

    assert!(matches!(
        adapter.resolve_project(&project),
        StageOutcome::Passed
    ));
}

#[test]
fn semantic_adapter_typechecks_imported_generic_sys_meta_with_declared_metadata() {
    // ORNA-GENERIC-001 requires explicit public generic parameters.  This
    // project follows the source-level route: the imported generic declaration
    // invokes the portable api/sys.json `sys.meta<T>` operation.
    let project = ProjectUnit {
        fixture_id: "imported-generic-sys-meta".into(),
        project_id: "logical/project".into(),
        environment_id: None,
        modules: vec![
            SourceUnit {
                fixture_id: "imported-generic-sys-meta".into(),
                source_id: "logical/project/library.orna".into(),
                parse_as: "module_unit".into(),
                source: "pub fn lookup<T>(value: T) = sys.meta<T>(value);".into(),
            },
            SourceUnit {
                fixture_id: "imported-generic-sys-meta".into(),
                source_id: "logical/project/main.orna".into(),
                parse_as: "module_unit".into(),
                source: "use library; pub fn read(value: Int) = library.lookup<Int>(value);"
                    .into(),
            },
        ],
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
            negative_cases: Vec::new(),
        },
    };

    let mut adapter = SemanticAdapter::default();
    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // The adapter intentionally exposes only phase outcomes.  Inspect the
    // same source graph through the native semantic result to prove that the
    // accepted call retained sys.meta's declared read/failure contract.
    let analysis = analyze_with_catalogue(
        &[
            ModuleInput::new(
                "library.orna",
                "pub fn lookup<T>(value: T) = sys.meta<T>(value);",
            ),
            ModuleInput::new(
                "main.orna",
                "use library; pub fn read(value: Int) = library.lookup<Int>(value);",
            ),
        ],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let read = analysis
        .modules
        .get(&Namespace(Vec::new()))
        .and_then(|module| module.exports.get("read"))
        .expect("imported generic read export");
    assert!(matches!(
        &read.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.ValueMetadata".into(),
                arguments: vec![Type::Int],
            }
    ));
    assert_eq!(
        read.effects.effects,
        std::collections::BTreeSet::from(["database read".into()])
    );
    assert!(read.effects.may_fail);
}

#[test]
fn semantic_adapter_rejects_invalid_imported_generic_sys_meta_argument() {
    // The imported generic declaration is valid; the consumer's explicit
    // `<Str>` is invalid for its `Int` argument and must fail typechecking.
    let project = ProjectUnit {
        fixture_id: "invalid-imported-generic-sys-meta".into(),
        project_id: "logical/project".into(),
        environment_id: None,
        modules: vec![
            SourceUnit {
                fixture_id: "invalid-imported-generic-sys-meta".into(),
                source_id: "logical/project/library.orna".into(),
                parse_as: "module_unit".into(),
                source: "pub fn lookup<T>(value: T) = sys.meta<T>(value);".into(),
            },
            SourceUnit {
                fixture_id: "invalid-imported-generic-sys-meta".into(),
                source_id: "logical/project/main.orna".into(),
                parse_as: "module_unit".into(),
                source: "use library; pub fn invalid(value: Int) = library.lookup<Str>(value);"
                    .into(),
            },
        ],
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
            negative_cases: Vec::new(),
        },
    };

    let mut adapter = SemanticAdapter::default();
    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("invalid explicit imported generic argument must fail typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S021-TYPE");
}
fn runtime_info_project(source: &str) -> ProjectUnit {
    ProjectUnit {
        fixture_id: "runtime-info-project".into(),
        project_id: "logical/runtime-info".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "runtime-info-module".into(),
            source_id: "logical/runtime-info/main.orna".into(),
            parse_as: "module_unit".into(),
            source: source.into(),
        }],
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
            negative_cases: Vec::new(),
        },
    }
}
fn snapshot_project(source: &str) -> ProjectUnit {
    ProjectUnit {
        fixture_id: "snapshot-project".into(),
        project_id: "logical/snapshot".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "snapshot-module".into(),
            source_id: "logical/snapshot/main.orna".into(),
            parse_as: "module_unit".into(),
            source: source.into(),
        }],
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
            negative_cases: Vec::new(),
        },
    }
}
fn database_project(source: &str) -> ProjectUnit {
    ProjectUnit {
        fixture_id: "database-project".into(),
        project_id: "logical/database".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "database-module".into(),
            source_id: "logical/database/main.orna".into(),
            parse_as: "module_unit".into(),
            source: source.into(),
        }],
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
            negative_cases: Vec::new(),
        },
    }
}
fn resolve_project(source: &str) -> ProjectUnit {
    ProjectUnit {
        fixture_id: "resolve-project".into(),
        project_id: "logical/resolve".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "resolve-module".into(),
            source_id: "logical/resolve/main.orna".into(),
            parse_as: "module_unit".into(),
            source: source.into(),
        }],
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
            negative_cases: Vec::new(),
        },
    }
}

#[test]
fn semantic_project_adapter_admits_sys_resolve_string_as_object_ref_read() {
    let source = r#"pub fn lookup() = sys.resolve("main.main");"#;
    let project = resolve_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // Project phase outcomes prove the production adapter path. The native
    // semantic graph proves the admitted return type and read effect retained
    // for sys.resolve, without claiming object lookup or history behaviour.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let lookup = &analysis.modules.values().next().unwrap().exports["lookup"];
    assert!(matches!(
        &lookup.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("sys.ObjectRef".into())
    ));
    assert_eq!(
        lookup.effects.effects,
        std::collections::BTreeSet::from(["database read".into()])
    );
    assert!(lookup.effects.may_fail);
}

#[test]
fn semantic_project_adapter_rejects_non_string_sys_resolve_argument_at_typecheck() {
    let source = "pub fn invalid() = sys.resolve(1);";
    let project = resolve_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("non-string sys.resolve argument must fail at typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S021-TYPE");
}


#[test]
fn semantic_project_adapter_admits_sys_snapshot_string_as_pinned_read() {
    let source = r#"pub fn before_change() = sys.snapshot("HEAD~3");"#;
    let project = snapshot_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // The project stages above exercise the production adapter boundary.  The
    // native semantic graph proves the typed result and read metadata retained
    // for the admitted source without loading or executing historical state.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let before_change = &analysis.modules.values().next().unwrap().exports["before_change"];
    assert!(matches!(
        &before_change.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("sys.SnapshotRef".into())
    ));
    assert_eq!(
        before_change.effects.effects,
        std::collections::BTreeSet::from(["database read".into()])
    );
    assert!(before_change.effects.may_fail);
}
#[test]
fn semantic_project_adapter_admits_sys_current_snapshot_as_snapshot_ref_observation() {
    let source = "pub fn current_snapshot() = sys.current.snapshot;";
    let project = snapshot_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // Project stages prove source admission through the production adapter.
    // Inspect the native graph separately for the typed SnapshotRef
    // observation. The current analyzer records no synthetic effect for this
    // immutable singleton field, so this witness preserves the native empty
    // effect summary rather than claiming a read effect it does not expose.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let current_snapshot =
        &analysis.modules.values().next().unwrap().exports["current_snapshot"];
    assert!(matches!(
        &current_snapshot.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("sys.SnapshotRef".into())
    ));
    assert!(current_snapshot.effects.effects.is_empty());
    assert!(!current_snapshot.effects.may_fail);
}

#[test]
fn semantic_project_adapter_rejects_unsupported_sys_current_member_at_typecheck() {
    let source = "pub fn legacy() = sys.current.legacy_member;";
    let project = snapshot_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("unsupported sys.current member must fail at typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S022-UNSUPPORTED");
}


#[test]
fn semantic_project_adapter_rejects_non_string_sys_snapshot_argument_at_typecheck() {
    let source = "pub fn invalid() = sys.snapshot(3);";
    let project = snapshot_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("non-string sys.snapshot argument must fail typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S021-TYPE");
}

#[test]
fn semantic_project_adapter_admits_sys_database_cwd_as_snapshot_ref_read() {
    let source = "pub fn cwd() = sys.snapshot(sys.database.cwd);";
    let project = database_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // The project stages exercise the production adapter route.  Inspect the
    // native graph separately to prove the admitted source retains the
    // descriptor's typed SnapshotRef result and database-read effect.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let cwd = &analysis.modules.values().next().unwrap().exports["cwd"];
    assert!(matches!(
        &cwd.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("sys.SnapshotRef".into())
    ));
    assert!(
        cwd.effects.effects.contains("database read"),
        "{:?}",
        cwd.effects
    );
}
#[test]
fn semantic_project_adapter_admits_sys_database_writable_as_bool_read() {
    let source = "pub fn writable() { let _snapshot = sys.snapshot(sys.database.cwd); sys.database.writable }";
    let project = database_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // The project stages exercise the production adapter route. Inspect the
    // same admitted native graph to prove the descriptor's Bool result and
    // database-read effect. The explicit snapshot read models attachment
    // observation; this witness makes no write or runtime mutation claim.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let writable = &analysis.modules.values().next().unwrap().exports["writable"];
    assert!(matches!(
        &writable.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Bool
    ));
    assert!(
        writable.effects.effects.contains("database read"),
        "{:?}",
        writable.effects
    );
}

#[test]
fn semantic_project_adapter_rejects_unsupported_sys_database_member_at_typecheck() {
    let source = "pub fn invalid() = sys.database.legacy_member;";
    let project = database_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("unsupported sys.database member must fail at typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S022-UNSUPPORTED");
}

#[test]
fn semantic_project_adapter_admits_sys_runtime_info_with_read_effect() {
    let source = "pub fn runtime_info() = sys.rt.info();";
    let project = runtime_info_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // The adapter proves source/project phase admission. Inspect the native
    // semantic graph separately so this witness also proves the declared
    // result type and effect, without relying on harness serialization.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let runtime_info = &analysis.modules.values().next().unwrap().exports["runtime_info"];
    assert!(matches!(
        &runtime_info.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("sys.RuntimeInfo".into())
    ));
    assert_eq!(
        runtime_info.effects.effects,
        std::collections::BTreeSet::from(["database read".into()])
    );
    assert!(runtime_info.effects.may_fail);
}

#[test]
fn semantic_project_adapter_rejects_removed_sys_runtime_with_native_diagnostic() {
    let source = "pub fn runtime_info() = sys.runtime;";
    let project = runtime_info_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.resolve_project(&project) else {
        panic!("removed sys.runtime must fail during source resolution");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA100-E-SYS-RUNTIME");
    assert_eq!(
        adapter.diagnostic_message(&diagnostic),
        "`sys.runtime` was renamed to `sys.rt`"
    );
}

fn typed_invoke_project(source: &str) -> ProjectUnit {
    ProjectUnit {
        fixture_id: "typed-invoke-project".into(),
        project_id: "logical/typed-invoke".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "typed-invoke-module".into(),
            source_id: "logical/typed-invoke/main.orna".into(),
            parse_as: "module_unit".into(),
            source: source.into(),
        }],
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
            negative_cases: Vec::new(),
        },
    }
}

#[test]
fn semantic_project_adapter_admits_typed_sys_invoke_with_explicit_witness() {
    let project = typed_invoke_project(
        "pub fn invoke_int(function: sys.FunctionRef, arguments: sys.ArgumentMap) = \
         sys.invoke<Int>(function, arguments, as: Int);",
    );
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);
}

#[test]
fn semantic_project_adapter_rejects_typed_sys_invoke_mismatched_witness() {
    let project = typed_invoke_project(
        "pub fn invoke_wrong(function: sys.FunctionRef, arguments: sys.ArgumentMap) = \
         sys.invoke<Str>(function, arguments, as: Int);",
    );
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("mismatched typed sys.invoke witness must fail at typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S021-TYPE");
    assert_eq!(
        adapter.diagnostic_message(&diagnostic),
        "sys.invoke explicit type argument must match the as: witness"
    );
}
#[test]
fn semantic_project_adapter_admits_erased_sys_invoke_with_value_result_and_invoke_effect() {
    let source =
        "pub fn erased(function: sys.FunctionRef, arguments: sys.ArgumentMap) = \
         sys.invoke(function, arguments);";
    let project = typed_invoke_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // The phase adapter proves source admission; inspect the native graph to
    // keep the erased result type and invoke effect distinct from report
    // serialization or runtime invocation evidence.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let erased = &analysis.modules.values().next().unwrap().exports["erased"];
    assert!(matches!(
        &erased.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("sys.Value".into())
    ));
    assert_eq!(
        erased.effects.effects,
        std::collections::BTreeSet::from(["invoke".into()])
    );
    assert!(erased.effects.may_fail);
}

#[test]
fn semantic_project_adapter_rejects_typed_sys_invoke_without_explicit_witness() {
    let project = typed_invoke_project(
        "pub fn missing(function: sys.FunctionRef, arguments: sys.ArgumentMap) = \
         sys.invoke<Int>(function, arguments);",
    );
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("typed sys.invoke without an explicit witness must fail at typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S021-TYPE");
    assert_eq!(
        adapter.diagnostic_message(&diagnostic),
        "typed sys.invoke requires an explicit as: T witness"
    );
}

#[test]
fn semantic_project_adapter_admits_typed_sys_start_with_explicit_witness() {
    let source = "pub fn start_int(function: sys.FunctionRef, arguments: sys.ArgumentMap) = \
         sys.start<Int>(function, arguments, as: Int);";
    let project = typed_invoke_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // Keep the source-stage adapter assertion paired with the native graph
    // result so this witness proves the real return type and effect contract,
    // rather than only reporting a phase outcome.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let start_int = &analysis.modules.values().next().unwrap().exports["start_int"];
    assert!(matches!(
        &start_int.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.InvocationHandle".into(),
                arguments: vec![Type::Int],
            }
    ));
    assert_eq!(
        start_int.effects.effects,
        std::collections::BTreeSet::from(["invoke".into()])
    );
    assert!(start_int.effects.may_fail);
}
#[test]
fn semantic_project_adapter_admits_erased_sys_start_with_value_handle_and_invoke_effect() {
    let source =
        "pub fn start_erased(function: sys.FunctionRef, arguments: sys.ArgumentMap) = \
         sys.start(function, arguments);";
    let project = typed_invoke_project(source);
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);

    // The adapter proves source admission; inspect the native graph to retain
    // the erased handle type and invoke effect without claiming runtime
    // invocation identity or generation.
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let start_erased = &analysis.modules.values().next().unwrap().exports["start_erased"];
    assert!(matches!(
        &start_erased.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.InvocationHandle".into(),
                arguments: vec![Type::Named("sys.Value".into())],
            }
    ));
    assert_eq!(
        start_erased.effects.effects,
        std::collections::BTreeSet::from(["invoke".into()])
    );
    assert!(start_erased.effects.may_fail);
}

#[test]
fn semantic_project_adapter_rejects_typed_sys_start_without_explicit_witness() {
    let project = typed_invoke_project(
        "pub fn start_missing(function: sys.FunctionRef, arguments: sys.ArgumentMap) = \
         sys.start<Int>(function, arguments);",
    );
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("typed sys.start without an explicit witness must fail at typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S021-TYPE");
    assert_eq!(
        adapter.diagnostic_message(&diagnostic),
        "typed sys.start requires an explicit as: T witness"
    );
}


#[test]
fn semantic_project_adapter_rejects_typed_sys_start_mismatched_witness() {
    let project = typed_invoke_project(
        "pub fn start_wrong(function: sys.FunctionRef, arguments: sys.ArgumentMap) = \
         sys.start<Str>(function, arguments, as: Int);",
    );
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("mismatched typed sys.start witness must fail at typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S021-TYPE");
    assert_eq!(
        adapter.diagnostic_message(&diagnostic),
        "sys.start explicit type argument must match the as: witness"
    );
}


#[test]
fn semantic_project_adapter_admits_inferred_and_explicit_typed_sys_await() {
    let project = typed_invoke_project(
        r#"
            pub fn inferred_await(job: sys.InvocationHandle<Int>) =
                sys.await(job, timeout: 1.s);
            pub fn explicit_await(job: sys.InvocationHandle<Int>) =
                sys.await<Int>(invocation: job, timeout: null);
        "#,
    );
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);
}

#[test]
fn semantic_project_adapter_admits_inferred_and_explicit_typed_sys_cancel() {
    let project = typed_invoke_project(
        r#"
            pub fn inferred_cancel(job: sys.InvocationHandle<Int>) =
                sys.cancel(job);
            pub fn explicit_cancel(job: sys.InvocationHandle<Int>) =
                sys.cancel<Int>(job, reason: "stop");
        "#,
    );
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.typecheck_project(&project), StageOutcome::Passed);
}

#[test]
fn semantic_project_adapter_rejects_mismatched_explicit_sys_await_type() {
    let project = typed_invoke_project(
        "pub fn mismatched(job: sys.InvocationHandle<Int>) = sys.await<Str>(job);",
    );
    let mut adapter = SemanticAdapter::default();

    assert_eq!(adapter.parse_project(&project), StageOutcome::Passed);
    assert_eq!(adapter.resolve_project(&project), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = adapter.typecheck_project(&project) else {
        panic!("mismatched explicit sys.await type must fail at typecheck");
    };
    assert_eq!(adapter.diagnostic_code(&diagnostic), "ORNA-S021-TYPE");
}


#[tokio::test]
async fn project_stream_ignores_unrelated_false_module_assertion() {
    let temp = TempDir::new().expect("temporary repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.email", "test@example.invalid"][..],
        &["config", "user.name", "conformance test"][..],
    ] {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(temp.path())
                .status()
                .expect("git command")
                .success()
        );
    }
    let repository = Repository::discover(temp.path()).expect("repository");
    let project = ProjectUnit {
        fixture_id: "stream-assertion-scope".into(),
        project_id: "stream-assertion-scope".into(),
        environment_id: None,
        modules: vec![
            SourceUnit {
                fixture_id: "stream-assertion-scope".into(),
                source_id: "stream-assertion-scope/library.orna".into(),
                parse_as: "module_unit".into(),
                source: r#"
                    pub table Book(id: Str) { title: Str, }
                    pub table Loan(book_id: Str) { borrower: Str, }
                    assert every(Loan, loan => exists(Book, book => book.id == loan.book_id));
                "#
                .into(),
            },
            SourceUnit {
                fixture_id: "stream-assertion-scope".into(),
                source_id: "stream-assertion-scope/sensors.orna".into(),
                parse_as: "module_unit".into(),
                source: r#"
                    pub type Sample { pub sensor: Str, pub sequence: Int, pub value: Decimal, }
                    pub table Reading(sensor: Str, sequence: Int) { value: Decimal, }
                    pub fn input() = Stream.from_list([
                        Sample { sensor: "greenhouse", sequence: 0, value: 18.25 },
                        Sample { sensor: "greenhouse", sequence: 1, value: 18.50 },
                    ], source_identity: "example:sensors:v1");
                    pub fn ingest() { input() | for_each(sample => {
                        Reading.insert({ sensor: sample.sensor, sequence: sample.sequence, value: sample.value });
                    }); }
                "#
                .into(),
            },
        ],
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
            negative_cases: Vec::new(),
        },
    };
    let identity = RuntimeIdentity {
        database_id: [81; 16],
        repository_id: [82; 16],
    };
    let evaluator =
        orna_conformance_v1::DurableTransactionalEvaluator::new("main", Limits::default());
    let state = RuntimeState::open(&repository, identity, [84; 32])
        .await
        .expect("runtime state");
    let lease = state.acquire_lease([83; 16]).await.expect("writer lease");
    let snapshot = state
        .begin_table_activation(&["Loan"])
        .await
        .expect("initial Loan snapshot");
    let key = Value::int(1.into()).encode().expect("Loan key");
    let row = Value::new(OvbRaw::Map(vec![
        (
            OvbRaw::Text("book_id".into()),
            OvbRaw::Text("missing".into()),
        ),
        (
            OvbRaw::Text("borrower".into()),
            OvbRaw::Text("reader".into()),
        ),
    ]))
    .expect("canonical Loan row")
    .encode()
    .expect("encoded Loan row");
    let mutation = TableMutation::new([86; 16], "Loan", key, Some(row)).expect("Loan mutation");
    state
        .commit_table_activation(lease, snapshot.context(), &[mutation], [87; 32], &NoFault)
        .await
        .expect("seed unrelated Loan row");

    let outcome = evaluator
        .execute_project_stream(
            &repository,
            identity,
            [83; 16],
            [84; 32],
            &project,
            "sensors.ingest",
        )
        .await;
    assert!(matches!(outcome, Ok(StageOutcome::Passed)), "{outcome:?}");

    let state = RuntimeState::open(&repository, identity, [84; 32])
        .await
        .expect("runtime state");
    assert_eq!(state.committed_table_rows("Loan").await.unwrap().len(), 1);
    assert_eq!(
        state.committed_table_rows("Reading").await.unwrap().len(),
        2
    );

    let runs = state.run_observations().await.expect("completed runs");
    assert_eq!(runs.len(), 1, "one durable Run must retain the ingest");
    let run = &runs[0];
    assert_eq!(run.status, RunObservationStatus::Completed);
    assert_eq!(run.checkpoint_count, 2);
    assert!(run.ended_ms.is_some());

    let streams = state
        .stream_observations()
        .await
        .expect("completed streams");
    assert_eq!(
        streams.len(),
        1,
        "one durable Stream must retain the ingest"
    );
    let stream = &streams[0];
    assert_eq!(stream.run, run.id);
    assert_eq!(stream.status, StreamObservationStatus::Completed);
    assert_eq!(
        (
            stream.items_seen,
            stream.items_committed,
            stream.items_failed
        ),
        (2, 2, 0)
    );
    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("latest checkpoint")
        .expect("completed stream checkpoint");
    // The unrelated seeded row is the first durable mutation; the two stream
    // deliveries advance the same runtime generation and mutation sequence.
    assert_eq!(checkpoint.generation, 3);
    assert_eq!(checkpoint.mutation_sequence, 3);
    assert_eq!(
        stream.checkpoint_reference.as_row_ref().snapshot,
        run.snapshot.snapshot().clone()
    );
    assert!(
        stream.reference(run).is_ok(),
        "Stream must link to its retained Run"
    );
}

#[tokio::test]
async fn project_stream_rolls_back_when_affected_module_assertion_fails() {
    let temp = TempDir::new().expect("temporary repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.email", "test@example.invalid"][..],
        &["config", "user.name", "conformance test"][..],
    ] {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(temp.path())
                .status()
                .expect("git command")
                .success()
        );
    }
    let repository = Repository::discover(temp.path()).expect("repository");
    let project = ProjectUnit {
        fixture_id: "stream-assertion-failure".into(),
        project_id: "stream-assertion-failure".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "stream-assertion-failure".into(),
            source_id: "stream-assertion-failure/sensors.orna".into(),
            parse_as: "module_unit".into(),
            source: r#"
                pub type Sample { pub sensor: Str, pub sequence: Int, pub value: Decimal, }
                pub table Reading(sensor: Str, sequence: Int) { value: Decimal, }
                pub table Marker(id: Int) { note: Str, }
                assert every(Reading, reading =>
                    exists(Marker, marker => marker.id == reading.sequence)
                );
                pub fn input() = Stream.from_list([
                    Sample { sensor: "greenhouse", sequence: 0, value: 18.25 },
                ], source_identity: "example:sensors:assertion-failure");
                pub fn ingest() { input() | for_each(sample => {
                    Reading.insert({ sensor: sample.sensor, sequence: sample.sequence, value: sample.value });
                }); }
            "#
            .into(),
        }],
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
            negative_cases: Vec::new(),
        },
    };
    let identity = RuntimeIdentity {
        database_id: [91; 16],
        repository_id: [92; 16],
    };
    let evaluator =
        orna_conformance_v1::DurableTransactionalEvaluator::new("main", Limits::default());

    let outcome = evaluator
        .execute_project_stream(
            &repository,
            identity,
            [93; 16],
            [94; 32],
            &project,
            "sensors.ingest",
        )
        .await;
    assert!(
        matches!(outcome, Ok(StageOutcome::Failed(ref diagnostic)) if diagnostic.code() == "ORNA-LIST-STREAM-DELIVERY"),
        "affected assertion failure must be retained: {outcome:?}"
    );

    let state = RuntimeState::open(&repository, identity, [94; 32])
        .await
        .expect("runtime state");
    assert!(
        state
            .committed_table_rows("Reading")
            .await
            .expect("Reading rows")
            .is_empty(),
        "failed affected assertion must roll back the delivered row"
    );
    assert!(
        state
            .latest_checkpoint()
            .await
            .expect("latest checkpoint")
            .is_none(),
        "failed affected assertion must not advance the durable checkpoint"
    );

    let runs = state.run_observations().await.expect("failed runs");
    assert_eq!(
        runs.len(),
        1,
        "one durable Run must retain the failed ingest"
    );
    let run = &runs[0];
    assert_eq!(run.status, RunObservationStatus::Failed);
    assert!(run.ended_ms.is_some());

    let streams = state.stream_observations().await.expect("failed streams");
    assert_eq!(
        streams.len(),
        1,
        "one durable Stream must retain the failure"
    );
    let stream = &streams[0];
    assert_eq!(stream.run, run.id);
    assert_eq!(stream.status, StreamObservationStatus::Failed);
    assert_eq!(stream.items_seen, 1);
    assert_eq!(stream.items_committed, 0);
    assert_eq!(stream.items_failed, 1);
    assert!(stream.last_failure.is_some());
    assert_eq!(
        stream.diagnostic,
        Some(orna_stream_v1::SafeDiagnostic {
            code: DiagnosticCode::TableAssertionFalse,
            class: DiagnosticClass::Permanent,
        })
    );
    assert!(
        stream.reference(run).is_ok(),
        "failed Stream must retain its Run/checkpoint linkage"
    );
}

#[tokio::test]
async fn project_stream_admission_rejects_multiple_applicable_module_assertions() {
    let temp = TempDir::new().expect("temporary repository");
    for arguments in [
        &["init", "--quiet"][..],
        &["config", "user.email", "test@example.invalid"][..],
        &["config", "user.name", "conformance test"][..],
    ] {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(temp.path())
                .status()
                .expect("git command")
                .success()
        );
    }
    let repository = Repository::discover(temp.path()).expect("repository");
    let project = ProjectUnit {
        fixture_id: "stream-assertion-ordering".into(),
        project_id: "stream-assertion-ordering".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "stream-assertion-ordering".into(),
            source_id: "stream-assertion-ordering/sensors.orna".into(),
            parse_as: "module_unit".into(),
            source: r#"
                pub table Reading(id: Int) { value: Int, }
                pub table Marker(id: Int) { note: Str, }
                assert every(Reading, reading =>
                    exists(Marker, marker => marker.id == reading.id)
                );
                assert every(Reading, reading =>
                    exists(Marker, marker => marker.id != reading.id)
                );
                pub fn input() = Stream.from_list([1], source_identity: "example:ordering");
                pub fn ingest() { input() | for_each(value => {
                    Reading.insert({ id: value, value: value });
                }); }
            "#
            .into(),
        }],
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
            negative_cases: Vec::new(),
        },
    };

    let outcome = orna_conformance_v1::DurableTransactionalEvaluator::default()
        .execute_project_stream(
            &repository,
            RuntimeIdentity {
                database_id: [95; 16],
                repository_id: [96; 16],
            },
            [97; 16],
            [98; 32],
            &project,
            "sensors.ingest",
        )
        .await
        .expect("stream admission result");
    assert!(
        matches!(outcome, StageOutcome::Skipped { ref reason } if reason.contains("one applicable module assertion")),
        "multiple applicable module assertions must fail admission closed: {outcome:?}"
    );
}

#[tokio::test]
async fn project_stream_cancellation_before_first_poll_is_retained_without_failure() {
    let temp = TempDir::new().expect("temporary repository");
    let repository = initialized_repository(&temp);
    let identity = RuntimeIdentity {
        database_id: [101; 16],
        repository_id: [102; 16],
    };
    let request = RequestIdentity {
        session_id: [103; 16],
        request_id: [104; 16],
    };
    let fingerprint = [105; 32];
    let control = CancelAtCheck::new(1);
    let outcome = orna_conformance_v1::DurableTransactionalEvaluator::default()
        .execute_project_stream_request_with_control(
            orna_conformance_v1::RuntimeTarget {
                repository: &repository,
                identity,
                owner_id: [106; 16],
                initial_digest: [107; 32],
            },
            request,
            fingerprint,
            &cancellation_project(),
            "sensors.ingest",
            &control,
        )
        .await
        .expect("cancellation result");
    assert!(
        matches!(outcome, StageOutcome::Cancelled(ref diagnostic) if diagnostic.code() == "ORNA-LIST-STREAM-CANCELLED"),
        "cancellation remains a narrow diagnostic outcome: {outcome:?}"
    );

    let state = RuntimeState::open(&repository, identity, [107; 32])
        .await
        .expect("runtime state");
    assert_eq!(
        state
            .request_status(request, fingerprint)
            .await
            .expect("request status")
            .expect("retained request")
            .state,
        RequestState::Cancelled
    );
    assert!(
        state
            .committed_table_rows("Reading")
            .await
            .expect("Reading rows")
            .is_empty()
    );
    assert!(
        state
            .latest_checkpoint()
            .await
            .expect("checkpoint")
            .is_none()
    );

    let runs = state.run_observations().await.expect("runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunObservationStatus::Cancelled);
    assert_eq!(runs[0].checkpoint_count, 0);
    let streams = state.stream_observations().await.expect("streams");
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].status, StreamObservationStatus::Cancelled);
    assert_eq!((streams[0].items_seen, streams[0].items_committed), (0, 0));
    assert_eq!(streams[0].items_failed, 0);
    assert!(streams[0].last_failure.is_none());
    assert!(streams[0].diagnostic.is_none());
}

#[tokio::test]
async fn project_stream_cancellation_after_one_commit_retains_progress_without_failure() {
    let temp = TempDir::new().expect("temporary repository");
    let repository = initialized_repository(&temp);
    let identity = RuntimeIdentity {
        database_id: [111; 16],
        repository_id: [112; 16],
    };
    let request = RequestIdentity {
        session_id: [113; 16],
        request_id: [114; 16],
    };
    let fingerprint = [115; 32];
    let control = CancelAtCheck::new(4);
    let outcome = orna_conformance_v1::DurableTransactionalEvaluator::default()
        .execute_project_stream_request_with_control(
            orna_conformance_v1::RuntimeTarget {
                repository: &repository,
                identity,
                owner_id: [116; 16],
                initial_digest: [117; 32],
            },
            request,
            fingerprint,
            &cancellation_project(),
            "sensors.ingest",
            &control,
        )
        .await
        .expect("cancellation result");
    assert!(
        matches!(outcome, StageOutcome::Cancelled(ref diagnostic) if diagnostic.code() == "ORNA-LIST-STREAM-CANCELLED"),
        "cancellation remains a narrow diagnostic outcome: {outcome:?}"
    );

    let state = RuntimeState::open(&repository, identity, [117; 32])
        .await
        .expect("runtime state");
    assert_eq!(
        state
            .request_status(request, fingerprint)
            .await
            .expect("request status")
            .expect("retained request")
            .state,
        RequestState::Cancelled
    );
    assert_eq!(
        state
            .committed_table_rows("Reading")
            .await
            .expect("Reading rows")
            .len(),
        1
    );
    assert!(
        state
            .latest_checkpoint()
            .await
            .expect("checkpoint")
            .is_some()
    );

    let runs = state.run_observations().await.expect("runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunObservationStatus::Cancelled);
    assert_eq!(runs[0].checkpoint_count, 1);
    let streams = state.stream_observations().await.expect("streams");
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].status, StreamObservationStatus::Cancelled);
    assert_eq!((streams[0].items_seen, streams[0].items_committed), (1, 1));
    assert_eq!(streams[0].items_failed, 0);
    assert!(streams[0].last_failure.is_none());
    assert!(streams[0].diagnostic.is_none());
}

#[test]
fn bounded_pure_function_admission_retains_a_digest_bound_executable_namespace() {
    let unit = SourceUnit {
        fixture_id: "pure-function-witness".into(),
        source_id: "main.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn add_one(value: Int): Int = value + 1;".into(),
    };
    let arguments = BTreeMap::from([(
        "value".into(),
        Value::new(OvbRaw::Int(41.into())).expect("canonical argument"),
    )]);

    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let admission = adapter
        .admit_pure_function(&unit, "add_one")
        .expect("pure function is admitted after compiler stages");
    assert_eq!(admission.source_id(), "main.orna");
    assert_eq!(admission.function(), "add_one");
    assert_eq!(
        admission.source_digest(),
        <[u8; 32]>::from(sha2::Sha256::digest(unit.source.as_bytes()))
    );

    let actual = adapter
        .invoke_admitted_pure_function(admission, &arguments)
        .expect("admitted namespace executes without source replay");

    assert_eq!(
        actual,
        Value::new(OvbRaw::Int(42.into())).expect("canonical result")
    );
}

#[test]
fn project_row_admission_resolves_declared_owner_path_key_and_evaluated_body() {
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let project = ProjectUnit {
        fixture_id: "project-rows".into(),
        project_id: "logical/project".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "project-rows".into(),
            source_id: "logical/project/inventory.orna".into(),
            parse_as: "module_unit".into(),
            source: "pub table Item(id: Int) { name: Str, available: Bool, price: Decimal, description: Str = \"default\", }".into(),
        }],
        loose_rows: vec![SourceUnit {
            fixture_id: "project-rows".into(),
            source_id: "logical/project/inventory/Item/42.orna".into(),
            parse_as: "row_unit".into(),
            source: "{ name: \"Pencil\", available: true, price: 1.00 + 0.25 }".into(),
        }],
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    };

    assert_eq!(adapter.validate_rows(&project), StageOutcome::Passed);
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "inventory.orna",
            project.modules[0].source.clone(),
        )],
        &Catalogue::authoritative_fixture(),
    );
    let rows = orna_conformance_v1::row_admission::admit_project_rows(
        &project,
        &analysis,
        Limits::default(),
    )
    .expect("project-context row admission");
    assert!(matches!(rows[0].key[0].raw(), OvbRaw::Int(value) if value == &42.into()));
    assert!(matches!(rows[0].body["price"].raw(), OvbRaw::Tag(60000, _)));
    assert!(!rows[0].body.contains_key("description"));
}

#[test]
fn project_row_admission_rejects_unsupported_key_types_without_string_fallback() {
    let project = ProjectUnit {
        fixture_id: "project-rows-unsupported".into(),
        project_id: "logical/project".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "project-rows-unsupported".into(),
            source_id: "logical/project/calendar.orna".into(),
            parse_as: "module_unit".into(),
            source: "pub table Event(id: Date) { name: Str, }".into(),
        }],
        loose_rows: vec![SourceUnit {
            fixture_id: "project-rows-unsupported".into(),
            source_id: "logical/project/calendar/Event/2026-09-05.orna".into(),
            parse_as: "row_unit".into(),
            source: "{ name: \"Review\" }".into(),
        }],
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    };

    let StageOutcome::Failed(diagnostic) =
        RuntimeAdapter::new(BoundedEvaluator::default()).validate_rows(&project)
    else {
        panic!("unsupported key types must not pass row admission");
    };
    assert_eq!(diagnostic.code(), "ORNA-CONFORMANCE-ROW-UNSUPPORTED-TYPE");
}

#[test]
fn project_row_admission_rejects_path_key_and_schema_failures() {
    let module = SourceUnit {
        fixture_id: "project-rows-negative".into(),
        source_id: "logical/project/inventory.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub table Item(id: Int) { name: Str, }".into(),
    };
    for (source_id, source, code) in [
        (
            "logical/project/inventory/Item/not-an-int.orna",
            "{ name: \"Pencil\" }",
            "ORNA-CONFORMANCE-ROW-PATH",
        ),
        (
            "logical/project/inventory/Item/042.orna",
            "{ name: \"Pencil\" }",
            "ORNA-CONFORMANCE-ROW-PATH",
        ),
        (
            "logical/project/inventory/Item/+42.orna",
            "{ name: \"Pencil\" }",
            "ORNA-CONFORMANCE-ROW-PATH",
        ),
        (
            "logical/project/inventory/Item/-0.orna",
            "{ name: \"Pencil\" }",
            "ORNA-CONFORMANCE-ROW-PATH",
        ),
        (
            "logical/project/inventory/Item/42/extra.orna",
            "{ name: \"Pencil\" }",
            "ORNA-CONFORMANCE-ROW-PATH",
        ),
        (
            "logical/project/inventory/Item/42.orna",
            "{ id: 42, name: \"Pencil\" }",
            "E3004",
        ),
        (
            "logical/project/inventory/Item/42.orna",
            "{}",
            "ORNA-CONFORMANCE-ROW-MISSING",
        ),
        (
            "logical/project/inventory/Item/42.orna",
            "{ name: true }",
            "ORNA-CONFORMANCE-ROW-TYPE",
        ),
        (
            "logical/project/inventory/Item/42.orna",
            "{ unknown: \"Pencil\" }",
            "ORNA-CONFORMANCE-ROW-UNKNOWN",
        ),
    ] {
        let project = ProjectUnit {
            fixture_id: "project-rows-negative".into(),
            project_id: "logical/project".into(),
            environment_id: None,
            modules: vec![module.clone()],
            loose_rows: vec![SourceUnit {
                fixture_id: "project-rows-negative".into(),
                source_id: source_id.into(),
                parse_as: "row_unit".into(),
                source: source.into(),
            }],
            expectations: ProjectExpectations {
                environment: ProjectEnvironment {
                    network: false,
                    credentials: false,
                    intrinsics: "Orna 1.0.0 core".into(),
                    stdlib: None,
                    initial_tables: "empty".into(),
                },
                steps: Vec::new(),
                negative_cases: Vec::new(),
            },
        };
        let StageOutcome::Failed(diagnostic) =
            RuntimeAdapter::new(BoundedEvaluator::default()).validate_rows(&project)
        else {
            panic!("row admission should fail");
        };
        assert_eq!(diagnostic.code(), code);
    }
}

#[test]
fn project_row_admission_admits_automatic_and_composite_keys() {
    let project = ProjectUnit {
        fixture_id: "project-rows-key-shapes".into(),
        project_id: "logical/project".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "project-rows-key-shapes".into(),
            source_id: "logical/project/inventory.orna".into(),
            parse_as: "module_unit".into(),
            source: "pub table Note { text: Str, } pub table Reading(sensor: Str, sequence: Int) { value: Decimal, }".into(),
        }],
        loose_rows: vec![
            SourceUnit {
                fixture_id: "project-rows-key-shapes".into(),
                source_id: "logical/project/inventory/Note/7.orna".into(),
                parse_as: "row_unit".into(),
                source: "{ text: \"memo\" }".into(),
            },
            SourceUnit {
                fixture_id: "project-rows-key-shapes".into(),
                source_id: "logical/project/inventory/Reading/greenhouse/2.orna".into(),
                parse_as: "row_unit".into(),
                source: "{ value: 18.50 }".into(),
            },
        ],
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    };

    assert_eq!(
        RuntimeAdapter::new(BoundedEvaluator::default()).validate_rows(&project),
        StageOutcome::Passed
    );
}

#[test]
fn project_row_admission_rejects_computed_fields() {
    let project = ProjectUnit {
        fixture_id: "project-rows-computed".into(),
        project_id: "logical/project".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "project-rows-computed".into(),
            source_id: "logical/project/inventory.orna".into(),
            parse_as: "module_unit".into(),
            source: "pub table Item(id: Int) { name: Str, label: Str => name, }".into(),
        }],
        loose_rows: vec![SourceUnit {
            fixture_id: "project-rows-computed".into(),
            source_id: "logical/project/inventory/Item/42.orna".into(),
            parse_as: "row_unit".into(),
            source: "{ name: \"Pencil\", label: \"Pencil\" }".into(),
        }],
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    };

    let StageOutcome::Failed(diagnostic) =
        RuntimeAdapter::new(BoundedEvaluator::default()).validate_rows(&project)
    else {
        panic!("computed fields must not be admitted from a loose row");
    };
    assert_eq!(diagnostic.code(), "ORNA-CONFORMANCE-ROW-COMPUTED");
}

#[test]
fn project_row_admission_accepts_exact_current_container_forms() {
    let project = container_row_project(
        "{ name: \"Pencil\", emails: [\"sales@example.com\"], contact: { email: \"sales@example.com\", verified: true }, coordinates: (51, \"north\"), nickname: null, backup_name: \"HB\" }",
    );

    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    assert_eq!(adapter.validate_rows(&project), StageOutcome::Passed);

    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "inventory.orna",
            project.modules[0].source.clone(),
        )],
        &Catalogue::authoritative_fixture(),
    );
    let rows = orna_conformance_v1::row_admission::admit_project_rows(
        &project,
        &analysis,
        Limits::default(),
    )
    .expect("container row admission");
    assert!(matches!(rows[0].body["emails"].raw(), OvbRaw::Array(_)));
    assert!(matches!(rows[0].body["contact"].raw(), OvbRaw::Map(_)));
    assert!(matches!(
        rows[0].body["coordinates"].raw(),
        OvbRaw::Array(_)
    ));
    assert!(matches!(rows[0].body["nickname"].raw(), OvbRaw::Null));
}

#[test]
fn project_row_admission_rejects_wrong_container_members_and_shapes() {
    for source in [
        "{ name: \"Pencil\", emails: [1], contact: { email: \"sales@example.com\", verified: true }, coordinates: (51, \"north\"), nickname: null, backup_name: \"HB\" }",
        "{ name: \"Pencil\", emails: [\"sales@example.com\"], contact: { email: \"sales@example.com\", verified: true, extra: \"no\" }, coordinates: (51, \"north\"), nickname: null, backup_name: \"HB\" }",
        "{ name: \"Pencil\", emails: [\"sales@example.com\"], contact: { email: \"sales@example.com\" }, coordinates: (51, \"north\"), nickname: null, backup_name: \"HB\" }",
        "{ name: \"Pencil\", emails: [\"sales@example.com\"], contact: { email: \"sales@example.com\", verified: true }, coordinates: [51, \"north\"], nickname: null, backup_name: \"HB\" }",
        "{ name: \"Pencil\", emails: (\"sales@example.com\",), contact: { email: \"sales@example.com\", verified: true }, coordinates: (51, \"north\"), nickname: null, backup_name: \"HB\" }",
        "{ name: \"Pencil\", emails: [\"sales@example.com\"], contact: { email: \"sales@example.com\", verified: true }, coordinates: (51, \"north\"), nickname: 1, backup_name: \"HB\" }",
    ] {
        let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
        let StageOutcome::Failed(diagnostic) =
            adapter.validate_rows(&container_row_project(source))
        else {
            panic!("invalid nested container value must not pass row admission");
        };
        assert_eq!(diagnostic.code(), "ORNA-CONFORMANCE-ROW-TYPE");
    }
}

#[test]
fn project_row_admission_reports_unsupported_nested_field_types() {
    let project = admission_project(
        "pub table Item(id: Int) { opaque_ids: [Uuid], }",
        vec![SourceUnit {
            fixture_id: "row-admission-containers".into(),
            source_id: "logical/project/inventory/Item/42.orna".into(),
            parse_as: "row_unit".into(),
            source: "{ opaque_ids: [\"not-an-admitted-uuid\"] }".into(),
        }],
    );
    let mut adapter = RuntimeAdapter::new(BoundedEvaluator::default());
    let StageOutcome::Failed(diagnostic) = adapter.validate_rows(&project) else {
        panic!("unsupported nested field type must not pass row admission");
    };
    assert_eq!(diagnostic.code(), "ORNA-CONFORMANCE-ROW-UNSUPPORTED-TYPE");
}

#[test]
fn semantic_adapter_keeps_type_errors_in_the_typecheck_phase() {
    let unit = SourceUnit {
        fixture_id: "type-error".into(),
        source_id: "logical/type-error.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub table Bad(value: Float) { text: Str, }".into(),
    };
    let mut adapter = SemanticAdapter::default();
    assert!(matches!(adapter.resolve(&unit), StageOutcome::Passed));
    let StageOutcome::Failed(diagnostic) = adapter.typecheck(&unit) else {
        panic!("type errors must be reported by typecheck");
    };
    assert_eq!(diagnostic.code(), "ORNA-S021-TYPE");
}

#[test]
fn calendar_zone_rejection_preserves_published_diagnostic_identity() {
    let unit = SourceUnit {
        fixture_id: "calendar-zone".into(),
        source_id: "calendar-zone.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn bad() = energy.Reading | bucket_by(1.day);".into(),
    };
    let mut adapter = SemanticAdapter::default();
    let StageOutcome::Failed(diagnostic) = adapter.typecheck(&unit) else {
        panic!("calendar bucketing without a zone must be rejected");
    };
    assert_eq!(
        adapter.diagnostic_code(&diagnostic),
        "E6001",
        "{diagnostic:?}"
    );
}

#[test]
fn semantic_adapter_preserves_published_closed_type_diagnostics() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut SemanticAdapter::default());
    for fixture_id in [
        "invalid/calendar-bucket-no-zone.orna",
        "invalid/unknown-field.orna",
        "invalid/wrong-field-type.orna",
        "invalid/affine-addition.orna",
        "invalid/affine-sum.orna",
        "invalid/currency-addition.orna",
        "invalid/currency-static-symbol.orna",
        "invalid/float-money-implicit.orna",
        "invalid/float-key.orna",
        "invalid/relation-equality.orna",
        "invalid/legacy-checkpoint-reset-method.orna",
        "invalid/legacy-failure-replay-method.orna",
        "invalid/legacy-failure-resolve-method.orna",
        "invalid/legacy-stream-retry-method.orna",
        "invalid/legacy-stream-skip-method.orna",
        "invalid/implicit-conversion-chain.orna",
        "invalid/money-float.orna",
        "invalid/module-single-table-assertion.orna",
        "invalid/module-zero-table-assertion.orna",
        "invalid/legacy-result.orna",
        "invalid/legacy-sys-runtime.orna",
        "invalid/legacy-sys-storage-call.orna",
        "invalid/legacy-tryfrom.orna",
        "invalid/legacy-assert-owner-pipe.orna",
        "invalid/legacy-assert-self-pipe.orna",
        "invalid/incompatible-dimensions.orna",
        "invalid/reserved-std.orna",
        "invalid/reserved-sys.orna",
        "invalid/range-key-overlap-magic.orna",
        "invalid/rekey-auto-id.orna",
        "invalid/effectful-display.orna",
        "invalid/secret-display.orna",
        "invalid/assert-effectful-table.orna",
        "invalid/computed-field-effect.orna",
        "invalid/mutate-sys-commit.orna",
    ] {
        let fixture = report
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == fixture_id)
            .expect("closed type fixture");
        assert!(fixture.passed, "{fixture_id}: {:?}", fixture.stages);
    }
}

#[derive(Default)]
struct RecordingRuntime {
    calls: usize,
}
impl RuntimeEvaluator for RecordingRuntime {
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
    fn run_scenario(&mut self, _: &Scenario) -> StageOutcome<Diagnostic> {
        self.calls += 1;
        StageOutcome::Passed
    }
}

#[test]
fn runtime_adapter_has_an_executable_seam_but_lazy_semantic_failures_precede_it() {
    let mut adapter = RuntimeAdapter::new(RecordingRuntime::default());
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut adapter);
    let runtime = adapter.into_runtime();
    assert!(runtime.calls >= report.scenarios.len());
    assert!(
        report
            .scenarios
            .iter()
            .all(|scenario| scenario.status == EvidenceStatus::Passed)
    );
}

#[derive(Default)]
struct ResolvedRowsRuntime {
    fallback_rows: usize,
    resolved_rows: usize,
}

impl RuntimeEvaluator for ResolvedRowsRuntime {
    fn evaluate(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Passed
    }
    fn validate_row(&mut self, _: &SourceUnit) -> StageOutcome<Diagnostic> {
        StageOutcome::Passed
    }
    fn validate_rows(&mut self, _: &ProjectUnit) -> StageOutcome<Diagnostic> {
        self.fallback_rows += 1;
        StageOutcome::Skipped {
            reason: "resolved row validation was not used".into(),
        }
    }
    fn validate_resolved_rows(
        &mut self,
        _: &ProjectUnit,
        analysis: &orna_semantic_v1::Analysis,
    ) -> StageOutcome<Diagnostic> {
        self.resolved_rows += analysis.modules.len();
        StageOutcome::Passed
    }
    fn run_scenario(&mut self, _: &Scenario) -> StageOutcome<Diagnostic> {
        StageOutcome::Passed
    }
}

#[test]
fn runtime_adapter_preserves_custom_resolved_row_validation() {
    let project = admission_project("pub table Item(id: Int) { name: Str, }", Vec::new());
    let mut adapter = RuntimeAdapter::new(ResolvedRowsRuntime::default());

    assert_eq!(adapter.validate_rows(&project), StageOutcome::Passed);

    let runtime = adapter.into_runtime();
    assert!(runtime.resolved_rows > 0);
    assert_eq!(runtime.fallback_rows, 0);
}

#[test]
fn bounded_row_evaluation_does_not_claim_schema_or_path_key_validation() {
    let mut evaluator = BoundedEvaluator::default();
    for source in [
        "{ name: \"Alice\" }",
        "{ id: \"alice\", name: \"Alice\" }",
        "42",
    ] {
        let row = SourceUnit {
            fixture_id: "row-admission".into(),
            source_id: "rows/alice.orna".into(),
            parse_as: "row_unit".into(),
            source: source.into(),
        };
        assert_eq!(evaluator.evaluate(&row), StageOutcome::Passed);
        assert!(matches!(
            evaluator.validate_row(&row),
            StageOutcome::Skipped { .. }
        ));
        let mut project = pure_project(Vec::new());
        assert!(matches!(
            evaluator.validate_rows(&project),
            StageOutcome::Skipped { .. }
        ));
        project.loose_rows.push(row);
        assert!(matches!(
            evaluator.validate_rows(&project),
            StageOutcome::Skipped { .. }
        ));
    }
    let report =
        Harness::new(Corpus::load_default().unwrap()).run(&mut RuntimeAdapter::new(evaluator));
    let fixture = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "invalid/unsafe-row-key-repeat.orna")
        .unwrap();
    assert!(!fixture.passed);
    let validation = fixture
        .stages
        .iter()
        .find(|stage| stage.stage == Some(orna_conformance_v1::Stage::RowValidation))
        .unwrap();
    assert_eq!(validation.status, EvidenceStatus::Skipped);
    assert_eq!(validation.class, EvidenceClass::Skipped);
}

#[test]
fn bounded_evaluator_executes_expression_units_and_redacts_failures() {
    let mut evaluator = BoundedEvaluator::default();
    let valid = SourceUnit {
        fixture_id: "test-valid".into(),
        source_id: "logical/test.orna".into(),
        parse_as: "row_unit".into(),
        source: "{ total: std.math.increment(1) }".into(),
    };
    assert_eq!(evaluator.evaluate(&valid), StageOutcome::Passed);

    let invalid = SourceUnit {
        fixture_id: "test-invalid".into(),
        source_id: "logical/test.orna".into(),
        parse_as: "row_unit".into(),
        source: "{ total: missing }".into(),
    };
    let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&invalid) else {
        panic!("unknown name must fail");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-NAME");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn reference_project_empty_row_discovery_passes_vacuously_through_project_admission() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut RuntimeAdapter::new(BoundedEvaluator::default()));
    let project = report
        .fixtures
        .iter()
        .find(|fixture| fixture.fixture == "PROJECT-REFERENCE")
        .expect("reference project result");

    assert!(project.passed);
    assert_eq!(
        project
            .stages
            .iter()
            .map(|stage| (
                stage.stage.clone(),
                stage.status.clone(),
                stage.class.clone()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                Some(orna_conformance_v1::Stage::Parse),
                EvidenceStatus::Passed,
                EvidenceClass::Runtime
            ),
            (
                Some(orna_conformance_v1::Stage::Resolve),
                EvidenceStatus::Passed,
                EvidenceClass::Semantic
            ),
            (
                Some(orna_conformance_v1::Stage::Typecheck),
                EvidenceStatus::Passed,
                EvidenceClass::Semantic
            ),
            (
                Some(orna_conformance_v1::Stage::Evaluate),
                EvidenceStatus::Skipped,
                EvidenceClass::Skipped
            ),
            (
                Some(orna_conformance_v1::Stage::RowValidation),
                EvidenceStatus::Passed,
                EvidenceClass::Semantic
            ),
        ]
    );
}

#[test]
fn harness_does_not_turn_unexpected_evaluation_into_fixture_failure() {
    let corpus = Corpus::load_default().expect("reference corpus loads");
    let report = Harness::new(corpus).run(&mut RuntimeAdapter::new(BoundedEvaluator::default()));
    for fixture_id in [
        "valid/coalesce-precedence.orna",
        "valid/question-coalesce-parenthesized.orna",
    ] {
        let fixture = report
            .fixtures
            .iter()
            .find(|fixture| fixture.fixture == fixture_id)
            .expect("coalesce fixture");
        assert!(fixture.passed, "{fixture_id}: {:?}", fixture.stages);
        assert_eq!(
            fixture.stages.last().expect("evaluation stage").status,
            EvidenceStatus::Skipped
        );
    }
}

fn pure_project(modules: Vec<SourceUnit>) -> ProjectUnit {
    ProjectUnit {
        fixture_id: "test-project".into(),
        project_id: "logical/project".into(),
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
            negative_cases: Vec::new(),
        },
    }
}

fn admission_project(module_source: &str, loose_rows: Vec<SourceUnit>) -> ProjectUnit {
    ProjectUnit {
        fixture_id: "row-admission-limits".into(),
        project_id: "logical/project".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "row-admission-limits".into(),
            source_id: "logical/project/inventory.orna".into(),
            parse_as: "module_unit".into(),
            source: module_source.into(),
        }],
        loose_rows,
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

fn container_row_project(source: &str) -> ProjectUnit {
    admission_project(
        "pub table Item(id: Int) { name: Str, emails: [Str], contact: { email: Str, verified: Bool, }, coordinates: (Int, Str), nickname: Str?, backup_name: Str?, }",
        vec![SourceUnit {
            fixture_id: "row-admission-containers".into(),
            source_id: "logical/project/inventory/Item/42.orna".into(),
            parse_as: "row_unit".into(),
            source: source.into(),
        }],
    )
}

#[test]
fn row_validation_preflights_source_items_and_steps_before_or_during_admission() {
    let oversized = admission_project("invalid(", Vec::new());
    let mut source_limited = RuntimeAdapter::new(BoundedEvaluator::new(Limits {
        max_source_bytes: 2,
        ..Default::default()
    }));
    let StageOutcome::Failed(diagnostic) = source_limited.validate_rows(&oversized) else {
        panic!("source limit must fail before semantic module parsing");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");

    let row = SourceUnit {
        fixture_id: "row-admission-limits".into(),
        source_id: "logical/project/inventory/Item/42.orna".into(),
        parse_as: "row_unit".into(),
        source: "{ name: \"Pencil\" }".into(),
    };
    let project = admission_project("pub table Item(id: Int) { name: Str, }", vec![row]);
    let mut item_limited = RuntimeAdapter::new(BoundedEvaluator::new(Limits {
        max_collection_items: 1,
        ..Default::default()
    }));
    let StageOutcome::Failed(diagnostic) = item_limited.validate_rows(&project) else {
        panic!("project unit capacity must fail before row admission allocation");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");

    let mut step_limited = RuntimeAdapter::new(BoundedEvaluator::new(Limits {
        max_steps: 1,
        ..Default::default()
    }));
    let StageOutcome::Failed(diagnostic) = step_limited.validate_rows(&project) else {
        panic!("row body evaluation must use the configured step limit");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
}

#[test]
fn bounded_evaluator_defers_invalid_function_bodies_until_explicit_invocation() {
    let pure_module = SourceUnit {
        fixture_id: "test-module".into(),
        source_id: "logical/pure.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn secret() = missing;".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&pure_module), StageOutcome::Passed);
    assert_eq!(
        evaluator.evaluate_project(&pure_project(vec![pure_module])),
        StageOutcome::Passed
    );
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("secret") else {
        panic!("invalid retained function body must fail when invoked");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-NAME");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn bounded_project_retains_owner_scoped_nominal_plans_for_functions_and_expressions() {
    let project = pure_project(vec![
        SourceUnit {
            fixture_id: "nominal-project".into(),
            source_id: "nominal/owner.orna".into(),
            parse_as: "module_unit".into(),
            source: r#"
                fn private_seed() = 7;
                pub type Box {
                    pub value: Int = private_seed(),
                    secret: Int = private_seed(),
                }
                pub type Required { pub value: Int, }
                pub fn omitted() = Box { };
                pub fn missing_required() = Required { };
            "#
            .into(),
        },
        SourceUnit {
            fixture_id: "nominal-project".into(),
            source_id: "nominal/caller.orna".into(),
            parse_as: "module_unit".into(),
            source: r#"
                use nominal.owner;
                pub fn external() = nominal.owner.Box { value: 3 };
                pub fn external_omitted() = nominal.owner.Box { };
            "#
            .into(),
        },
    ]);
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate_project(&project), StageOutcome::Passed);

    let expected_omitted = OvbRaw::Tag(
        60009,
        Box::new(OvbRaw::Array(vec![
            nominal_type_raw("nominal.owner.Box"),
            OvbRaw::Array(vec![
                OvbRaw::Array(vec![
                    nominal_field_raw("nominal.owner.Box", "value"),
                    OvbRaw::Int(7.into()),
                ]),
                OvbRaw::Array(vec![
                    nominal_field_raw("nominal.owner.Box", "secret"),
                    OvbRaw::Int(7.into()),
                ]),
            ]),
        ])),
    );
    let actual_omitted = evaluator
        .invoke_value_with("nominal.owner.omitted", &BTreeMap::new())
        .expect("omitted defaults construct the nominal");
    assert_eq!(actual_omitted.raw(), &expected_omitted);

    let actual_external_omitted = evaluator
        .invoke_value_with("nominal.caller.external_omitted", &BTreeMap::new())
        .expect("external construction may omit both defaults");
    assert_eq!(actual_external_omitted.raw(), &expected_omitted);

    let expected_external = OvbRaw::Tag(
        60009,
        Box::new(OvbRaw::Array(vec![
            nominal_type_raw("nominal.owner.Box"),
            OvbRaw::Array(vec![
                OvbRaw::Array(vec![
                    nominal_field_raw("nominal.owner.Box", "value"),
                    OvbRaw::Int(3.into()),
                ]),
                OvbRaw::Array(vec![
                    nominal_field_raw("nominal.owner.Box", "secret"),
                    OvbRaw::Int(7.into()),
                ]),
            ]),
        ])),
    );
    let actual_external = evaluator
        .invoke_value_with("nominal.caller.external", &BTreeMap::new())
        .expect("external construction may omit a private default");
    assert_eq!(actual_external.raw(), &expected_external);

    let expression = SourceUnit {
        fixture_id: "nominal-expression".into(),
        source_id: "nominal/expression.orna".into(),
        parse_as: "expression_unit".into(),
        source: "nominal.owner.Box { value: 9 }".into(),
    };
    assert_eq!(evaluator.evaluate(&expression), StageOutcome::Passed);

    let diagnostic = evaluator
        .invoke_value_with("nominal.owner.missing_required", &BTreeMap::new())
        .expect_err("missing public construction fields must fail");
    assert_eq!(diagnostic.code(), "ORNA-EVAL-ARGUMENT");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn bounded_evaluator_invokes_a_function_with_its_earlier_immutable_binding() {
    let pure_module = SourceUnit {
        fixture_id: "test-module".into(),
        source_id: "logical/pure.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn incremented() = if true { let answer = 41; std.math.increment(answer) } else { 0 };".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&pure_module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("incremented"), StageOutcome::Passed);
}

fn value(raw: OvbRaw) -> Value {
    Value::new(raw).expect("test values are canonical")
}

fn nominal_type_raw(identity: &str) -> OvbRaw {
    OvbRaw::Tag(
        37,
        Box::new(OvbRaw::Bytes(nominal_type_id(identity).to_vec())),
    )
}

fn nominal_field_raw(identity: &str, name: &str) -> OvbRaw {
    OvbRaw::Tag(
        37,
        Box::new(OvbRaw::Bytes(nominal_field_id(identity, name).to_vec())),
    )
}

fn nominal_type_id(identity: &str) -> [u8; 16] {
    sha2::Sha256::digest(identity.as_bytes())[..16]
        .try_into()
        .expect("truncated digest has the ObjectId width")
}

fn nominal_field_id(identity: &str, name: &str) -> [u8; 16] {
    let mut input = Vec::with_capacity(identity.len() + name.len() + 1);
    input.extend_from_slice(identity.as_bytes());
    input.push(0);
    input.extend_from_slice(name.as_bytes());
    sha2::Sha256::digest(input)[..16]
        .try_into()
        .expect("truncated digest has the ObjectId width")
}

#[test]
fn bounded_evaluator_invokes_retained_functions_with_named_arguments_and_defaults() {
    let pure_module = SourceUnit {
        fixture_id: "test-module".into(),
        source_id: "logical/pure.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn increment(number, label) = std.math.increment(number); pub fn add_one(value, increment = 1) = value + increment;".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&pure_module), StageOutcome::Passed);

    let arguments = BTreeMap::from([
        ("label".into(), value(OvbRaw::Text("named".into()))),
        ("number".into(), value(OvbRaw::Int(41.into()))),
    ]);
    assert_eq!(
        evaluator.invoke_with("increment", &arguments),
        StageOutcome::Passed
    );
    assert_eq!(
        evaluator.invoke_with(
            "add_one",
            &BTreeMap::from([("value".into(), value(OvbRaw::Int(41.into())))]),
        ),
        StageOutcome::Passed
    );
}

#[test]
fn bounded_evaluator_executes_only_profile_verified_pure_standard_sources() {
    let source = "pub fn increment(value: Int): Int = value + 1;";
    let profile = orna_semantic_v1::StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [("std/math.orna".into(), source.into())],
    )
    .unwrap();
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(
        evaluator.load_standard_sources(&profile, [("std/math.orna".into(), source.into())],),
        StageOutcome::Passed
    );
    assert_eq!(
        evaluator.invoke_with(
            "increment",
            &BTreeMap::from([("value".into(), value(OvbRaw::Int(41.into())),)])
        ),
        StageOutcome::Passed
    );
    assert!(matches!(
        evaluator.load_standard_sources(
            &profile,
            [(
                "std/math.orna".into(),
                "pub fn increment(value: Int): Int = value;".into()
            )],
        ),
        StageOutcome::Failed(_)
    ));
}

#[test]
fn module_admission_checks_source_and_zero_limits_before_parsing() {
    let unit = SourceUnit {
        fixture_id: "module-admission".into(),
        source_id: "logical/module-admission.orna".into(),
        parse_as: "module_unit".into(),
        source: "invalid(".into(),
    };
    for limits in [
        orna_evaluator_v1::Limits {
            max_source_bytes: 2,
            ..Default::default()
        },
        orna_evaluator_v1::Limits {
            max_steps: 0,
            ..Default::default()
        },
    ] {
        let mut evaluator = BoundedEvaluator::new(limits);
        let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&unit) else {
            panic!("module admission limits must fail before parsing");
        };
        assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
        assert_eq!(diagnostic.message(), "<redacted>");
        let StageOutcome::Failed(diagnostic) =
            evaluator.evaluate_project(&pure_project(vec![unit.clone()]))
        else {
            panic!("project preflight must apply source limits before parsing");
        };
        assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    }
}

#[test]
fn rejected_project_capacity_does_not_publish_partial_function_updates() {
    let original = SourceUnit {
        fixture_id: "retained-limit".into(),
        source_id: "logical/retained-limit.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn stable() = 1;".into(),
    };
    let mut evaluator = BoundedEvaluator::new(orna_evaluator_v1::Limits {
        max_collection_items: 2,
        ..Default::default()
    });
    assert_eq!(evaluator.evaluate(&original), StageOutcome::Passed);
    let replacement = SourceUnit {
        source: "fn stable() = 99; fn added() = 2;".into(),
        ..original.clone()
    };
    let overflow = SourceUnit {
        source: "fn excess() = 3;".into(),
        ..original.clone()
    };
    let StageOutcome::Failed(diagnostic) =
        evaluator.evaluate_project(&pure_project(vec![replacement, overflow]))
    else {
        panic!("retained function count must be bounded across modules");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    assert!(matches!(
        evaluator.invoke("added"),
        StageOutcome::Skipped { .. }
    ));
    let probe = SourceUnit {
        parse_as: "expression_unit".into(),
        source: "if stable() == 1 { 1 } else { 1 / 0 }".into(),
        ..original.clone()
    };
    assert_eq!(evaluator.evaluate(&probe), StageOutcome::Passed);
    // Replacing an existing definition does not consume another retained slot.
    let replacement = SourceUnit {
        source: "fn stable() = 1; fn added() = 2;".into(),
        ..original
    };
    assert_eq!(evaluator.evaluate(&replacement), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("added"), StageOutcome::Passed);
}

#[test]
fn expression_units_use_retained_functions_and_rejected_modules_preserve_them() {
    let module = SourceUnit {
        fixture_id: "retained-functions".into(),
        source_id: "logical/retained-functions.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn increment(value: Int) = value + 1;".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    let expression = SourceUnit {
        parse_as: "expression_unit".into(),
        source: "if increment(41) == 42 { 1 } else { 1 / 0 }".into(),
        ..module.clone()
    };
    assert_eq!(evaluator.evaluate(&expression), StageOutcome::Passed);
    let failed_module = SourceUnit {
        source: "fn increment(value: Int) = value + 100; let answer = increment(1);".into(),
        ..module
    };
    let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&failed_module) else {
        panic!("module-level let is not part of the module grammar");
    };
    assert_eq!(diagnostic.code(), "ORNA-PARSE-001");
    assert_eq!(evaluator.evaluate(&expression), StageOutcome::Passed);
}

#[test]
fn registry_expression_dispatch_preserves_source_limits() {
    let mut evaluator = BoundedEvaluator::new(orna_evaluator_v1::Limits {
        max_source_bytes: 2,
        ..Default::default()
    });
    let unit = SourceUnit {
        fixture_id: "source-budget".into(),
        source_id: "logical/source-budget.orna".into(),
        parse_as: "expression_unit".into(),
        source: "invalid(".into(),
    };
    let StageOutcome::Failed(diagnostic) = evaluator.evaluate(&unit) else {
        panic!("source size limits apply before parsing");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn retained_functions_admit_structured_parameters_and_wildcards() {
    let module = SourceUnit {
        fixture_id: "parameter-patterns".into(),
        source_id: "logical/parameter-patterns.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn add((a, b) = (1, 2)) = a + b; fn ignore(_, _) = 7; fn verify() = if add((10, 20)) == 30 && ignore(1, 2) == 7 { 1 } else { 1 / 0 }; fn reject() = add(1);".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("verify"), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("add"), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("reject") else {
        panic!("parameter patterns must match the supplied values");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-TYPE");
}

#[test]
fn retained_functions_execute_closures_without_mutating_captures() {
    let module = SourceUnit {
        fixture_id: "closure-capture".into(),
        source_id: "logical/closure-capture.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn verify() { let seed = 2; let compute = value => value + seed; seed = 9; if (10 | compute) == 12 { 1 } else { 1 / 0 } } fn reject() { let seed = 1; let mutate = () => { seed += 1; seed }; mutate() }".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("verify"), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("reject") else {
        panic!("captured values are immutable");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-IMMUTABLE-CAPTURE");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn retained_function_pipeline_executes_and_checks_its_result() {
    let module = SourceUnit {
        fixture_id: "function-pipeline".into(),
        source_id: "logical/function-pipeline.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn add(value: Int, extra = 6) = value + extra; fn verify() = if (10 | add(extra: 6)) == 16 { 1 } else { 1 / 0 }; fn reject() = 10 | add(value: 3);".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("verify"), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("reject") else {
        panic!("the pipe input occupies the first parameter");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-ARGUMENT");
}

#[test]
fn retained_module_functions_call_helpers_in_defaults_and_bodies() {
    let module = SourceUnit {
        fixture_id: "nested-functions".into(),
        source_id: "logical/nested-functions.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn entry(value = helper(40)) = helper(value); fn helper(value: Int) = value + 1; fn recurse() = recurse();".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    assert_eq!(evaluator.invoke("entry"), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("recurse") else {
        panic!("recursive calls must hit the shared invocation limits");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn retained_function_defaults_cannot_reset_the_invocation_budget() {
    let module = SourceUnit {
        fixture_id: "default-budget".into(),
        source_id: "logical/default-budget.orna".into(),
        parse_as: "module_unit".into(),
        source: "fn compute(first = 1 + 2, second = 3 + 4) = first + second;".into(),
    };
    let mut evaluator = BoundedEvaluator::new(orna_evaluator_v1::Limits {
        max_steps: 6,
        ..Default::default()
    });
    assert_eq!(evaluator.evaluate(&module), StageOutcome::Passed);
    let StageOutcome::Failed(diagnostic) = evaluator.invoke("compute") else {
        panic!("defaults and body must share one invocation budget");
    };
    assert_eq!(diagnostic.code(), "ORNA-EVAL-LIMIT");
    assert_eq!(diagnostic.message(), "<redacted>");
}

#[test]
fn bounded_evaluator_redacts_missing_and_unknown_retained_function_arguments() {
    let pure_module = SourceUnit {
        fixture_id: "test-module".into(),
        source_id: "logical/pure.orna".into(),
        parse_as: "module_unit".into(),
        source: "pub fn increment(value) = std.math.increment(value);".into(),
    };
    let mut evaluator = BoundedEvaluator::default();
    assert_eq!(evaluator.evaluate(&pure_module), StageOutcome::Passed);

    for arguments in [
        BTreeMap::new(),
        BTreeMap::from([("unknown".into(), value(OvbRaw::Int(41.into())))]),
    ] {
        let StageOutcome::Failed(diagnostic) = evaluator.invoke_with("increment", &arguments)
        else {
            panic!("missing and unknown arguments must fail");
        };
        assert_eq!(diagnostic.code(), "ORNA-EVAL-ARGUMENT");
        assert_eq!(diagnostic.message(), "<redacted>");
    }
}
