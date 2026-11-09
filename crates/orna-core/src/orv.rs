//! Layout constants of the Orna runtime value (ORV) wire format.
//!
//! Every ORV frame opens with a four-byte marker, a one-byte tag, a
//! sixteen-byte type identity, and a big-endian `u32` body length. Inspect
//! carriers and the runtime value codecs both validate that layout, so they
//! share these constants instead of restating the numbers.

/// The marker that opens an ORV3 value frame.
pub(crate) const ORV3_MARKER: &[u8; 4] = b"ORV3";

/// The marker that opens an ORV5 value frame or descriptor.
pub(crate) const ORV5_MARKER: &[u8; 4] = b"ORV5";

/// Bytes in the fixed header shared by ORV3 and ORV5 value frames.
pub(crate) const ORV_HEADER_BYTES: usize = 4 + 1 + 16 + 4;

/// The deepest nesting of ORV5 values and descriptors one cell may carry.
pub(crate) const ORV_MAX_VALUE_DEPTH: usize = 32;

/// The fewest bytes one ORV5 record field occupies: its sixteen-byte identity
/// and the `u32` length of its nested value.
pub(crate) const ORV_RECORD_FIELD_HEADER_BYTES: usize = 16 + 4;

/// The largest body one ORV5 value may declare.
pub(crate) const ORV_MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
