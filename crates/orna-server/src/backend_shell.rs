//! The host-only PostgreSQL escape hatch.

use std::fmt;

/// A failure that prevents the host-only backend shell from starting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BackendShellError {
    /// The command does not have three terminal standard streams.
    TerminalRequired,
    /// `ORNA_SERVER_POSTGRES_URL` is absent or empty.
    MissingConfiguration,
    /// `ORNA_SERVER_POSTGRES_URL` does not use the accepted form.
    InvalidConfiguration,
    /// An executable `psql` could not be found or started.
    PsqlUnavailable,
}

impl fmt::Display for BackendShellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::TerminalRequired => "orna: backend-shell must be run in an interactive terminal",
            Self::MissingConfiguration => "orna: backend-shell needs ORNA_SERVER_POSTGRES_URL",
            Self::InvalidConfiguration => {
                "orna: ORNA_SERVER_POSTGRES_URL must use postgresql://user[:password]@host:port/database"
            }
            Self::PsqlUnavailable => "orna: could not start psql from PATH",
        };

        formatter.write_str(message)
    }
}

impl std::error::Error for BackendShellError {}

#[cfg(unix)]
mod unix {
    use super::BackendShellError;
    use crate::{ServerHostConfig, ServerHostConfigError};
    use nix::unistd::{AccessFlags, access};
    use std::{
        convert::Infallible,
        env,
        ffi::{OsStr, OsString},
        fs,
        io::{self, IsTerminal},
        os::unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::PermissionsExt,
            process::CommandExt,
        },
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    const URL_ENV: &[u8] = b"ORNA_SERVER_POSTGRES_URL";
    const PGHOST: &[u8] = b"PGHOST";
    const PGPORT: &[u8] = b"PGPORT";
    const PGUSER: &[u8] = b"PGUSER";
    const PGDATABASE: &[u8] = b"PGDATABASE";
    const PGPASSWORD: &[u8] = b"PGPASSWORD";
    const PGPASSFILE: &[u8] = b"PGPASSFILE";
    const PGSSLMODE: &[u8] = b"PGSSLMODE";
    const PGGSSENCMODE: &[u8] = b"PGGSSENCMODE";

    /// Attaches the caller to the configured PostgreSQL backend.
    pub fn run_backend_shell() -> Result<Infallible, BackendShellError> {
        if !terminals_are_interactive() {
            return Err(BackendShellError::TerminalRequired);
        }

        let config = ServerHostConfig::from_env().map_err(map_configuration_error)?;
        let executable = resolve_psql(env::var_os("PATH").as_deref())
            .ok_or(BackendShellError::PsqlUnavailable)?;
        let specification = prepare_command(&config, &executable, env::vars_os());

        exec(specification).map_err(|_| BackendShellError::PsqlUnavailable)
    }

    fn map_configuration_error(error: ServerHostConfigError) -> BackendShellError {
        match error {
            ServerHostConfigError::MissingOrEmpty => BackendShellError::MissingConfiguration,
            ServerHostConfigError::Invalid => BackendShellError::InvalidConfiguration,
        }
    }

