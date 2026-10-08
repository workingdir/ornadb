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
    io::{self, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

const INDEX_FILE: &str = "index.tsv";
const MEDIA_DIR: &str = "media";
const HEADER: &str = "orna-offline-copy 1";

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
    if directory.exists() && fs::read_dir(directory)?.next().is_some() {
        return Err(OfflineCopyError::InvalidBundle(
            "target directory is not empty",
        ));
    }
    let mut index = format!("{HEADER}\n");
    let media = directory.join(MEDIA_DIR);
    for row in rows {
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

    /// Reads and verifies one payload. This is the only call that touches
    /// `media/`.
    pub fn hydrate(&self, key: &[u8]) -> Result<Vec<u8>, OfflineCopyError> {
        let row = self
            .metadata(key)
            .ok_or_else(|| OfflineCopyError::NotFound { key: key.to_vec() })?;
        if !row.has_payload {
            return Err(OfflineCopyError::NoPayload { key: key.to_vec() });
        }
        let payload = fs::read(self.directory.join(MEDIA_DIR).join(hex(&row.sha256)))?;
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
        if self
            .history
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
        {
            return Err(OfflineCopyError::InvalidBundle("history is not strictly ordered"));
        }
        let mut rows = Vec::with_capacity(self.rows.len());
        for metadata in &self.rows {
            let payload = self.hydrate(&metadata.key)?;
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
