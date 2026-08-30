//! Bounded binary primitives shared by canonical artifact formats.

use orna_core::{FieldId, FunctionId, ParameterId, TypeId};

/// Format-neutral failures produced while traversing canonical bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecodeError {
    Truncated,
    TrailingBytes,
}

/// Canonical big-endian artifact writer.
#[derive(Default)]
pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn i64(&mut self, value: i64) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn boolean(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    pub(crate) fn type_id(&mut self, value: TypeId) {
        self.bytes(&value.to_bytes());
    }

    pub(crate) fn field_id(&mut self, value: FieldId) {
        self.bytes(&value.to_bytes());
    }

    pub(crate) fn function_id(&mut self, value: FunctionId) {
        self.bytes(&value.to_bytes());
    }

    pub(crate) fn parameter_id(&mut self, value: ParameterId) {
        self.bytes(&value.to_bytes());
    }
}

/// Bounds-checked cursor over one canonical artifact.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(DecodeError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(DecodeError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    pub(crate) fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], DecodeError> {
        self.take(LENGTH)?
            .try_into()
            .map_err(|_| DecodeError::Truncated)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    pub(crate) fn type_id(&mut self) -> Result<TypeId, DecodeError> {
        Ok(TypeId::from_bytes(self.array()?))
    }

    pub(crate) fn field_id(&mut self) -> Result<FieldId, DecodeError> {
        Ok(FieldId::from_bytes(self.array()?))
    }

    pub(crate) fn function_id(&mut self) -> Result<FunctionId, DecodeError> {
        Ok(FunctionId::from_bytes(self.array()?))
    }

    pub(crate) fn parameter_id(&mut self) -> Result<ParameterId, DecodeError> {
        Ok(ParameterId::from_bytes(self.array()?))
    }

    pub(crate) fn require_finished(&self) -> Result<(), DecodeError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes)
        }
    }
}
