use orna_compiler::check;
use orna_core::{
    CatalogueRevisionId,
    catalogue::CatalogueSnapshot,
    source::{SourceBundle, SourceUnit},
};
use proptest::prelude::*;

const MAX_SOURCE_CHARS: usize = 256;
const MAX_SOURCE_UNITS: usize = 4;

fn arbitrary_source() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..=MAX_SOURCE_CHARS)
        .prop_map(|characters| characters.into_iter().collect())
}

fn arbitrary_bundle() -> impl Strategy<Value = SourceBundle> {
    prop::collection::vec(arbitrary_source(), 0..=MAX_SOURCE_UNITS).prop_map(|sources| {
        let units = sources
            .into_iter()
            .enumerate()
            .map(|(index, content)| SourceUnit::new(format!("unit-{index}.orna"), content));
        SourceBundle::new(units).expect("generated logical paths are unique and non-empty")
    })
}

fn empty_catalogue() -> CatalogueSnapshot {
    CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([7; 16]), vec![], vec![])
        .expect("an empty catalogue is a valid resolver context")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn arbitrary_bounded_bundles_do_not_panic_and_bound_resolver_diagnostics(
        bundle in arbitrary_bundle(),
    ) {
        let base = empty_catalogue();
        let report = check(&bundle, &base);

        for unit in report.parse_report().units() {
            prop_assert_eq!(unit.syntax_text(), unit.source_text());
        }

        for diagnostic in report.diagnostics() {
            let path = diagnostic.location().logical_path();
            let source = bundle
                .units()
                .iter()
                .find(|unit| unit.logical_path() == path)
                .map(SourceUnit::content);
            prop_assert!(source.is_some());
            let source = source.expect("diagnostic path must identify a submitted source unit");
            let span = diagnostic.location().span();
            prop_assert!(span.start() <= span.end());
            prop_assert!(span.end() <= source.len());
            prop_assert!(source.is_char_boundary(span.start()));
            prop_assert!(source.is_char_boundary(span.end()));
        }

        // Warning-only reports, such as unreachable-code diagnostics, may still have a checked bundle.
        prop_assert_eq!(report.checked_bundle().is_none(), report.has_errors());
    }
}
