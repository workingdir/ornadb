//! Regenerates a committed `.orna` import source for one media root: the
//! `__MEDIA_ROOT__` placeholder becomes the root as a quoted string literal, and
//! trailing whitespace is trimmed, as the import tests expect.

use std::path::Path;

const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";

/// Returns the fixture's source with its placeholder replaced by `root`.
pub fn regenerate_import_source(fixture: &Path, root: &str) -> String {
    let source = std::fs::read_to_string(fixture).expect("fixture is readable");
    let root = format!("{root:?}");
    source.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root)
}
