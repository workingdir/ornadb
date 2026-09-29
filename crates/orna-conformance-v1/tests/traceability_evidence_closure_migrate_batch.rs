use orna_syntax_v1::{TokenKind, lex, parse_module};
use serde_json::Value;
use std::{fs, path::PathBuf};

const FIXTURE: &str = include_str!("fixtures/traceability-evidence-closure-migrate.orna");

const ROWS: [(&str, &str, usize, &str); 10] = [
    (
        "ORNA-TEST-004",
        "source/32-conformance.md",
        23,
        "status register and trace report remain non-executed",
    ),
    (
        "ORNA-TEST-010",
        "source/32-conformance.md",
        31,
        "evidence obligation remains planned; no implementation pass is inferred",
    ),
    (
        "ORNA-TEST-011",
        "source/32-conformance.md",
        33,
        "fixture parsing is a bounded check, not production conformance evidence",
    ),
    (
        "ORNA-EVIDENCE-001",
        "source/32-conformance.md",
        69,
        "authored plan and implementation execution remain distinct",
    ),
    (
        "ORNA-EVIDENCE-002",
        "source/32-conformance.md",
        71,
        "project completeness is not exercised by this source fixture",
    ),
    (
        "ORNA-CLOSURE-002",
        "source/32-conformance.md",
        76,
        "profile evidence does not authorize semantic changes; no profile matrix is claimed",
    ),
    (
        "ORNA-CLOSURE-003",
        "source/32-conformance.md",
        78,
        "unsupported-feature diagnostic behavior is outside this fixture's observation",
    ),
    (
        "ORNA-CLOSURE-004",
        "source/01-scope.md",
        62,
        "configuration-invariance behavior is not exercised",
    ),
    (
        "ORNA-MIGRATE-003",
        "source/33-diagnostics.md",
        34,
        "no source migration tool exists; token-aware automated correction is unproven",
    ),
    (
        "ORNA-MIGRATE-004",
        "source/33-diagnostics.md",
        36,
        "no source migration tool exists; the fixture only observes the lambda token",
    ),
];

fn reference_root() -> PathBuf {
    std::env::var_os("ORNA_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference/Orna-1.0.0")
        })
}

fn records(path: &str) -> Value {
    serde_json::from_slice(
        &fs::read(reference_root().join(path)).expect("read pinned Orna 1.0.0 reference"),
    )
    .expect("parse pinned reference JSON")
}

#[test]
fn evidence_closure_and_migration_rows_keep_specified_exists_and_passed_distinct() {
    let requirements = records("tests/requirements.json");
    let evidence = records("tests/requirement-evidence.json");
    let requirement_rows = requirements.as_array().expect("requirement array");
    let evidence_rows = evidence["requirements"].as_array().expect("evidence array");

    for (id, source, line, boundary) in ROWS {
        let requirement = requirement_rows
            .iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("{id} is specified in the normative requirement register"));
        assert_eq!(requirement["source"], source, "source anchor for {id}");
        let source_text = fs::read_to_string(reference_root().join(source))
            .unwrap_or_else(|_| panic!("read normative source for {id}"));
        let clause = source_text
            .lines()
            .nth(line - 1)
            .unwrap_or_else(|| panic!("line anchor for {id}"));
        assert!(clause.contains(id), "{id} must occur at {source}:{line}");

        let row = evidence_rows
            .iter()
            .find(|row| row["requirement"] == id)
            .unwrap_or_else(|| panic!("evidence obligation exists for {id}"));
        assert_eq!(row["implementation_result"], "not executed", "{id}: {boundary}");
        assert!(
            row["tests"].as_array().is_some_and(|tests| !tests.is_empty()),
            "{id} has a specified test obligation"
        );
        assert!(
            row["tests"].as_array().unwrap().iter().all(|test| test["status"] == "planned"),
            "this bounded guard test must not rewrite planned evidence for {id} as passed"
        );
    }
}

#[test]
fn lambda_in_case_fixture_parses_and_retains_its_arrow_token() {
    let parsed = parse_module(FIXTURE);
    assert!(parsed.diagnostics.is_empty(), "fixture parse: {:?}", parsed.diagnostics);
    let tokens = lex(FIXTURE).expect("fixture lexes");
    let arrows = tokens
        .iter()
        .filter(|token| matches!(&token.kind, TokenKind::Punct(punct) if *punct == "=>"))
        .collect::<Vec<_>>();
    assert_eq!(arrows.len(), 1, "anonymous-function => remains a single token");
}
