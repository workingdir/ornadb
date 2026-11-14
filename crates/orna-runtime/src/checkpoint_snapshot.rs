//! Portable stream checkpoint watermark records and their bounded codec.

use crate::error::RuntimeError;
use orna_repository::{CommittedTreeEntryKind, GitCommitRef, Repository};
use orna_stream::Checkpoint as StreamCheckpoint;
use orna_stream_identity::{CheckpointKey, Component, ConsumerIdentity, Position};
use orna_value::CwdCapture;
use std::fmt;
use std::path::Path;

/// The terminal transition that committed one historical stream watermark.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamCheckpointTransition {
    Complete,
    Skip,
}

/// One durable stream checkpoint watermark retained for snapshot selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamCheckpointWatermark {
    pub checkpoint: StreamCheckpoint,
    pub capture: CwdCapture,
    pub transition: StreamCheckpointTransition,
}

pub(crate) const CHECKPOINT_SNAPSHOT_HEADER: &[u8] = b"ORNA-CHECKPOINT-SNAPSHOT/";

const CHECKPOINT_SNAPSHOT_VERSION: u64 = 1;

/// A checkpoint snapshot watermark decoded without local CWD state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortableStreamCheckpointWatermark {
    pub checkpoint: StreamCheckpoint,
    pub transition: StreamCheckpointTransition,
}

/// Failure to encode or decode a tracked checkpoint snapshot watermark.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointSnapshotCodecError {
    InvalidMagic,
    InvalidHeader,
    UnsupportedVersion,
    Truncated,
    TrailingBytes,
    InvalidEncoding,
    InvalidComponent,
    InvalidPartitionMarker,
    InvalidTransition,
    ZeroCheckpointVersion,
    MissingCommittedPosition,
}

impl fmt::Display for CheckpointSnapshotCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidMagic => "invalid checkpoint snapshot magic",
            Self::InvalidHeader => "invalid checkpoint snapshot header",
            Self::UnsupportedVersion => "unsupported checkpoint snapshot version",
            Self::Truncated => "truncated checkpoint snapshot",
            Self::TrailingBytes => "trailing checkpoint snapshot bytes",
            Self::InvalidEncoding => "invalid checkpoint snapshot encoding",
            Self::InvalidComponent => "invalid checkpoint snapshot component",
            Self::InvalidPartitionMarker => "invalid checkpoint snapshot partition marker",
            Self::InvalidTransition => "invalid checkpoint snapshot transition",
            Self::ZeroCheckpointVersion => "checkpoint snapshot watermark version must be positive",
            Self::MissingCommittedPosition => "checkpoint snapshot has no committed position",
        })
    }
}

impl std::error::Error for CheckpointSnapshotCodecError {}

/// Encodes the portable, committed portion of a stream watermark for a
/// versioned `.orna/checkpoints/` snapshot file. The local CWD capture is
/// intentionally excluded.
pub fn encode_checkpoint_snapshot_watermark(
    watermark: &StreamCheckpointWatermark,
) -> Result<Vec<u8>, CheckpointSnapshotCodecError> {
    let checkpoint = &watermark.checkpoint;
    if checkpoint.version == 0 {
        return Err(CheckpointSnapshotCodecError::ZeroCheckpointVersion);
    }
    let committed = checkpoint
        .committed
        .as_ref()
        .ok_or(CheckpointSnapshotCodecError::MissingCommittedPosition)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(CHECKPOINT_SNAPSHOT_HEADER);
    append_decimal_line(&mut bytes, CHECKPOINT_SNAPSHOT_VERSION);

    let key = &checkpoint.key;
    append_component(&mut bytes, &key.consumer.principal)?;
    append_component(&mut bytes, &key.consumer.root)?;
    append_component(&mut bytes, &key.consumer.function)?;
    append_component(&mut bytes, &key.consumer.binding)?;
    append_component(&mut bytes, &key.source_format)?;
    append_component(&mut bytes, &key.source)?;
    append_component(&mut bytes, &key.partition_format)?;
    match &key.partition {
        None => bytes.extend_from_slice(b"0\n"),
        Some(partition) => {
            bytes.extend_from_slice(b"1\n");
            append_component(&mut bytes, partition)?;
        }
    }
    append_component(&mut bytes, &key.position_format)?;
    append_decimal_line(&mut bytes, checkpoint.version);
    bytes.push(match watermark.transition {
        StreamCheckpointTransition::Complete => b'C',
        StreamCheckpointTransition::Skip => b'S',
    });
    bytes.push(b'\n');
    append_component(&mut bytes, &committed.token)?;
    Ok(bytes)
}

