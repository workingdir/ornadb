//! Evaluates the committed `offline-edit-image.orna` capture into an offline
//! row, so an import-edit cycle can start from the real `.orna` source.

use std::path::Path;

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_repository_v1::offline_copy::OfflineRow;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};

use super::format3::empty_format3_repository;
use super::offline_roundtrip::row_from_capture;

const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";

/// Captures `pixel.png` from `media` through the fixture's `.orna` source and
/// returns it as an offline row keyed `song`.
pub fn edit_capture_row(fixture: &Path, media: &Path) -> OfflineRow {
    let (_directory, repository, relation_id) = empty_format3_repository();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let source = std::fs::read_to_string(fixture).unwrap();
    let root = format!("{:?}", media.to_string_lossy().as_ref());
    let expression = source.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root);
    let value = evaluate_expression_ovb2_with_effects(
        &expression,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    )
    .unwrap();
    let binding = bindings.accept_captured_blob_for_row(&value).unwrap();
    row_from_capture("song", binding.encoded_value(), media, "pixel.png")
}
