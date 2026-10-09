//! Self-contained offline copies of committed media rows.
//!
//! A bundle is a plain directory with two parts:
//!
//! - `index.tsv`: a header line, then one `row` line per committed row and
//!   one `history` line per admitted commit. Keys and digests are hex encoded,
//!   so every field is a single tab-free token.
//! - `media/<sha256-hex>`: the selected payload bytes, one file per digest.
//!
//! Opening and querying a bundle reads only `index.tsv`. Payload files are
//! read only when a caller explicitly hydrates a row, and every hydrated
//! payload is checked against its recorded length and SHA-256. Nothing here
//! talks to the repository, a remote, or the network.

use std::{
    error::Error,
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

const INDEX_FILE: &str = "index.tsv";
const MEDIA_DIR: &str = "media";
const HEADER: &str = "orna-offline-copy 1";

/// Fixed buffer used to hash one payload. Verification retention is bounded by
/// this constant, not by the payload length, so it is also the temporary
/// storage bound a caller can report for a streamed import (ACCEPTANCE Gate B).
pub const PAYLOAD_BUFFER_BYTES: usize = 64 * 1024;

/// One committed row to copy, with its payload when the caller selects it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineRow {
    pub key: Vec<u8>,
    pub media_type: String,
    pub suffix: Option<String>,
    pub length: u64,
    pub sha256: [u8; 32],
    pub payload: Option<Vec<u8>>,
}

/// Payload-free view of one row as stored in a bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineRowMetadata {
    pub key: Vec<u8>,
    pub media_type: String,
    pub suffix: Option<String>,
    pub length: u64,
    pub sha256: [u8; 32],
    pub has_payload: bool,
}

/// One admitted commit that produced the copied rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineHistoryEntry {
    pub sequence: u64,
    pub commit: [u8; 32],
}

#[derive(Debug)]
pub enum OfflineCopyError {
    Io(io::Error),
    InvalidBundle(&'static str),
    InvalidRow(&'static str),
    PayloadMismatch { key: Vec<u8> },
    NotFound { key: Vec<u8> },
    NoPayload { key: Vec<u8> },
    PayloadAbsent { key: Vec<u8>, sha256: [u8; 32] },
}

impl fmt::Display for OfflineCopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "offline copy I/O failed: {error}"),
            Self::InvalidBundle(reason) => write!(formatter, "invalid offline bundle: {reason}"),
            Self::InvalidRow(reason) => write!(formatter, "invalid offline row: {reason}"),
            Self::PayloadMismatch { key } => {
                write!(formatter, "payload for row {} fails its digest", hex(key))
            }
            Self::NotFound { key } => write!(formatter, "no offline row {}", hex(key)),
            Self::NoPayload { key } => {
                write!(formatter, "offline row {} has no copied payload", hex(key))
            }
            Self::PayloadAbsent { key, sha256 } => {
                write!(
                    formatter,
                    "payload {} for offline row {} is not present in the copy",
                    hex(sha256),
                    hex(key)
                )
            }
        }
    }
}

impl Error for OfflineCopyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for OfflineCopyError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Writes a bundle into `directory`, which must be absent or empty.
///
/// Payloads are verified against their row metadata before anything is
/// written. The index is written last, so a bundle without `index.tsv` is
/// visibly incomplete.
pub fn write_offline_copy(
    directory: &Path,
    rows: &[OfflineRow],
    history: &[OfflineHistoryEntry],
) -> Result<(), OfflineCopyError> {
    write_offline_copy_limited(directory, rows, history, rows.len())
}

/// Whether an export writes its progress lines to the output stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfflineExportOutput {
    Quiet,
    Verbose,
}

/// Writes the first `limit` rows like `write_offline_copy_limited`. In
/// `Verbose` mode one line per written row is written to `out`, only after
/// the whole bundle has been written; a failed export writes no lines.
pub fn write_offline_copy_to_writer(
    directory: &Path,
    rows: &[OfflineRow],
    history: &[OfflineHistoryEntry],
    limit: usize,
    output: OfflineExportOutput,
    out: &mut impl Write,
) -> Result<(), OfflineCopyError> {
    let written = &rows[..rows.len().min(limit)];
    write_offline_copy_limited(directory, rows, history, limit)?;
    if output == OfflineExportOutput::Verbose {
        for (index, row) in written.iter().enumerate() {
            writeln!(
                out,
                "exported {}/{} {} ({} bytes)",
                index + 1,
                written.len(),
                String::from_utf8_lossy(&row.key),
                row.length
            )?;
        }
        let payload_bytes: u64 = written.iter().map(|row| row.length).sum();
        writeln!(
            out,
            "exported {} rows, {} payload bytes, {} history entries",
            written.len(),
            payload_bytes,
            history.len()
        )?;
    }
    Ok(())
}

