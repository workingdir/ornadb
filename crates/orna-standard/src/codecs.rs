//! Checked-in opaque codec registration for accepted standard snapshots.

use super::*;
/// Builds the immutable opaque codec registry for the accepted standard snapshot.
///
/// The registration is compiled into this crate. The supplied snapshot can
/// validate it, but cannot add or select a codec. The accepted V1 and V2
/// snapshots bind only the fixed-length `std.types.opaque_token` codec; the
/// accepted `orna.std/3` snapshot additionally binds the two framed output
/// codecs for `std.terminal.Document` and `std.io.ByteStream` (work ADR
/// 0058); the accepted `orna.std/4` snapshot additionally binds the
/// `ORNA-UI/1 ` length-prefixed canonical UI-value codec for `std.ui.UI` (work ADR 0062);
/// the accepted V6 snapshot additionally binds the structurally checked
/// `ORNA-ACTION/1 ` action descriptor codec (ADR 0079).
pub fn registered_opaque_codecs(
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<OpaqueCodecRegistry, RegisteredOpaqueCodecsError> {
    let opaque_token = OpaqueCodecRegistration::fixed_length_identity(
        OPAQUE_TOKEN_TYPE_ID,
        semantic_name(
            "std.types.opaque_token",
            ["std", "types", OPAQUE_TOKEN_LOCAL_NAME],
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
        OPAQUE_TOKEN_CONTRACT,
        16,
    )
    .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;

    let registrations = if is_accepted_v10_standard(standard)
        || is_accepted_v9_standard(standard)
        || is_accepted_v8_standard(standard)
    {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let ui = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_UI_TYPE_ID,
            semantic_name("std.ui.ui", ["std", "ui", "ui"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_UI_CONTRACT,
            UI_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let json = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_JSON_VALUE_TYPE_ID,
            semantic_name("std.json.value", ["std", "json", "value"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_JSON_CONTRACT,
            JSON_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let action = OpaqueCodecRegistration::length_prefixed_action(
            STD_ACTION_TYPE_ID,
            semantic_name("std.action.action", ["std", "action", "action"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_ACTION_CONTRACT,
            ACTION_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let rows = OpaqueCodecRegistration::rows(
            STD_DATA_ROWS_TYPE_ID,
            semantic_name(STD_DATA_ROWS_SEMANTIC_NAME, ["std", "data", "rows"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_DATA_ROWS_CONTRACT,
            "ORNA-ROWS/1 ",
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream, ui, json, action, rows]
    } else if is_accepted_v7_standard(standard) || is_accepted_v6_standard(standard) {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let ui = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_UI_TYPE_ID,
            semantic_name("std.ui.ui", ["std", "ui", "ui"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_UI_CONTRACT,
            UI_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let json = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_JSON_VALUE_TYPE_ID,
            semantic_name("std.json.value", ["std", "json", "value"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_JSON_CONTRACT,
            JSON_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let action = OpaqueCodecRegistration::length_prefixed_action(
            STD_ACTION_TYPE_ID,
            semantic_name("std.action.action", ["std", "action", "action"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_ACTION_CONTRACT,
            ACTION_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream, ui, json, action]
    } else if is_accepted_v5_standard(standard) {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let ui = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_UI_TYPE_ID,
            semantic_name("std.ui.ui", ["std", "ui", "ui"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_UI_CONTRACT,
            UI_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let json = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_JSON_VALUE_TYPE_ID,
            semantic_name("std.json.value", ["std", "json", "value"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_JSON_CONTRACT,
            JSON_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream, ui, json]
    } else if is_accepted_v4_standard(standard) {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let ui = OpaqueCodecRegistration::length_prefixed_canonical_json(
            STD_UI_TYPE_ID,
            semantic_name("std.ui.ui", ["std", "ui", "ui"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_UI_CONTRACT,
            UI_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream, ui]
    } else if is_accepted_v3_standard(standard) {
        let document = OpaqueCodecRegistration::length_prefixed_utf8(
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            semantic_name("std.terminal.document", ["std", "terminal", "document"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_TERMINAL_DOCUMENT_CONTRACT,
            TERMINAL_DOCUMENT_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        let byte_stream = OpaqueCodecRegistration::media_type_framed(
            STD_IO_BYTE_STREAM_TYPE_ID,
            semantic_name("std.io.bytestream", ["std", "io", "bytestream"])
                .map_err(|source| RegisteredOpaqueCodecsError::Manifest { source })?,
            STD_IO_BYTE_STREAM_CONTRACT,
            BYTE_STREAM_MAGIC,
        )
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })?;
        vec![opaque_token, document, byte_stream]
    } else if is_accepted_v1_or_v2_standard(standard) {
        vec![opaque_token]
    } else {
        return Err(RegisteredOpaqueCodecsError::UnacceptedStandardSnapshot);
    };

    OpaqueCodecRegistry::new(standard, registrations)
        .map_err(|source| RegisteredOpaqueCodecsError::Registry { source })
}

/// Returns the deterministic checked-in contracts for the nine sealed
/// `sys.inspect` snapshot and projection carriers.
///
/// These system carriers are intentionally not added to a standard snapshot's
/// [`OpaqueCodecRegistry`]. They are recognised through their sealed TypeIds
/// and contracts, independently of application catalogue definitions.
pub fn registered_inspect_carrier_codecs() -> &'static [InspectCarrierCodecRegistration] {
    INSPECT_CARRIER_CODEC_REGISTRATIONS
}

/// Returns whether a TypeId is one of the fixed sealed Inspector carriers.
pub fn is_registered_inspect_carrier_type(opaque_type: TypeId) -> bool {
    registered_inspect_carrier_codecs()
        .iter()
        .any(|registration| registration.opaque_type() == opaque_type)
}

/// Returns whether one verified snapshot is exactly the accepted version-one
/// or version-two standard library (ADR 0055).
///
/// Version two retains the version-one types byte-for-byte and adds no new
/// opaque type or codec, so both accepted snapshots bind the same opaque-token
/// codec.
fn is_accepted_v1_or_v2_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    (standard.revision() == STANDARD_LIBRARY_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_REVISION_ID
        && standard.source().revision_hash() == ACCEPTED_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_STANDARD_LIBRARY_DIGEST)
        || (standard.revision() == STANDARD_LIBRARY_V2_REVISION_ID
            && standard.catalogue().revision() == STANDARD_CATALOGUE_V2_REVISION_ID
            && standard.source().bundle() == STANDARD_SOURCE_V2_BUNDLE_ID
            && standard.source().id() == STANDARD_SOURCE_V2_REVISION_ID
            && standard.source().revision_hash() == ACCEPTED_V2_SOURCE_REVISION_DIGEST
            && standard.digest() == ACCEPTED_V2_STANDARD_LIBRARY_DIGEST)
}

/// Returns whether one verified snapshot is exactly the accepted `orna.std/3`
/// standard library (work ADR 0058).
fn is_accepted_v3_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V3_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V3_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V3_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V3_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V2_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V3_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V3_STANDARD_LIBRARY_DIGEST
}

/// Returns whether one verified snapshot is exactly the accepted `orna.std/4`
/// standard library (work ADR 0062).
fn is_accepted_v4_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V4_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V4_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V4_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V4_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V3_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V4_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V4_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v10_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V10_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V10_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V10_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V10_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V9_REVISION_ID)
        && standard.source().units().len() == 10
        && standard.source().units()[9].content_hash() == ACCEPTED_V10_CLI_CONTENT_DIGEST
        && standard.source().revision_hash() == ACCEPTED_V10_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V10_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v9_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V9_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V9_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V9_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V9_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V8_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V9_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V9_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v8_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V8_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V8_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V8_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V8_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V7_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V8_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V8_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v7_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V7_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V7_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V7_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V7_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V6_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V7_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V7_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v6_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V6_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V6_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V6_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V6_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V5_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V6_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V6_STANDARD_LIBRARY_DIGEST
}

fn is_accepted_v5_standard(standard: &VerifiedStandardLibrarySnapshot) -> bool {
    standard.revision() == STANDARD_LIBRARY_V5_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V5_REVISION_ID
        && standard.source().bundle() == STANDARD_SOURCE_V5_BUNDLE_ID
        && standard.source().id() == STANDARD_SOURCE_V5_REVISION_ID
        && standard.source().parent() == Some(STANDARD_SOURCE_V4_REVISION_ID)
        && standard.source().revision_hash() == ACCEPTED_V5_SOURCE_REVISION_DIGEST
        && standard.digest() == ACCEPTED_V5_STANDARD_LIBRARY_DIGEST
}

/// An error from binding checked-in opaque codecs to a standard snapshot.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisteredOpaqueCodecsError {
    /// The supplied verified snapshot is not the hard-coded accepted standard.
    UnacceptedStandardSnapshot,
    /// A checked-in codec semantic name is invalid.
    Manifest {
        /// The standard manifest error.
        source: StandardLibraryManifestError,
    },
    /// The checked-in registry does not match the accepted definitions.
    Registry {
        /// The exact core registry validation error.
        source: OpaqueCodecRegistryError,
    },
}

impl fmt::Display for RegisteredOpaqueCodecsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnacceptedStandardSnapshot => {
                formatter.write_str("opaque codecs require the accepted standard snapshot")
            }
            Self::Manifest { source } => {
                write!(formatter, "opaque codec name is invalid: {source}")
            }
            Self::Registry { source } => {
                write!(formatter, "opaque codec registry is invalid: {source}")
            }
        }
    }
}

impl Error for RegisteredOpaqueCodecsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnacceptedStandardSnapshot => None,
            Self::Manifest { source } => Some(source),
            Self::Registry { source } => Some(source),
        }
    }
}
