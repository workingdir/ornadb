//! Compile-only migration evidence: existing isolated foundation crates can
//! carry the shared types without this crate defining a second harness,
//! parser, or Git representation.

use orna_conformance_v1::StageOutcome;
use orna_foundation_v1::{CanonicalSnapshot, Diagnostic, FileRef, OvbRaw, RowRef, SourceSpan};
use orna_repository_v1::Repository;
use orna_syntax_v1::parse_expression_with_file;

fn snapshot() -> CanonicalSnapshot {
    CanonicalSnapshot::Commit {
        database: [1; 16],
        algorithm: orna_foundation_v1::GitHash::Sha256,
        oid: vec![2; 32],
    }
}

fn file() -> FileRef {
    FileRef::from_row_ref(
        RowRef::new([1; 16], [2; 16], OvbRaw::Text("file".into()), snapshot()).unwrap(),
    )
}

fn harness_can_carry_shared_diagnostic(_: StageOutcome<Diagnostic>) {}
fn repository_can_be_adapted_later(_: Option<&Repository>) {}

#[test]
fn harness_syntax_and_repository_compile_against_one_foundation_abi() {
    let parsed = parse_expression_with_file("alpha", "src/main.orna");
    let syntax_span = parsed.value.span();
    let shared =
        SourceSpan::from_utf8_offsets(file(), "alpha", syntax_span.start, syntax_span.end).unwrap();
    assert_eq!(shared.start_byte, 0.into());
    harness_can_carry_shared_diagnostic(StageOutcome::Skipped {
        reason: "fixture".into(),
    });
    repository_can_be_adapted_later(None);
}
