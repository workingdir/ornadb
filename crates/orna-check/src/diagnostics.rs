//! Diagnostic codes and the notes that classify them.

use orna_value::Diagnostic;

pub const DIAG_PARSE: &str = "ORNA-S000-PARSE";
pub const DIAG_BAD_PATH: &str = "ORNA-S001-PATH";
pub const DIAG_NAMESPACE: &str = "ORNA-S002-NAMESPACE";
pub const DIAG_RESERVED: &str = "ORNA-S003-RESERVED";
pub const DIAG_IMPORT: &str = "ORNA-S010-IMPORT";
pub const DIAG_AMBIGUOUS: &str = "ORNA-S011-AMBIGUOUS";
pub const DIAG_UNRESOLVED: &str = "ORNA-S012-UNRESOLVED";
/// Note prefix carrying the identifier of an `ORNA-S012-UNRESOLVED` diagnostic.
pub const UNRESOLVED_NAME_NOTE: &str = "unresolved name: ";

/// Returns the unresolved identifier recorded on an unresolved-name diagnostic.
pub fn unresolved_name(diagnostic: &Diagnostic) -> Option<&str> {
    diagnostic
        .notes()
        .iter()
        .find_map(|note| note.as_str().strip_prefix(UNRESOLVED_NAME_NOTE))
}

/// Note prefix carrying `module.member` for an unresolved module member.
pub const UNRESOLVED_MEMBER_NOTE: &str = "unresolved member: ";

/// Returns the `(module, member)` pair recorded on an unresolved module member diagnostic.
pub fn unresolved_member(diagnostic: &Diagnostic) -> Option<(&str, &str)> {
    diagnostic
        .notes()
        .iter()
        .find_map(|note| note.as_str().strip_prefix(UNRESOLVED_MEMBER_NOTE))
        .and_then(|path| path.rsplit_once('.'))
}

pub const DIAG_DUPLICATE: &str = "ORNA-S013-DUPLICATE";
pub const DIAG_ANNOTATION: &str = "ORNA-S020-ANNOTATION";
pub const DIAG_TYPE: &str = "ORNA-S021-TYPE";
pub const DIAG_UNSUPPORTED: &str = "ORNA-S022-UNSUPPORTED";
pub const DIAG_ASSERTION: &str = "ORNA-A091-004";
pub const DIAG_ASSERTION_ONE_TABLE: &str = "ORNA-A091-003";
pub const DIAG_ASSERTION_SCOPE: &str = "ORNA-A091-012";
pub const DIAG_ASSERTION_EFFECT: &str = "ORNA-A091-007";
pub const DIAG_LEGACY_SYS_RUNTIME: &str = "ORNA100-E-SYS-RUNTIME";
pub const DIAG_LEGACY_TRYFROM: &str = "ORNA091-E-TRYFROM";
pub const DIAG_LEGACY_RESULT: &str = "ORNA091-E-RESULT";
/// Published typecheck classifications for construction fields
/// (`tests/invalid-metadata.json`: wrong-field-type, missing-required-field
/// and unknown-field). Enum payload construction emits them at the source.
pub const DIAG_FIELD_TYPE: &str = "E2001";
pub const DIAG_MISSING_FIELD: &str = "E2002";
pub const DIAG_UNKNOWN_FIELD: &str = "E2003";
pub const DIAG_ASSERTION_OWNER_PIPE: &str = "ORNA-A091-002";
/// Spec diagnostic codes (`33-diagnostics.md`) emitted directly at the rule
/// that detects the violation; the message text never selects the code.
pub const DIAG_RESERVED_SYS: &str = "E1008";
pub const DIAG_RESERVED_STD: &str = "E1009";
pub const DIAG_PRIMARY_KEY_IMMUTABLE: &str = "E3002";
pub const DIAG_PRIMARY_KEY_FLOAT: &str = "E3003";
pub const DIAG_PRIMARY_KEY_RANGE: &str = "E3010";
pub const DIAG_COMPUTED_INSERT: &str = "E3010";
pub const DIAG_COMPUTED_DIRECT_WRITE: &str = "E3011";
pub const DIAG_COMPUTED_NONDETERMINISTIC: &str = "E3012";
pub const DIAG_AUTOMATIC_KEY_REKEY: &str = "E3013";
pub const DIAG_RELATION_EQUALITY_AMBIGUOUS: &str = "E4107";
pub const DIAG_TIME_ENERGY_ADD: &str = "E5001";
pub const DIAG_AFFINE_TEMPERATURE_ADD: &str = "E5002";
pub const DIAG_ABSOLUTE_QUANTITY_SUM: &str = "E5003";
pub const DIAG_CURRENCY_MISMATCH: &str = "E5003";
pub const DIAG_MONEY_FROM_FLOAT: &str = "E5004";
pub const DIAG_MONEY_BINARY_FLOAT: &str = "E5103";
pub const DIAG_CALENDAR_BUCKET_ZONE: &str = "E6001";
pub const DIAG_SYS_COMMIT_READ_ONLY: &str = "E7001";
pub const DIAG_SECRET_DISPLAY: &str = "E7002";
pub const DIAG_DISPLAY_READ_ONLY: &str = "E7201";
pub const DIAG_CROSS_DATABASE_WRITE: &str = "E9001";
pub const DIAG_AMBIGUOUS_DURABLE_CONSUMER: &str = "E9101";
pub const DIAG_DURABLE_CONSUMER_SOURCE_ROOTS: &str = "E9102";
pub const DIAG_CURRENCY_SYMBOL: &str = "ORNA091-E-CURRENCY-SYMBOL";
pub const DIAG_CONVERSION_CHAIN: &str = "ORNA091-E-CONVERSION-CHAIN";
pub const DIAG_SYS_ADMIN_METHOD: &str = "ORNA100-E-SYS-ADMIN-METHOD";
pub const DIAG_SYS_STORAGE_CALL: &str = "ORNA100-E-SYS-STORAGE-CALL";
pub const DIAG_TABLE_FIELD_TYPE: &str = "E2001";
pub const DIAG_MISSING_REQUIRED_FIELD: &str = "E2002";
pub const DIAG_TABLE_UNKNOWN_FIELD: &str = "E2003";
/// Published typecheck classification for an operand whose static type does
/// not fit its operator (the spec's "expected X, found Y" type mismatch).
pub const DIAG_TYPE_MISMATCH: &str = "E2001";
/// Published classification for adding or subtracting quantities whose
/// reduced dimensions differ (ORNA-UNIT-004).
pub const DIAG_INCOMPATIBLE_DIMENSIONS: &str = "E5001";
/// Portable failure code for a required `sys` member that has no linked
/// production provider (ORNA-SYS-008, `api/sys.json` `failure_codes`).
pub const DIAG_SYS_VERSION_INCOMPATIBLE: &str = "sys.version.incompatible";
