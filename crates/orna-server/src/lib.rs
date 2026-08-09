//! Shared configuration for the OrnaDB server host.
//!
//! This crate owns the private PostgreSQL connection boundary. It accepts
//! one environment variable and keeps the parsed value separate from its
//! original text so later host operations do not need a second parser.

use std::{env, ffi::OsString, fmt, string::FromUtf8Error};

use url::{Host, Url};

const POSTGRES_URL_ENV: &str = "ORNA_SERVER_POSTGRES_URL";

/// A parsed, immutable connection target for the private server host.
///
/// The password is not included in the public representation. A future host
/// shell module within this crate can use the private field directly.
#[derive(Clone, Eq, PartialEq)]
pub struct ServerHostConfig {
    host: String,
    port: u16,
    user: String,
    database: String,
    password: Option<String>,
}

/// The reason that the private server-host configuration could not be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ServerHostConfigError {
    /// The environment variable is absent or has an empty value.
    MissingOrEmpty,
    /// The environment variable is present but does not have the accepted form.
    Invalid,
}

impl fmt::Display for ServerHostConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOrEmpty => {
                formatter.write_str("server host configuration is missing or empty")
            }
            Self::Invalid => formatter.write_str("server host configuration is invalid"),
        }
    }
}

impl std::error::Error for ServerHostConfigError {}

