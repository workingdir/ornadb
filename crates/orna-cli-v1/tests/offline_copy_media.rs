use std::path::Path;

use orna_repository_v1::offline_copy::{
    OfflineCopy, OfflineCopyError, OfflineHistoryEntry, OfflineImportOutput, OfflineRow,
    write_offline_copy,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

/// Builds the row an import would commit for one fixture file. The payload is
/// included so the bundle writer can copy it.
fn committed_row(key: &str, media_type: &str, file: &str) -> OfflineRow {
    let bytes = std::fs::read(Path::new(MEDIA_FIXTURES).join(file)).unwrap();
    OfflineRow {
        key: key.as_bytes().to_vec(),
        media_type: media_type.to_owned(),
        suffix: None,
        length: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).into(),
        payload: Some(bytes),
    }
}

#[test]
fn offline_copy_answers_metadata_queries_without_media_files() {
    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    let rows = [
        committed_row("image", "image/png", "pixel.png"),
        committed_row("song", "audio/wav", "tone.wav"),
    ];
    let history = [OfflineHistoryEntry {
        sequence: 1,
        commit: [0x5a; 32],
    }];
    write_offline_copy(&bundle, &rows, &history).unwrap();

    // The copy is self-contained: drop every media file and query again.
    std::fs::remove_dir_all(bundle.join("media")).unwrap();
    let copy = OfflineCopy::open(&bundle).unwrap();

    let listing = copy.rows();
    assert_eq!(listing.len(), 2);
    let song = copy.metadata(b"song").expect("song row is in the copy");
    assert_eq!(song.media_type, "audio/wav");
    assert_eq!(song.length, 4044);
    assert_eq!(song.sha256, rows[1].sha256);
    assert!(song.has_payload);
    let image = copy.metadata(b"image").expect("image row is in the copy");
    assert_eq!(image.media_type, "image/png");
    assert_eq!(image.length, 73);
    assert_eq!(copy.history(), &history);
    assert!(copy.metadata(b"missing").is_none());

    // Hydration is the only path that reads media, and it now fails cleanly.
    assert!(matches!(
        copy.hydrate(b"song"),
        Err(OfflineCopyError::Io(_))
    ));
}

