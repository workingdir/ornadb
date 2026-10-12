use orna_foundation_v1::{CanonicalSnapshot, FileRef, OvbRaw, RowRef};
use orna_syntax_v1::{
    ParseContext, SourceDocumentId, SyntaxAdmissionError, SyntaxSpan, admit_span,
};
use std::panic::{AssertUnwindSafe, catch_unwind};

const UNICODE_SOURCE: &str = include_str!("fixtures/admission-unicode.orna");

fn pinned() -> ParseContext {
    let snapshot = CanonicalSnapshot::Commit {
        database: [1; 16],
        algorithm: orna_foundation_v1::GitHash::Sha256,
        oid: vec![2; 32],
    };
    let file = FileRef::from_row_ref(
        RowRef::new(
            [1; 16],
            [2; 16],
            OvbRaw::Text("file".into()),
            snapshot.clone(),
        )
        .unwrap(),
    );
    ParseContext {
        document: SourceDocumentId::Pinned(file),
        snapshot,
        source: UNICODE_SOURCE.into(),
    }
}
#[test]
fn pinned_span_admits_losslessly_and_ephemeral_is_rejected() {
    let span = SyntaxSpan::new(2, 4).located("src/main.orna", UNICODE_SOURCE);
    assert_eq!(admit_span(&pinned(), &span).unwrap().start_byte, 2.into());
    let context = ParseContext {
        document: SourceDocumentId::Ephemeral("editor-1".into()),
        snapshot: pinned().snapshot,
        source: UNICODE_SOURCE.into(),
    };
    assert_eq!(
        admit_span(&context, &span),
        Err(SyntaxAdmissionError::NotPinned)
    );
}

#[test]
fn locating_a_non_boundary_offset_does_not_panic_before_admission() {
    let unicode_suffix = UNICODE_SOURCE
        .get(2..)
        .expect("the fixture's Unicode identifier starts at byte two");
    let result = catch_unwind(AssertUnwindSafe(|| {
        SyntaxSpan::new(1, 2).located("memory.orna", unicode_suffix)
    }));
    assert!(
        result.is_ok(),
        "span annotation must not panic on UTF-8 offsets"
    );
}
