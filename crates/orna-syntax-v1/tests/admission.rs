use orna_foundation_v1::{CanonicalSnapshot, FileRef, OvbRaw, RowRef};
use orna_syntax_v1::{
    ParseContext, SourceDocumentId, SyntaxAdmissionError, SyntaxSpan, admit_span,
};

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
        source: "a\nβ".into(),
    }
}
#[test]
fn pinned_span_admits_losslessly_and_ephemeral_is_rejected() {
    let span = SyntaxSpan::new(2, 4).located("src/main.orna", "a\nβ");
    assert_eq!(admit_span(&pinned(), &span).unwrap().start_byte, 2.into());
    let context = ParseContext {
        document: SourceDocumentId::Ephemeral("editor-1".into()),
        snapshot: pinned().snapshot,
        source: "a\nβ".into(),
    };
    assert_eq!(
        admit_span(&context, &span),
        Err(SyntaxAdmissionError::NotPinned)
    );
}
