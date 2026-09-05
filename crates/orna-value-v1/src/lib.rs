//! The closed Orna 1.0 canonical-value boundary (OVB-1).
//!
//! This crate deliberately has no storage or runtime dependencies.  Values are
//! validated before they become digest input, and errors describe malformed
//! structure without echoing decoded payloads.

use std::{cmp::Ordering, collections::BTreeMap, fmt};

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, Zero};
use sha2::{Digest as _, Sha256};

pub const OVB_VERSION: &str = "OVB-1";
pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;
const MAX_DEPTH: usize = 128;
const MAX_ITEMS: usize = 1_000_000;

/// A deliberately payload-free failure.  Do not use `Debug` output of input
/// values in diagnostics or logs at this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    TrailingBytes,
    Limit,
    Unsupported,
    NonCanonical,
    InvalidUtf8,
    InvalidTag,
    InvalidValue,
    DuplicateOrUnorderedMapKey,
    ProtectedValue,
    InvalidPath,
    InvalidSchema,
    DecimalLimit,
    DivisionByZero,
    NonFiniteDecimal,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OVB-1 {self:?}")
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Canonical CBOR's supported data model. `Tag` is public for schema and
/// protocol adapters, but both construction and decoding validate the closed
/// Orna tag registry before bytes may be emitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Raw {
    Null,
    Bool(bool),
    Int(BigInt),
    Float(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Raw>),
    Map(Vec<(Raw, Raw)>),
    Tag(u64, Box<Raw>),
}

/// A validated OVB-1 value.  The inner raw value is never exposed mutably.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Value(Raw);
impl Value {
    pub fn new(raw: Raw) -> Result<Self> {
        validate_raw(&raw, 0)?;
        Ok(Self(raw))
    }
    pub fn raw(&self) -> &Raw {
        &self.0
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_raw(&self.0)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        let raw = r.raw(0)?;
        if r.at != bytes.len() {
            return Err(Error::TrailingBytes);
        }
        validate_raw(&raw, 0)?;
        if encode_raw(&raw)? != bytes {
            return Err(Error::NonCanonical);
        }
        Ok(Self(raw))
    }
    pub fn int(v: BigInt) -> Self {
        Self(Raw::Int(v))
    }
    pub fn float_bits(bits: u64) -> Self {
        Self(Raw::Float(if is_nan_bits(bits) {
            CANONICAL_NAN_BITS
        } else {
            bits
        }))
    }
    pub fn decimal(coefficient: BigInt, exponent10: BigInt) -> Result<Self> {
        let (c, e) = normal_decimal(coefficient, exponent10);
        Self::new(tag(60000, Raw::Array(vec![Raw::Int(c), Raw::Int(e)])))
    }
    pub fn option(value: Option<Value>) -> Result<Self> {
        let mut x = vec![Raw::Int(0.into())];
        if let Some(v) = value {
            x[0] = Raw::Int(1.into());
            x.push(v.0);
        }
        Self::new(tag(60013, Raw::Array(x)))
    }
    pub fn unit() -> Self {
        Self(tag(60014, Raw::Array(vec![])))
    }
    pub fn uuid(bytes: [u8; 16]) -> Self {
        Self(tag(37, Raw::Bytes(bytes.to_vec())))
    }
    pub fn protected() -> Self {
        Self(Raw::Tag(0, Box::new(Raw::Null)))
    } // unencodable marker
}

fn tag(n: u64, raw: Raw) -> Raw {
    Raw::Tag(n, Box::new(raw))
}
fn is_nan_bits(bits: u64) -> bool {
    bits & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && bits & 0x000f_ffff_ffff_ffff != 0
}

/// IEEE-754 `totalOrder` key, defined directly by FLOAT-TOTAL-1.
pub const fn float_total_key(bits: u64) -> u64 {
    if bits >> 63 == 1 {
        !bits
    } else {
        bits ^ 0x8000_0000_0000_0000
    }
}
pub fn float_total_cmp(left: u64, right: u64) -> Ordering {
    float_total_key(left).cmp(&float_total_key(right))
}
pub fn float_ordinary_eq(left: u64, right: u64) -> bool {
    !is_nan_bits(left)
        && !is_nan_bits(right)
        && (left == right || ((left | right) & 0x7fff_ffff_ffff_ffff) == 0)
}
pub fn float_min(values: &[u64]) -> Option<u64> {
    aggregate(values, true)
}
pub fn float_max(values: &[u64]) -> Option<u64> {
    aggregate(values, false)
}
fn aggregate(values: &[u64], min: bool) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    if values.iter().any(|v| is_nan_bits(*v)) {
        return Some(CANONICAL_NAN_BITS);
    }
    let mut candidate = values[0];
    for &v in &values[1..] {
        if (float_total_cmp(v, candidate) == Ordering::Less) == min {
            candidate = v;
        }
    }
    Some(candidate)
}

/// Exact normalised decimal coefficient times 10 to its exponent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decimal {
    coefficient: BigInt,
    exponent10: BigInt,
}
impl Decimal {
    pub const MAX_ABS_EXPONENT: u64 = 1_000_000;
    fn new(coefficient: BigInt, exponent10: BigInt) -> Self {
        let (coefficient, exponent10) = normal_decimal(coefficient, exponent10);
        Self {
            coefficient,
            exponent10,
        }
    }
    pub fn try_new(coefficient: BigInt, exponent10: BigInt) -> Result<Self> {
        let decimal = Self::new(coefficient, exponent10);
        if decimal.exponent10.magnitude() > BigInt::from(Self::MAX_ABS_EXPONENT).magnitude() {
            return Err(Error::DecimalLimit);
        }
        Ok(decimal)
    }
    pub fn try_multiply(&self, other: &Self) -> Result<Self> {
        Self::try_new(
            &self.coefficient * &other.coefficient,
            &self.exponent10 + &other.exponent10,
        )
    }
    pub fn try_add(&self, other: &Self) -> Result<Self> {
        let e = if self.exponent10 < other.exponent10 {
            self.exponent10.clone()
        } else {
            other.exponent10.clone()
        };
        let shift_a = &self.exponent10 - &e;
        let shift_b = &other.exponent10 - &e;
        if shift_a > BigInt::from(Self::MAX_ABS_EXPONENT)
            || shift_b > BigInt::from(Self::MAX_ABS_EXPONENT)
        {
            return Err(Error::DecimalLimit);
        }
        let shift_a = bounded_exponent(&shift_a)?;
        let shift_b = bounded_exponent(&shift_b)?;
        Self::try_new(
            &self.coefficient * pow10(shift_a) + &other.coefficient * pow10(shift_b),
            e,
        )
    }
    /// Exact finite decimal division.  A denominator with primes other than 2
    /// and 5 has no finite base-10 result and is rejected without rounding.
    pub fn divide_exact(&self, other: &Self) -> Result<Self> {
        if other.coefficient.is_zero() {
            return Err(Error::DivisionByZero);
        }
        if self.coefficient.is_zero() {
            return Ok(Self::new(BigInt::zero(), BigInt::zero()));
        }
        let gcd = self.coefficient.gcd(&other.coefficient);
        let mut numerator = &self.coefficient / &gcd;
        let mut denominator = (&other.coefficient / gcd).abs();
        if other.coefficient.sign() == Sign::Minus {
            numerator = -numerator;
        }
        let mut twos = 0u64;
        let mut fives = 0u64;
        while (&denominator % BigInt::from(2u8)).is_zero() {
            denominator /= 2u8;
            twos += 1;
        }
        while (&denominator % BigInt::from(5u8)).is_zero() {
            denominator /= 5u8;
            fives += 1;
        }
        if denominator != BigInt::from(1) {
            return Err(Error::NonFiniteDecimal);
        }
        let scale = twos.max(fives);
        if scale > Self::MAX_ABS_EXPONENT {
            return Err(Error::DecimalLimit);
        }
        numerator *= BigInt::from(2u8).pow((scale - twos) as u32);
        numerator *= BigInt::from(5u8).pow((scale - fives) as u32);
        Self::try_new(
            numerator,
            &self.exponent10 - &other.exponent10 - BigInt::from(scale),
        )
    }
}
fn bounded_exponent(n: &BigInt) -> Result<u64> {
    if n.sign() == Sign::Minus {
        return Err(Error::DecimalLimit);
    }
    let limbs = n.to_u64_digits().1;
    match limbs.as_slice() {
        [] => Ok(0),
        [value] if *value <= Decimal::MAX_ABS_EXPONENT => Ok(*value),
        _ => Err(Error::DecimalLimit),
    }
}
fn pow10(n: u64) -> BigInt {
    BigInt::from(10u8).pow(n as u32)
}
fn normal_decimal(mut c: BigInt, mut e: BigInt) -> (BigInt, BigInt) {
    if c.is_zero() {
        return (BigInt::zero(), BigInt::zero());
    }
    let ten = BigInt::from(10);
    while (&c % &ten).is_zero() {
        c /= &ten;
        e += 1;
    }
    (c, e)
}

