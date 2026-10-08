//! Zero-drift double regeneration: two regeneration passes over the committed
//! `import-image.orna` fixture for the same root match each other and the
//! recorded expected source, and the committed fixture bytes never change.

use std::path::Path;

#[path = "support/regeneration.rs"]
mod regeneration;
use regeneration::regenerate_import_source;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const SCRATCH_ROOT: &str = "/scratch-root";

#[test]
fn two_regeneration_passes_drift_by_zero_bytes() {
    let fixtures = Path::new(MEDIA_FIXTURES);
    let fixture = fixtures.join("import-image.orna");
    let committed_before = std::fs::read(&fixture).unwrap();
    let expected =
        std::fs::read_to_string(fixtures.join("import-image.regenerated-scratch-root.orna"))
            .unwrap();

    let first = regenerate_import_source(&fixture, SCRATCH_ROOT);
    let second = regenerate_import_source(&fixture, SCRATCH_ROOT);

    assert_eq!(
        first, expected,
        "the first pass matches the recorded source"
    );
    assert_eq!(second, first, "the second pass drifts by zero bytes");
    assert_eq!(
        std::fs::read(&fixture).unwrap(),
        committed_before,
        "regeneration leaves the committed fixture unchanged"
    );
}
