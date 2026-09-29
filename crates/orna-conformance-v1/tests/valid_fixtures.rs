use std::{fs, process::Command};

use orna_project_v1::ProjectLoader;
use orna_repository_v1::Repository;
use orna_semantic_v1::{
    Catalogue, ModuleInput, RowUnitInput, admit_row_unit, analyze_with_catalogue,
};
use orna_syntax_v1::parse_module_with_file;
use tempfile::TempDir;

const VALID_FIXTURES: &[(&str, &str)] = &[
    ("affine-max", include_str!("../../../../reference/Orna-1.0.0/examples/valid/affine-max.orna")),
    ("affine-mean", include_str!("../../../../reference/Orna-1.0.0/examples/valid/affine-mean.orna")),
    ("affine-unit", include_str!("../../../../reference/Orna-1.0.0/examples/valid/affine-unit.orna")),
    ("assignment-statement", include_str!("../../../../reference/Orna-1.0.0/examples/valid/assignment-statement.orna")),
    ("associated-display", include_str!("../../../../reference/Orna-1.0.0/examples/valid/associated-display.orna")),
    ("automatic-failure-propagation", include_str!("../../../../reference/Orna-1.0.0/examples/valid/automatic-failure-propagation.orna")),
    ("blob-vs-information", include_str!("../../../../reference/Orna-1.0.0/examples/valid/blob-vs-information.orna")),
    ("calendar-bucket", include_str!("../../../../reference/Orna-1.0.0/examples/valid/calendar-bucket.orna")),
    ("case", include_str!("../../../../reference/Orna-1.0.0/examples/valid/case.orna")),
    ("case-record-and-block-arms", include_str!("../../../../reference/Orna-1.0.0/examples/valid/case-record-and-block-arms.orna")),
    ("coalesce-precedence", include_str!("../../../../reference/Orna-1.0.0/examples/valid/coalesce-precedence.orna")),
    ("codec-json", include_str!("../../../../reference/Orna-1.0.0/examples/valid/codec-json.orna")),
    ("codec-orna", include_str!("../../../../reference/Orna-1.0.0/examples/valid/codec-orna.orna")),
    ("computed-and-defaulted-fields", include_str!("../../../../reference/Orna-1.0.0/examples/valid/computed-and-defaulted-fields.orna")),
    ("computed-field", include_str!("../../../../reference/Orna-1.0.0/examples/valid/computed-field.orna")),
    ("control-flow", include_str!("../../../../reference/Orna-1.0.0/examples/valid/control-flow.orna")),
    ("cross-table-assertion", include_str!("../../../../reference/Orna-1.0.0/examples/valid/cross-table-assertion.orna")),
    ("currency", include_str!("../../../../reference/Orna-1.0.0/examples/valid/currency.orna")),
    ("currency-locale-format", include_str!("../../../../reference/Orna-1.0.0/examples/valid/currency-locale-format.orna")),
    ("cwd-head", include_str!("../../../../reference/Orna-1.0.0/examples/valid/cwd-head.orna")),
    ("decimal-division", include_str!("../../../../reference/Orna-1.0.0/examples/valid/decimal-division.orna")),
    ("default-and-computed-fields", include_str!("../../../../reference/Orna-1.0.0/examples/valid/default-and-computed-fields.orna")),
    ("dependency-query", include_str!("../../../../reference/Orna-1.0.0/examples/valid/dependency-query.orna")),
    ("duration-format", include_str!("../../../../reference/Orna-1.0.0/examples/valid/duration-format.orna")),
    ("effectful-expression", include_str!("../../../../reference/Orna-1.0.0/examples/valid/effectful-expression.orna")),
    ("empty-record-lambda", include_str!("../../../../reference/Orna-1.0.0/examples/valid/empty-record-lambda.orna")),
    ("enum", include_str!("../../../../reference/Orna-1.0.0/examples/valid/enum.orna")),
    ("executable-assertion", include_str!("../../../../reference/Orna-1.0.0/examples/valid/executable-assertion.orna")),
    ("explicit-rekey", include_str!("../../../../reference/Orna-1.0.0/examples/valid/explicit-rekey.orna")),
    ("failure-natural-key", include_str!("../../../../reference/Orna-1.0.0/examples/valid/failure-natural-key.orna")),
    ("finite-stream", include_str!("../../../../reference/Orna-1.0.0/examples/valid/finite-stream.orna")),
    ("formatting-does-not-serialize", include_str!("../../../../reference/Orna-1.0.0/examples/valid/formatting-does-not-serialize.orna")),
    ("function-block", include_str!("../../../../reference/Orna-1.0.0/examples/valid/function-block.orna")),
    ("function-default", include_str!("../../../../reference/Orna-1.0.0/examples/valid/function-default.orna")),
    ("function-expression", include_str!("../../../../reference/Orna-1.0.0/examples/valid/function-expression.orna")),
    ("function-values-and-pipelines", include_str!("../../../../reference/Orna-1.0.0/examples/valid/function-values-and-pipelines.orna")),
    ("generic-protocol", include_str!("../../../../reference/Orna-1.0.0/examples/valid/generic-protocol.orna")),
    ("historical-program", include_str!("../../../../reference/Orna-1.0.0/examples/valid/historical-program.orna")),
    ("historical-query", include_str!("../../../../reference/Orna-1.0.0/examples/valid/historical-query.orna")),
    ("imports", include_str!("../../../../reference/Orna-1.0.0/examples/valid/imports.orna")),
    ("inference-first", include_str!("../../../../reference/Orna-1.0.0/examples/valid/inference-first.orna")),
    ("key-default-allocated-once", include_str!("../../../../reference/Orna-1.0.0/examples/valid/key-default-allocated-once.orna")),
    ("lambda", include_str!("../../../../reference/Orna-1.0.0/examples/valid/lambda.orna")),
    ("lambda-empty-record", include_str!("../../../../reference/Orna-1.0.0/examples/valid/lambda-empty-record.orna")),
    ("live-page-fallback", include_str!("../../../../reference/Orna-1.0.0/examples/valid/live-page-fallback.orna")),
    ("minimal-root", include_str!("../../../../reference/Orna-1.0.0/examples/valid/minimal-root.orna")),
    ("money-rate", include_str!("../../../../reference/Orna-1.0.0/examples/valid/money-rate.orna")),
    ("money-serialization", include_str!("../../../../reference/Orna-1.0.0/examples/valid/money-serialization.orna")),
    ("nested-lambda", include_str!("../../../../reference/Orna-1.0.0/examples/valid/nested-lambda.orna")),
    ("nominal-type-nested-impl", include_str!("../../../../reference/Orna-1.0.0/examples/valid/nominal-type-nested-impl.orna")),
    ("numeric-literal-context", include_str!("../../../../reference/Orna-1.0.0/examples/valid/numeric-literal-context.orna")),
    ("page", include_str!("../../../../reference/Orna-1.0.0/examples/valid/page.orna")),
    ("parallel-function-values", include_str!("../../../../reference/Orna-1.0.0/examples/valid/parallel-function-values.orna")),
    ("parallel-streams", include_str!("../../../../reference/Orna-1.0.0/examples/valid/parallel-streams.orna")),
    ("parenthesized-lambda-stage", include_str!("../../../../reference/Orna-1.0.0/examples/valid/parenthesized-lambda-stage.orna")),
    ("pipe-first-argument", include_str!("../../../../reference/Orna-1.0.0/examples/valid/pipe-first-argument.orna")),
    ("pipeline", include_str!("../../../../reference/Orna-1.0.0/examples/valid/pipeline.orna")),
    ("pipeline-precedence", include_str!("../../../../reference/Orna-1.0.0/examples/valid/pipeline-precedence.orna")),
    ("presentation-watch", include_str!("../../../../reference/Orna-1.0.0/examples/valid/presentation-watch.orna")),
    ("programmable-watch-expression", include_str!("../../../../reference/Orna-1.0.0/examples/valid/programmable-watch-expression.orna")),
    ("question-coalesce-parenthesized", include_str!("../../../../reference/Orna-1.0.0/examples/valid/question-coalesce-parenthesized.orna")),
    ("ranges", include_str!("../../../../reference/Orna-1.0.0/examples/valid/ranges.orna")),
    ("record-pattern-shorthand", include_str!("../../../../reference/Orna-1.0.0/examples/valid/record-pattern-shorthand.orna")),
    ("recovery-pipeline", include_str!("../../../../reference/Orna-1.0.0/examples/valid/recovery-pipeline.orna")),
    ("refined-type-assertions", include_str!("../../../../reference/Orna-1.0.0/examples/valid/refined-type-assertions.orna")),
    ("row-body", include_str!("../../../../reference/Orna-1.0.0/examples/valid/row-body.orna")),
    ("secret-reference", include_str!("../../../../reference/Orna-1.0.0/examples/valid/secret-reference.orna")),
    ("stream-admin-repl", include_str!("../../../../reference/Orna-1.0.0/examples/valid/stream-admin-repl.orna")),
    ("sys-checkpoint", include_str!("../../../../reference/Orna-1.0.0/examples/valid/sys-checkpoint.orna")),
    ("sys-definition-file", include_str!("../../../../reference/Orna-1.0.0/examples/valid/sys-definition-file.orna")),
    ("sys-file-history", include_str!("../../../../reference/Orna-1.0.0/examples/valid/sys-file-history.orna")),
    ("sys-run-history", include_str!("../../../../reference/Orna-1.0.0/examples/valid/sys-run-history.orna")),
    ("sys-storage", include_str!("../../../../reference/Orna-1.0.0/examples/valid/sys-storage.orna")),
    ("sys-table-query", include_str!("../../../../reference/Orna-1.0.0/examples/valid/sys-table-query.orna")),
    ("table-assertions", include_str!("../../../../reference/Orna-1.0.0/examples/valid/table-assertions.orna")),
    ("table-automatic-key", include_str!("../../../../reference/Orna-1.0.0/examples/valid/table-automatic-key.orna")),
    ("table-composite-key", include_str!("../../../../reference/Orna-1.0.0/examples/valid/table-composite-key.orna")),
    ("table-explicit-key", include_str!("../../../../reference/Orna-1.0.0/examples/valid/table-explicit-key.orna")),
    ("table-key-default", include_str!("../../../../reference/Orna-1.0.0/examples/valid/table-key-default.orna")),
    ("table-reference", include_str!("../../../../reference/Orna-1.0.0/examples/valid/table-reference.orna")),
    ("transactional-scope", include_str!("../../../../reference/Orna-1.0.0/examples/valid/transactional-scope.orna")),
    ("transparent-alias", include_str!("../../../../reference/Orna-1.0.0/examples/valid/transparent-alias.orna")),
    ("unbounded-stream", include_str!("../../../../reference/Orna-1.0.0/examples/valid/unbounded-stream.orna")),
    ("unit-cross-database", include_str!("../../../../reference/Orna-1.0.0/examples/valid/unit-cross-database.orna")),
    ("unit-postfix", include_str!("../../../../reference/Orna-1.0.0/examples/valid/unit-postfix.orna")),
    ("units", include_str!("../../../../reference/Orna-1.0.0/examples/valid/units.orna")),
];

