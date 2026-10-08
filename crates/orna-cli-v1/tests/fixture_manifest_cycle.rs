//! One regeneration cycle over the committed `import-image.orna` fixture leaves
//! the committed media fixtures byte-identical to their `manifest.sha256`.

use std::path::Path;

use tempfile::TempDir;

#[path = "support/fixture_manifest.rs"]
mod fixture_manifest;
use fixture_manifest::manifest_bytes;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const IMAGE_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";
const FIXTURE_NAMES: [&str; 2] = ["import-image.orna", "pixel.png"];

#[test]
fn one_regeneration_cycle_leaves_the_fixture_manifest_byte_identical() {
    let fixtures = Path::new(MEDIA_FIXTURES);
    let committed = std::fs::read(fixtures.join("manifest.sha256")).unwrap();
    let before = manifest_bytes(fixtures, &FIXTURE_NAMES);
    assert_eq!(
        before, committed,
        "the committed manifest matches the fixtures"
    );

    // One cycle: regenerate the import source for a scratch root and write it
    // out. Regeneration must touch only the scratch copy.
    let scratch = TempDir::new().unwrap();
    let regenerated = regenerate_image_import(scratch.path());
    std::fs::write(scratch.path().join("import-image.orna"), &regenerated).unwrap();
    assert!(!regenerated.contains(MEDIA_ROOT_PLACEHOLDER));

    let after = manifest_bytes(fixtures, &FIXTURE_NAMES);
    assert_eq!(
        after, before,
        "one regeneration cycle leaves the manifest unchanged"
    );
    assert_eq!(after, committed);
}

/// Regenerates the committed import source for one scratch root, with the root
/// written in as a quoted string literal.
fn regenerate_image_import(root: &Path) -> String {
    let fixture = std::fs::read_to_string(IMAGE_IMPORT_FIXTURE).unwrap();
    let root = format!("{:?}", root.to_string_lossy().as_ref());
    fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root)
}
