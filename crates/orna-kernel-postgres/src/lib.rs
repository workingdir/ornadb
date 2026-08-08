//! Concrete PostgreSQL storage and transaction kernel for OrnaDB.
//!
//! PostgreSQL is private implementation machinery. This crate does not expose
//! PostgreSQL as an Orna language, protocol, or catalogue contract.

use std::{error::Error, fmt, str::FromStr};

use tokio::task::{JoinError, JoinHandle};
use tokio_postgres::{Client, Config, NoTls};

mod bootstrap;
mod physical;

pub use bootstrap::ActiveRevision;

/// A concrete connection point for the private PostgreSQL kernel.
#[derive(Clone)]
pub struct PostgresKernel {
    config: Config,
}

impl PostgresKernel {
    /// Creates a kernel from an already parsed PostgreSQL connection config.
    pub const fn new(config: Config) -> Self {
        Self { config }
    }

    /// Verifies that the configured private kernel accepts a query.
    pub async fn health_check(&self) -> Result<(), PostgresKernelError> {
        let session = self.open().await?;
        let query_result = session.client.query_one("SELECT 1", &[]).await;
        let shutdown_result = session.shutdown().await;

        query_result.map_err(PostgresKernelError::Database)?;
        shutdown_result
    }

    async fn open(&self) -> Result<PostgresSession, PostgresKernelError> {
        let (client, connection) = self
            .config
            .connect(NoTls)
            .await
            .map_err(PostgresKernelError::Database)?;
        let driver = tokio::spawn(connection);

        Ok(PostgresSession { client, driver })
    }
}

impl FromStr for PostgresKernel {
    type Err = PostgresKernelError;

    fn from_str(connection_string: &str) -> Result<Self, Self::Err> {
        connection_string
            .parse::<Config>()
            .map(Self::new)
            .map_err(PostgresKernelError::Configuration)
    }
}

struct PostgresSession {
    client: Client,
    driver: JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl PostgresSession {
    async fn shutdown(self) -> Result<(), PostgresKernelError> {
        let Self { client, driver } = self;
        drop(client);
        driver
            .await
            .map_err(PostgresKernelError::DriverTask)?
            .map_err(PostgresKernelError::Database)
    }
}

/// A failure to configure or communicate with the private PostgreSQL kernel.
#[derive(Debug)]
pub enum PostgresKernelError {
    /// The configured PostgreSQL connection parameters are invalid.
    Configuration(tokio_postgres::Error),
    /// PostgreSQL rejected or failed a connection or query.
    Database(tokio_postgres::Error),
    /// The asynchronous PostgreSQL connection driver terminated abnormally.
    DriverTask(JoinError),
    /// A recorded schema migration does not match this binary.
    MigrationMismatch {
        /// The incompatible migration version.
        version: i64,
    },
    /// Protected catalogue rows violate a durable kernel invariant.
    CatalogueInvariant(&'static str),
}

impl fmt::Display for PostgresKernelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => {
                write!(formatter, "invalid PostgreSQL configuration: {error}")
            }
            Self::Database(error) => {
                write!(formatter, "private PostgreSQL kernel failure: {error}")
            }
            Self::DriverTask(error) => {
                write!(
                    formatter,
                    "private PostgreSQL connection task failed: {error}"
                )
            }
            Self::MigrationMismatch { version } => {
                write!(
                    formatter,
                    "PostgreSQL kernel migration {version} does not match this binary"
                )
            }
            Self::CatalogueInvariant(message) => {
                write!(
                    formatter,
                    "private PostgreSQL catalogue invariant failed: {message}"
                )
            }
        }
    }
}

impl Error for PostgresKernelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(error) | Self::Database(error) => Some(error),
            Self::DriverTask(error) => Some(error),
            Self::MigrationMismatch { .. } | Self::CatalogueInvariant(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{PostgresKernel, PostgresKernelError};

    #[test]
    fn parses_connection_parameters_without_connecting() {
        let kernel =
            PostgresKernel::from_str("host=127.0.0.1 port=55432 dbname=ornadb_dev user=ornadb_dev");

        assert!(kernel.is_ok());
    }

    #[test]
    fn rejects_invalid_connection_parameters() {
        let error = PostgresKernel::from_str("host=127.0.0.1 port=not-a-number")
            .err()
            .expect("invalid port must fail");

        assert!(matches!(error, PostgresKernelError::Configuration(_)));
    }

    #[tokio::test]
    #[ignore = "requires the private PostgreSQL development harness"]
    async fn probes_a_running_private_kernel() {
        let connection_string = std::env::var("ORNA_TEST_POSTGRES_URL")
            .expect("ORNA_TEST_POSTGRES_URL must identify the test kernel");
        let kernel = PostgresKernel::from_str(&connection_string).expect("config must parse");

        kernel.health_check().await.expect("kernel must answer");
    }
}