/// A snapshot pin, never an implicit reference to current CWD.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Snapshot {
    Cwd {
        database: [u8; 16],
        runtime: [u8; 16],
        generation: BigInt,
        id: [u8; 32],
    },
    Commit {
        database: [u8; 16],
        algorithm: GitHash,
        oid: Vec<u8>,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitHash {
    Sha1,
    Sha256,
}
impl Snapshot {
    pub fn cwd(database: [u8; 16], runtime: [u8; 16], generation: BigInt) -> Result<Self> {
        if generation.sign() == Sign::Minus {
            return Err(Error::InvalidValue);
        }
        let core = Raw::Array(vec![
            Raw::Int(0.into()),
            uuid_raw(database),
            uuid_raw(runtime),
            Raw::Int(generation.clone()),
        ]);
        let id = domain_digest("orna.snapshot.v1", &core)?;
        Ok(Self::Cwd {
            database,
            runtime,
            generation,
            id,
        })
    }
    pub fn raw(&self) -> Raw {
        match self {
            Self::Cwd {
                database,
                runtime,
                generation,
                id,
            } => Raw::Array(vec![
                Raw::Int(0.into()),
                uuid_raw(*database),
                uuid_raw(*runtime),
                Raw::Int(generation.clone()),
                Raw::Bytes(id.to_vec()),
            ]),
            Self::Commit {
                database,
                algorithm,
                oid,
            } => Raw::Array(vec![
                Raw::Int(1.into()),
                uuid_raw(*database),
                Raw::Text(
                    match algorithm {
                        GitHash::Sha1 => "sha1",
                        GitHash::Sha256 => "sha256",
                    }
                    .into(),
                ),
                Raw::Bytes(oid.clone()),
            ]),
        }
    }
    pub fn decode(raw: &Raw) -> Result<Self> {
        let a = array(raw)?;
        match int_u64(a.first().ok_or(Error::InvalidValue)?)? {
            0 => {
                if a.len() != 5 {
                    return Err(Error::InvalidValue);
                }
                let db = uuid_array(&a[1])?;
                let rt = uuid_array(&a[2])?;
                let generation = integer(&a[3])?.clone();
                if generation.sign() == Sign::Minus {
                    return Err(Error::InvalidValue);
                };
                let _id = bytes32(&a[4])?;
                let expected = Self::cwd(db, rt, generation.clone())?;
                if expected.raw() != *raw {
                    return Err(Error::NonCanonical);
                };
                Ok(expected)
            }
            1 => {
                if a.len() != 4 {
                    return Err(Error::InvalidValue);
                }
                let database = uuid_array(&a[1])?;
                let algorithm = match text(&a[2])? {
                    "sha1" => GitHash::Sha1,
                    "sha256" => GitHash::Sha256,
                    _ => return Err(Error::InvalidValue),
                };
                let oid = bytes(&a[3])?.to_vec();
                if oid.len()
                    != match algorithm {
                        GitHash::Sha1 => 20,
                        GitHash::Sha256 => 32,
                    }
                {
                    return Err(Error::InvalidValue);
                };
                Ok(Self::Commit {
                    database,
                    algorithm,
                    oid,
                })
            }
            _ => Err(Error::InvalidValue),
        }
    }
}
fn uuid_raw(bytes: [u8; 16]) -> Raw {
    tag(37, Raw::Bytes(bytes.to_vec()))
}
fn uuid_array(r: &Raw) -> Result<[u8; 16]> {
    let Raw::Tag(37, v) = r else {
        return Err(Error::InvalidValue);
    };
    let b = bytes(v)?;
    if b.len() != 16 {
        return Err(Error::InvalidValue);
    };
    let mut o = [0; 16];
    o.copy_from_slice(b);
    Ok(o)
}
fn bytes32(r: &Raw) -> Result<[u8; 32]> {
    let b = bytes(r)?;
    if b.len() != 32 {
        return Err(Error::InvalidValue);
    };
    let mut o = [0; 32];
    o.copy_from_slice(b);
    Ok(o)
}

/// SHA-256 of ASCII domain, NUL and exact OVB-1 bytes.
pub fn domain_digest(domain: &str, payload: &Raw) -> Result<[u8; 32]> {
    if !domain.is_ascii() || domain.as_bytes().contains(&0) {
        return Err(Error::InvalidValue);
    }
    let bytes = encode_raw(payload)?;
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    Ok(hash.finalize().into())
}
pub fn row_identity(
    database: [u8; 16],
    table: [u8; 16],
    key: Raw,
    stored: Raw,
) -> Result<[u8; 32]> {
    domain_digest(
        "orna.row.v1",
        &Raw::Array(vec![uuid_raw(database), uuid_raw(table), key, stored]),
    )
}
pub fn schema_identity(schema: &SchemaDescriptor) -> Result<[u8; 32]> {
    domain_digest("orna.schema.v1", schema.raw())
}
pub fn argument_identity(arguments: Vec<(String, Raw, Raw)>) -> Result<[u8; 32]> {
    let mut a = arguments;
    a.sort_by(|x, y| x.0.as_bytes().cmp(y.0.as_bytes()));
    if a.windows(2).any(|x| x[0].0 == x[1].0) {
        return Err(Error::InvalidValue);
    };
    domain_digest(
        "orna.arguments.v1",
        &Raw::Array(
            a.into_iter()
                .map(|(n, t, v)| Raw::Array(vec![Raw::Text(n), t, v]))
                .collect(),
        ),
    )
}

/// The closed descriptor map defined in Format §29.  It is retained as a
/// checked raw representation to avoid accidentally dropping unknown details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaDescriptor(Raw);
impl SchemaDescriptor {
    pub fn new(raw: Raw) -> Result<Self> {
        validate_raw(&raw, 0)?;
        validate_schema(&raw)?;
        Ok(Self(raw))
    }
    pub fn raw(&self) -> &Raw {
        &self.0
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_raw(&self.0)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let v = Value::decode(bytes)?;
        Self::new(v.0)
    }
}

/// Repository key path component codec (ORNA-PATH-001 through 012).
pub fn path_encode_component(text: &str) -> Result<String> {
    if text.is_empty() {
        return Ok("~ff".into());
    }
    let raw = text.as_bytes();
    let reserved = reserved_name(text);
    let trailing = raw.iter().rposition(|b| *b != b'.').map_or(0, |x| x + 1);
    let mut out = String::new();
    for (i, b) in raw.iter().enumerate() {
        let force = (reserved && i == 0) || (*b == b'.' && i >= trailing);
        if !force && matches!(*b,b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'.'|b'_'|b'-') {
            out.push(*b as char)
        } else {
            out.push_str(&format!("~{b:02x}"))
        }
    }
    if out.len() > 200 {
        return Err(Error::InvalidPath);
    }
    Ok(out)
}
pub fn path_decode_component(encoded: &str) -> Result<String> {
    if !encoded.is_ascii() || encoded.len() > 200 {
        return Err(Error::InvalidPath);
    }
    if encoded == "~ff" {
        return Ok(String::new());
    }
    if encoded.contains("~ff") {
        return Err(Error::InvalidPath);
    }
    let mut o = Vec::new();
    let b = encoded.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'~' {
            if i + 2 >= b.len() {
                return Err(Error::InvalidPath);
            }
            let h = std::str::from_utf8(&b[i + 1..i + 3]).map_err(|_| Error::InvalidPath)?;
            if !h
                .bytes()
                .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))
            {
                return Err(Error::InvalidPath);
            };
            o.push(u8::from_str_radix(h, 16).map_err(|_| Error::InvalidPath)?);
            i += 3
        } else {
            if !matches!(b[i],b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'.'|b'_'|b'-') {
                return Err(Error::InvalidPath);
            };
            o.push(b[i]);
            i += 1
        }
    }
    let s = String::from_utf8(o).map_err(|_| Error::InvalidPath)?;
    if path_encode_component(&s)? != encoded {
        return Err(Error::NonCanonical);
    }
    Ok(s)
}
pub fn path_collision_key(encoded: &str) -> String {
    encoded
        .bytes()
        .map(|x| {
            if x.is_ascii_uppercase() {
                (x + 32) as char
            } else {
                x as char
            }
        })
        .collect()
}
/// Encodes a composite editable-row key.  Only the final component has the
/// `.orna` extension; the resulting repository-relative path is bounded.
pub fn path_encode_key_components(components: &[String]) -> Result<Vec<String>> {
    if components.is_empty() {
        return Err(Error::InvalidPath);
    }
    let mut output: Vec<String> = components
        .iter()
        .map(|component| path_encode_component(component))
        .collect::<Result<_>>()?;
    let last = output.last_mut().ok_or(Error::InvalidPath)?;
    last.push_str(".orna");
    if output.join("/").len() > 1024 {
        return Err(Error::InvalidPath);
    }
    Ok(output)
}
/// Decodes table-relative editable-row path components into canonical key text
/// in declared component order. The owning schema must still interpret that
/// text as typed keys. Filesystem containment and sibling collisions remain
/// the responsibility of the repository adapter.
pub fn path_decode_key_components(components: &[String]) -> Result<Vec<String>> {
    let (last, parents) = components.split_last().ok_or(Error::InvalidPath)?;
    let mut length = 0usize;
    for (index, component) in components.iter().enumerate() {
        length = length
            .checked_add(component.len())
            .and_then(|sum| sum.checked_add(usize::from(index != 0)))
            .filter(|sum| *sum <= 1024)
            .ok_or(Error::InvalidPath)?;
    }
    path_validate_relative_components(components)?;
    let mut decoded = parents
        .iter()
        .map(|component| path_decode_component(component))
        .collect::<Result<Vec<_>>>()?;
    decoded.push(path_decode_component(
        last.strip_suffix(".orna").ok_or(Error::InvalidPath)?,
    )?);
    Ok(decoded)
}
/// Rejects path traversal syntax before a repository layer performs a
/// symlink/reparse-point-aware containment check at materialisation time.
pub fn path_validate_relative_components(components: &[String]) -> Result<()> {
    if components.is_empty()
        || components.iter().any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.contains('/')
                || component.contains('\\')
        })
    {
        return Err(Error::InvalidPath);
    }
    if components.join("/").len() > 1024 {
        return Err(Error::InvalidPath);
    }
    Ok(())
}
fn reserved_name(s: &str) -> bool {
    if s == "." || s == ".." {
        return true;
    }
    let t = s.trim_end_matches([' ', '.']);
    let lower = t.to_ascii_lowercase();
    if lower == ".git" {
        return true;
    }
    let first = lower.split('.').next().unwrap_or("");
    matches!(
        first,
        "con" | "prn" | "aux" | "nul" | "clock$" | "conin$" | "conout$"
    ) || (first.len() == 4
        && ((first.starts_with("com") || first.starts_with("lpt"))
            && matches!(first.as_bytes()[3], b'1'..=b'9')))
}

