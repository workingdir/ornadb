//! The OVB tag registry as named constants.
//!
//! Integer codes are exact and not extensible without a negotiated profile
//! version (`reference/Orna-1.1.0/source/29-formats.md`). The portable values
//! below come from the "Type-preserving values" and "Portable opaque and
//! contextual values" tables in that file; the profile tags come from its
//! "Current value profiles and boundary" section and are normative in
//! `profiles/store-format-3.md`.

/// UUID / ObjectId: exactly 16 network-order bytes.
pub const UUID_TAG: u64 = 37;
/// Decimal `[coefficient, exponent10]`.
pub const DECIMAL_TAG: u64 = 60_000;
/// Gregorian date as canonical `YYYY-MM-DD` text.
pub const DATE_TAG: u64 = 60_001;
/// Instant `[unix_seconds, nanosecond]`.
pub const INSTANT_TAG: u64 = 60_002;
/// Offset-free local date/time text with nine fractional digits.
pub const LOCAL_DATE_TIME_TAG: u64 = 60_003;
/// IANA time-zone identifier text.
pub const TIME_ZONE_TAG: u64 = 60_004;
/// Duration `[seconds, nanosecond]`.
pub const DURATION_TAG: u64 = 60_005;
/// Quantity `[number, unit_object_id]`.
pub const QUANTITY_TAG: u64 = 60_006;
/// Money `[Decimal, currency_object_id]`.
pub const MONEY_TAG: u64 = 60_007;
/// Enum `[type_object_id, variant_object_id, payload_record_or_null]`.
pub const ENUM_TAG: u64 = 60_008;
/// Record, nominal record or logical row `[type_object_id_or_null, fields]`.
pub const RECORD_TAG: u64 = 60_009;
/// Pinned row reference `[database_id, table_id, key, snapshot]`.
pub const PINNED_ROW_REFERENCE_TAG: u64 = 60_010;
/// Diagnostic map carrying safe fields only.
pub const DIAGNOSTIC_TAG: u64 = 60_011;
/// Present node `[kind, stable_key_or_null, properties, children]`.
pub const PRESENT_NODE_TAG: u64 = 60_012;
/// Option `[0]` for `null` or `[1, value]` for `Some(value)`.
pub const OPTION_TAG: u64 = 60_013;
/// Unit: the empty array `[]`.
pub const UNIT_TAG: u64 = 60_014;
/// Tuple: the ordered array of component values.
pub const TUPLE_TAG: u64 = 60_015;
/// Error map `{0: code, 1: message, 2: causes, 3: safe_details}`.
pub const ERROR_TAG: u64 = 60_016;
/// Time of day as nanoseconds since local midnight.
pub const TIME_OF_DAY_TAG: u64 = 60_017;
/// Zoned date/time `[Instant, TimeZone, offset_seconds]`.
pub const ZONED_DATE_TIME_TAG: u64 = 60_018;
/// Range `[lower: Option, upper: Option, upper_inclusive: Bool]`.
pub const RANGE_TAG: u64 = 60_019;
/// Refined nominal value `[type_object_id, base_value]`.
pub const REFINED_NOMINAL_TAG: u64 = 60_020;
/// Stored table reference `[database_id, table_id, key]`.
pub const STORED_TABLE_REFERENCE_TAG: u64 = 60_021;
/// Live-handle description `[original_type_id, runtime_id, natural_key, false]`.
pub const LIVE_HANDLE_TAG: u64 = 60_022;
/// Portable opaque system identifier `[qualified_type_name, representation]`.
pub const OPAQUE_SYSTEM_ID_TAG: u64 = 60_023;
/// Path `[flavour, text]`.
pub const PATH_TAG: u64 = 60_024;
/// Digest `["sha256", bytes32]`.
pub const DIGEST_TAG: u64 = 60_025;
/// Explicit `sys.Value` box `[closed_type_descriptor, value]`.
pub const SYS_VALUE_BOX_TAG: u64 = 60_026;
/// Materialised finite Relation `[ordered: Bool, rows]`.
pub const RELATION_TAG: u64 = 60_027;

pub const OVB2_BLOB_TAG: u64 = 60_110;
pub const ROV3_BLOB_TAG: u64 = 60_111;
pub const SOV3_BLOB_TAG: u64 = 60_112;
pub const ROV3_OVERFLOW_TAG: u64 = 60_113;

/// Whether a CBOR tag may appear in format-3 native node data.
///
/// Store format 3 admits registered tags only: the bignums 2 and 3, UUID tag
/// 37, the portable value tags 60000..=60025 and the profile tags
/// 60110..=60113 (`profiles/store-format-3.md` §2, `tools/final_model.py`).
/// Where a profile tag may appear (for example, 60111 and 60113 only in ROV-3
/// rows) is checked by the reader that interprets the node.
pub const fn is_format3_node_tag(number: u64) -> bool {
    matches!(
        number,
        2 | 3 | UUID_TAG | DECIMAL_TAG..=DIGEST_TAG | OVB2_BLOB_TAG..=ROV3_OVERFLOW_TAG
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry numbers are wire format, so they are spelled out again
    /// here from `source/29-formats.md` rather than derived from the constants.
    #[test]
    fn constants_match_the_spec_tag_registry() {
        for (name, actual, expected) in [
            ("UUID", UUID_TAG, 37),
            ("Decimal", DECIMAL_TAG, 60000),
            ("Date", DATE_TAG, 60001),
            ("Instant", INSTANT_TAG, 60002),
            ("LocalDateTime", LOCAL_DATE_TIME_TAG, 60003),
            ("TimeZone", TIME_ZONE_TAG, 60004),
            ("Duration", DURATION_TAG, 60005),
            ("Quantity", QUANTITY_TAG, 60006),
            ("Money", MONEY_TAG, 60007),
            ("Enum", ENUM_TAG, 60008),
            ("Record", RECORD_TAG, 60009),
            ("PinnedRowReference", PINNED_ROW_REFERENCE_TAG, 60010),
            ("Diagnostic", DIAGNOSTIC_TAG, 60011),
            ("PresentNode", PRESENT_NODE_TAG, 60012),
            ("Option", OPTION_TAG, 60013),
            ("Unit", UNIT_TAG, 60014),
            ("Tuple", TUPLE_TAG, 60015),
            ("Error", ERROR_TAG, 60016),
            ("TimeOfDay", TIME_OF_DAY_TAG, 60017),
            ("ZonedDateTime", ZONED_DATE_TIME_TAG, 60018),
            ("Range", RANGE_TAG, 60019),
            ("RefinedNominal", REFINED_NOMINAL_TAG, 60020),
            ("StoredTableReference", STORED_TABLE_REFERENCE_TAG, 60021),
            ("LiveHandle", LIVE_HANDLE_TAG, 60022),
            ("OpaqueSystemId", OPAQUE_SYSTEM_ID_TAG, 60023),
            ("Path", PATH_TAG, 60024),
            ("Digest", DIGEST_TAG, 60025),
            ("SysValueBox", SYS_VALUE_BOX_TAG, 60026),
            ("Relation", RELATION_TAG, 60027),
            ("Ovb2Blob", OVB2_BLOB_TAG, 60110),
            ("Rov3Blob", ROV3_BLOB_TAG, 60111),
            ("Sov3Blob", SOV3_BLOB_TAG, 60112),
            ("Rov3Overflow", ROV3_OVERFLOW_TAG, 60113),
        ] {
            assert_eq!(actual, expected, "{name}");
        }
    }
}
