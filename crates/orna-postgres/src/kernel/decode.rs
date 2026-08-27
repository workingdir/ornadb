// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
use tokio_postgres::{Row, types::FromSqlOwned};

use crate::PostgresKernelError;

#[derive(Clone, Debug)]
pub(crate) struct DurableRecord {
    relation: &'static str,
    record: String,
}

impl DurableRecord {
    pub(crate) fn new(relation: &'static str, record: impl Into<String>) -> Self {
        Self {
            relation,
            record: record.into(),
        }
    }

    pub(crate) fn column<T>(
        &self,
        row: &Row,
        column: &'static str,
        rule: &'static str,
    ) -> Result<T, PostgresKernelError>
    where
        T: FromSqlOwned,
    {
        row.try_get(column)
            .map_err(|source| PostgresKernelError::RowDecode {
                relation: self.relation,
                record: self.record.clone(),
                column,
                rule,
                source,
            })
    }

    pub(crate) fn invariant(&self, rule: &'static str) -> PostgresKernelError {
        PostgresKernelError::DurableInvariant {
            relation: self.relation,
            record: self.record.clone(),
            rule,
        }
    }
}

pub(crate) fn identity_bytes(
    bytes: Vec<u8>,
    record: &DurableRecord,
    rule: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    bytes.try_into().map_err(|_| record.invariant(rule))
}

pub(crate) fn optional_identity_bytes(
    bytes: Option<Vec<u8>>,
    record: &DurableRecord,
    rule: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    bytes
        .map(|bytes| identity_bytes(bytes, record, rule))
        .transpose()
}

pub(crate) fn digest_bytes(
    bytes: Vec<u8>,
    record: &DurableRecord,
    rule: &'static str,
) -> Result<[u8; 32], PostgresKernelError> {
    bytes.try_into().map_err(|_| record.invariant(rule))
}

pub(crate) fn u32_from_i64(
    value: i64,
    record: &DurableRecord,
    rule: &'static str,
) -> Result<u32, PostgresKernelError> {
    u32::try_from(value).map_err(|_| record.invariant(rule))
}

pub(crate) fn u64_from_i64(
    value: i64,
    record: &DurableRecord,
    rule: &'static str,
) -> Result<u64, PostgresKernelError> {
    u64::try_from(value).map_err(|_| record.invariant(rule))
}

pub(crate) fn exact_enum<T: Copy>(
    value: &str,
    variants: &[(&str, T)],
    record: &DurableRecord,
    rule: &'static str,
) -> Result<T, PostgresKernelError> {
    variants
        .iter()
        .find_map(|(name, decoded)| (*name == value).then_some(*decoded))
        .ok_or_else(|| record.invariant(rule))
}

#[cfg(test)]
mod tests {
    use super::{
        DurableRecord, digest_bytes, exact_enum, identity_bytes, optional_identity_bytes,
        u32_from_i64, u64_from_i64,
    };

    fn record() -> DurableRecord {
        DurableRecord::new("_orna_kernel.test", "record-1")
    }

    #[test]
    fn identities_require_exactly_sixteen_bytes() {
        assert_eq!(
            identity_bytes(vec![7; 16], &record(), "identity must be 16 bytes")
                .expect("exact identity"),
            [7; 16]
        );
        assert!(identity_bytes(vec![7; 15], &record(), "identity must be 16 bytes").is_err());
        assert!(identity_bytes(vec![7; 17], &record(), "identity must be 16 bytes").is_err());
        assert_eq!(
            optional_identity_bytes(None, &record(), "identity must be 16 bytes")
                .expect("null identity"),
            None
        );
    }

    #[test]
    fn digests_require_exactly_thirty_two_bytes() {
        assert_eq!(
            digest_bytes(vec![9; 32], &record(), "digest must be 32 bytes").expect("exact digest"),
            [9; 32]
        );
        assert!(digest_bytes(vec![9; 31], &record(), "digest must be 32 bytes").is_err());
        assert!(digest_bytes(vec![9; 33], &record(), "digest must be 32 bytes").is_err());
    }

    #[test]
    fn signed_integers_use_checked_unsigned_conversions() {
        assert_eq!(u32_from_i64(17, &record(), "u32").expect("valid u32"), 17);
        assert!(u32_from_i64(-1, &record(), "u32").is_err());
        assert!(u32_from_i64(i64::from(u32::MAX) + 1, &record(), "u32").is_err());

        assert_eq!(u64_from_i64(29, &record(), "u64").expect("valid u64"), 29);
        assert!(u64_from_i64(-1, &record(), "u64").is_err());
    }

    #[test]
    fn enums_accept_only_an_exact_declared_value() {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        enum Algorithm {
            Sha256,
        }

        let variants = &[("sha256", Algorithm::Sha256)];
        assert_eq!(
            exact_enum("sha256", variants, &record(), "algorithm").expect("known algorithm"),
            Algorithm::Sha256
        );
        assert!(exact_enum("SHA256", variants, &record(), "algorithm").is_err());
        assert!(exact_enum("sha512", variants, &record(), "algorithm").is_err());
    }
}
