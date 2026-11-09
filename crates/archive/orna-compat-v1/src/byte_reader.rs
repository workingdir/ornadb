//! One big-endian byte reader shared by the crate's binary codecs.
//!
//! Each codec reports truncation through its own error, so the reader is generic
//! over that error and can fail in exactly one way: a field needs more bytes
//! than remain.

use std::marker::PhantomData;

/// A codec error that a truncated read maps to.
pub(crate) trait TruncatedError: Sized {
    /// Returns the codec's truncation error.
    fn truncated() -> Self;
}

/// A cursor that reads big-endian fields from a byte slice.
///
/// The error type is a type parameter rather than an associated output so that
/// an existing `Reader<'_>` signature only gains one caller-chosen argument.
pub(crate) struct Reader<'a, E> {
    bytes: &'a [u8],
    position: usize,
    error: PhantomData<fn() -> E>,
}

impl<'a, E: TruncatedError> Reader<'a, E> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            error: PhantomData,
        }
    }

    /// Returns the current offset from the start of the slice.
    pub(crate) fn position(&self) -> usize {
        self.position
    }

    /// Takes the next `length` bytes, or fails when fewer than `length` remain.
    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], E> {
        let end = self.position.checked_add(length).ok_or_else(E::truncated)?;
        let taken = self
            .bytes
            .get(self.position..end)
            .ok_or_else(E::truncated)?;
        self.position = end;
        Ok(taken)
    }

    /// Takes the next `N` bytes as a fixed-size array.
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], E> {
        self.take(N)?.try_into().map_err(|_| E::truncated())
    }

    pub(crate) fn u8(&mut self) -> Result<u8, E> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    pub(crate) fn u16(&mut self) -> Result<u16, E> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, E> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    /// Reads a big-endian `u32` length or count as a `usize`.
    pub(crate) fn length(&mut self) -> Result<usize, E> {
        Ok(self.u32()? as usize)
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Returns every byte past the current position.
    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.bytes[self.position..]
    }
}

/// The opaque-codec validators collapse every failure into `()`, so a truncated
/// read is indistinguishable from any other rejection they report.
impl TruncatedError for () {
    fn truncated() -> Self {}
}
