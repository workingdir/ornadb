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

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}