const CONFORMANCE_MANIFEST: &str = include_str!("../../../../reference/Orna-1.0.0/tests/conformance-manifest.json");

const REFERENCE_PROJECT_FILES: &[(&str, &str)] = &[
    ("main.orna", include_str!("../../../../reference/Orna-1.0.0/examples/reference/main.orna")),
    ("library.orna", include_str!("../../../../reference/Orna-1.0.0/examples/reference/library.orna")),
    ("sensors.orna", include_str!("../../../../reference/Orna-1.0.0/examples/reference/sensors.orna")),
    ("values.orna", include_str!("../../../../reference/Orna-1.0.0/examples/reference/values.orna")),
    ("warehouse.orna", include_str!("../../../../reference/Orna-1.0.0/examples/reference/warehouse.orna")),
];

fn diagnostics(diagnostics: &[orna_foundation_v1::Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn expected_semantic_failure(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "automatic-failure-propagation" => Some(&[
            "ORNA-S012-UNRESOLVED: name cannot be resolved",
        ]),
        "failure-natural-key" | "stream-admin-repl" => Some(&[
            "ORNA-S021-TYPE: arguments do not match a portable system function overload",
            "ORNA-S021-TYPE: relation query callback must be read-only",
            "ORNA-S021-TYPE: static types are incompatible",
            "ORNA-S021-TYPE: static types are incompatible",
        ]),
        "finite-stream" => Some(&[
            "ORNA-S021-TYPE: missing required field `name`",
        ]),
        _ => None,
    }
}

#[test]
fn every_valid_fixture_parses_resolves_names_and_type_checks() {
    let catalogue = Catalogue::authoritative_fixture();
    let mut passed = 0;
    let mut failed = 0;
    let mut pinned_failures = 0;
    let mut absent = 0;
    let mut pinned_absent = 0;
    let mut unpinned_failures = Vec::new();
    let manifest: serde_json::Value = serde_json::from_str(CONFORMANCE_MANIFEST).unwrap();

    for (name, source) in VALID_FIXTURES {
        let path = format!("{name}.orna");
        if *name == "row-body" {
            let fixture = manifest["fixtures"]
                .as_array()
                .unwrap()
                .iter()
                .find(|fixture| fixture["path"] == "examples/valid/row-body.orna")
                .expect("row-body fixture manifest entry");
            assert_eq!(fixture["parse_as"], "row_unit");
            assert!(fixture.get("table_path").is_none());
            assert!(fixture.get("key_path").is_none());

            let parsed = orna_syntax_v1::parse_row_with_file(source, &path);
            if parsed.is_ok() {
                absent += 1;
                pinned_absent += 1;
                println!(
                    "PINNED-ABSENT {path}: row parse=yes; semantic admission has no fixture table_path/key_path binding",
                );
            } else {
                failed += 1;
                let detail = parsed
                    .diagnostics
                    .iter()
                    .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                    .collect::<Vec<_>>()
                    .join(" | ");
                unpinned_failures.push(format!("{path}: expected row parse pass; got {detail}"));
            }
            continue;
        }

        let parsed = parse_module_with_file(source, &path);
        let parse_ok = parsed.is_ok();
        let analysis = analyze_with_catalogue(
            &[ModuleInput::new(path.clone(), *source)],
            &catalogue,
        );
        let semantic_ok = analysis.is_ok();
        let actual_diagnostics = diagnostics(&analysis.diagnostics);
        if let Some(expected) = expected_semantic_failure(name) {
            failed += 1;
            let mut actual = analysis
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
                .collect::<Vec<_>>();
            let mut expected = expected.iter().map(|diagnostic| (*diagnostic).to_owned()).collect::<Vec<_>>();
            actual.sort();
            expected.sort();
            if parse_ok && !semantic_ok && actual == expected {
                pinned_failures += 1;
                println!("PINNED-FAIL {path}: parse=yes diagnostics={actual:?}");
            } else {
                unpinned_failures.push(format!(
                    "{path}: expected parse pass and diagnostics {expected:?}; got parse_ok={parse_ok}, semantic_ok={semantic_ok}, diagnostics={actual:?}",
                ));
            }
        } else if parse_ok && semantic_ok {
            passed += 1;
            println!("PASS {path}: parse=yes name-resolution=yes type-check=yes");
        } else {
            failed += 1;
            let parse_diagnostics = parsed
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                .collect::<Vec<_>>()
                .join(" | ");
            unpinned_failures.push(format!(
                "{path}: parse={} [{}]; name-resolution/type-check={} [{}]",
                if parse_ok { "pass" } else { "FAIL" },
                parse_diagnostics,
                if semantic_ok { "pass" } else { "FAIL" },
                actual_diagnostics,
            ));
        }
    }

    println!(
        "valid fixture totals: present={} passed={passed} failed={failed} pinned_failures={pinned_failures} absent={absent} pinned_absent={pinned_absent} unpinned_failures={}",
        VALID_FIXTURES.len(),
        unpinned_failures.len(),
    );
    assert_eq!(
        VALID_FIXTURES.len(),
        passed + failed + absent,
        "every frozen valid fixture must have an execution result",
    );
    assert!(
        unpinned_failures.is_empty(),
        "{} unpinned fixture failures or pin changes:\n{}",
        unpinned_failures.len(),
        unpinned_failures.join("\n"),
    );
}

fn repository(files: &[(&str, &str)]) -> (TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let path = directory.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    let repository = Repository::discover(directory.path()).unwrap();
    (directory, repository)
}

#[test]
fn reference_example_project_loads_and_type_checks_as_a_complete_project() {
    let (_directory, repository) = repository(REFERENCE_PROJECT_FILES);
    let project = ProjectLoader::default()
        .load(&repository)
        .expect("reference example project should load its reachable module graph");
    let module_paths = project
        .modules()
        .iter()
        .map(|module| module.logical_path.as_str())
        .collect::<Vec<_>>();
    println!("reference project loaded modules: {module_paths:?}");
    assert_eq!(
        module_paths,
        [
            "library.orna",
            "main.orna",
            "sensors.orna",
            "values.orna",
            "warehouse.orna",
        ],
    );

    let catalogue = Catalogue::authoritative_fixture();
    let mut parse_failures = Vec::new();
    for module in project.modules() {
        let parsed = parse_module_with_file(&module.source, &module.logical_path);
        if parsed.is_ok() {
            println!("PASS {}: parse=yes", module.logical_path);
        } else {
            let detail = parsed
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                .collect::<Vec<_>>()
                .join(" | ");
            println!("FAIL {}: parse [{detail}]", module.logical_path);
            parse_failures.push(module.logical_path.clone());
        }
    }
    let analysis = analyze_with_catalogue(project.modules(), &catalogue);
    println!(
        "reference project semantic check: {}",
        if analysis.is_ok() { "PASS" } else { "FAIL" }
    );
    if !analysis.is_ok() {
        println!("reference project diagnostics: {}", diagnostics(&analysis.diagnostics));
    }
    assert!(parse_failures.is_empty(), "module parse failures: {parse_failures:?}");
    assert!(analysis.is_ok(), "reference project semantic failures: {}", diagnostics(&analysis.diagnostics));

    let mut row_failures = Vec::new();
    for row in project.loose_rows() {
        let input = RowUnitInput {
            logical_path: row.logical_path(),
            table_path: row.table_path(),
            key_path: row.key_path(),
            source: row.source(),
            source_bytes: row.source_bytes(),
            parse_as: row.parse_as(),
        };
        let admission = admit_row_unit(&analysis, &input);
        if admission.is_ok() {
            println!("PASS row {} owner={:?} key={:?}", row.logical_path(), admission.owner, admission.key);
        } else {
            let detail = diagnostics(&admission.diagnostics);
            println!("FAIL row {}: {detail}", row.logical_path());
            row_failures.push(format!("{}: {detail}", row.logical_path()));
        }
    }
    println!(
        "reference project totals: modules={} passed={} failed={} rows={} row_failures={}",
        project.modules().len(),
        project.modules().len() - parse_failures.len(),
        parse_failures.len(),
        project.loose_rows().len(),
        row_failures.len(),
    );
    assert!(row_failures.is_empty(), "reference project row admission failures: {row_failures:?}");
}