/// Writes the first `limit` rows, in the order given, with their payloads.
/// The history is written in full, because the bundle does not record which
/// commit produced which row.
pub fn write_offline_copy_limited(
    directory: &Path,
    rows: &[OfflineRow],
    history: &[OfflineHistoryEntry],
    limit: usize,
) -> Result<(), OfflineCopyError> {
    if directory.exists() && fs::read_dir(directory)?.next().is_some() {
        return Err(OfflineCopyError::InvalidBundle(
            "target directory is not empty",
        ));
    }
    let mut index = format!("{HEADER}\n");
    let media = directory.join(MEDIA_DIR);
    for row in &rows[..rows.len().min(limit)] {
        validate_row(row)?;
        if let Some(payload) = &row.payload {
            verify_payload(row, payload)?;
            fs::create_dir_all(&media)?;
            let path = media.join(hex(&row.sha256));
            if !path.exists() {
                fs::write(&path, payload)?;
            }
        }
        index.push_str(&format!(
            "row\t{}\t{}\t{}\t{}\t{}\t{}\n",
            hex(&row.key),
            row.media_type,
            row.suffix.as_deref().unwrap_or("-"),
            row.length,
            hex(&row.sha256),
            u8::from(row.payload.is_some()),
        ));
    }
    for entry in history {
        index.push_str(&format!(
            "history\t{}\t{}\n",
            entry.sequence,
            hex(&entry.commit)
        ));
    }
    fs::create_dir_all(directory)?;
    let mut file = fs::File::create(directory.join(INDEX_FILE))?;
    file.write_all(index.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// An opened bundle. Opening reads the index only.
#[derive(Debug)]
pub struct OfflineCopy {
    directory: PathBuf,
    rows: Vec<OfflineRowMetadata>,
    history: Vec<OfflineHistoryEntry>,
}

impl OfflineCopy {
    pub fn open(directory: &Path) -> Result<Self, OfflineCopyError> {
        let index = fs::read_to_string(directory.join(INDEX_FILE))?;
        let mut lines = index.lines();
        if lines.next() != Some(HEADER) {
            return Err(OfflineCopyError::InvalidBundle("unknown header"));
        }
        let mut rows = Vec::new();
        let mut history = Vec::new();
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["row", key, media_type, suffix, length, sha256, has_payload] => {
                    rows.push(OfflineRowMetadata {
                        key: unhex(key).ok_or(OfflineCopyError::InvalidBundle("row key"))?,
                        media_type: (*media_type).to_owned(),
                        suffix: (*suffix != "-").then(|| (*suffix).to_owned()),
                        length: length
                            .parse()
                            .map_err(|_| OfflineCopyError::InvalidBundle("row length"))?,
                        sha256: unhex_digest(sha256)
                            .ok_or(OfflineCopyError::InvalidBundle("row digest"))?,
                        has_payload: match *has_payload {
                            "1" => true,
                            "0" => false,
                            _ => return Err(OfflineCopyError::InvalidBundle("row payload flag")),
                        },
                    });
                }
                ["history", sequence, commit] => history.push(OfflineHistoryEntry {
                    sequence: sequence
                        .parse()
                        .map_err(|_| OfflineCopyError::InvalidBundle("history sequence"))?,
                    commit: unhex_digest(commit)
                        .ok_or(OfflineCopyError::InvalidBundle("history commit"))?,
                }),
                _ => return Err(OfflineCopyError::InvalidBundle("unrecognised index line")),
            }
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            rows,
            history,
        })
    }

    /// Lists every row as metadata. No media file is read.
    pub fn rows(&self) -> &[OfflineRowMetadata] {
        &self.rows
    }

    pub fn history(&self) -> &[OfflineHistoryEntry] {
        &self.history
    }

    /// Looks up one row's metadata by key. No media file is read.
    pub fn metadata(&self, key: &[u8]) -> Option<&OfflineRowMetadata> {
        self.rows.iter().find(|row| row.key == key)
    }

    /// Lists, in bundle order, the rows whose payload this copy does not hold:
    /// a row without a copied payload, or one whose media file is absent. This
    /// is a metadata-only question. It asks which objects are missing, never
    /// what they contain, so it opens no media file and returns the `(key,
    /// sha256)` pairs a fetch-on-demand caller must retrieve.
    pub fn missing_payloads(&self) -> Vec<(Vec<u8>, [u8; 32])> {
        self.rows
            .iter()
            .filter(|row| self.payload_path(row).is_none())
            .map(|row| (row.key.clone(), row.sha256))
            .collect()
    }

    /// Reads and verifies one payload. This is the only call that touches
    /// `media/`.
    pub fn hydrate(&self, key: &[u8]) -> Result<Vec<u8>, OfflineCopyError> {
        let row = self
            .metadata(key)
            .ok_or_else(|| OfflineCopyError::NotFound { key: key.to_vec() })?;
        self.read_payload(row)
    }

    /// Hydrates every row back into full rows, in bundle order. Each payload
    /// is verified against its recorded digest, so the result can be written
    /// into another bundle or committed to a repository without re-reading
    /// the original copy.
    pub fn import_rows(&self) -> Result<Vec<OfflineRow>, OfflineCopyError> {
        self.rows
            .iter()
            .map(|row| {
                let payload = row
                    .has_payload
                    .then(|| self.read_payload(row))
                    .transpose()?;
                Ok(OfflineRow {
                    key: row.key.clone(),
                    media_type: row.media_type.clone(),
                    suffix: row.suffix.clone(),
                    length: row.length,
                    sha256: row.sha256,
                    payload,
                })
            })
            .collect()
    }

    /// The media file holding one row's payload, or `None` when this copy does
    /// not hold it: the row records no copied payload, or the file is absent.
    /// Presence only; the bytes are read and verified by `read_payload`.
    fn payload_path(&self, row: &OfflineRowMetadata) -> Option<PathBuf> {
        if !row.has_payload {
            return None;
        }
        let path = self.directory.join(MEDIA_DIR).join(hex(&row.sha256));
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Some(path),
            _ => None,
        }
    }

    fn read_payload(&self, row: &OfflineRowMetadata) -> Result<Vec<u8>, OfflineCopyError> {
        if !row.has_payload {
            return Err(OfflineCopyError::NoPayload {
                key: row.key.clone(),
            });
        }
        let Some(path) = self.payload_path(row) else {
            return Err(OfflineCopyError::PayloadAbsent {
                key: row.key.clone(),
                sha256: row.sha256,
            });
        };
        let payload = match fs::read(&path) {
            Ok(payload) => payload,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(OfflineCopyError::PayloadAbsent {
                    key: row.key.clone(),
                    sha256: row.sha256,
                });
            }
            Err(error) => return Err(OfflineCopyError::Io(error)),
        };
        let probe = OfflineRow {
            key: row.key.clone(),
            media_type: row.media_type.clone(),
            suffix: row.suffix.clone(),
            length: row.length,
            sha256: row.sha256,
            payload: None,
        };
        verify_payload(&probe, &payload)?;
        Ok(payload)
    }
}

