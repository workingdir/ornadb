//! Gate E media reader: a 64 MiB WAV Blob served through the Linux FUSE mount is read the way a
//! media reader reads it. Sequential reads, boundary and random ranges, seeks at and past EOF,
//! readahead hints, concurrent positional readers and a real mpv playback must each return the
//! source bytes exactly. Stat and directory listing must not fetch blob content (ORNA-VFS-018).

#![cfg(target_os = "linux")]

use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{FileExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::Repository;
use orna_runtime_v1::RuntimeState;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use rustix::fs::{Advice, fadvise};
use tempfile::TempDir;

#[path = "support/editor_mount.rs"]
#[allow(dead_code, unused_imports)]
mod editor_mount;
#[path = "support/format3.rs"]
#[allow(dead_code, unused_imports)]
mod format3;
#[path = "support/media_commit.rs"]
#[allow(dead_code, unused_imports)]
mod media_commit;

use editor_mount::MountedProcess;

const MIB: usize = 1 << 20;
const BLOB_LEN: usize = 64 * MIB;
const CONCURRENT_LEN: usize = 8 * MIB;
const MEDIA_NAME: &str = "content.wav";
const SAMPLE_RATE: u32 = 16_000;
const MPV_START_SECONDS: usize = 1_000;

/// The 64 MiB fixture is a real WAV file: a 44-byte RIFF header over varying PCM samples.
fn wav_fixture(total_len: usize) -> Vec<u8> {
    let data = (0..total_len - 44)
        .map(|index| ((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8)
        .collect::<Vec<u8>>();
    wav_with_samples(&data)
}

/// Canonical 44-byte RIFF/WAVE header over 16-bit mono PCM `samples`.
fn wav_with_samples(samples: &[u8]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len()).unwrap();
    let block_align: u16 = 2;
    let mut wav = Vec::with_capacity(44 + samples.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * u32::from(block_align)).to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(samples);
    wav
}

fn next_entropy(entropy: u64) -> u64 {
    entropy
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
}

/// A repository whose `Music/london` row holds `original` as its `content` Blob.
struct Captured {
    repository_directory: TempDir,
    _repository: Repository,
    media_root: TempDir,
}

async fn capture_wav(original: &[u8], capture_limit: usize) -> Captured {
    let media_root = TempDir::new().unwrap();
    std::fs::write(media_root.path().join(MEDIA_NAME), original).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"{MEDIA_NAME}\", {capture_limit}), \"audio/wav\")"
    );
    let row_source = format!("{{\n    content: {capture},\n}}\n");
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(source, "Music");
    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(capture_limit, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let blob_captures = [("content", capture.as_str())];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: &row_source,
        ordinal: 0x95,
        insert_only: true,
    }];
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &capture_rows,
    )
    .await;
    drop(state);
    Captured {
        repository_directory,
        _repository: repository,
        media_root,
    }
}

/// Starts `mount --cwd` over the captured repository and waits until the captured Blob is served.
fn mount_blob(captured: &Captured, mount_parent: &Path) -> (MountedProcess, PathBuf) {
    let mountpoint = mount_parent.join("library");
    let mut mounted =
        editor_mount::spawn_mount_process(captured.repository_directory.path(), &mountpoint);
    let blob = mountpoint.join("Music/london").join(MEDIA_NAME);
    mounted.await_path(&blob);
    (mounted, blob)
}

/// Runs `orna unmount` and requires the mount process to exit cleanly.
fn unmount(captured: &Captured, mounted: &mut MountedProcess) {
    editor_mount::run_orna_unmount(captured.repository_directory.path(), mounted.mountpoint());
    let status = mounted
        .wait_for_exit(Duration::from_secs(60))
        .expect("mount exits after unmount");
    assert!(
        status.success(),
        "mount exits cleanly after unmount; status={status}; stderr={}",
        mounted.stderr_text()
    );
}

fn assert_bytes_eq(actual: &[u8], expected: &[u8], context: &str) {
    assert_eq!(actual.len(), expected.len(), "{context}: length");
    if let Some(index) = actual.iter().zip(expected).position(|(a, b)| a != b) {
        panic!(
            "{context}: byte {index} is {:#04x}, source has {:#04x}",
            actual[index], expected[index]
        );
    }
}

/// Seeks to `offset`, reads exactly `len` bytes and compares them with the source.
fn assert_range(file: &mut File, original: &[u8], offset: usize, len: usize) {
    file.seek(SeekFrom::Start(offset as u64)).unwrap();
    let mut range = vec![0u8; len];
    file.read_exact(&mut range).unwrap();
    assert_bytes_eq(
        &range,
        &original[offset..offset + len],
        &format!("range at {offset} length {len}"),
    );
}

