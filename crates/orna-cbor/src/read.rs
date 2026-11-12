use std::marker::PhantomData;

use crate::{Build, CANONICAL_NAN_BITS, Error, Limits, is_nan_bits};

/// Parses canonical CBOR into the model `B`, under the bounds in [`Limits`].
///
/// The reader rejects every encoding the encoder would not write: non-shortest
/// heads, a NaN other than [`CANONICAL_NAN_BITS`], unsorted or duplicate map
/// keys, and bignums that a 64-bit head could carry. A value it accepts
/// therefore re-encodes to the bytes it came from.
pub struct Reader<'a, B> {
    bytes: &'a [u8],
    at: usize,
    items: usize,
    limits: Limits,
    model: PhantomData<fn() -> B>,
}

impl<'a, B: Build> Reader<'a, B> {
    /// A reader over the whole of `bytes`, positioned at its first item.
    pub fn new(bytes: &'a [u8], limits: Limits) -> Self {
        Self {
            bytes,
            at: 0,
            items: 0,
            limits,
            model: PhantomData,
        }
    }

    /// Whether every byte has been consumed.
    pub fn is_finished(&self) -> bool {
        self.at == self.bytes.len()
    }

    /// Reads the next top-level item.
    pub fn value(&mut self) -> Result<B::Value, Error> {
        self.item(0)
    }

    fn item(&mut self, depth: usize) -> Result<B::Value, Error> {
        self.enter(depth)?;
        let (major, additional) = self.initial()?;
        self.rest(major, additional, depth)
    }

    /// Counts one item at `depth` against the limits.
    fn enter(&mut self, depth: usize) -> Result<(), Error> {
        if depth > self.limits.max_depth || self.items >= self.limits.max_items {
            return Err(Error::Limit);
        }
        self.items += 1;
        Ok(())
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(count).ok_or(Error::Limit)?;
        let bytes = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(bytes)
    }

    /// The major type and additional information of the next head.
    fn initial(&mut self) -> Result<(u8, u8), Error> {
        let first = self.take(1)?[0];
        Ok((first >> 5, first & 31))
    }

    /// The argument of a head whose additional information is `additional`,
    /// read in the shortest form only.
    fn argument(&mut self, additional: u8) -> Result<u64, Error> {
        let (minimum, width): (u64, usize) = match additional {
            0..=23 => return Ok(additional as u64),
            24 => (24, 1),
            25 => (256, 2),
            26 => (65_536, 4),
            27 => (4_294_967_296, 8),
            _ => return Err(Error::Unsupported),
        };
        let mut value: u64 = 0;
        for byte in self.take(width)? {
            value = value << 8 | *byte as u64;
        }
        if value < minimum {
            return Err(Error::NonCanonical);
        }
        Ok(value)
    }

    /// The rest of an item whose head has been read.
    fn rest(&mut self, major: u8, additional: u8, depth: usize) -> Result<B::Value, Error> {
        if major == 7 {
            return self.simple(additional);
        }
        let argument = self.argument(additional)?;
        match major {
            0 => Ok(B::unsigned(argument)),
            1 => Ok(B::negative(argument)),
            2 => Ok(B::bytes(self.take(argument as usize)?)),
            3 => {
                let text = self.take(argument as usize)?;
                let text = std::str::from_utf8(text).map_err(|_| Error::InvalidUtf8)?;
                Ok(B::text(text))
            }
            4 => {
                let mut values = Vec::new();
                for _ in 0..argument {
                    values.push(self.item(depth + 1)?);
                }
                Ok(B::array(values))
            }
            5 => {
                let bytes = self.bytes;
                let mut entries = Vec::new();
                let mut last: Option<&'a [u8]> = None;
                for _ in 0..argument {
                    let start = self.at;
                    let key = self.item(depth + 1)?;
                    let encoded = &bytes[start..self.at];
                    if last.is_some_and(|previous| encoded <= previous) {
                        return Err(Error::DuplicateOrUnorderedMapKey);
                    }
                    last = Some(encoded);
                    let value = self.item(depth + 1)?;
                    entries.push((key, value));
                }
                Ok(B::map(entries))
            }
            6 => self.tagged(argument, depth),
            _ => Err(Error::Unsupported),
        }
    }

    fn simple(&mut self, additional: u8) -> Result<B::Value, Error> {
        match additional {
            20 => Ok(B::boolean(false)),
            21 => Ok(B::boolean(true)),
            22 => Ok(B::null()),
            27 => {
                let mut bits = [0; 8];
                bits.copy_from_slice(self.take(8)?);
                let bits = u64::from_be_bytes(bits);
                if is_nan_bits(bits) && bits != CANONICAL_NAN_BITS {
                    return Err(Error::NonCanonical);
                }
                Ok(B::float(bits))
            }
            _ => Err(Error::Unsupported),
        }
    }

    fn tagged(&mut self, number: u64, depth: usize) -> Result<B::Value, Error> {
        if number == 2 || number == 3 {
            let magnitude = self.magnitude(depth + 1)?;
            return Ok(B::bignum(number == 3, magnitude));
        }
        let value = self.item(depth + 1)?;
        Ok(B::tag(number, value))
    }

    /// The payload of a bignum tag: a byte string of at least nine bytes with
    /// no leading zero, since any shorter integer has a 64-bit head. A payload
    /// of any other major type is read whole before it is refused, so its own
    /// errors take precedence.
    fn magnitude(&mut self, depth: usize) -> Result<&'a [u8], Error> {
        self.enter(depth)?;
        let (major, additional) = self.initial()?;
        if major != 2 {
            self.rest(major, additional, depth)?;
            return Err(Error::BignumPayload);
        }
        let argument = self.argument(additional)?;
        let magnitude = self.take(argument as usize)?;
        if magnitude.len() < 9 || magnitude[0] == 0 {
            return Err(Error::NonCanonical);
        }
        Ok(magnitude)
    }
}