fn encode_raw(v: &Raw) -> Result<Vec<u8>> {
    validate_raw(v, 0)?;
    let mut out = Vec::new();
    write_raw(v, &mut out)?;
    Ok(out)
}
fn head(out: &mut Vec<u8>, major: u8, n: u64) {
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
fn write_raw(v: &Raw, out: &mut Vec<u8>) -> Result<()> {
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

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    items: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
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
    fn raw(&mut self, depth: usize) -> Result<Raw> {
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

fn validate_raw(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    match v {
        Raw::Tag(0, _) => return Err(Error::ProtectedValue),
        Raw::Float(bits) if is_nan_bits(*bits) && *bits != CANONICAL_NAN_BITS => {
            return Err(Error::NonCanonical);
        }
        Raw::Array(a) => {
            for x in a {
                validate_raw(x, depth + 1)?
            }
        }
        Raw::Map(m) => {
            let mut previous = None;
            for (k, v) in m {
                validate_raw(k, depth + 1)?;
                validate_raw(v, depth + 1)?;
                let b = encode_raw(k)?;
                if previous.as_ref().is_some_and(|x: &Vec<u8>| b <= *x) {
                    return Err(Error::DuplicateOrUnorderedMapKey);
                }
                previous = Some(b)
            }
        }
        Raw::Tag(n, x) => {
            validate_tag(*n, x)?;
            validate_raw(x, depth + 1)?
        }
        _ => {}
    }
    Ok(())
}
fn validate_tag(n: u64, v: &Raw) -> Result<()> {
    match n {
        37 => {
            if bytes(v)?.len() != 16 {
                return Err(Error::InvalidTag);
            }
        }
        60000 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let (c, e) = (integer(&a[0])?, integer(&a[1])?);
            if normal_decimal(c.clone(), e.clone()) != (c.clone(), e.clone()) {
                return Err(Error::NonCanonical);
            }
        }
        60001 => {
            validate_date(text(v)?)?;
        }
        60003 => {
            let s = text(v)?;
            validate_local_date_time(s)?;
        }
        60004 => {
            let s = text(v)?;
            if s.is_empty()
                || s.as_bytes()
                    .iter()
                    .any(|b| !matches!(*b, b'A'..=b'Z'|b'a'..=b'z'|b'0'..=b'9'|b'_'|b'-'|b'+'|b'/'))
            {
                return Err(Error::InvalidTag);
            }
        }
        60002 | 60005 => {
            let a = array(v)?;
            if a.len() != 2 || !matches!(a[0], Raw::Int(_)) || int_u64(&a[1])? >= 1_000_000_000 {
                return Err(Error::InvalidTag);
            }
        }
        60006 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[1])?;
        }
        60007 => {
            let a = array(v)?;
            if a.len() != 2 || !matches!(a[0], Raw::Tag(60000, _)) {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[1])?;
        }
        60008 => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
            if !matches!(a[2], Raw::Null | Raw::Tag(60009, _)) {
                return Err(Error::InvalidTag);
            }
        }
        60009 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            if !matches!(a[0], Raw::Null | Raw::Tag(37, _)) {
                return Err(Error::InvalidTag);
            }
            validate_fields(&a[1])?;
        }
        60010 => {
            let a = array(v)?;
            if a.len() != 4 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
            Snapshot::decode(&a[3])?;
        }
        60013 => {
            let a = array(v)?;
            match a.as_slice() {
                [Raw::Int(x)] if x.is_zero() => {}
                [Raw::Int(x), _] if *x == BigInt::from(1) => {}
                _ => return Err(Error::InvalidTag),
            }
        }
        60015 => {
            if array(v)?.is_empty() {
                return Err(Error::NonCanonical);
            }
        }
        60018 => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            if !matches!(a[0], Raw::Tag(60002, _)) || !matches!(a[1], Raw::Tag(60004, _)) {
                return Err(Error::InvalidTag);
            }
            let offset = integer(&a[2])?;
            // Exact zone-database agreement is contextual: this pure codec can
            // validate the pinned shape and legal offset range only. The
            // snapshot's recorded tzdata resolver must confirm the offset.
            if offset < &BigInt::from(-64_800) || offset > &BigInt::from(64_800) {
                return Err(Error::InvalidTag);
            }
        }
        60019 => {
            let a = array(v)?;
            if a.len() != 3
                || !matches!(a[0], Raw::Tag(60013, _))
                || !matches!(a[1], Raw::Tag(60013, _))
                || !matches!(a[2], Raw::Bool(_))
            {
                return Err(Error::InvalidTag);
            }
        }
        60020 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
        }
        60021 => {
            let a = array(v)?;
            if a.len() != 3 {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
            uuid_array(&a[1])?;
        }
        60022 => {
            let a = array(v)?;
            if a.len() != 4 || !matches!(a[3], Raw::Bool(false)) {
                return Err(Error::InvalidTag);
            }
            uuid_array(&a[0])?;
        }
        60023 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let name = text(&a[0])?;
            match name {
                value if UUID_SYSTEM_IDS.contains(&value) => {
                    uuid_array(&a[1])?;
                }
                "sys.RevisionId" | "sys.SnapshotId" | "sys.ConsumerIdentity" => {
                    if bytes(&a[1])?.len() != 32 {
                        return Err(Error::InvalidTag);
                    }
                }
                "sys.CheckpointVersion" | "sys.FailureVersion" => {
                    if integer(&a[1])?.sign() == Sign::Minus {
                        return Err(Error::InvalidTag);
                    }
                }
                "sys.GitOid" => {
                    let git = array(&a[1])?;
                    if git.len() != 2 {
                        return Err(Error::InvalidTag);
                    }
                    let size = match text(&git[0])? {
                        "sha1" => 20,
                        "sha256" => 32,
                        _ => return Err(Error::InvalidTag),
                    };
                    if bytes(&git[1])?.len() != size {
                        return Err(Error::InvalidTag);
                    }
                }
                _ => return Err(Error::InvalidTag),
            }
        }
        60024 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let flavour = text(&a[0])?;
            let value = text(&a[1])?;
            if !matches!(flavour, "repo" | "posix" | "windows")
                || (flavour == "repo"
                    && (!value.is_ascii()
                        || value.starts_with('/')
                        || value
                            .split('/')
                            .any(|x| x.is_empty() || x == "." || x == "..")))
            {
                return Err(Error::InvalidTag);
            }
        }
        60025 => {
            let a = array(v)?;
            if a.len() != 2 || text(&a[0])? != "sha256" || bytes(&a[1])?.len() != 32 {
                return Err(Error::InvalidTag);
            }
        }
        60026 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            validate_type(&a[0])?;
            validate_value_as_type(&a[1], &a[0])?;
        }
        60027 => {
            let a = array(v)?;
            if a.len() != 2 {
                return Err(Error::InvalidTag);
            }
            let ordered = match a[0] {
                Raw::Bool(value) => value,
                _ => return Err(Error::InvalidTag),
            };
            let rows = array(&a[1])?;
            if !ordered {
                let mut previous = None;
                for row in rows {
                    let encoded = encode_raw(row)?;
                    if previous.as_ref().is_some_and(|x: &Vec<u8>| encoded < *x) {
                        return Err(Error::NonCanonical);
                    }
                    previous = Some(encoded);
                }
            }
        }
        60011 => validate_diagnostic(v, 0)?,
        60012 => validate_present(v, 0)?,
        60016 => validate_error(v, 0)?,
        60014 => {
            if !array(v)?.is_empty() {
                return Err(Error::InvalidTag);
            }
        }
        60017 => {
            if int_u64(v)? >= 86_400_000_000_000 {
                return Err(Error::InvalidTag);
            }
        }
        _ => return Err(Error::InvalidTag),
    }
    Ok(())
}
fn integer_map<'a>(
    v: &'a Raw,
    required: &[u64],
    optional: &[u64],
) -> Result<BTreeMap<u64, &'a Raw>> {
    let mut result = BTreeMap::new();
    for (key, value) in map(v)? {
        let key = int_u64(key)?;
        if !required.contains(&key) && !optional.contains(&key)
            || result.insert(key, value).is_some()
        {
            return Err(Error::InvalidTag);
        }
    }
    if required.iter().any(|key| !result.contains_key(key)) {
        return Err(Error::InvalidTag);
    }
    Ok(result)
}
fn validate_diagnostic(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let fields = integer_map(v, &[0, 1, 2, 3, 4, 5, 6], &[7])?;
    text(fields[&0])?;
    if int_u64(fields[&1])? > 4 {
        return Err(Error::InvalidTag);
    }
    text(fields[&2])?;
    for span in array(fields[&3])? {
        let span = array(span)?;
        if span.len() != 4 {
            return Err(Error::InvalidTag);
        }
        Snapshot::decode(&span[0])?;
        let path = text(&span[1])?;
        if path != "<redacted>"
            && (!path.is_ascii()
                || path.starts_with('/')
                || path
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == ".."))
        {
            return Err(Error::InvalidTag);
        }
        let start = int_u64(&span[2])?;
        if int_u64(&span[3])? < start {
            return Err(Error::InvalidTag);
        }
    }
    for note in array(fields[&4])? {
        text(note)?;
    }
    for cause in array(fields[&5])? {
        if let Raw::Tag(60011, inner) = cause {
            validate_diagnostic(inner, depth + 1)?;
        } else {
            return Err(Error::InvalidTag);
        }
    }
    if !matches!(fields[&6], Raw::Bool(_)) {
        return Err(Error::InvalidTag);
    }
    if let Some(id) = fields.get(&7) {
        uuid_array(id)?;
    }
    Ok(())
}
fn validate_error(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let fields = integer_map(v, &[0, 1, 2, 3], &[])?;
    text(fields[&0])?;
    text(fields[&1])?;
    for cause in array(fields[&2])? {
        if let Raw::Tag(60016, inner) = cause {
            validate_error(inner, depth + 1)?;
        } else {
            return Err(Error::InvalidTag);
        }
    }
    for (key, value) in map(fields[&3])? {
        text(key)?;
        validate_raw(value, depth + 1)?;
    }
    Ok(())
}
fn validate_present(v: &Raw, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit);
    }
    let a = array(v)?;
    if a.len() != 4 {
        return Err(Error::InvalidTag);
    }
    if !matches!(a[0], Raw::Text(_) | Raw::Tag(37, _)) {
        return Err(Error::InvalidTag);
    }
    if !matches!(a[1], Raw::Null) {
        let key = array(&a[1])?;
        match key.as_slice() {
            [Raw::Int(kind), Raw::Text(_) | Raw::Tag(37, _)] if *kind == BigInt::from(0) => {}
            [Raw::Int(kind), table, _] if *kind == BigInt::from(1) => {
                let _ = uuid_array(table)?;
            }
            [Raw::Int(kind), _] if *kind == BigInt::from(3) => {}
            _ => return Err(Error::InvalidTag),
        }
    }
    for (key, value) in map(&a[2])? {
        if !matches!(key, Raw::Text(_) | Raw::Tag(37, _)) {
            return Err(Error::InvalidTag);
        }
        validate_raw(value, depth + 1)?;
    }
    let mut keys = Vec::new();
    for child in array(&a[3])? {
        let Raw::Tag(60012, node) = child else {
            return Err(Error::InvalidTag);
        };
        validate_present(node, depth + 1)?;
        let child_fields = array(node)?;
        if !matches!(child_fields[1], Raw::Null) {
            let k = encode_raw(&child_fields[1])?;
            if keys.contains(&k) {
                return Err(Error::DuplicateOrUnorderedMapKey);
            }
            keys.push(k)
        }
    }
    Ok(())
}
const UUID_SYSTEM_IDS: [&str; 15] = [
    "sys.DatabaseId",
    "sys.FileId",
    "sys.DefinitionId",
    "sys.ObjectId",
    "sys.RuntimeId",
    "sys.TransactionId",
    "sys.InvocationId",
    "sys.RunId",
    "sys.QueryId",
    "sys.SessionId",
    "sys.ClientId",
    "sys.TraceId",
    "sys.SpanId",
    "sys.SegmentId",
    "sys.BuildId",
];
fn validate_fields(v: &Raw) -> Result<()> {
    let fields = array(v)?;
    let mut prior = None;
    for f in fields {
        let p = array(f)?;
        if p.len() != 2 {
            return Err(Error::InvalidTag);
        }
        let k = encode_raw(&p[0])?;
        if prior.as_ref().is_some_and(|x: &Vec<u8>| k <= *x) {
            return Err(Error::DuplicateOrUnorderedMapKey);
        }
        prior = Some(k);
    }
    Ok(())
}
fn validate_date(s: &str) -> Result<()> {
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, x)| matches!(i, 4 | 7) || x.is_ascii_digit())
    {
        return Err(Error::InvalidTag);
    }
    let y = s[..4].parse::<i32>().map_err(|_| Error::InvalidTag)?;
    let m = s[5..7].parse::<u32>().map_err(|_| Error::InvalidTag)?;
    let d = s[8..].parse::<u32>().map_err(|_| Error::InvalidTag)?;
    let max = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        _ => return Err(Error::InvalidTag),
    };
    if d == 0 || d > max {
        return Err(Error::InvalidTag);
    }
    Ok(())
}
fn validate_local_date_time(s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    if bytes.len() != 29
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || !bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        })
    {
        return Err(Error::InvalidTag);
    }
    validate_date(&s[..10])?;
    let hour = s[11..13].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    let minute = s[14..16].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    let second = s[17..19].parse::<u8>().map_err(|_| Error::InvalidTag)?;
    if hour > 23 || minute > 59 || second > 59 {
        return Err(Error::InvalidTag);
    }
    Ok(())
}
fn array(v: &Raw) -> Result<&Vec<Raw>> {
    if let Raw::Array(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn map(v: &Raw) -> Result<&Vec<(Raw, Raw)>> {
    if let Raw::Map(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn bytes(v: &Raw) -> Result<&[u8]> {
    if let Raw::Bytes(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn text(v: &Raw) -> Result<&str> {
    if let Raw::Text(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn integer(v: &Raw) -> Result<&BigInt> {
    if let Raw::Int(x) = v {
        Ok(x)
    } else {
        Err(Error::InvalidValue)
    }
}
fn int_u64(v: &Raw) -> Result<u64> {
    let integer = integer(v)?;
    if integer.sign() == Sign::Minus {
        return Err(Error::InvalidValue);
    }
    let limbs = integer.to_u64_digits().1;
    match limbs.as_slice() {
        [] => Ok(0),
        [value] => Ok(*value),
        _ => Err(Error::InvalidValue),
    }
}

fn validate_schema(raw: &Raw) -> Result<()> {
    let m = map(raw)?;
    if m.len() != 5 {
        return Err(Error::InvalidSchema);
    }
    let mut got = BTreeMap::new();
    for (k, v) in m {
        got.insert(int_u64(k)?, v);
    }
    if got.keys().copied().collect::<Vec<_>>() != vec![0, 1, 2, 3, 4] || int_u64(got[&0])? != 1 {
        return Err(Error::InvalidSchema);
    }
    uuid_array(got[&1])?;
    let keys = array(got[&2])?;
    let fields = array(got[&3])?;
    let defs = array(got[&4])?;
    let mut ids = Vec::new();
    let mut key_role_ids = Vec::new();
    for f in fields {
        let a = array(f)?;
        if a.len() != 5 {
            return Err(Error::InvalidSchema);
        }
        let id = uuid_array(&a[0])?;
        ids.push(id);
        text(&a[1])?;
        validate_type(&a[2])?;
        let role = int_u64(&a[3])?;
        if role > 2 {
            return Err(Error::InvalidSchema);
        }
        let fallback = array(&a[4])?;
        match fallback.as_slice() {
            [Raw::Int(kind)] if kind.is_zero() => {}
            [Raw::Int(kind), value] if *kind == BigInt::from(1) && role != 2 => {
                validate_value_as_type(value, &a[2]).map_err(|_| Error::InvalidSchema)?;
            }
            [Raw::Int(kind), Raw::Bytes(bytes)]
                if *kind == BigInt::from(2) && role == 2 && bytes.len() == 32 => {}
            _ => return Err(Error::InvalidSchema),
        }
        if role == 0 && fallback.len() != 1 {
            return Err(Error::InvalidSchema);
        }
        if role == 0 {
            key_role_ids.push(id);
        }
    }
    if ids.windows(2).any(|x| x[0] >= x[1]) {
        return Err(Error::InvalidSchema);
    }
    let mut key_ids = Vec::new();
    for key in keys {
        let key = uuid_array(key)?;
        if !ids.contains(&key) || key_ids.contains(&key) {
            return Err(Error::InvalidSchema);
        }
        key_ids.push(key);
    }
    if key_ids != key_role_ids {
        return Err(Error::InvalidSchema);
    }
    let mut definition_ids = Vec::new();
    for d in defs {
        let a = array(d)?;
        if a.len() != 3 {
            return Err(Error::InvalidSchema);
        }
        let id = uuid_array(&a[0])?;
        if definition_ids
            .last()
            .is_some_and(|previous| *previous >= id)
        {
            return Err(Error::InvalidSchema);
        }
        definition_ids.push(id);
        validate_definition(int_u64(&a[1])?, &a[2])?;
    }
    for field in fields {
        validate_type_refs(&array(field)?[2], &definition_ids)?;
    }
    for definition in defs {
        validate_definition_refs(
            int_u64(&array(definition)?[1])?,
            &array(definition)?[2],
            &definition_ids,
        )?;
    }
    Ok(())
}
fn validate_type_refs(node: &Raw, definitions: &[[u8; 16]]) -> Result<()> {
    let a = array(node)?;
    match int_u64(&a[0])? {
        5 | 8 => {
            if !definitions.contains(&uuid_array(&a[1])?) {
                return Err(Error::InvalidSchema);
            }
        }
        1 | 2 | 9 => validate_type_refs(&a[1], definitions)?,
        3 => {
            for x in array(&a[1])? {
                validate_type_refs(x, definitions)?
            }
        }
        4 => {
            for x in array(&a[1])? {
                validate_type_refs(&array(x)?[1], definitions)?
            }
        }
        6 => {
            for x in array(&a[3])? {
                validate_type_refs(x, definitions)?
            }
        }
        7 => validate_type_refs(&a[1], definitions)?,
        0 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
fn validate_definition_refs(kind: u64, body: &Raw, definitions: &[[u8; 16]]) -> Result<()> {
    match kind {
        0 => {
            for f in array(body)? {
                validate_type_refs(&array(f)?[2], definitions)?
            }
        }
        1 => {
            for v in array(body)? {
                for f in array(&array(v)?[2])? {
                    validate_type_refs(&array(f)?[2], definitions)?
                }
            }
        }
        2 => validate_type_refs(&array(body)?[0], definitions)?,
        3 | 4 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
fn validate_definition(kind: u64, body: &Raw) -> Result<()> {
    match kind {
        0 => {
            validate_nested_field_list(array(body)?)?;
        }
        1 => {
            let mut previous = None;
            for variant in array(body)? {
                let v = array(variant)?;
                if v.len() != 3 {
                    return Err(Error::InvalidSchema);
                }
                let id = uuid_array(&v[0])?;
                if previous.is_some_and(|p| p >= id) {
                    return Err(Error::InvalidSchema);
                }
                previous = Some(id);
                text(&v[1])?;
                validate_nested_field_list(array(&v[2])?)?;
            }
        }
        2 => {
            let a = array(body)?;
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[0])?;
            for digest in array(&a[1])? {
                if bytes(digest)?.len() != 32 {
                    return Err(Error::InvalidSchema);
                }
            }
        }
        3 => {
            let a = array(body)?;
            if a.len() != 6 {
                return Err(Error::InvalidSchema);
            }
            let dimension = array(&a[0])?;
            if dimension.len() != 7 || dimension.iter().any(|v| !matches!(v, Raw::Int(_))) {
                return Err(Error::InvalidSchema);
            }
            for value in &a[1..5] {
                integer(value)?;
            }
            if !matches!(a[5], Raw::Bool(_)) {
                return Err(Error::InvalidSchema);
            }
        }
        4 => {
            let a = array(body)?;
            if a.len() != 2 || text(&a[0])?.len() != 3 || int_u64(&a[1])? > 18 {
                return Err(Error::InvalidSchema);
            }
        }
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
fn validate_nested_field(field: &[Raw]) -> Result<()> {
    if field.len() != 5 {
        return Err(Error::InvalidSchema);
    }
    uuid_array(&field[0])?;
    let name = text(&field[1])?;
    if name.is_empty() {
        return Err(Error::InvalidSchema);
    }
    validate_type(&field[2])?;
    if int_u64(&field[3])? > 2 {
        return Err(Error::InvalidSchema);
    }
    let fallback = array(&field[4])?;
    match fallback.as_slice() {
        [Raw::Int(x)] if x.is_zero() => {}
        [Raw::Int(x), value] if *x == BigInt::from(1) => {
            validate_value_as_type(value, &field[2]).map_err(|_| Error::InvalidSchema)?
        }
        [Raw::Int(x), Raw::Bytes(bytes)] if *x == BigInt::from(2) && bytes.len() == 32 => {}
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
fn validate_nested_field_list(fields: &[Raw]) -> Result<()> {
    let mut previous = None;
    for field in fields {
        let f = array(field)?;
        let id = uuid_array(&f[0])?;
        if previous.is_some_and(|p| p >= id) {
            return Err(Error::InvalidSchema);
        }
        previous = Some(id);
        validate_nested_field(f)?;
    }
    Ok(())
}
fn validate_type(v: &Raw) -> Result<()> {
    let a = array(v)?;
    if a.is_empty() {
        return Err(Error::InvalidSchema);
    }
    match int_u64(&a[0])? {
        0 => {
            if a.len() != 2
                || !matches!(
                    text(&a[1])?,
                    "Bool"
                        | "Int"
                        | "Float"
                        | "Decimal"
                        | "Str"
                        | "Blob"
                        | "Uuid"
                        | "Date"
                        | "Instant"
                        | "LocalDateTime"
                        | "TimeOfDay"
                        | "Duration"
                        | "TimeZone"
                        | "ZonedDateTime"
                        | "Unit"
                )
            {
                return Err(Error::InvalidSchema);
            }
        }
        1 | 2 | 9 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[1])?;
        }
        3 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            for x in array(&a[1])? {
                validate_type(x)?;
            }
        }
        4 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            let mut last = "";
            for x in array(&a[1])? {
                let p = array(x)?;
                if p.len() != 2 {
                    return Err(Error::InvalidSchema);
                }
                let n = text(&p[0])?;
                if n <= last {
                    return Err(Error::InvalidSchema);
                }
                last = n;
                validate_type(&p[1])?;
            }
        }
        5 | 8 => {
            if a.len() != 2 {
                return Err(Error::InvalidSchema);
            }
            let _ = uuid_array(&a[1])?;
        }
        6 => {
            if a.len() != 4 {
                return Err(Error::InvalidSchema);
            }
            let _ = uuid_array(&a[1])?;
            let _ = uuid_array(&a[2])?;
            for x in array(&a[3])? {
                validate_type(x)?;
            }
        }
        7 => {
            if a.len() != 3 {
                return Err(Error::InvalidSchema);
            }
            validate_type(&a[1])?;
            let _ = uuid_array(&a[2])?;
        }
        _ => return Err(Error::InvalidSchema),
    }
    Ok(())
}
/// Validates the portable portion of a value against its exact closed type
/// node. Nominal and table semantics are deliberately rejected here because a
/// standalone node cannot prove membership without the complete descriptor.
fn validate_value_as_type(value: &Raw, ty: &Raw) -> Result<()> {
    let node = array(ty)?;
    match int_u64(&node[0])? {
        0 => match text(&node[1])? {
            "Bool" if matches!(value, Raw::Bool(_)) => Ok(()),
            "Int" if matches!(value, Raw::Int(_)) => Ok(()),
            "Float" if matches!(value, Raw::Float(_)) => Ok(()),
            "Decimal" if matches!(value, Raw::Tag(60000, _)) => Ok(()),
            "Str" if matches!(value, Raw::Text(_)) => Ok(()),
            "Blob" if matches!(value, Raw::Bytes(_)) => Ok(()),
            "Uuid" if matches!(value, Raw::Tag(37, _)) => Ok(()),
            "Unit" if matches!(value, Raw::Tag(60014, _)) => Ok(()),
            "Date" if matches!(value, Raw::Tag(60001, _)) => Ok(()),
            "Instant" if matches!(value, Raw::Tag(60002, _)) => Ok(()),
            "LocalDateTime" if matches!(value, Raw::Tag(60003, _)) => Ok(()),
            "TimeOfDay" if matches!(value, Raw::Tag(60017, _)) => Ok(()),
            "Duration" if matches!(value, Raw::Tag(60005, _)) => Ok(()),
            "TimeZone" if matches!(value, Raw::Tag(60004, _)) => Ok(()),
            "ZonedDateTime" if matches!(value, Raw::Tag(60018, _)) => Ok(()),
            _ => Err(Error::InvalidValue),
        },
        1 => {
            for item in array(value)? {
                validate_value_as_type(item, &node[1])?;
            }
            Ok(())
        }
        2 => {
            let Raw::Tag(60013, option) = value else {
                return Err(Error::InvalidValue);
            };
            let option = array(option)?;
            if option.len() == 2 {
                validate_value_as_type(&option[1], &node[1])?;
            }
            Ok(())
        }
        3 => {
            let types = array(&node[1])?;
            if types.is_empty() {
                return if matches!(value, Raw::Tag(60014, _)) {
                    Ok(())
                } else {
                    Err(Error::InvalidValue)
                };
            }
            let Raw::Tag(60015, tuple) = value else {
                return Err(Error::InvalidValue);
            };
            let tuple = array(tuple)?;
            if tuple.len() != types.len() {
                return Err(Error::InvalidValue);
            }
            for (v, t) in tuple.iter().zip(types) {
                validate_value_as_type(v, t)?;
            }
            Ok(())
        }
        4 => {
            let fields = array(&node[1])?;
            let Raw::Tag(60009, record) = value else {
                return Err(Error::InvalidValue);
            };
            let record = array(record)?;
            if record.len() != 2 {
                return Err(Error::InvalidValue);
            }
            if !matches!(record[0], Raw::Null) {
                return Err(Error::InvalidValue);
            }
            let values = array(&record[1])?;
            if values.len() != fields.len() {
                return Err(Error::InvalidValue);
            }
            for (field, entry) in fields.iter().zip(values) {
                let spec = array(field)?;
                let entry = array(entry)?;
                if entry[0] != spec[0] {
                    return Err(Error::InvalidValue);
                }
                validate_value_as_type(&entry[1], &spec[1])?;
            }
            Ok(())
        }
        7 => {
            let Raw::Tag(60006, quantity) = value else {
                return Err(Error::InvalidValue);
            };
            let quantity = array(quantity)?;
            if quantity.len() != 2 || quantity[1] != node[2] {
                return Err(Error::InvalidValue);
            }
            validate_value_as_type(&quantity[0], &node[1])
        }
        8 => {
            uuid_array(&node[1])?;
            let Raw::Tag(60007, money) = value else {
                return Err(Error::InvalidValue);
            };
            let money = array(money)?;
            if money.len() != 2 || !matches!(money[0], Raw::Tag(60000, _)) {
                return Err(Error::InvalidValue);
            };
            if uuid_array(&money[1])? != uuid_array(&node[1])? {
                return Err(Error::InvalidValue);
            }
            Ok(())
        }
        9 => {
            let Raw::Tag(60019, range) = value else {
                return Err(Error::InvalidValue);
            };
            let range = array(range)?;
            for endpoint in &range[..2] {
                let Raw::Tag(60013, option) = endpoint else {
                    return Err(Error::InvalidValue);
                };
                let option = array(option)?;
                if option.len() == 2 {
                    validate_value_as_type(&option[1], &node[1])?;
                }
            }
            Ok(())
        }
        5 | 6 => Err(Error::InvalidSchema),
        _ => Err(Error::InvalidSchema),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(s: &str) -> Vec<u8> {
        s.as_bytes()
            .chunks(2)
            .map(|x| u8::from_str_radix(std::str::from_utf8(x).unwrap(), 16).unwrap())
            .collect()
    }
    #[test]
    fn values_match_supplied_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/value-vectors.json"
        ))
        .unwrap();
        for vector in fixture.as_array().unwrap() {
            let x = vector["hex"].as_str().unwrap();
            let b = h(x);
            let decoded = Value::decode(&b).unwrap_or_else(|e| panic!("{x}: {e}"));
            assert_eq!(decoded.encode().unwrap(), b, "{x}")
        }
    }
    #[test]
    fn rejects_noncanonical() {
        for x in [
            "1817",
            "c2490000000000000000",
            "fa3f800000",
            "fb7ff8000000000001",
            "9f00ff",
            "a101000100",
        ] {
            assert!(Value::decode(&h(x)).is_err(), "{x}")
        }
    }
    #[test]
    fn float_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/float-vectors.json"
        ))
        .unwrap();
        let parse = |text: &str| u64::from_str_radix(text, 16).unwrap();
        let ordered: Vec<u64> = fixture["ascending_total_order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| parse(v["bits"].as_str().unwrap()))
            .collect();
        assert!(
            ordered
                .windows(2)
                .all(|p| float_total_cmp(p[0], p[1]) == Ordering::Less)
        );
        for vector in fixture["ordinary_equality"].as_array().unwrap() {
            assert_eq!(
                float_ordinary_eq(
                    parse(vector["left"].as_str().unwrap()),
                    parse(vector["right"].as_str().unwrap())
                ),
                vector["equal"].as_bool().unwrap()
            );
        }
        for vector in fixture["aggregate"].as_array().unwrap() {
            let values: Vec<u64> = vector["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| parse(v.as_str().unwrap()))
                .collect();
            let actual = if vector["operation"] == "min" {
                float_min(&values)
            } else {
                float_max(&values)
            };
            assert_eq!(actual, Some(parse(vector["result"].as_str().unwrap())));
        }
        for vector in fixture["parquet_statistics"].as_array().unwrap() {
            let values: Vec<u64> = vector["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| parse(v.as_str().unwrap()))
                .collect();
            assert_eq!(
                values.iter().filter(|x| is_nan_bits(**x)).count(),
                vector["nan_count"].as_u64().unwrap() as usize
            );
            let non_nan: Vec<u64> = values
                .iter()
                .copied()
                .filter(|x| !is_nan_bits(*x))
                .collect();
            let (min, max, key_min, key_max) = if non_nan.is_empty() {
                let canonical: Vec<u64> = values
                    .into_iter()
                    .map(|x| {
                        if is_nan_bits(x) {
                            CANONICAL_NAN_BITS
                        } else {
                            x
                        }
                    })
                    .collect();
                (
                    *canonical
                        .iter()
                        .min_by_key(|x| float_total_key(**x))
                        .unwrap(),
                    *canonical
                        .iter()
                        .max_by_key(|x| float_total_key(**x))
                        .unwrap(),
                    "min_total",
                    "max_total",
                )
            } else {
                (
                    *non_nan.iter().min_by_key(|x| float_total_key(**x)).unwrap(),
                    *non_nan.iter().max_by_key(|x| float_total_key(**x)).unwrap(),
                    "min_non_nan",
                    "max_non_nan",
                )
            };
            assert_eq!(min, parse(vector[key_min].as_str().unwrap()));
            assert_eq!(max, parse(vector[key_max].as_str().unwrap()));
        }
    }
    #[test]
    fn decimal_is_exact() {
        let a = Decimal::new(1.into(), (-1).into());
        let b = Decimal::new(2.into(), (-1).into());
        assert_eq!(a.try_add(&b).unwrap(), Decimal::new(3.into(), (-1).into()));
        assert_eq!(
            Value::decimal(12340.into(), (-3).into())
                .unwrap()
                .encode()
                .unwrap(),
            h("d9ea60821904d221")
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into())
                .divide_exact(&Decimal::new(8.into(), 0.into()))
                .unwrap(),
            Decimal::new(125.into(), (-3).into())
        );
        assert_eq!(
            Decimal::new(1.into(), 0.into()).divide_exact(&Decimal::new(3.into(), 0.into())),
            Err(Error::NonFiniteDecimal)
        );
    }
    #[test]
    fn path_vectors() {
        for (a, b) in [
            ("alice-smith", "alice-smith"),
            ("a~b", "a~7eb"),
            ("foo/bar", "foo~2fbar"),
            ("é", "~c3~a9"),
            ("", "~ff"),
            ("con", "~63on"),
            (".git", "~2egit"),
        ] {
            assert_eq!(path_encode_component(a).unwrap(), b);
            assert_eq!(path_decode_component(b).unwrap(), a)
        }
        for x in ["~e", "~FF", "~61lice", "x~ff", "../x", "/absolute"] {
            assert!(path_decode_component(x).is_err())
        }
        assert_eq!(
            path_encode_key_components(&[
                "018f7f43-7b3a-7cc2-8c11-37e728c96f4a".into(),
                "2026-09-03".into()
            ])
            .unwrap(),
            vec!["018f7f43-7b3a-7cc2-8c11-37e728c96f4a", "2026-09-03.orna"]
        );
        assert!(path_validate_relative_components(&["..".into()]).is_err());
    }
    #[test]
    fn composite_path_decoding_is_canonical_and_bounded() {
        for keys in [
            vec![""],
            vec!["..", "foo/bar", "é"],
            vec!["first.orna", "last.orna"],
            vec!["con", "a\\b", ".git"],
        ] {
            let keys: Vec<String> = keys.into_iter().map(String::from).collect();
            let encoded = path_encode_key_components(&keys).unwrap();
            let decoded = path_decode_key_components(&encoded).unwrap();
            assert_eq!(decoded, keys);
            assert_eq!(path_encode_key_components(&decoded).unwrap(), encoded);
        }
        assert_eq!(
            path_decode_key_components(&["first.orna".into(), "last.orna.orna".into()]).unwrap(),
            vec!["first.orna", "last.orna"]
        );
        for encoded in [
            vec![],
            vec![""],
            vec![".orna"],
            vec!["a"],
            vec!["a.ORNA"],
            vec!["~61lice.orna"],
            vec!["~FF.orna"],
            vec!["x~ff.orna"],
            vec!["~e.orna"],
            vec!["..", "a.orna"],
            vec!["", "a.orna"],
            vec!["/a.orna"],
            vec!["a/b.orna"],
            vec!["a\\b.orna"],
        ] {
            let encoded: Vec<String> = encoded.into_iter().map(String::from).collect();
            assert!(path_decode_key_components(&encoded).is_err(), "{encoded:?}");
        }
        let mut boundary = vec!["a".repeat(200); 5];
        boundary.push(format!("{}.orna", "b".repeat(14)));
        assert_eq!(boundary.join("/").len(), 1024);
        assert!(path_decode_key_components(&boundary).is_ok());
        boundary[5].insert(0, 'b');
        assert!(path_decode_key_components(&boundary).is_err());
        assert!(path_decode_key_components(&[format!("{}.orna", "a".repeat(201))]).is_err());
    }
    #[test]
    fn construction_and_safe_containers_reject_aliases() {
        assert!(Value::new(Raw::Float(0x7ff8_0000_0000_0001)).is_err());
        assert_eq!(
            Value::float_bits(0x7ff8_0000_0000_0001).raw(),
            &Raw::Float(CANONICAL_NAN_BITS)
        );
        assert!(Value::new(tag(60011, Raw::Map(vec![]))).is_err());
        assert!(Value::new(tag(60016, Raw::Map(vec![]))).is_err());
        assert!(Value::new(tag(60012, Raw::Array(vec![]))).is_err());
    }
    #[test]
    fn sys_value_money_binds_its_currency_witness() {
        let currency = [7u8; 16];
        let other = [8u8; 16];
        let type_node = Raw::Array(vec![Raw::Int(8.into()), uuid_raw(currency)]);
        let amount = tag(
            60000,
            Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())]),
        );
        let valid = tag(
            60026,
            Raw::Array(vec![
                type_node.clone(),
                tag(60007, Raw::Array(vec![amount.clone(), uuid_raw(currency)])),
            ]),
        );
        assert!(Value::new(valid).is_ok());
        let invalid = tag(
            60026,
            Raw::Array(vec![
                type_node,
                tag(60007, Raw::Array(vec![amount, uuid_raw(other)])),
            ]),
        );
        assert!(Value::new(invalid).is_err());
    }
    #[test]
    fn schema_frozen_fallback_is_type_directed() {
        let table = [9u8; 16];
        let key = [1u8; 16];
        let stored = [2u8; 16];
        let type_int = Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]);
        let schema = |fallback: Raw| {
            Raw::Map(vec![
                (Raw::Int(0.into()), Raw::Int(1.into())),
                (Raw::Int(1.into()), uuid_raw(table)),
                (Raw::Int(2.into()), Raw::Array(vec![uuid_raw(key)])),
                (
                    Raw::Int(3.into()),
                    Raw::Array(vec![
                        Raw::Array(vec![
                            uuid_raw(key),
                            Raw::Text("id".into()),
                            type_int.clone(),
                            Raw::Int(0.into()),
                            Raw::Array(vec![Raw::Int(0.into())]),
                        ]),
                        Raw::Array(vec![
                            uuid_raw(stored),
                            Raw::Text("value".into()),
                            type_int.clone(),
                            Raw::Int(1.into()),
                            fallback,
                        ]),
                    ]),
                ),
                (Raw::Int(4.into()), Raw::Array(vec![])),
            ])
        };
        assert!(
            SchemaDescriptor::new(schema(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Int(7.into())
            ])))
            .is_ok()
        );
        assert!(
            SchemaDescriptor::new(schema(Raw::Array(vec![
                Raw::Int(1.into()),
                Raw::Text("not-an-int".into())
            ])))
            .is_err()
        );
    }
    #[test]
    fn snapshot_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/snapshot-vectors.json"
        ))
        .unwrap();
        let db = h("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let rt = h("101112131415161718191a1b1c1d1e1f").try_into().unwrap();
        let s = Snapshot::cwd(db, rt, 1.into()).unwrap();
        assert_eq!(
            encode_raw(&s.raw()).unwrap(),
            h(
                "8500d82550000102030405060708090a0b0c0d0e0fd82550101112131415161718191a1b1c1d1e1f015820732adfe16f749548c7184797f01a99247b4633cc89d32d5f47d99df94e53727e"
            )
        );
        for name in ["cwd_1", "cwd_2", "commit_sha1"] {
            let vector = fixture[name].as_str().unwrap();
            let raw = Value::decode(&h(vector)).unwrap().raw().clone();
            assert_eq!(
                encode_raw(&Snapshot::decode(&raw).unwrap().raw()).unwrap(),
                h(vector)
            );
        }
        let invalid = fixture["invalid_bare_cwd"].as_str().unwrap();
        assert!(Value::decode(&h(invalid)).is_ok());
        assert!(Snapshot::decode(Value::decode(&h(invalid)).unwrap().raw()).is_err());
    }
    #[test]
    fn fixture_path_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/path-vectors.json"
        ))
        .unwrap();
        for vector in fixture["round_trip"].as_array().unwrap() {
            let source = vector["value"].as_str().unwrap();
            let encoded = vector["encoded"].as_str().unwrap();
            assert_eq!(path_encode_component(source).unwrap(), encoded);
            assert_eq!(path_decode_component(encoded).unwrap(), source);
            assert_eq!(
                path_decode_key_components(&[format!("{encoded}.orna")]).unwrap(),
                vec![source]
            );
        }
        for vector in fixture["reject_encoded"].as_array().unwrap() {
            assert!(path_decode_component(vector["encoded"].as_str().unwrap()).is_err());
            assert!(
                path_decode_key_components(&[format!(
                    "{}.orna",
                    vector["encoded"].as_str().unwrap()
                )])
                .is_err()
            );
        }
        for vector in fixture["portable_collisions"].as_array().unwrap() {
            let left = path_encode_component(vector["left"].as_str().unwrap()).unwrap();
            let right = path_encode_component(vector["right"].as_str().unwrap()).unwrap();
            assert_eq!(
                path_collision_key(&left),
                vector["collision_key"].as_str().unwrap()
            );
            assert_eq!(path_collision_key(&left), path_collision_key(&right));
        }
        for vector in fixture["limits"]["component"].as_array().unwrap() {
            let input = "a".repeat(vector["encoded_length"].as_u64().unwrap() as usize);
            assert_eq!(
                path_encode_component(&input).is_ok(),
                vector["expect"] == "pass"
            );
        }
        for vector in fixture["limits"]["table_relative_path"].as_array().unwrap() {
            let size = vector["encoded_length"].as_u64().unwrap() as usize;
            let mut remaining = size - 5;
            let mut components = Vec::new();
            while remaining > 200 {
                components.push("a".repeat(200));
                remaining -= 201;
            }
            components.push("a".repeat(remaining));
            assert_eq!(
                path_encode_key_components(&components).is_ok(),
                vector["expect"] == "pass"
            );
        }
    }
    #[test]
    fn fixture_numeric_vectors_are_exact() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../reference/Orna-1.0.0/tests/numeric-vectors.json"
        ))
        .unwrap();
        assert_eq!(fixture["money_add"]["a"], "0.1");
        assert_eq!(fixture["money_add"]["b"], "0.2");
        assert_eq!(fixture["money_add"]["result"], "0.3");
        assert_eq!(fixture["mile_per_hour_mps"], "0.44704");
        let a = Decimal::new(1.into(), (-1).into());
        let b = Decimal::new(2.into(), (-1).into());
        assert_eq!(a.try_add(&b).unwrap(), Decimal::new(3.into(), (-1).into()));
        // 1609.344 / 3600 has the finite exact decimal supplied by the fixture.
        let mile = Decimal::new(1_609_344.into(), (-3).into());
        let seconds = Decimal::new(3_600.into(), 0.into());
        assert_eq!(
            mile.divide_exact(&seconds).unwrap(),
            Decimal::new(44_704.into(), (-5).into())
        );
    }
}
