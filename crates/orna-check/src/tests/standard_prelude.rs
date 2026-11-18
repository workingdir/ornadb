use crate::*;

#[test]
fn standard_prelude_exports_are_derived_from_source_without_root_intrinsics() {
    let prelude = "pub fn count<T>(values: [T]): Int = 0; pub fn first<T>(values: [T]): T? = values.first(); pub fn trim(value: Str): Str = value;";
    let exports = standard_prelude_exports(prelude).unwrap();
    assert_eq!(
        exports.iter().map(String::as_str).collect::<Vec<_>>(),
        ["trim"]
    );
    let sources = [(STANDARD_PRELUDE_PATH.to_owned(), prelude.to_owned())];
    let profile = StandardDependencyProfile::from_sources("std-snapshot-3", sources).unwrap();
    assert_eq!(
        profile.module_prelude_exports()[STANDARD_PRELUDE_PATH]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["trim"]
    );
    assert_eq!(
        StandardDependencyProfile::from_sources(
            "std-snapshot-3",
            [(STANDARD_PRELUDE_PATH.to_owned(), "pub fn (".to_owned())],
        )
        .err(),
        Some(StandardProfileError::InvalidSource)
    );
}
