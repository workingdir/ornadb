//! The production `sys.blob.capture_file` binding streams a source larger than
//! the process's memory budget into the repository graph and returns a lazy
//! value (W11-03, W18-04; store-format-3 §5, ORNA-BLOB-007, ORNA-BLOB-014).
//!
//! The default size keeps the run short. Set `ORNA_CAPTURE_RSS_BYTES` to a
//! byte count (for example 10737418240) to run the same check at that size.
#![cfg(target_os = "linux")]

#[path = "support/format3.rs"]
mod format3;

use std::{fs::File, io::Write, path::Path};

use orna_evaluator_v1::{Limits, SysHostBindingRegistry, evaluate_expression_ovb2_with_effects};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::{Blob, ValueFormat};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use format3::empty_format3_repository;

/// Large enough that a capture holding the source in memory cannot pass.
const DEFAULT_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
/// Peak resident memory the whole test process may reach.
const PEAK_RSS_BOUND: u64 = 160 * 1024 * 1024;
/// ORNA-BLOB-011: a read returns at most 8 MiB per call.
const READ_CHUNK: u64 = 8 * 1024 * 1024;

#[test]
fn product_capture_streams_a_source_larger_than_its_memory_budget() {
    let source_bytes = std::env::var("ORNA_CAPTURE_RSS_BYTES")
        .ok()
        .map(|bytes| {
            bytes
                .parse::<u64>()
                .expect("ORNA_CAPTURE_RSS_BYTES is a byte count")
        })
        .unwrap_or(DEFAULT_SOURCE_BYTES);
    assert!(
        source_bytes > PEAK_RSS_BOUND,
        "the source must exceed the memory bound for the check to prove streaming"
    );
    let (directory, repository, relation_id) = empty_format3_repository();
    let media = TempDir::new_in(directory.path()).expect("create the capture root");
    let expected = write_fixture(&media.path().join("large.bin"), source_bytes);

    // The REPL's 64 KiB text bound stays; only the capture quota is raised.
    let mut filesystem = FilesystemProvider::with_limits(65_536, 16)
        .unwrap()
        .with_capture_limit(source_bytes)
        .unwrap();
    filesystem.allow_root(media.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(repository.capture_capability(relation_id).unwrap());
    // The walkthrough's generous bound: the source, not max_bytes, must fit.
    let expression = format!(
        "sys.blob.capture_file({:?}, \"large.bin\", 20000000000)",
        media.path().to_string_lossy().as_ref()
    );
    let value = evaluate_expression_ovb2_with_effects(
        &expression,
        &Default::default(),
        Limits::default(),
        &mut bindings,
    )
    .unwrap_or_else(|error| panic!("capture failed: {}", error.code()));

    assert_eq!(value.format(), ValueFormat::Rov3);
    let blob = value.blob().unwrap();
    assert!(!blob.is_hydrated(), "the captured value holds no payload");
    assert_eq!(blob.length(), source_bytes);
    assert_eq!(blob.content_identity().sha256(), expected);

    // Read the bytes back through the capture context in bounded calls and
    // compare them with the digest taken while the source was written.
    let context = value
        .context()
        .expect("a captured Blob keeps its capture context");
    let reference = context
        .reference(
            blob.content_identity(),
            blob.descriptor_oid()
                .expect("a captured Blob names its descriptor")
                .clone(),
        )
        .unwrap();
    let readable = Blob::from_reference(reference).unwrap();
    let mut digest = Sha256::new();
    let mut offset = 0;
    while offset < source_bytes {
        let length = (source_bytes - offset).min(READ_CHUNK);
        let bytes = readable.read(offset, length).unwrap();
        assert_eq!(bytes.len() as u64, length);
        digest.update(&bytes);
        offset += length;
    }
    assert_eq!(<[u8; 32]>::from(digest.finalize()), expected);
    bindings
        .accept_captured_blob_for_row(&value)
        .expect("the streamed capture is accepted for a row");

    let peak = process_peak_rss_bytes();
    assert!(
        peak < PEAK_RSS_BOUND,
        "capturing {source_bytes} bytes peaked at {peak} resident bytes"
    );
}

/// Writes `length` pseudo-random bytes, so content-defined chunking sees
/// varied data, and returns their SHA-256.
fn write_fixture(path: &Path, length: u64) -> [u8; 32] {
    let mut output = File::create(path).expect("create the capture source");
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut digest = Sha256::new();
    let mut remaining = length;
    while remaining != 0 {
        for word in buffer.chunks_exact_mut(8) {
            // xorshift64*: cheap, deterministic and well mixed.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            word.copy_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
        }
        let count = usize::try_from(remaining.min(buffer.len() as u64)).unwrap();
        output.write_all(&buffer[..count]).unwrap();
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    output.sync_all().unwrap();
    digest.finalize().into()
}

fn process_peak_rss_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read process status");
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("Linux reports VmHWM");
    kib * 1024
}
