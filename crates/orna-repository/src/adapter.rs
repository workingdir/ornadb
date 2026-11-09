//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

/// Bridges real Git observation to the shared Orna CWD contract. The runtime
/// store is the sole authority for database/runtime identity and monotonic
/// logical generation; a Git commit selector stays a Git `SnapshotRef`.
pub struct OrnaRepositoryAdapter<S> {
    repository: Repository,
    runtime: S,
}
impl<S> OrnaRepositoryAdapter<S> {
    pub fn new(repository: Repository, runtime: S) -> Self {
        Self {
            repository,
            runtime,
        }
    }
}
#[derive(Debug)]
pub enum OrnaRepositoryAdapterError<E> {
    Runtime(E),
    Repository(RepositoryError),
    Bare,
    Identity,
    NonMonotonicGeneration,
}
impl<E: fmt::Display> fmt::Display for OrnaRepositoryAdapterError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(f, "runtime identity store: {error}"),
            Self::Repository(error) => error.fmt(f),
            Self::Bare => f.write_str("a bare repository has no CWD"),
            Self::Identity => f.write_str("CWD capture belongs to a different database or runtime"),
            Self::NonMonotonicGeneration => {
                f.write_str("CWD publication must strictly advance logical generation")
            }
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for OrnaRepositoryAdapterError<E> {}
impl<S: orna_value::RuntimeIdentityStore> OrnaRepositoryAdapter<S> {
    fn identity(
        &self,
    ) -> Result<orna_value::RepositoryIdentity, OrnaRepositoryAdapterError<S::Error>> {
        Ok(orna_value::RepositoryIdentity {
            database_id: self
                .runtime
                .database_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
            repository_id: self
                .runtime
                .repository_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
        })
    }
    fn cwd(&self) -> Result<orna_value::CwdCapture, OrnaRepositoryAdapterError<S::Error>> {
        let _ = self
            .repository
            .cwd_generation(RuntimeGeneration::new(0))
            .map_err(OrnaRepositoryAdapterError::Repository)?;
        let capture = self
            .runtime
            .capture_cwd()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        self.validate_capture(&capture)?;
        Ok(capture)
    }
    fn validate_capture(
        &self,
        capture: &orna_value::CwdCapture,
    ) -> Result<(), OrnaRepositoryAdapterError<S::Error>> {
        let database_id = self
            .runtime
            .database_id()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        let runtime_id = self
            .runtime
            .runtime_id()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        if capture.database_id() != database_id || capture.runtime_id() != runtime_id {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        Ok(())
    }
}
impl<S: orna_value::RuntimeIdentityStore> orna_value::RepositoryGenerationAdapter
    for OrnaRepositoryAdapter<S>
{
    type Error = OrnaRepositoryAdapterError<S::Error>;
    fn require_cwd(&self) -> Result<orna_value::RepositoryIdentity, Self::Error> {
        self.identity()
    }
    fn profile(&self) -> Result<orna_value::RepositoryProfile, Self::Error> {
        Ok(orna_value::RepositoryProfile { bare: false })
    }
    fn committed_snapshot(&self) -> Result<Option<orna_value::CanonicalSnapshot>, Self::Error> {
        let Some(head) = self
            .repository
            .head()
            .map_err(OrnaRepositoryAdapterError::Repository)?
        else {
            return Ok(None);
        };
        let oid =
            decode_object_id(head.as_str()).map_err(OrnaRepositoryAdapterError::Repository)?;
        let algorithm = match oid.len() {
            20 => orna_value::GitHash::Sha1,
            32 => orna_value::GitHash::Sha256,
            _ => {
                return Err(OrnaRepositoryAdapterError::Repository(
                    RepositoryError::InvalidObjectId,
                ));
            }
        };
        Ok(Some(orna_value::CanonicalSnapshot::Commit {
            database: self
                .runtime
                .database_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
            algorithm,
            oid,
        }))
    }
    fn capture_cwd(
        &self,
        identity: orna_value::RepositoryIdentity,
    ) -> Result<orna_value::CwdCapture, Self::Error> {
        if identity != self.identity()? {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        self.cwd()
    }
    fn compare_and_set_cwd(
        &self,
        identity: orna_value::RepositoryIdentity,
        expected: &orna_value::CwdCapture,
        next: &orna_value::CwdCapture,
    ) -> Result<orna_value::CwdCas, Self::Error> {
        if identity != self.identity()? {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        let current = self.cwd()?;
        self.validate_capture(expected)?;
        self.validate_capture(next)?;
        if &current != expected {
            return Ok(orna_value::CwdCas::Stale { current });
        }
        if next.generation() <= expected.generation() {
            return Err(OrnaRepositoryAdapterError::NonMonotonicGeneration);
        }
        self.runtime
            .compare_and_set_cwd(expected, next)
            .map_err(OrnaRepositoryAdapterError::Runtime)
    }
}

fn decode_object_id(value: &str) -> Result<Vec<u8>, RepositoryError> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RepositoryError::InvalidObjectId);
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let (pairs, []) = value.as_bytes().as_chunks::<2>() else {
        return Err(RepositoryError::InvalidObjectId);
    };
    for pair in pairs {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or(RepositoryError::InvalidObjectId)?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or(RepositoryError::InvalidObjectId)?;
        bytes.push(u8::try_from((high << 4) | low).map_err(|_| RepositoryError::InvalidObjectId)?);
    }
    Ok(bytes)
}