/// One row ready to be re-committed, with its payload verified against its
/// recorded length and SHA-256.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineImportRow {
    pub key: Vec<u8>,
    pub media_type: String,
    pub suffix: Option<String>,
    pub length: u64,
    pub sha256: [u8; 32],
    pub payload: Vec<u8>,
}

/// A complete, verified import of a bundle. Every row carries its payload,
/// and history is strictly ordered by sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineImportPlan {
    pub rows: Vec<OfflineImportRow>,
    pub history: Vec<OfflineHistoryEntry>,
}

impl OfflineCopy {
    /// Verifies the whole bundle before any repository write. Import is
    /// all-or-nothing: a row without a copied payload, a payload that fails its
    /// digest, or out-of-order history rejects the entire bundle.
    pub fn import_plan(&self) -> Result<OfflineImportPlan, OfflineCopyError> {
        self.import_plan_with_progress(|_| {})
    }

    /// Same verification as `import_plan`, reporting each row once its
    /// payload has been verified. Progress is reported in bundle order.
    pub fn import_plan_with_progress(
        &self,
        progress: impl FnMut(OfflineImportProgress),
    ) -> Result<OfflineImportPlan, OfflineCopyError> {
        self.import_plan_limited_with_progress(self.rows.len(), progress)
    }

    /// Verifies and plans only the first `limit` rows, in bundle order. The
    /// history is checked and returned in full, because the bundle does not
    /// record which commit produced which row.
    pub fn import_plan_limited(&self, limit: usize) -> Result<OfflineImportPlan, OfflineCopyError> {
        self.import_plan_limited_with_progress(limit, |_| {})
    }