/// Decodes one canonical snapshot watermark into a portable checkpoint
/// record. The result contains no local CWD capture or runtime identity.
pub fn decode_checkpoint_snapshot_watermark(
    bytes: &[u8],
) -> Result<PortableStreamCheckpointWatermark, CheckpointSnapshotCodecError> {
    let mut reader = CheckpointSnapshotReader { bytes, cursor: 0 };
    if reader.take(CHECKPOINT_SNAPSHOT_HEADER.len())? != CHECKPOINT_SNAPSHOT_HEADER {
        return Err(CheckpointSnapshotCodecError::InvalidMagic);
    }
    let version = reader.decimal(b'\n').map_err(|error| match error {
        CheckpointSnapshotCodecError::InvalidEncoding => {
            CheckpointSnapshotCodecError::InvalidHeader
        }
        other => other,
    })?;
    if version != CHECKPOINT_SNAPSHOT_VERSION {
        return Err(CheckpointSnapshotCodecError::UnsupportedVersion);
    }

    let consumer = ConsumerIdentity {
        principal: reader.component()?,
        root: reader.component()?,
        function: reader.component()?,
        binding: reader.component()?,
    };
    let source_format = reader.component()?;
    let source = reader.component()?;
    let partition_format = reader.component()?;
    let partition = match reader.byte()? {
        b'0' => {
            reader.expect_newline()?;
            None
        }
        b'1' => {
            reader.expect_newline()?;
            Some(reader.component()?)
        }
        _ => return Err(CheckpointSnapshotCodecError::InvalidPartitionMarker),
    };
    let position_format = reader.component()?;
    let version = reader.decimal(b'\n')?;
    if version == 0 {
        return Err(CheckpointSnapshotCodecError::ZeroCheckpointVersion);
    }
    let transition = match reader.byte()? {
        b'C' => StreamCheckpointTransition::Complete,
        b'S' => StreamCheckpointTransition::Skip,
        _ => return Err(CheckpointSnapshotCodecError::InvalidTransition),
    };
    reader.expect_newline()?;
    if reader.cursor == bytes.len() {
        return Err(CheckpointSnapshotCodecError::MissingCommittedPosition);
    }
    let committed = Position {
        token: reader.component()?,
    };
    if reader.cursor != bytes.len() {
        return Err(CheckpointSnapshotCodecError::TrailingBytes);
    }

    Ok(PortableStreamCheckpointWatermark {
        checkpoint: StreamCheckpoint {
            key: CheckpointKey {
                consumer,
                source_format,
                source,
                partition_format,
                partition,
                position_format,
            },
            version,
            committed: Some(committed),
        },
        transition,
    })
}

const CHECKPOINT_SNAPSHOT_MAX_TREE_ENTRIES: usize = 4_096;

const CHECKPOINT_SNAPSHOT_MAX_RECORD_BYTES: usize = 64 * 1024;

const CHECKPOINT_SNAPSHOT_DIRECTORY: &str = ".orna/checkpoints";

/// Reads the unique portable checkpoint watermark for `key` from the selected
/// immutable commit. It never consults HEAD, the index, the worktree, or
/// runtime state.
pub fn read_checkpoint_snapshot_watermark(
    repository: &Repository,
    selected_commit: &GitCommitRef,
    key: &CheckpointKey,
) -> Result<Option<PortableStreamCheckpointWatermark>, RuntimeError> {
    read_checkpoint_snapshot_watermark_bounded(
        repository,
        selected_commit,
        key,
        CHECKPOINT_SNAPSHOT_MAX_TREE_ENTRIES,
        CHECKPOINT_SNAPSHOT_MAX_RECORD_BYTES,
    )
}

