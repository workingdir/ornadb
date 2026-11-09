//! Primitive encodings shared by every OrnaDB layer.
//!
//! This is the only crate that may name the `hex` or `base64` crates
//! (`deny.toml` bans both elsewhere), and the only crate that implements the
//! lowercase hexadecimal and RFC 4648 Base64 spellings the workspace writes.
//! Every other layer depends on this crate instead of re-implementing them.
//!
//! The module also owns the Git-metadata path predicate because both the
//! repository layer and the host layer below it must agree on it.

mod base64_codec;
mod hex_codec;

pub mod git;

pub use base64_codec::{
    decode as base64_decode, decode_url as base64_decode_url, encode as base64_encode,
    encode_url as base64_encode_url,
};
pub use git::is_git_metadata_component;
pub use hex_codec::{decode as hex_decode, digit as hex_digit, hex};

/// A codec rejected its input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecError {
    /// The text was not canonical lowercase hexadecimal of even length.
    InvalidHex,
    /// The text was not canonical RFC 4648 Base64 in the requested alphabet.
    InvalidBase64,
}