    fn import_plan_limited_with_progress(
        &self,
        limit: usize,
        mut progress: impl FnMut(OfflineImportProgress),
    ) -> Result<OfflineImportPlan, OfflineCopyError> {
        if self
            .history
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
        {
            return Err(OfflineCopyError::InvalidBundle(
                "history is not strictly ordered",
            ));
        }
        let selected = &self.rows[..self.rows.len().min(limit)];
        let mut rows = Vec::with_capacity(selected.len());
        for (index, metadata) in selected.iter().enumerate() {
            let payload = self.hydrate(&metadata.key)?;
            progress(OfflineImportProgress {
                verified: index + 1,
                total: selected.len(),
                key: metadata.key.clone(),
                bytes: metadata.length,
            });
            rows.push(OfflineImportRow {
                key: metadata.key.clone(),
                media_type: metadata.media_type.clone(),
                suffix: metadata.suffix.clone(),
                length: metadata.length,
                sha256: metadata.sha256,
                payload,
            });
        }
        Ok(OfflineImportPlan {
            rows,
            history: self.history.clone(),
        })
    }
}

/// Whether an import writes its progress lines to the output stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfflineImportOutput {
    Quiet,
    Verbose,
}

impl OfflineCopy {
    /// Verifies the whole bundle like `import_plan`. In `Verbose` mode, each
    /// verified row's progress line is written to `out`; in `Quiet` mode
    /// nothing is written.
    pub fn import_plan_to_writer(
        &self,
        output: OfflineImportOutput,
        out: &mut impl Write,
    ) -> Result<OfflineImportPlan, OfflineCopyError> {
        let mut write_error = None;
        let plan = self.import_plan_with_progress(|progress| {
            if output == OfflineImportOutput::Verbose && write_error.is_none() {
                if let Err(error) = writeln!(out, "{progress}") {
                    write_error = Some(error);
                }
            }
        })?;
        match write_error {
            Some(error) => Err(error.into()),
            None => Ok(plan),
        }
    }
}

impl OfflineCopy {
    /// Verifies the whole bundle like `import_plan`, then sets every row's
    /// media type to `media_type`. Only the annotation changes: the length and
    /// SHA-256 that identify the payload are still checked against the bytes.
    pub fn import_plan_as_media_type(
        &self,
        media_type: &str,
    ) -> Result<OfflineImportPlan, OfflineCopyError> {
        if media_type.is_empty() || !media_type.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(OfflineCopyError::InvalidRow("media type must be one token"));
        }
        let mut plan = self.import_plan()?;
        for row in &mut plan.rows {
            row.media_type = media_type.to_owned();
        }
        Ok(plan)
    }
}

impl OfflineRow {
    /// Builds the export row for one committed Blob. Metadata comes from the
    /// Blob's descriptor; the payload is read in full and must match that
    /// descriptor's length and SHA-256 when the bundle is written.
    pub fn from_blob(key: &[u8], blob: &orna_value_v1::Blob) -> Result<Self, OfflineCopyError> {
        let metadata = blob.metadata();
        let payload = blob
            .read_to_end()
            .map_err(|_| OfflineCopyError::InvalidRow("blob payload is not readable"))?;
        Ok(Self {
            key: key.to_vec(),
            media_type: metadata.media_type().to_owned(),
            suffix: metadata.suffix().map(str::to_owned),
            length: metadata.length(),
            sha256: metadata.sha256(),
            payload: Some(payload),
        })
    }
}

/// What an import would write, reported after full verification. Payload
/// bytes are not kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineImportDryRun {
    pub rows: usize,
    pub payload_bytes: u64,
    pub history: usize,
}

/// One verified row, reported while an import is being checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineImportProgress {
    pub verified: usize,
    pub total: usize,
    pub key: Vec<u8>,
    pub bytes: u64,
}

impl fmt::Display for OfflineImportProgress {
    /// Renders one progress line, for example:
    /// `verified 1/2 image (73 bytes)`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "verified {}/{} {} ({} bytes)",
            self.verified,
            self.total,
            String::from_utf8_lossy(&self.key),
            self.bytes
        )
    }
}

