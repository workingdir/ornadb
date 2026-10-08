//! Builds one offline-copy row from a captured blob reference. The metadata
//! comes from the captured reference; the payload is read from the captured
//! file and must match the recorded length and digest.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineRow;
use orna_value_v1::decode_rov3_blob_metadata;
use sha2::{Digest, Sha256};

/// Pairs one captured reference with the file it captured.
pub fn row_from_capture(key: &str, encoded: &[u8], root: &Path, file: &str) -> OfflineRow {
    let metadata = decode_rov3_blob_metadata(encoded).expect("captured reference decodes");
    let payload = std::fs::read(root.join(file)).expect("captured file is readable");
    assert_eq!(
        payload.len() as u64,
        metadata.length(),
        "captured length matches the file"
    );
    let sha256: [u8; 32] = Sha256::digest(&payload).into();
    assert_eq!(
        metadata.sha256(),
        sha256,
        "captured digest matches the file"
    );
    OfflineRow {
        key: key.as_bytes().to_vec(),
        media_type: metadata.media_type().to_owned(),
        suffix: metadata.suffix().map(str::to_owned),
        length: metadata.length(),
        sha256,
        payload: Some(payload),
    }
}