/// Peak resident set size of `pid` in KiB, from its `VmHWM` status line.
fn peak_rss_kib(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|kib| kib.parse().ok())
        .expect("VmHWM in /proc status")
}

/// Plays `blob` with mpv when it is installed. `ORNA_REQUIRE_MEDIA_PLAYER=1` makes a missing mpv a failure.
fn play_with_mpv(blob: &Path) {
    let required =
        std::env::var_os("ORNA_REQUIRE_MEDIA_PLAYER").as_deref() == Some(OsStr::new("1"));
    let installed = Command::new("mpv")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !installed {
        assert!(
            !required,
            "ORNA_REQUIRE_MEDIA_PLAYER=1 but mpv is not installed"
        );
        eprintln!("SKIP: mpv is not installed; the decoder-pattern reads are the media leg");
        return;
    }
    let start = format!("--start={MPV_START_SECONDS}");
    let end = format!("--end={}", MPV_START_SECONDS + 2);
    let output = Command::new("mpv")
        .args([
            "--no-config",
            "--no-video",
            "--ao=null",
            "--no-terminal",
            "--msg-level=all=error",
            start.as_str(),
            end.as_str(),
        ])
        .arg(blob)
        .output()
        .expect("run mpv");
    assert!(
        output.status.success(),
        "mpv plays the mounted WAV from {MPV_START_SECONDS}s; status={:?}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
/// ORNA-VFS-018 and ACCEPTANCE Gate E: a 64 MiB WAV Blob is served through a real FUSE mount with
/// byte-exact sequential, boundary, EOF, random, readahead-hinted and mpv reads.
async fn cwd_mount_64_mib_wav_blob_serves_media_reader_patterns_byte_exact() {
    if !editor_mount::require_fuse_or_skip() {
        return;
    }
    let original = wav_fixture(BLOB_LEN);
    let captured = capture_wav(&original, BLOB_LEN + MIB).await;
    let mount_parent = TempDir::new().unwrap();
    let (mut mounted, blob) = mount_blob(&captured, mount_parent.path());

    // The row carries the size, so stat answers without a payload read.
    assert_eq!(std::fs::metadata(&blob).unwrap().len(), BLOB_LEN as u64);

    let mut file = File::open(&blob).unwrap();

    // Sequential 1 MiB reads cover the whole Blob, then EOF.
    let mut chunk = vec![0u8; MIB];
    for index in 0..BLOB_LEN / MIB {
        file.read_exact(&mut chunk).unwrap();
        assert_bytes_eq(
            &chunk,
            &original[index * MIB..(index + 1) * MIB],
            &format!("sequential chunk {index}"),
        );
    }
    assert_eq!(
        file.read(&mut chunk).unwrap(),
        0,
        "EOF after the last chunk"
    );

    // Offsets on page, chunk and half-way boundaries; reads clipped at the end of the Blob.
    let boundaries = [
        0,
        1,
        4095,
        4096,
        4097,
        65_535,
        65_536,
        MIB - 1,
        MIB,
        MIB + 1,
        BLOB_LEN / 2 - 1,
        BLOB_LEN / 2,
        BLOB_LEN - 4097,
        BLOB_LEN - 4096,
        BLOB_LEN - 1,
    ];
    for offset in boundaries {
        for len in [1, 2, 4096, 4097, 65_536] {
            assert_range(&mut file, &original, offset, len.min(BLOB_LEN - offset));
        }
    }

    // Seeks at or past EOF read zero bytes; a seek from the end returns the exact tail.
    file.seek(SeekFrom::Start(BLOB_LEN as u64)).unwrap();
    assert_eq!(file.read(&mut chunk).unwrap(), 0, "read at EOF");
    file.seek(SeekFrom::Start(BLOB_LEN as u64 + 4096)).unwrap();
    assert_eq!(file.read(&mut chunk).unwrap(), 0, "read past EOF");
    file.seek(SeekFrom::End(-100)).unwrap();
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).unwrap();
    assert_bytes_eq(&tail, &original[BLOB_LEN - 100..], "tail read to EOF");

    // Deterministic random ranges of up to 1 MiB, the access pattern of a seeking decoder.
    let mut entropy = 0x5eed_0064_u64;
    for _ in 0..256 {
        entropy = next_entropy(entropy);
        let len = 1 + ((entropy >> 40) as usize % MIB);
        entropy = next_entropy(entropy);
        let offset = (entropy as usize) % (BLOB_LEN - len);
        assert_range(&mut file, &original, offset, len);
    }

    // Readahead hints a media stack issues; the bytes must not change under them.
    for advice in [
        Advice::Normal,
        Advice::Sequential,
        Advice::Random,
        Advice::WillNeed,
    ] {
        fadvise(&file, 0, BLOB_LEN as u64, advice).unwrap();
        assert_range(&mut file, &original, 12_345_678, 65_536);
    }

    // The mount daemon keeps no whole-Blob copy: its peak resident set stays well below the Blob.
    let peak_kib = peak_rss_kib(mounted.pid());
    eprintln!("mount daemon peak RSS after 64 MiB of reads: {peak_kib} KiB");
    assert!(
        peak_kib < (BLOB_LEN / 1024) as u64,
        "mount daemon peak RSS {peak_kib} KiB reaches the 64 MiB Blob size"
    );

    drop(file);
    play_with_mpv(&blob);
    unmount(&captured, &mut mounted);
}

#[tokio::test]
/// ORNA-VFS-018: concurrent positional readers on one mounted Blob each receive exact ranges.
async fn cwd_mount_concurrent_readers_get_exact_ranges_from_one_blob() {
    if !editor_mount::require_fuse_or_skip() {
        return;
    }
    let original = wav_fixture(CONCURRENT_LEN);
    let captured = capture_wav(&original, CONCURRENT_LEN + MIB).await;
    let mount_parent = TempDir::new().unwrap();
    let (mut mounted, blob) = mount_blob(&captured, mount_parent.path());

    std::thread::scope(|scope| {
        for reader in 0..4u64 {
            let original = original.as_slice();
            let blob = blob.as_path();
            scope.spawn(move || {
                let file = File::open(blob).unwrap();
                let mut entropy = 0xc0ff_ee00_u64 ^ reader;
                for _ in 0..128 {
                    entropy = next_entropy(entropy);
                    let len = 1 + ((entropy >> 40) as usize % (256 * 1024));
                    entropy = next_entropy(entropy);
                    let offset = (entropy as usize) % (original.len() - len);
                    let mut range = vec![0u8; len];
                    file.read_exact_at(&mut range, offset as u64).unwrap();
                    assert_bytes_eq(
                        &range,
                        &original[offset..offset + len],
                        &format!("reader {reader} range at {offset} length {len}"),
                    );
                }
            });
        }
    });

    unmount(&captured, &mut mounted);
}

#[tokio::test]
/// ORNA-VFS-018: stat and directory listing answer from row metadata; only a read fetches the
/// payload, so a corrupt loose chunk surfaces as EIO on read and nowhere earlier.
async fn cwd_mount_stat_and_readdir_do_not_fetch_corrupt_blob_payload() {
    if !editor_mount::require_fuse_or_skip() {
        return;
    }
    let original = wav_fixture(8 * 1024);
    let captured = capture_wav(&original, 1 << 20).await;

    let source_file = captured.media_root.path().join(MEDIA_NAME);
    let repository = captured.repository_directory.path();
    let chunk_oid = git_output(repository, &["hash-object", source_file.to_str().unwrap()]);
    let objects = repository.join(git_output(
        repository,
        &["rev-parse", "--git-path", "objects"],
    ));
    let chunk_path = objects.join(&chunk_oid[..2]).join(&chunk_oid[2..]);
    assert!(chunk_path.is_file(), "captured chunk is a loose object");
    std::fs::set_permissions(&chunk_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&chunk_path, b"corrupt Git loose object bytes").unwrap();

    let mount_parent = TempDir::new().unwrap();
    let (mut mounted, blob) = mount_blob(&captured, mount_parent.path());

    assert_eq!(
        std::fs::metadata(&blob)
            .expect("stat succeeds over a corrupt payload")
            .len(),
        original.len() as u64
    );
    let names: Vec<String> = std::fs::read_dir(blob.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().any(|name| name == MEDIA_NAME),
        "listing names the Blob: {names:?}"
    );

    let mut file = File::open(&blob).unwrap();
    let mut byte = [0u8; 1];
    let error = file
        .read_exact(&mut byte)
        .expect_err("a corrupt chunk object must not return data");
    assert_eq!(
        error.raw_os_error(),
        Some(5),
        "a corrupt chunk object surfaces as Linux EIO"
    );
    drop(file);

    unmount(&captured, &mut mounted);
}

fn git_output(repository: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repository)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
