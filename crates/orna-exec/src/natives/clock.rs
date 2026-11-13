//! The clock and entropy intrinsics: `now` and `uuid7`.
//!
//! `now` returns the activation's instant, which the machine read from the host once when it started (ORNA-TXN-005),
//! so every call in one activation is equal. `uuid7` takes its timestamp from that same instant and its random bits
//! from the host's entropy (`docs/design/hir-host.md` section A1).

use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

/// The largest unix millisecond a version 7 UUID carries: its timestamp is 48 bits.
const UUID7_MAX_MILLIS: u64 = (1 << 48) - 1;

/// `now(): Instant`, the instant of the activation.
pub(crate) fn now(machine: &mut Machine<'_>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let instant = machine.activation_time;
    Ok(Value::Instant {
        unix_seconds: instant.unix_seconds,
        nanosecond: instant.nanos,
    })
}

/// `uuid7(): Uuid`, a version 7 UUID: the activation's unix milliseconds, then random bits from the host.
pub(crate) fn uuid7(machine: &mut Machine<'_>) -> Result<Value, Stop> {
    machine.tick(1)?;
    let instant = machine.activation_time;
    let millis = u64::try_from(instant.unix_seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .and_then(|seconds| seconds.checked_add(u64::from(instant.nanos / 1_000_000)))
        .filter(|millis| *millis <= UUID7_MAX_MILLIS)
        .ok_or(Stop::Error("ORNA-EVAL-VALUE"))?;
    let mut random = [0_u8; 10];
    machine
        .host
        .entropy(&mut random)
        .map_err(|failure| Stop::Error(failure.code))?;
    Ok(Value::Uuid(uuid7_bytes(millis, random)))
}

/// Lays out a version 7 UUID: 48 bits of milliseconds, the version nibble 7, 12 random bits, the variant bits `10`,
/// and 62 random bits. `random` supplies the 74 random bits in order.
fn uuid7_bytes(millis: u64, random: [u8; 10]) -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = 0x70 | (random[0] & 0x0f);
    bytes[7] = random[1];
    bytes[8] = 0x80 | (random[2] & 0x3f);
    bytes[9..].copy_from_slice(&random[3..]);
    bytes
}