    fn terminals_are_interactive() -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal() && io::stderr().is_terminal()
    }

    fn resolve_psql(path: Option<&OsStr>) -> Option<PathBuf> {
        resolve_psql_with(path, |candidate| {
            fs::metadata(candidate)
                .map(|metadata| {
                    metadata.is_file()
                        && metadata.permissions().mode() & 0o111 != 0
                        && access(candidate, AccessFlags::X_OK).is_ok()
                })
                .unwrap_or(false)
        })
    }

    fn resolve_psql_with<F>(path: Option<&OsStr>, is_executable: F) -> Option<PathBuf>
    where
        F: FnMut(&Path) -> bool,
    {
        let mut is_executable = is_executable;
        let path = path?;
        let path = path.as_bytes();
        if path.is_empty() {
            return None;
        }

        for entry in path.split(|byte| *byte == b':') {
            if entry.is_empty() {
                continue;
            }

            let directory = Path::new(OsStr::from_bytes(entry));
            if !directory.is_absolute() {
                continue;
            }

            let candidate = directory.join("psql");
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }

        None
    }

    struct CommandSpecification {
        executable: PathBuf,
        arguments: Vec<OsString>,
        environment: Vec<(OsString, OsString)>,
    }

    fn prepare_command<I>(
        config: &ServerHostConfig,
        executable: &Path,
        inherited: I,
    ) -> CommandSpecification
    where
        I: IntoIterator<Item = (OsString, OsString)>,
    {
        let mut environment = inherited
            .into_iter()
            .filter(|(name, _)| !is_connection_input(name))
            .collect::<Vec<_>>();

        environment.extend([
            os_pair(PGHOST, config.host()),
            os_pair(PGPORT, &config.port().to_string()),
            os_pair(PGUSER, config.user()),
            os_pair(PGDATABASE, config.database()),
            os_pair(PGPASSFILE, "/dev/null"),
            os_pair(PGSSLMODE, "disable"),
            os_pair(PGGSSENCMODE, "disable"),
        ]);
        if let Some(password) = config.password.as_deref() {
            environment.push(os_pair(PGPASSWORD, password));
        }

        CommandSpecification {
            executable: executable.to_owned(),
            arguments: vec![OsString::from("--no-psqlrc")],
            environment,
        }
    }

    fn is_connection_input(name: &OsStr) -> bool {
        let name = name.as_bytes();
        name == URL_ENV || name.starts_with(b"PG")
    }

    fn os_pair(name: &[u8], value: &str) -> (OsString, OsString) {
        (OsString::from_vec(name.to_vec()), OsString::from(value))
    }

    fn exec(specification: CommandSpecification) -> Result<std::convert::Infallible, io::Error> {
        let mut command = Command::new(&specification.executable);
        command
            .env_clear()
            .envs(specification.environment)
            .args(specification.arguments)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let error = command.exec();
        Err(error)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::BTreeMap;
        use std::os::unix::ffi::OsStringExt;

        fn config(url: &str) -> ServerHostConfig {
            ServerHostConfig::parse(url).expect("test URL is valid")
        }

        fn environment_map(environment: &[(OsString, OsString)]) -> BTreeMap<Vec<u8>, Vec<u8>> {
            environment
                .iter()
                .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
                .collect()
        }

        #[test]
        fn path_search_requires_an_absolute_entry_and_finds_the_first_executable() {
            let path = OsString::from(":relative:/missing:/first:/second");
            let first = Path::new("/first/psql");
            let second = Path::new("/second/psql");
            let mut visited = Vec::new();
            let result = resolve_psql_with(Some(&path), |candidate| {
                visited.push(candidate.to_owned());
                candidate == first || candidate == second
            });

            assert_eq!(result.as_deref(), Some(first));
            assert_eq!(
                visited,
                vec![PathBuf::from("/missing/psql"), first.to_owned()]
            );
        }

        #[test]
        fn path_search_rejects_absent_empty_and_non_executable_entries() {
            let empty = OsString::new();
            assert_eq!(resolve_psql_with(None, |_| true), None);
            assert_eq!(resolve_psql_with(Some(&empty), |_| true), None);

            let relative = OsString::from("relative:also-relative");
            assert_eq!(resolve_psql_with(Some(&relative), |_| true), None);

            let no_executable = OsString::from("/first:/second");
            assert_eq!(resolve_psql_with(Some(&no_executable), |_| false), None);
        }

        #[test]
        fn path_search_preserves_absolute_directory_bytes_and_ignores_empty_entries() {
            let path = OsString::from("::/dir with spaces/$shell:/last");
            let expected = PathBuf::from("/dir with spaces/$shell/psql");
            let result = resolve_psql_with(Some(&path), |candidate| candidate == expected);

            assert_eq!(result, Some(expected));
        }

        #[test]
        fn child_environment_removes_url_and_all_raw_pg_names() {
            let non_unicode_pg = OsString::from_vec(b"PG\xffHOST".to_vec());
            let inherited = vec![
                (OsString::from("PATH"), OsString::from("/bin")),
                (OsString::from("DATABASE_URL"), OsString::from("other")),
                (
                    OsString::from("ORNA_SERVER_POSTGRES_URL"),
                    OsString::from("secret"),
                ),
                (OsString::from("PGHOST"), OsString::from("hostile")),
                (non_unicode_pg, OsString::from("hostile")),
            ];
            let specification = prepare_command(
                &config("postgresql://user:secret@db.example:5432/catalogue"),
                Path::new("/bin/psql"),
                inherited,
            );
            let environment = environment_map(&specification.environment);

            assert_eq!(environment.get(b"PATH".as_slice()), Some(&b"/bin".to_vec()));
            assert_eq!(
                environment.get(b"DATABASE_URL".as_slice()),
                Some(&b"other".to_vec())
            );
            assert!(!environment.contains_key(URL_ENV));
            assert_eq!(
                environment.get(b"PGHOST".as_slice()),
                Some(&b"db.example".to_vec())
            );
            assert!(!environment.contains_key(b"PG\xffHOST".as_slice()));
        }

        #[test]
        fn child_environment_maps_target_and_fixed_transport_values() {
            let specification = prepare_command(
                &config("postgresql://user@db.example:5432/catalogue"),
                Path::new("/bin/psql"),
                std::iter::empty(),
            );
            let environment = environment_map(&specification.environment);

            assert_eq!(
                environment.get(b"PGHOST".as_slice()),
                Some(&b"db.example".to_vec())
            );
            assert_eq!(
                environment.get(b"PGPORT".as_slice()),
                Some(&b"5432".to_vec())
            );
            assert_eq!(
                environment.get(b"PGUSER".as_slice()),
                Some(&b"user".to_vec())
            );
            assert_eq!(
                environment.get(b"PGDATABASE".as_slice()),
                Some(&b"catalogue".to_vec())
            );
            assert_eq!(
                environment.get(b"PGPASSFILE".as_slice()),
                Some(&b"/dev/null".to_vec())
            );
            assert_eq!(
                environment.get(b"PGSSLMODE".as_slice()),
                Some(&b"disable".to_vec())
            );
            assert_eq!(
                environment.get(b"PGGSSENCMODE".as_slice()),
                Some(&b"disable".to_vec())
            );
            assert!(!environment.contains_key(b"PGPASSWORD".as_slice()));
        }

        #[test]
        fn child_environment_preserves_absent_empty_and_nonempty_password_state() {
            let absent = prepare_command(
                &config("postgresql://user@db.example:5432/catalogue"),
                Path::new("/bin/psql"),
                std::iter::empty(),
            );
            let empty = prepare_command(
                &config("postgresql://user:@db.example:5432/catalogue"),
                Path::new("/bin/psql"),
                std::iter::empty(),
            );
            let present = prepare_command(
                &config("postgresql://user:secret@db.example:5432/catalogue"),
                Path::new("/bin/psql"),
                std::iter::empty(),
            );

            let password = |specification: &CommandSpecification| {
                environment_map(&specification.environment)
                    .get(b"PGPASSWORD".as_slice())
                    .cloned()
            };

            assert_eq!(password(&absent), None);
            assert_eq!(password(&empty), Some(Vec::new()));
            assert_eq!(password(&present), Some(b"secret".to_vec()));
        }

        #[test]
        fn command_specification_has_only_the_psqlrc_argument_and_absolute_executable() {
            let specification = prepare_command(
                &config("postgresql://user@db.example:5432/catalogue"),
                Path::new("/safe/path/psql"),
                std::iter::empty(),
            );

            assert_eq!(specification.executable, Path::new("/safe/path/psql"));
            assert_eq!(specification.arguments, vec![OsString::from("--no-psqlrc")]);
        }
    }
}

#[cfg(unix)]
pub use unix::run_backend_shell;

#[cfg(not(unix))]
compile_error!("orna-server backend shell requires a Unix host");

#[cfg(test)]
mod tests {
    use super::BackendShellError;
    use std::error::Error;

    #[test]
    fn errors_have_the_exact_human_readable_lines() {
        let cases = [
            (
                BackendShellError::TerminalRequired,
                "orna: backend-shell must be run in an interactive terminal",
            ),
            (
                BackendShellError::MissingConfiguration,
                "orna: backend-shell needs ORNA_SERVER_POSTGRES_URL",
            ),
            (
                BackendShellError::InvalidConfiguration,
                "orna: ORNA_SERVER_POSTGRES_URL must use postgresql://user[:password]@host:port/database",
            ),
            (
                BackendShellError::PsqlUnavailable,
                "orna: could not start psql from PATH",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            assert!(error.source().is_none());
            assert!(!format!("{error:?}").contains("secret"));
            assert!(!error.to_string().contains("secret"));
        }
    }
}
