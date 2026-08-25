use orna_syntax::parse;
use proptest::prelude::*;

const MAX_SOURCE_CHARS: usize = 256;

fn arbitrary_source() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..=MAX_SOURCE_CHARS)
        .prop_map(|characters| characters.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn arbitrary_bounded_utf8_sources_are_lossless_and_diagnostics_stay_in_bounds(
        source in arbitrary_source(),
    ) {
        let parsed = parse(&source);

        prop_assert_eq!(parsed.syntax().text(), source.as_str());
        for diagnostic in parsed.diagnostics() {
            prop_assert!(diagnostic.span.start <= diagnostic.span.end);
            prop_assert!(diagnostic.span.end <= source.len());
            prop_assert!(source.is_char_boundary(diagnostic.span.start));
            prop_assert!(source.is_char_boundary(diagnostic.span.end));
        }
    }
}