pub(crate) fn read_checkpoint_snapshot_watermark_bounded(
    repository: &Repository,
    selected_commit: &GitCommitRef,
    key: &CheckpointKey,
    max_tree_entries: usize,
    max_record_bytes: usize,
) -> Result<Option<PortableStreamCheckpointWatermark>, RuntimeError> {
    if max_tree_entries == 0 || max_record_bytes == 0 {
        return Err(RuntimeError::RecoveryInvalid);
    }

    let entries = repository
        .list_committed_tree(selected_commit, max_tree_entries)
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    if entries.len() > max_tree_entries {
        return Err(RuntimeError::RecoveryInvalid);
    }

    let checkpoint_directory = Path::new(CHECKPOINT_SNAPSHOT_DIRECTORY);
    let mut matching = None;
    for entry in entries {
        let path = entry.path().as_path();
        let Ok(relative_path) = path.strip_prefix(checkpoint_directory) else {
            continue;
        };
        if relative_path.as_os_str().is_empty() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        match entry.kind() {
            CommittedTreeEntryKind::File { .. } => {}
            CommittedTreeEntryKind::Symlink | CommittedTreeEntryKind::Submodule => {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }

        let bytes = repository
            .read_committed_file(selected_commit, path, max_record_bytes)
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let watermark = decode_checkpoint_snapshot_watermark(&bytes)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        if watermark.checkpoint.key == *key {
            if matching.is_some() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            matching = Some(watermark);
        }
    }

    Ok(matching)
}

fn append_component(
    bytes: &mut Vec<u8>,
    component: &Component,
) -> Result<(), CheckpointSnapshotCodecError> {
    let value = component.as_str();
    let length =
        u32::try_from(value.len()).map_err(|_| CheckpointSnapshotCodecError::InvalidComponent)?;
    append_decimal(bytes, u64::from(length));
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(b'\n');
    Ok(())
}

fn append_decimal(bytes: &mut Vec<u8>, value: u64) {
    append_decimal_delimited(bytes, value, b':');
}

fn append_decimal_line(bytes: &mut Vec<u8>, value: u64) {
    append_decimal_delimited(bytes, value, b'\n');
}

fn append_decimal_delimited(bytes: &mut Vec<u8>, value: u64, delimiter: u8) {
    let mut buffer = [0; 20];
    let mut start = buffer.len();
    let mut remaining = value;
    loop {
        start -= 1;
        buffer[start] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    bytes.extend_from_slice(&buffer[start..]);
    bytes.push(delimiter);
}

struct CheckpointSnapshotReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl CheckpointSnapshotReader<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], CheckpointSnapshotCodecError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(CheckpointSnapshotCodecError::Truncated)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(CheckpointSnapshotCodecError::Truncated)?;
        self.cursor = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, CheckpointSnapshotCodecError> {
        Ok(self.take(1)?[0])
    }

    fn expect_newline(&mut self) -> Result<(), CheckpointSnapshotCodecError> {
        if self.byte()? != b'\n' {
            return Err(CheckpointSnapshotCodecError::InvalidEncoding);
        }
        Ok(())
    }

    fn decimal(&mut self, delimiter: u8) -> Result<u64, CheckpointSnapshotCodecError> {
        let start = self.cursor;
        while let Some(&byte) = self.bytes.get(self.cursor) {
            if byte == delimiter {
                let digits = &self.bytes[start..self.cursor];
                if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') {
                    return Err(CheckpointSnapshotCodecError::InvalidEncoding);
                }
                let digits = std::str::from_utf8(digits)
                    .map_err(|_| CheckpointSnapshotCodecError::InvalidEncoding)?;
                let value = digits
                    .parse()
                    .map_err(|_| CheckpointSnapshotCodecError::InvalidEncoding)?;
                self.cursor += 1;
                return Ok(value);
            }
            if !byte.is_ascii_digit() {
                return Err(CheckpointSnapshotCodecError::InvalidEncoding);
            }
            self.cursor += 1;
        }
        Err(CheckpointSnapshotCodecError::Truncated)
    }

    fn component(&mut self) -> Result<Component, CheckpointSnapshotCodecError> {
        let length = usize::try_from(self.decimal(b':')?)
            .map_err(|_| CheckpointSnapshotCodecError::InvalidEncoding)?;
        let value = std::str::from_utf8(self.take(length)?)
            .map_err(|_| CheckpointSnapshotCodecError::InvalidComponent)?
            .to_owned();
        self.expect_newline()?;
        Component::new(value).map_err(|_| CheckpointSnapshotCodecError::InvalidComponent)
    }
}
