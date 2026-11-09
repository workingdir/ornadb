//! Canonical CBOR encoding and decoding primitives.

use crate::error::Error;
use crate::raw::{Raw, bytes, is_nan_bits, tag};
use crate::validation::validate_raw;
use crate::{CANONICAL_NAN_BITS, MAX_DEPTH, MAX_ITEMS, Result};
use num_bigint::BigInt;
use num_bigint::Sign;
use num_traits::Zero;

pub(crate) fn encode_raw(v: &Raw) -> Result<Vec<u8>> {
    validate_raw(v, 0)?;
    let mut out = Vec::new();
    write_raw(v, &mut out)?;
    Ok(out)
}

pub(crate) fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    if n < 24 {
        out.push(major << 5 | n as u8)
    } else if n <= u8::MAX as u64 {
        out.extend([major << 5 | 24, n as u8])
    } else if n <= u16::MAX as u64 {
        out.push(major << 5 | 25);
        out.extend((n as u16).to_be_bytes())
    } else if n <= u32::MAX as u64 {
        out.push(major << 5 | 26);
        out.extend((n as u32).to_be_bytes())
    } else {
        out.push(major << 5 | 27);
        out.extend(n.to_be_bytes())
    }
}

pub(crate) fn write_raw(v: &Raw, out: &mut Vec<u8>) -> Result<()> {
    match v {
        Raw::Null => {
            out.push(0xf6);
        }
        Raw::Bool(false) => {
            out.push(0xf4);
        }
        Raw::Bool(true) => {
            out.push(0xf5);
        }
        Raw::Int(i) => {
            let neg = i.sign() == Sign::Minus;
            let n = if neg { -i - BigInt::from(1) } else { i.clone() };
            if n.is_zero() {
                head(out, if neg { 1 } else { 0 }, 0);
            } else if n.to_u64_digits().1.len() == 1 {
                let limb = n.to_u64_digits().1[0];
                head(out, if neg { 1 } else { 0 }, limb);
            } else {
                let (_, b) = n.to_bytes_be();
                head(out, 6, if neg { 3 } else { 2 });
                head(out, 2, b.len() as u64);
                out.extend(b)
            }
        }
        Raw::Float(bits) => {
            out.push(0xfb);
            out.extend(
                (if is_nan_bits(*bits) {
                    CANONICAL_NAN_BITS
                } else {
                    *bits
                })
                .to_be_bytes(),
            )
        }
        Raw::Bytes(b) => {
            head(out, 2, b.len() as u64);
            out.extend(b)
        }
        Raw::Text(s) => {
            head(out, 3, s.len() as u64);
            out.extend(s.as_bytes())
        }
        Raw::Array(a) => {
            head(out, 4, a.len() as u64);
            for x in a {
                write_raw(x, out)?
            }
        }
        Raw::Map(m) => {
            let mut items = Vec::with_capacity(m.len());
            for (k, v) in m {
                items.push((encode_raw(k)?, v))
            }
            items.sort_by(|a, b| a.0.cmp(&b.0));
            if items.windows(2).any(|x| x[0].0 == x[1].0) {
                return Err(Error::DuplicateOrUnorderedMapKey);
            }
            head(out, 5, items.len() as u64);
            for (k, v) in items {
                out.extend(k);
                write_raw(v, out)?
            }
        }
        Raw::Tag(n, v) => {
            head(out, 6, *n);
            write_raw(v, out)?
        }
    }
    Ok(())
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pub(crate) at: usize,
    items: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            items: 0,
        }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let e = self.at.checked_add(n).ok_or(Error::Limit)?;
        let x = self.bytes.get(self.at..e).ok_or(Error::Truncated)?;
        self.at = e;
        Ok(x)
    }
    fn arg(&mut self, ai: u8) -> Result<u64> {
        let (n, w) = match ai {
            0..=23 => return Ok(ai as u64),
            24 => (24, 1),
            25 => (256, 2),
            26 => (65536, 4),
            27 => (4294967296, 8),
            _ => return Err(Error::Unsupported),
        };
        let mut x = 0;
        for b in self.take(w)? {
            x = x << 8 | *b as u64
        }
        if x < n {
            return Err(Error::NonCanonical);
        }
        Ok(x)
    }
    pub(crate) fn raw(&mut self, depth: usize) -> Result<Raw> {
        if depth > MAX_DEPTH || self.items >= MAX_ITEMS {
            return Err(Error::Limit);
        }
        self.items += 1;
        let first = self.take(1)?[0];
        let major = first >> 5;
        let ai = first & 31;
        if major == 7 {
            return match ai {
                20 => Ok(Raw::Bool(false)),
                21 => Ok(Raw::Bool(true)),
                22 => Ok(Raw::Null),
                27 => {
                    let mut a = [0; 8];
                    a.copy_from_slice(self.take(8)?);
                    let bits = u64::from_be_bytes(a);
                    if is_nan_bits(bits) && bits != CANONICAL_NAN_BITS {
                        return Err(Error::NonCanonical);
                    }
                    Ok(Raw::Float(bits))
                }
                _ => Err(Error::Unsupported),
            };
        }
        let n = self.arg(ai)?;
        match major {
            0 => Ok(Raw::Int(BigInt::from(n))),
            1 => Ok(Raw::Int(-BigInt::from(n) - 1)),
            2 => Ok(Raw::Bytes(self.take(n as usize)?.to_vec())),
            3 => Ok(Raw::Text(
                String::from_utf8(self.take(n as usize)?.to_vec())
                    .map_err(|_| Error::InvalidUtf8)?,
            )),
            4 => {
                let mut a = Vec::new();
                for _ in 0..n {
                    a.push(self.raw(depth + 1)?)
                }
                Ok(Raw::Array(a))
            }
            5 => {
                let mut m = Vec::new();
                let mut last = None;
                for _ in 0..n {
                    let start = self.at;
                    let k = self.raw(depth + 1)?;
                    let kb = &self.bytes[start..self.at];
                    if last.as_ref().is_some_and(|x: &Vec<u8>| kb <= x.as_slice()) {
                        return Err(Error::DuplicateOrUnorderedMapKey);
                    }
                    last = Some(kb.to_vec());
                    let v = self.raw(depth + 1)?;
                    m.push((k, v))
                }
                Ok(Raw::Map(m))
            }
            6 => {
                let v = self.raw(depth + 1)?;
                if n == 2 || n == 3 {
                    let b = bytes(&v)?;
                    if b.is_empty() || b[0] == 0 || b.len() < 9 {
                        return Err(Error::NonCanonical);
                    }
                    let i = BigInt::from_bytes_be(Sign::Plus, b);
                    return Ok(Raw::Int(if n == 2 { i } else { -i - 1 }));
                }
                Ok(tag(n, v))
            }
            _ => Err(Error::Unsupported),
        }
    }
}
