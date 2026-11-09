//! Snapshot pins and their Git object-hash algorithm.

use crate::Result;
use crate::error::Error;
use crate::identity::{bytes32, domain_digest, uuid_array, uuid_raw};
use crate::raw::{Raw, Value, array, bytes, int_u64, integer, text};
use num_bigint::BigInt;
use num_bigint::Sign;

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

impl GitHash {
    pub const fn oid_width(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }
}

pub(crate) fn snapshot_database(snapshot: &Snapshot) -> [u8; 16] {
    match snapshot {
        Snapshot::Cwd { database, .. } | Snapshot::Commit { database, .. } => *database,
    }
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

    /// Encodes this pin as its canonical OVB-1 value.
    ///
    /// Snapshot variants are publicly constructible for compatibility, so an
    /// encoder must revalidate the generation-derived CWD identity instead of
    /// serializing a forged `id` supplied through the enum fields.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let canonical = Self::decode(&self.raw())?;
        Value::new(canonical.raw())?.encode()
    }

    /// Decodes a snapshot pin from canonical OVB-1 bytes. The inner decoder
    /// verifies the CWD id against its database, runtime, and full generation.
    pub fn decode_bytes(bytes: &[u8]) -> Result<Self> {
        let value = Value::decode(bytes)?;
        Self::decode(value.raw())
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