impl fmt::Debug for ServerHostConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerHostConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("database", &self.database)
            .field("password", &self.password.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl ServerHostConfig {
    /// Reads and parses `ORNA_SERVER_POSTGRES_URL`.
    pub fn from_env() -> Result<Self, ServerHostConfigError> {
        parse_env_value(env::var_os(POSTGRES_URL_ENV))
    }

    /// Parses one private PostgreSQL connection URL.
    pub fn parse(value: &str) -> Result<Self, ServerHostConfigError> {
        if value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
        {
            return Err(ServerHostConfigError::Invalid);
        }

        let parsed = Url::parse(value).map_err(|_| ServerHostConfigError::Invalid)?;

        if parsed.scheme() != "postgresql"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || value.contains('\\')
            || !has_one_userinfo_separator(value)
        {
            return Err(ServerHostConfigError::Invalid);
        }

        let host = match parsed.host() {
            Some(Host::Domain(host)) if is_supported_domain(host) => host.to_owned(),
            Some(Host::Ipv4(host)) => host.to_string(),
            Some(Host::Ipv6(host)) => host.to_string(),
            Some(Host::Domain(_)) => return Err(ServerHostConfigError::Invalid),
            None => return Err(ServerHostConfigError::Invalid),
        };

        let port = parsed
            .port()
            .filter(|port| *port != 0)
            .ok_or(ServerHostConfigError::Invalid)?;

        let user = decode_component(parsed.username())?;
        if user.is_empty() {
            return Err(ServerHostConfigError::Invalid);
        }

        let password = match parsed.password() {
            Some(password) => Some(decode_component(password)?),
            None if has_password_separator(value) => Some(String::new()),
            None => None,
        };

        let database_text = raw_database_component(value).ok_or(ServerHostConfigError::Invalid)?;
        let database = decode_component(database_text)?;
        if database.is_empty() {
            return Err(ServerHostConfigError::Invalid);
        }

        Ok(Self {
            host,
            port,
            user,
            database,
            password,
        })
    }

    /// Returns the single resolved TCP host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the explicit TCP port.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Returns the configured PostgreSQL role.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// Returns the configured database name.
    pub fn database(&self) -> &str {
        &self.database
    }
}

fn parse_env_value(value: Option<OsString>) -> Result<ServerHostConfig, ServerHostConfigError> {
    let value = value.ok_or(ServerHostConfigError::MissingOrEmpty)?;
    let value = value
        .into_string()
        .map_err(|_| ServerHostConfigError::Invalid)?;
    if value.is_empty() {
        return Err(ServerHostConfigError::MissingOrEmpty);
    }

    ServerHostConfig::parse(&value)
}

fn has_one_userinfo_separator(value: &str) -> bool {
    let Some(authority) = value
        .strip_prefix("postgresql://")
        .and_then(|rest| rest.split_once('/').map(|(authority, _)| authority))
    else {
        return false;
    };

    authority.bytes().filter(|byte| *byte == b'@').count() == 1
}

fn has_password_separator(value: &str) -> bool {
    value
        .strip_prefix("postgresql://")
        .and_then(|rest| rest.split_once('/').map(|(authority, _)| authority))
        .and_then(|authority| authority.split_once('@').map(|(userinfo, _)| userinfo))
        .is_some_and(|userinfo| userinfo.contains(':'))
}

fn raw_database_component(value: &str) -> Option<&str> {
    let (_, database) = value.strip_prefix("postgresql://")?.split_once('/')?;
    (!database.is_empty() && !database.contains('/')).then_some(database)
}

fn is_supported_domain(host: &str) -> bool {
    !host.is_empty()
        && !host.chars().any(|character| {
            matches!(
                character,
                ',' | '/' | '\\' | ':' | '[' | ']' | '%' | '?' | '#'
            ) || character.is_whitespace()
        })
}

fn decode_component(value: &str) -> Result<String, ServerHostConfigError> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut characters = value.bytes();

    while let Some(byte) = characters.next() {
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }

        let high = characters
            .next()
            .and_then(hex_value)
            .ok_or(ServerHostConfigError::Invalid)?;
        let low = characters
            .next()
            .and_then(hex_value)
            .ok_or(ServerHostConfigError::Invalid)?;
        bytes.push(high << 4 | low);
    }

    let decoded =
        String::from_utf8(bytes).map_err(|_: FromUtf8Error| ServerHostConfigError::Invalid)?;
    if decoded.contains('\0') {
        return Err(ServerHostConfigError::Invalid);
    }

    Ok(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_complete_supported_url() {
        let config = ServerHostConfig::parse(
            "postgresql://read%20user:p%40ss%20word@db.example:5433/catalogue",
        )
        .expect("valid configuration");

        assert_eq!(config.host(), "db.example");
        assert_eq!(config.port(), 5433);
        assert_eq!(config.user(), "read user");
        assert_eq!(config.database(), "catalogue");
        assert_eq!(config.password.as_deref(), Some("p@ss word"));
    }

    #[test]
    fn decodes_escaped_delimiters_and_preserves_plus_signs_as_data() {
        let config = ServerHostConfig::parse(
            "postgresql://u%2Fname:p%40ss%3Aplus+sign@db:5432/catalogue%2Fpart",
        )
        .expect("valid escaped components");

        assert_eq!(config.user(), "u/name");
        assert_eq!(config.password.as_deref(), Some("p@ss:plus+sign"));
        assert_eq!(config.database(), "catalogue/part");
    }

    #[test]
    fn preserves_database_dot_segments_as_names() {
        for (value, expected) in [
            ("postgresql://u@db:5432/.", "."),
            ("postgresql://u@db:5432/..", ".."),
            ("postgresql://u@db:5432/%2E", "."),
            ("postgresql://u@db:5432/%2e%2E", ".."),
        ] {
            let config = ServerHostConfig::parse(value).expect("valid database name");
            assert_eq!(config.database(), expected);
        }
    }

    #[test]
    fn parses_ipv4_and_bracketed_ipv6_hosts() {
        let ipv4 = ServerHostConfig::parse("postgresql://u@127.0.0.1:1/db").unwrap();
        let ipv6 = ServerHostConfig::parse("postgresql://u@[::1]:65535/db").unwrap();

        assert_eq!(ipv4.host(), "127.0.0.1");
        assert_eq!(ipv6.host(), "::1");
        assert_eq!(ipv6.port(), 65535);
    }

    #[test]
    fn preserves_absent_and_explicit_empty_passwords() {
        let absent = ServerHostConfig::parse("postgresql://u@db:5432/db").unwrap();
        let empty = ServerHostConfig::parse("postgresql://u:@db:5432/db").unwrap();

        assert_eq!(absent.password.as_deref(), None);
        assert_eq!(empty.password.as_deref(), Some(""));
    }

    #[test]
    fn parses_the_required_environment_value() {
        assert!(parse_env_value(Some(OsString::from("postgresql://u@db:5432/db",))).is_ok());
        assert_eq!(
            parse_env_value(None),
            Err(ServerHostConfigError::MissingOrEmpty)
        );
    }

    #[test]
    fn rejects_empty_environment_value() {
        assert_eq!(
            parse_env_value(Some(OsString::new())),
            Err(ServerHostConfigError::MissingOrEmpty)
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_environment_value_as_invalid() {
        let value = non_unicode_environment_value();

        assert_eq!(
            parse_env_value(Some(value)),
            Err(ServerHostConfigError::Invalid)
        );
    }

    #[cfg(unix)]
    fn non_unicode_environment_value() -> OsString {
        use std::os::unix::ffi::OsStringExt;

        OsString::from_vec(b"postgresql://u@db:5432/db\xff".to_vec())
    }

    #[test]
    fn rejects_other_schemes_and_missing_components() {
        for value in [
            "postgres://u@db:5432/db",
            "postgresql:///db",
            "postgresql://@db:5432/db",
            "postgresql://u@db:5432/",
            "postgresql://u@db/db",
            "postgresql://u@db:0/db",
            "postgresql://u@db:65536/db",
            "postgresql://u@db:abc/db",
        ] {
            assert_eq!(
                ServerHostConfig::parse(value),
                Err(ServerHostConfigError::Invalid),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_multiple_hosts_socket_service_options_query_and_fragment() {
        for value in [
            "postgresql://u@db1,db2:5432/db",
            "postgresql://u@/var/run/postgresql/db",
            "postgresql://u@db:5432/db?sslmode=disable",
            "postgresql://u@db:5432/db#fragment",
            "postgresql://u@db:5432/db?service=prod",
        ] {
            assert_eq!(
                ServerHostConfig::parse(value),
                Err(ServerHostConfigError::Invalid),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_malformed_escapes_invalid_utf8_and_nul() {
        for value in [
            "postgresql://u%2@db:5432/db",
            "postgresql://u%GG@db:5432/db",
            "postgresql://u%FF@db:5432/db",
            "postgresql://u%00@db:5432/db",
            "postgresql://u@db:5432/db%00",
            "postgresql://u:p%00@db:5432/db",
        ] {
            assert_eq!(
                ServerHostConfig::parse(value),
                Err(ServerHostConfigError::Invalid),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_unescaped_extra_path_and_userinfo_separators() {
        for value in [
            "postgresql://u@db:5432/db/extra",
            "postgresql://u@x@db:5432/db",
            "postgresql://u@db:5432/db\\extra",
        ] {
            assert_eq!(
                ServerHostConfig::parse(value),
                Err(ServerHostConfigError::Invalid),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_raw_controls_and_spaces_before_url_normalisation() {
        for value in [
            "\tpostgresql://u@db:5432/db",
            "postgresql://u@db:5432/db\n",
            "postgresql://us\ner@db:5432/db",
            "postgresql://u:p\tass@db:5432/db",
            "postgresql://u@d b:5432/db",
            "postgresql://u@db:5432/d\rb",
            "postgresql://u@db:5432/db\u{7f}",
        ] {
            assert_eq!(
                ServerHostConfig::parse(value),
                Err(ServerHostConfigError::Invalid),
                "input with a raw control or space was accepted"
            );
        }
    }

    #[test]
    fn debug_redacts_password_and_does_not_keep_the_original_url() {
        let config = ServerHostConfig::parse("postgresql://user:super-secret@db:5432/db").unwrap();
        let debug = format!("{config:?}");
        let display_error =
            ServerHostConfig::parse("postgresql://user:super-secret@db:5432/db?bad").unwrap_err();

        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("super-secret"));
        assert!(!debug.contains("postgresql://"));
        assert_eq!(display_error, ServerHostConfigError::Invalid);
        assert!(!display_error.to_string().contains("super-secret"));
    }
}
