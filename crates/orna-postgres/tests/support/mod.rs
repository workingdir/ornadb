//! Isolated PostgreSQL support for live kernel integration tests.

use std::{
    error::Error,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::{task::JoinHandle, task::yield_now};
use tokio_postgres::{Client, Config, NoTls};

pub type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const TEST_DATABASE_PREFIX: &str = "orna_test_";
const MAX_POSTGRES_IDENTIFIER_LENGTH: usize = 63;
static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Runs a test against a new private database and then removes that database.
///
/// The cleanup runs after both a successful test and an error return from the
/// test body. The database name always passes the identifier validation below.
pub async fn with_test_database<T, F, Future>(test: F) -> TestResult<T>
where
    F: FnOnce(TestDatabase) -> Future,
    Future: std::future::Future<Output = TestResult<T>>,
{
    let database = TestDatabase::create().await?;
    let test_result = test(database.clone()).await;
    let cleanup_result = database.cleanup().await;

    match (test_result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(test_error), Err(cleanup_error)) => Err(failure(format!(
            "test failed: {test_error}; temporary database cleanup failed: {cleanup_error}"
        ))),
    }
}

/// A uniquely named private PostgreSQL database for one integration test.
#[derive(Clone, Debug)]
pub struct TestDatabase {
    name: String,
    admin_connection_string: String,
}

impl TestDatabase {
    async fn create() -> TestResult<Self> {
        let name = unique_database_name()?;
        let admin_connection_string = std::env::var("ORNA_TEST_POSTGRES_ADMIN_URL").map_err(|_| {
            failure(
                "ORNA_TEST_POSTGRES_ADMIN_URL must identify the Compose PostgreSQL admin database",
            )
        })?;
        let database = Self {
            name,
            admin_connection_string,
        };
        let create_result = database
            .admin_statement(&identifier_statement("CREATE DATABASE", &database.name)?)
            .await;

        match create_result {
            Ok(()) => Ok(database),
            Err(create_error) => match database.cleanup().await {
                Ok(()) => Err(create_error),
                Err(cleanup_error) => Err(failure(format!(
                    "temporary database creation failed: {create_error}; cleanup failed: {cleanup_error}"
                ))),
            },
        }
    }

    /// Returns a libpq connection string for this temporary database.
    pub fn connection_string(&self) -> String {
        format!("{} dbname={}", self.admin_connection_string, self.name)
    }

    /// Returns PostgreSQL connection configuration for this temporary database.
    pub fn config(&self) -> TestResult<Config> {
        let mut config = self.admin_connection_string.parse::<Config>()?;
        config.dbname(&self.name);
        Ok(config)
    }

    /// Opens a direct inspection connection to this temporary database.
    pub async fn open(&self) -> TestResult<TestSession> {
        TestSession::connect(self.config()?).await
    }

    async fn cleanup(&self) -> TestResult<()> {
        self.admin_statement(&identifier_statement("DROP DATABASE", &self.name)?)
            .await
    }

    async fn admin_statement(&self, statement: &str) -> TestResult<()> {
        let session = TestSession::connect(self.admin_connection_string.parse::<Config>()?).await?;
        let statement_result = session
            .client()
            .batch_execute(statement)
            .await
            .map_err(boxed);
        let shutdown_result = session.shutdown().await;

        match (statement_result, shutdown_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(statement_error), Err(shutdown_error)) => Err(failure(format!(
                "PostgreSQL statement failed: {statement_error}; connection driver shutdown failed: {shutdown_error}"
            ))),
        }
    }
}

/// A PostgreSQL test connection whose driver has an explicit shutdown path.
pub struct TestSession {
    client: Client,
    driver: JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl TestSession {
    async fn connect(config: Config) -> TestResult<Self> {
        let (client, connection) = config.connect(NoTls).await?;
        let driver = tokio::spawn(connection);
        Ok(Self { client, driver })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Closes the client and waits for its connection driver task.
    pub async fn shutdown(self) -> TestResult<()> {
        let Self { client, driver } = self;
        drop(client);
        yield_now().await;
        driver.await?.map_err(boxed)
    }
}

/// Builds a test error without using panic-based assertions.
pub fn failure(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    Box::new(TestFailure(message.into()))
}

#[derive(Debug)]
struct TestFailure(String);

impl fmt::Display for TestFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for TestFailure {}

fn boxed<E>(error: E) -> Box<dyn Error + Send + Sync>
where
    E: Error + Send + Sync + 'static,
{
    Box::new(error)
}

fn unique_database_name() -> TestResult<String> {
    let process_id = std::process::id();
    let timestamp_hex = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let sequence_hex = DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!("{TEST_DATABASE_PREFIX}{process_id}_{timestamp_hex:x}_{sequence_hex:x}");

    if valid_test_database_name(&name) {
        Ok(name)
    } else {
        Err(failure(
            "generated PostgreSQL test database identifier is invalid",
        ))
    }
}

fn identifier_statement(operation: &str, name: &str) -> TestResult<String> {
    if !matches!(operation, "CREATE DATABASE" | "DROP DATABASE") {
        return Err(failure("unsupported PostgreSQL identifier operation"));
    }
    if !valid_test_database_name(name) {
        return Err(failure(
            "refused unsafe PostgreSQL test database identifier",
        ));
    }

    // PostgreSQL does not parameterise identifiers. This is the only SQL text
    // interpolation in this support module. The generated name has a fixed
    // prefix plus trusted lowercase decimal and hexadecimal components.
    Ok(match operation {
        "CREATE DATABASE" => format!("CREATE DATABASE {name}"),
        "DROP DATABASE" => format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
        _ => return Err(failure("unsupported PostgreSQL identifier operation")),
    })
}

fn valid_test_database_name(name: &str) -> bool {
    name.len() <= MAX_POSTGRES_IDENTIFIER_LENGTH
        && name
            .strip_prefix(TEST_DATABASE_PREFIX)
            .is_some_and(|suffix| !suffix.is_empty())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}