impl OfflineCopy {
    /// Verifies the whole bundle exactly as `import_plan` does, then reports
    /// the counts an import would write. Nothing is written to the bundle or
    /// to any repository.
    pub fn dry_run_import(&self) -> Result<OfflineImportDryRun, OfflineCopyError> {
        let plan = self.import_plan()?;
        Ok(OfflineImportDryRun {
            rows: plan.rows.len(),
            payload_bytes: plan.rows.iter().map(|row| row.length).sum(),
            history: plan.history.len(),
        })
    }
}

/// One verified row without its payload: what the bundle promised, after its
/// bytes were hashed and discarded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineImportMetadata {
    pub key: Vec<u8>,
    pub media_type: String,
    pub suffix: Option<String>,
    pub length: u64,
    pub sha256: [u8; 32],
}

/// A complete bundle verification that keeps no payload bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfflineImportSummary {
    pub rows: Vec<OfflineImportMetadata>,
    pub history: Vec<OfflineHistoryEntry>,
}

impl OfflineCopy {
    /// Verifies every selected payload against its recorded length and
    /// SHA-256 from a fixed-size buffer and returns only metadata. This is the
    /// bounded-retention counterpart to `import_plan`, for callers that plan
    /// an import without holding the payloads (CAPTURE-3 step 2).
    pub fn import_summary(&self, limit: usize) -> Result<OfflineImportSummary, OfflineCopyError> {
        self.import_summary_with_progress(limit, |_| {})
    }

    /// Same verification as `import_summary`, reporting each row once its
    /// bytes have been hashed and released. Progress is reported in bundle
    /// order.
    pub fn import_summary_with_progress(
        &self,
        limit: usize,
        mut progress: impl FnMut(OfflineImportProgress),
    ) -> Result<OfflineImportSummary, OfflineCopyError> {
        if self
            .history
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
        {
            return Err(OfflineCopyError::InvalidBundle(
                "history is not strictly ordered",
            ));
        }
        let selected = &self.rows[..self.rows.len().min(limit)];
        let mut rows = Vec::with_capacity(selected.len());
        for (index, metadata) in selected.iter().enumerate() {
            if !metadata.has_payload {
                return Err(OfflineCopyError::NoPayload {
                    key: metadata.key.clone(),
                });
            }
            let path = self.directory.join(MEDIA_DIR).join(hex(&metadata.sha256));
            verify_payload_stream(
                &metadata.key,
                metadata.length,
                metadata.sha256,
                fs::File::open(path)?,
            )?;
            progress(OfflineImportProgress {
                verified: index + 1,
                total: selected.len(),
                key: metadata.key.clone(),
                bytes: metadata.length,
            });
            rows.push(OfflineImportMetadata {
                key: metadata.key.clone(),
                media_type: metadata.media_type.clone(),
                suffix: metadata.suffix.clone(),
                length: metadata.length,
                sha256: metadata.sha256,
            });
        }
        Ok(OfflineImportSummary {
            rows,
            history: self.history.clone(),
        })
    }
}

fn validate_row(row: &OfflineRow) -> Result<(), OfflineCopyError> {
    let token_ok = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_graphic());
    if !token_ok(&row.media_type) {
        return Err(OfflineCopyError::InvalidRow("media type must be one token"));
    }
    if let Some(suffix) = &row.suffix {
        if !token_ok(suffix) {
            return Err(OfflineCopyError::InvalidRow("suffix must be one token"));
        }
    }
    Ok(())
}

fn verify_payload(row: &OfflineRow, payload: &[u8]) -> Result<(), OfflineCopyError> {
    let digest: [u8; 32] = Sha256::digest(payload).into();
    if payload.len() as u64 != row.length || digest != row.sha256 {
        return Err(OfflineCopyError::PayloadMismatch {
            key: row.key.clone(),
        });
    }
    Ok(())
}

/// Hashes an already-published payload from a fixed-size buffer, so a row's
/// verification retention does not grow with its payload length.
fn verify_payload_stream(
    key: &[u8],
    length: u64,
    sha256: [u8; 32],
    source: impl Read,
) -> Result<(), OfflineCopyError> {
    let mut source = source;
    let mut buffer = vec![0_u8; PAYLOAD_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut read_total: u64 = 0;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        read_total += read as u64;
        hasher.update(&buffer[..read]);
    }
    if read_total != length || <[u8; 32]>::from(hasher.finalize()) != sha256 {
        return Err(OfflineCopyError::PayloadMismatch { key: key.to_vec() });
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(digits, 16).ok()
        })
        .collect()
}

fn unhex_digest(text: &str) -> Option<[u8; 32]> {
    unhex(text)?.try_into().ok()
}
