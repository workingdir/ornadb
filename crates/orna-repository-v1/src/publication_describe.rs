//! Human-readable dumps of protected content pins and their publication
//! descriptors.
//!
//! The output is for operators and recovery reports. It is not a stable wire
//! format and is never parsed back. Every value is printed in full, so a dump
//! can be compared byte for byte against a fresh dump of the same pin.

use std::fmt::Write;

use crate::{ProtectedContentPin, ProtectedContentTransfer};

/// Describes one protected pin, including the publication descriptor it
/// protects.
pub fn describe_pin(pin: &ProtectedContentPin) -> String {
    describe_transfer(&pin.transfer_record())
}

/// Describes the transfer record that binds a pin to its publication
/// descriptor.
pub fn describe_transfer(transfer: &ProtectedContentTransfer) -> String {
    let identity = transfer.content_identity();
    let mut out = String::new();
    // Writing into a String cannot fail.
    let _ = writeln!(out, "pin {}", hex(transfer.pin_id()));
    let _ = writeln!(out, "  repository {}", hex(transfer.repository_id()));
    let _ = writeln!(out, "  database {}", hex(transfer.database_id()));
    let _ = writeln!(out, "  owner {}", hex(transfer.owner_id()));
    let _ = writeln!(
        out,
        "  descriptor {:?} {}",
        transfer.descriptor_oid().algorithm(),
        transfer.descriptor_oid().to_hex()
    );
    let _ = writeln!(out, "  content length {}", identity.length());
    let _ = writeln!(out, "  content sha256 {}", hex(&identity.sha256()));
    out
}

/// Describes every pin in order, separated by a blank line.
pub fn describe_pins<'a>(pins: impl IntoIterator<Item = &'a ProtectedContentPin>) -> String {
    pins.into_iter()
        .map(describe_pin)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Describes one protected pin as a single-line JSON object. Every value is a
/// hex string, a decimal number, or a fixed algorithm name, so no escaping is
/// needed.
pub fn describe_pin_json(pin: &ProtectedContentPin) -> String {
    describe_transfer_json(&pin.transfer_record())
}

/// Describes the transfer record as a single-line JSON object with the same
/// fields as `describe_transfer`.
pub fn describe_transfer_json(transfer: &ProtectedContentTransfer) -> String {
    let identity = transfer.content_identity();
    format!(
        "{{\"pin_id\":\"{}\",\"repository_id\":\"{}\",\"database_id\":\"{}\",\"owner_id\":\"{}\",\"descriptor\":{{\"algorithm\":\"{:?}\",\"oid\":\"{}\"}},\"content\":{{\"length\":{},\"sha256\":\"{}\"}}}}",
        hex(transfer.pin_id()),
        hex(transfer.repository_id()),
        hex(transfer.database_id()),
        hex(transfer.owner_id()),
        transfer.descriptor_oid().algorithm(),
        transfer.descriptor_oid().to_hex(),
        identity.length(),
        hex(&identity.sha256()),
    )
}

/// Describes every pin as a JSON array, in order.
pub fn describe_pins_json<'a>(pins: impl IntoIterator<Item = &'a ProtectedContentPin>) -> String {
    let entries = pins
        .into_iter()
        .map(describe_pin_json)
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(","))
}

/// The column names for `describe_pins_csv`, in the order each row prints
/// them.
pub const PIN_CSV_HEADER: &str = "pin_id,repository_id,database_id,owner_id,descriptor_algorithm,descriptor_oid,content_length,content_sha256";

/// Describes one protected pin as a single CSV row with the columns in
/// `PIN_CSV_HEADER`. Every value is a hex string, a decimal number, or a fixed
/// algorithm name, so no quoting is needed.
pub fn describe_pin_csv(pin: &ProtectedContentPin) -> String {
    describe_transfer_csv(&pin.transfer_record())
}

/// Describes the transfer record as one CSV row with the same fields as
/// `describe_transfer`.
pub fn describe_transfer_csv(transfer: &ProtectedContentTransfer) -> String {
    let identity = transfer.content_identity();
    format!(
        "{},{},{},{},{:?},{},{},{}",
        hex(transfer.pin_id()),
        hex(transfer.repository_id()),
        hex(transfer.database_id()),
        hex(transfer.owner_id()),
        transfer.descriptor_oid().algorithm(),
        transfer.descriptor_oid().to_hex(),
        identity.length(),
        hex(&identity.sha256()),
    )
}

/// Describes every pin as CSV: the header row followed by one row per pin, in
/// order, each terminated by a newline.
pub fn describe_pins_csv<'a>(pins: impl IntoIterator<Item = &'a ProtectedContentPin>) -> String {
    let mut out = String::new();
    out.push_str(PIN_CSV_HEADER);
    out.push('\n');
    for pin in pins {
        out.push_str(&describe_pin_csv(pin));
        out.push('\n');
    }
    out
}

/// The column names for `describe_pins_tsv`, in the order each row prints
/// them.
pub const PIN_TSV_HEADER: &str = "pin_id\trepository_id\tdatabase_id\towner_id\tdescriptor_algorithm\tdescriptor_oid\tcontent_length\tcontent_sha256";

/// Describes one protected pin as a single TSV row with the columns in
/// `PIN_TSV_HEADER`. Every value is a hex string, a decimal number, or a fixed
/// algorithm name, so no escaping is needed.
pub fn describe_pin_tsv(pin: &ProtectedContentPin) -> String {
    describe_transfer_tsv(&pin.transfer_record())
}

/// Describes the transfer record as one TSV row with the same fields as
/// `describe_transfer`.
pub fn describe_transfer_tsv(transfer: &ProtectedContentTransfer) -> String {
    let identity = transfer.content_identity();
    format!(
        "{}\t{}\t{}\t{}\t{:?}\t{}\t{}\t{}",
        hex(transfer.pin_id()),
        hex(transfer.repository_id()),
        hex(transfer.database_id()),
        hex(transfer.owner_id()),
        transfer.descriptor_oid().algorithm(),
        transfer.descriptor_oid().to_hex(),
        identity.length(),
        hex(&identity.sha256()),
    )
}

/// Describes every pin as TSV: the header row followed by one row per pin, in
/// order, each terminated by a newline.
pub fn describe_pins_tsv<'a>(pins: impl IntoIterator<Item = &'a ProtectedContentPin>) -> String {
    let mut out = String::new();
    out.push_str(PIN_TSV_HEADER);
    out.push('\n');
    for pin in pins {
        out.push_str(&describe_pin_tsv(pin));
        out.push('\n');
    }
    out
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
