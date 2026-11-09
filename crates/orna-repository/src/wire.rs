//! Length-prefixed journal fields shared by the publication, checkout and
//! compact journals.
//!
//! The journals predate this module and already have published byte layouts:
//! the publication and checkout journals are little-endian and the compact
//! publication journal is big-endian. Both orders therefore keep their own
//! module, generated from one body, so an endianness is chosen once per
//! caller instead of being re-decided at every field.
//!
//! Fields are a `u32` length followed by that many bytes. Optional fields
//! spell absence as `u32::MAX`, and every length read is bounded before a
//! slice is taken, so a malformed journal cannot ask for an unbounded read.

/// Most bytes one journal file may occupy, and the bound a declared optional
/// field length must stay under before it is read.
pub(crate) const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;

macro_rules! wire_codec {
    (
        $module:ident,
        $to_bytes:ident,
        $from_bytes:ident,
        { $($extra:item)* }
    ) => {
        pub(crate) mod $module {
            use crate::RepositoryError;

            fn encode_u32(value: u32) -> [u8; 4] {
                value.$to_bytes()
            }

            fn decode_u32(bytes: [u8; 4]) -> u32 {
                u32::$from_bytes(bytes)
            }

            pub(crate) fn put_u32(bytes: &mut Vec<u8>, value: usize) -> Result<(), RepositoryError> {
                let value =
                    u32::try_from(value).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
                bytes.extend_from_slice(&encode_u32(value));
                Ok(())
            }

            pub(crate) fn put_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), RepositoryError> {
                put_u32(bytes, value.len())?;
                bytes.extend_from_slice(value.as_bytes());
                Ok(())
            }

            pub(crate) fn take_fixed<const N: usize>(
                bytes: &[u8],
                cursor: &mut usize,
            ) -> Result<[u8; N], RepositoryError> {
                let end = cursor
                    .checked_add(N)
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                let value = bytes
                    .get(*cursor..end)
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                *cursor = end;
                value
                    .try_into()
                    .map_err(|_| RepositoryError::InvalidPublicationJournal)
            }

            pub(crate) fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, RepositoryError> {
                Ok(decode_u32(take_fixed::<4>(bytes, cursor)?))
            }

            pub(crate) fn take_string(
                bytes: &[u8],
                cursor: &mut usize,
            ) -> Result<String, RepositoryError> {
                let length = usize::try_from(take_u32(bytes, cursor)?)
                    .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
                let end = cursor
                    .checked_add(length)
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                let value = bytes
                    .get(*cursor..end)
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                *cursor = end;
                String::from_utf8(value.to_vec())
                    .map_err(|_| RepositoryError::InvalidPublicationJournal)
            }

            $($extra)*
        }
    };
}

wire_codec!(little, to_le_bytes, from_le_bytes, {
    use super::MAX_JOURNAL_BYTES;

    pub(crate) fn put_optional_string(
        bytes: &mut Vec<u8>,
        value: Option<&str>,
    ) -> Result<(), RepositoryError> {
        match value {
            Some(value) => put_string(bytes, value),
            None => {
                bytes.extend_from_slice(&encode_u32(u32::MAX));
                Ok(())
            }
        }
    }

    pub(crate) fn put_optional_bytes(
        bytes: &mut Vec<u8>,
        value: Option<&[u8]>,
    ) -> Result<(), RepositoryError> {
        match value {
            Some(value) => {
                let length = u32::try_from(value.len())
                    .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
                bytes.extend_from_slice(&encode_u32(length));
                bytes.extend_from_slice(value);
            }
            None => bytes.extend_from_slice(&encode_u32(u32::MAX)),
        }
        Ok(())
    }

    pub(crate) fn take_byte(bytes: &[u8], cursor: &mut usize) -> Result<u8, RepositoryError> {
        let value = *bytes
            .get(*cursor)
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        *cursor += 1;
        Ok(value)
    }

    /// Takes an optional field's declared length, or `None` for the
    /// `u32::MAX` marker. A declared length above the journal bound is
    /// refused before the cursor moves.
    fn take_optional_length(
        bytes: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<usize>, RepositoryError> {
        let length = take_u32(bytes, cursor)?;
        if length == u32::MAX {
            return Ok(None);
        }
        let length =
            usize::try_from(length).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        if length > MAX_JOURNAL_BYTES {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(Some(length))
    }

    pub(crate) fn take_optional_string(
        bytes: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<String>, RepositoryError> {
        let Some(length) = take_optional_length(bytes, cursor)? else {
            return Ok(None);
        };
        let end = cursor
            .checked_add(length)
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        let value = bytes
            .get(*cursor..end)
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        *cursor = end;
        Ok(Some(
            String::from_utf8(value.to_vec())
                .map_err(|_| RepositoryError::InvalidPublicationJournal)?,
        ))
    }

    pub(crate) fn take_optional_bytes(
        bytes: &[u8],
        cursor: &mut usize,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        let Some(length) = take_optional_length(bytes, cursor)? else {
            return Ok(None);
        };
        let end = cursor
            .checked_add(length)
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        let value = bytes
            .get(*cursor..end)
            .ok_or(RepositoryError::InvalidPublicationJournal)?
            .to_vec();
        *cursor = end;
        Ok(Some(value))
    }
});

wire_codec!(big, to_be_bytes, from_be_bytes, {
    pub(crate) fn put_u64(bytes: &mut Vec<u8>, value: u64) {
        bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn take_u64(bytes: &[u8], cursor: &mut usize) -> Result<u64, RepositoryError> {
        Ok(u64::from_be_bytes(take_fixed::<8>(bytes, cursor)?))
    }
});

#[cfg(test)]
mod tests {
    use super::{big, little};

    /// Both orders keep the byte layout their journals already publish, so a
    /// shared codec never silently rewrites an existing field.
    #[test]
    fn each_order_keeps_its_published_bytes() {
        let mut encoded = Vec::new();
        little::put_string(&mut encoded, "ab").expect("little string");
        assert_eq!(encoded, [2, 0, 0, 0, b'a', b'b']);
        let mut encoded = Vec::new();
        big::put_string(&mut encoded, "ab").expect("big string");
        assert_eq!(encoded, [0, 0, 0, 2, b'a', b'b']);

        let mut encoded = Vec::new();
        little::put_optional_string(&mut encoded, None).expect("absent little string");
        assert_eq!(encoded, u32::MAX.to_le_bytes());
        let mut encoded = Vec::new();
        big::put_u64(&mut encoded, 1);
        assert_eq!(encoded, [0, 0, 0, 0, 0, 0, 0, 1]);

        let mut encoded = Vec::new();
        little::put_string(&mut encoded, "ab").expect("little string");
        let mut cursor = 0;
        assert_eq!(
            little::take_string(&encoded, &mut cursor).expect("little string"),
            "ab"
        );
        assert_eq!(cursor, encoded.len());
        let mut cursor = 0;
        assert!(big::take_string(&encoded, &mut cursor).is_err());
    }
}