#[test]
fn offline_copy_hydrates_verified_payloads_and_rejects_tampering() {
    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    let song = committed_row("song", "audio/wav", "tone.wav");
    let expected = song.payload.clone().unwrap();
    write_offline_copy(&bundle, std::slice::from_ref(&song), &[]).unwrap();

    let copy = OfflineCopy::open(&bundle).unwrap();
    assert_eq!(copy.hydrate(b"song").unwrap(), expected);

    // A payload that does not match its recorded digest is refused at write time.
    let mut tampered = song.clone();
    tampered.payload = Some(vec![0; expected.len()]);
    let refused = write_offline_copy(&directory.path().join("bad"), &[tampered], &[]);
    assert!(matches!(
        refused,
        Err(OfflineCopyError::PayloadMismatch { .. })
    ));

    // A bit flipped on disk is refused at hydration time.
    let media_file = bundle.join("media").join(hex(&song.sha256));
    let mut on_disk = std::fs::read(&media_file).unwrap();
    on_disk[0] ^= 0xff;
    std::fs::write(&media_file, on_disk).unwrap();
    assert!(matches!(
        copy.hydrate(b"song"),
        Err(OfflineCopyError::PayloadMismatch { .. })
    ));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn offline_import_plan_restores_every_verified_row_or_rejects_the_bundle() {
    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    let image = committed_row("image", "image/png", "pixel.png");
    let song = committed_row("song", "audio/wav", "tone.wav");
    let history = [
        OfflineHistoryEntry {
            sequence: 1,
            commit: [0x11; 32],
        },
        OfflineHistoryEntry {
            sequence: 2,
            commit: [0x22; 32],
        },
    ];
    write_offline_copy(&bundle, &[image.clone(), song.clone()], &history).unwrap();

    // The plan carries every payload, verified, in bundle order.
    let plan = OfflineCopy::open(&bundle).unwrap().import_plan().unwrap();
    assert_eq!(plan.history, history);
    assert_eq!(plan.rows.len(), 2);
    assert_eq!(plan.rows[0].key, b"image");
    assert_eq!(plan.rows[0].payload, image.payload.clone().unwrap());
    assert_eq!(plan.rows[1].payload, song.payload.clone().unwrap());

    // A row without a copied payload cannot restore its pin, so the whole
    // bundle is refused.
    let metadata_only = directory.path().join("metadata-only");
    let mut without_payload = song.clone();
    without_payload.payload = None;
    write_offline_copy(&metadata_only, &[without_payload], &[]).unwrap();
    assert!(matches!(
        OfflineCopy::open(&metadata_only).unwrap().import_plan(),
        Err(OfflineCopyError::NoPayload { .. })
    ));

    // Out-of-order history is refused before any row is read.
    let reversed = directory.path().join("reversed");
    write_offline_copy(
        &reversed,
        &[],
        &[
            OfflineHistoryEntry {
                sequence: 2,
                commit: [0x22; 32],
            },
            OfflineHistoryEntry {
                sequence: 1,
                commit: [0x11; 32],
            },
        ],
    )
    .unwrap();
    assert!(matches!(
        OfflineCopy::open(&reversed).unwrap().import_plan(),
        Err(OfflineCopyError::InvalidBundle(_))
    ));

    // A flipped media byte makes the whole import fail.
    let media_file = bundle.join("media").join(hex(&song.sha256));
    let mut on_disk = std::fs::read(&media_file).unwrap();
    on_disk[0] ^= 0xff;
    std::fs::write(&media_file, on_disk).unwrap();
    assert!(matches!(
        OfflineCopy::open(&bundle).unwrap().import_plan(),
        Err(OfflineCopyError::PayloadMismatch { .. })
    ));
}

#[test]
fn offline_bundle_round_trips_rows_media_and_history() {
    let directory = TempDir::new().unwrap();
    let first = directory.path().join("first");
    let rows = [
        committed_row("image", "image/png", "pixel.png"),
        committed_row("song", "audio/wav", "tone.wav"),
    ];
    let history = [
        OfflineHistoryEntry {
            sequence: 1,
            commit: [0x5a; 32],
        },
        OfflineHistoryEntry {
            sequence: 2,
            commit: [0x5b; 32],
        },
    ];
    write_offline_copy(&first, &rows, &history).unwrap();

    // Import the copy back: every row, payload, and history entry must match.
    let imported = OfflineCopy::open(&first).unwrap().import_rows().unwrap();
    assert_eq!(imported, rows);
    let reopened = OfflineCopy::open(&first).unwrap();
    assert_eq!(reopened.history(), &history);

    // Writing the imported rows again yields a byte-identical bundle.
    let second = directory.path().join("second");
    write_offline_copy(&second, &imported, reopened.history()).unwrap();
    assert_eq!(
        std::fs::read(first.join("index.tsv")).unwrap(),
        std::fs::read(second.join("index.tsv")).unwrap()
    );
    for row in &rows {
        let name = hex(&row.sha256);
        assert_eq!(
            std::fs::read(first.join("media").join(&name)).unwrap(),
            std::fs::read(second.join("media").join(&name)).unwrap()
        );
    }
}

#[test]
fn offline_import_dry_run_reports_counts_and_writes_nothing() {
    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    let rows = [
        committed_row("image", "image/png", "pixel.png"),
        committed_row("song", "audio/wav", "tone.wav"),
    ];
    let history = [
        OfflineHistoryEntry {
            sequence: 1,
            commit: [0x31; 32],
        },
        OfflineHistoryEntry {
            sequence: 2,
            commit: [0x32; 32],
        },
    ];
    write_offline_copy(&bundle, &rows, &history).unwrap();
    let index_before = std::fs::read(bundle.join("index.tsv")).unwrap();

    let report = OfflineCopy::open(&bundle)
        .unwrap()
        .dry_run_import()
        .unwrap();
    assert_eq!(report.rows, 2);
    assert_eq!(report.payload_bytes, 73 + 4044);
    assert_eq!(report.history, 2);
    assert_eq!(
        std::fs::read(bundle.join("index.tsv")).unwrap(),
        index_before
    );

    // A flipped media byte fails the dry run as it would fail the import.
    let media_file = bundle.join("media").join(hex(&rows[1].sha256));
    let mut on_disk = std::fs::read(&media_file).unwrap();
    on_disk[0] ^= 0xff;
    std::fs::write(&media_file, on_disk).unwrap();
    assert!(matches!(
        OfflineCopy::open(&bundle).unwrap().dry_run_import(),
        Err(OfflineCopyError::PayloadMismatch { .. })
    ));
}

#[test]
fn offline_import_progress_reports_each_verified_row_in_order() {
    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    let rows = [
        committed_row("image", "image/png", "pixel.png"),
        committed_row("song", "audio/wav", "tone.wav"),
    ];
    write_offline_copy(&bundle, &rows, &[]).unwrap();

    let mut lines = Vec::new();
    OfflineCopy::open(&bundle)
        .unwrap()
        .import_plan_with_progress(|progress| lines.push(progress.to_string()))
        .unwrap();
    assert_eq!(
        lines,
        [
            "verified 1/2 image (73 bytes)",
            "verified 2/2 song (4044 bytes)",
        ]
    );
}

#[test]
fn offline_import_limit_plans_only_the_first_rows_and_keeps_history() {
    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    let rows = [
        committed_row("image", "image/png", "pixel.png"),
        committed_row("song", "audio/wav", "tone.wav"),
    ];
    let history = [OfflineHistoryEntry {
        sequence: 1,
        commit: [0x41; 32],
    }];
    write_offline_copy(&bundle, &rows, &history).unwrap();
    let copy = OfflineCopy::open(&bundle).unwrap();

    let first = copy.import_plan_limited(1).unwrap();
    assert_eq!(first.rows.len(), 1);
    assert_eq!(first.rows[0].key, b"image");
    assert_eq!(first.history, history);

    assert!(copy.import_plan_limited(0).unwrap().rows.is_empty());
    assert_eq!(copy.import_plan_limited(99).unwrap().rows.len(), 2);
}

#[test]
fn offline_import_quiet_writes_nothing_and_verbose_writes_one_line_per_row() {
    let directory = TempDir::new().unwrap();
    let bundle = directory.path().join("bundle");
    let rows = [
        committed_row("image", "image/png", "pixel.png"),
        committed_row("song", "audio/wav", "tone.wav"),
    ];
    write_offline_copy(&bundle, &rows, &[]).unwrap();
    let copy = OfflineCopy::open(&bundle).unwrap();

    let mut quiet = Vec::new();
    let plan = copy
        .import_plan_to_writer(OfflineImportOutput::Quiet, &mut quiet)
        .unwrap();
    assert!(quiet.is_empty());
    assert_eq!(plan.rows.len(), 2);

    let mut verbose = Vec::new();
    copy.import_plan_to_writer(OfflineImportOutput::Verbose, &mut verbose)
        .unwrap();
    assert_eq!(
        String::from_utf8(verbose).unwrap(),
        "verified 1/2 image (73 bytes)\nverified 2/2 song (4044 bytes)\n"
    );
}
