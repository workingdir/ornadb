use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

/// The default TLS port for a remote OrnaDB endpoint.
pub const DEFAULT_REMOTE_PORT: u16 = 7443;

/// One database endpoint selected by a client session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DatabaseEndpoint {
    /// The managed local instance selected by name.
    ManagedLocal { instance: String },
    /// An explicit local Orna protocol Unix socket.
    UnixSocket { path: PathBuf },
    /// A local path selected by the host's workspace policy.
    LocalPath { path: PathBuf },
    /// A remote Orna protocol endpoint protected by TLS.
    RemoteTls {
        host: String,
        port: u16,
        database: String,
    },
}

impl DatabaseEndpoint {
    /// Returns the default managed local endpoint.
    pub fn managed_local() -> Self {
        Self::ManagedLocal {
            instance: "default".to_owned(),
        }
    }

    /// Parses one path or Orna endpoint URI.
    pub fn parse(value: &str) -> Result<Self, EndpointParseError> {
        if value.is_empty() {
            return Err(EndpointParseError::Invalid("the endpoint is empty"));
        }
        if value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(EndpointParseError::Invalid(
                "the endpoint contains a control character",
            ));
        }
        if value.starts_with('-') {
            return Err(EndpointParseError::Invalid(
                "the endpoint cannot start with '-'",
            ));
        }

        let Some((scheme, remainder)) = value.split_once("://") else {
            return Ok(Self::LocalPath {
                path: PathBuf::from(value),
            });
        };

        match scheme {
            "orna" => parse_orna_uri(remainder),
            "orna+unix" => parse_unix_uri(remainder),
            _ => Err(EndpointParseError::UnsupportedScheme(scheme.to_owned())),
        }
    }

    /// Returns the managed local instance name, when this is one.
    pub fn instance(&self) -> Option<&str> {
        match self {
            Self::ManagedLocal { instance } => Some(instance),
            Self::UnixSocket { .. } | Self::LocalPath { .. } | Self::RemoteTls { .. } => None,
        }
    }

    /// Returns the local socket or workspace path, when this is one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::UnixSocket { path } | Self::LocalPath { path } => Some(path),
            Self::ManagedLocal { .. } | Self::RemoteTls { .. } => None,
        }
    }

    /// Returns the remote host, when this is a remote endpoint.
    pub fn host(&self) -> Option<&str> {
        match self {
            Self::RemoteTls { host, .. } => Some(host),
            Self::ManagedLocal { .. } | Self::UnixSocket { .. } | Self::LocalPath { .. } => None,
        }
    }

    /// Returns the remote port, when this is a remote endpoint.
    pub fn port(&self) -> Option<u16> {
        match self {
            Self::RemoteTls { port, .. } => Some(*port),
            Self::ManagedLocal { .. } | Self::UnixSocket { .. } | Self::LocalPath { .. } => None,
        }
    }

    /// Returns the remote database name, when this is a remote endpoint.
    pub fn database(&self) -> Option<&str> {
        match self {
            Self::RemoteTls { database, .. } => Some(database),
            Self::ManagedLocal { .. } | Self::UnixSocket { .. } | Self::LocalPath { .. } => None,
        }
    }

    /// Returns whether this endpoint requires a remote transport.
    pub const fn is_remote(&self) -> bool {
        matches!(self, Self::RemoteTls { .. })
    }
}

impl FromStr for DatabaseEndpoint {
    type Err = EndpointParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for DatabaseEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManagedLocal { instance } => write!(formatter, "orna://local/{instance}"),
            Self::UnixSocket { path } => write!(formatter, "orna+unix://{}", path.display()),
            Self::LocalPath { path } => path.display().fmt(formatter),
            Self::RemoteTls {
                host,
                port,
                database,
            } => write!(formatter, "orna://{host}:{port}/{database}"),
        }
    }
}

/// A malformed or unsupported endpoint selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndpointParseError {
    /// The URI scheme is not supported by the Orna client.
    UnsupportedScheme(String),
    /// The endpoint has a closed-shape violation.
    Invalid(&'static str),
}

impl fmt::Display for EndpointParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedScheme(scheme) => {
                write!(formatter, "unsupported Orna endpoint scheme `{scheme}`")
            }
            Self::Invalid(reason) => write!(formatter, "invalid Orna endpoint: {reason}"),
        }
    }
}

impl std::error::Error for EndpointParseError {}

fn parse_unix_uri(remainder: &str) -> Result<DatabaseEndpoint, EndpointParseError> {
    if remainder.is_empty() || !remainder.starts_with('/') {
        return Err(EndpointParseError::Invalid(
            "a Unix endpoint needs an absolute path",
        ));
    }
    reject_uri_suffixes(remainder)?;
    let path = percent_decode_path(&remainder[1..])?;
    if path.is_empty() {
        return Err(EndpointParseError::Invalid("the Unix socket path is empty"));
    }
    Ok(DatabaseEndpoint::UnixSocket {
        path: PathBuf::from(format!("/{path}")),
    })
}

fn parse_orna_uri(remainder: &str) -> Result<DatabaseEndpoint, EndpointParseError> {
    reject_uri_suffixes(remainder)?;
    let (authority, path) = remainder
        .split_once('/')
        .ok_or(EndpointParseError::Invalid("the URI needs a database path"))?;
    if authority.is_empty() || path.is_empty() {
        return Err(EndpointParseError::Invalid(
            "the URI needs a host and database path",
        ));
    }
    if authority.contains('@') {
        return Err(EndpointParseError::Invalid(
            "credentials are not allowed in an endpoint URI",
        ));
    }

    let (host, port) = parse_authority(authority)?;
    let database = percent_decode_database(path)?;
    if host == "local" {
        if port.is_some() || database.contains('/') {
            return Err(EndpointParseError::Invalid(
                "managed local endpoints accept one instance name and no port",
            ));
        }
        return Ok(DatabaseEndpoint::ManagedLocal { instance: database });
    }

    Ok(DatabaseEndpoint::RemoteTls {
        host,
        port: port.unwrap_or(DEFAULT_REMOTE_PORT),
        database,
    })
}

fn parse_authority(authority: &str) -> Result<(String, Option<u16>), EndpointParseError> {
    if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or(EndpointParseError::Invalid("the IPv6 host is not closed"))?;
        let host = &authority[1..end];
        if host.is_empty() {
            return Err(EndpointParseError::Invalid("the host is empty"));
        }
        let suffix = &authority[end + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            let port = suffix.strip_prefix(':').ok_or(EndpointParseError::Invalid(
                "the authority has invalid trailing text",
            ))?;
            Some(parse_port(port)?)
        };
        validate_host(host)?;
        return Ok((host.to_owned(), port));
    }

    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(parse_port(port)?)),
        None => (authority, None),
    };
    if host.contains(':') {
        return Err(EndpointParseError::Invalid(
            "IPv6 hosts must use square brackets",
        ));
    }
    validate_host(host)?;
    Ok((host.to_owned(), port))
}

fn parse_port(value: &str) -> Result<u16, EndpointParseError> {
    if value.is_empty() {
        return Err(EndpointParseError::Invalid("the port is empty"));
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(EndpointParseError::Invalid(
            "the port is not a valid non-zero u16",
        ))
}

fn validate_host(host: &str) -> Result<(), EndpointParseError> {
    if host.is_empty()
        || host.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte.is_ascii_whitespace()
                || matches!(byte, b'/' | b'?' | b'#')
        })
    {
        return Err(EndpointParseError::Invalid("the host is invalid"));
    }
    Ok(())
}

fn reject_uri_suffixes(value: &str) -> Result<(), EndpointParseError> {
    if value.contains('#') {
        return Err(EndpointParseError::Invalid("fragments are not allowed"));
    }
    if value.contains('?') {
        return Err(EndpointParseError::Invalid(
            "query parameters are not allowed",
        ));
    }
    Ok(())
}

fn percent_decode_database(value: &str) -> Result<String, EndpointParseError> {
    let mut segments = Vec::new();
    for segment in value.split('/') {
        if segment.is_empty() {
            return Err(EndpointParseError::Invalid(
                "the database path contains an empty segment",
            ));
        }
        segments.push(percent_decode(segment)?);
    }
    let database = segments.join("/");
    if database.is_empty() || database.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(EndpointParseError::Invalid("the database name is invalid"));
    }
    Ok(database)
}

fn percent_decode_path(value: &str) -> Result<String, EndpointParseError> {
    let decoded = percent_decode(value)?;
    if decoded.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(EndpointParseError::Invalid(
            "the path contains a control character",
        ));
    }
    Ok(decoded)
}

fn percent_decode(value: &str) -> Result<String, EndpointParseError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(EndpointParseError::Invalid(
                "the percent escape is incomplete",
            ));
        }
        let high = hex_digit(bytes[index + 1])?;
        let low = hex_digit(bytes[index + 2])?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded)
        .map_err(|_| EndpointParseError::Invalid("the percent-decoded value is not UTF-8"))
}

fn hex_digit(value: u8) -> Result<u8, EndpointParseError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(EndpointParseError::Invalid(
            "the percent escape is not hexadecimal",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_managed_local_and_path_endpoints() {
        assert_eq!(
            DatabaseEndpoint::parse("orna://local/default").expect("managed endpoint"),
            DatabaseEndpoint::ManagedLocal {
                instance: "default".to_owned(),
            }
        );
        assert_eq!(
            DatabaseEndpoint::parse("orna+unix:///tmp/orna/default/orna.sock")
                .expect("Unix endpoint"),
            DatabaseEndpoint::UnixSocket {
                path: PathBuf::from("/tmp/orna/default/orna.sock"),
            }
        );
        assert_eq!(
            DatabaseEndpoint::parse("./workspace").expect("path endpoint"),
            DatabaseEndpoint::LocalPath {
                path: PathBuf::from("./workspace"),
            }
        );
    }

    #[test]
    fn parses_remote_tls_endpoints_with_default_and_explicit_ports() {
        assert_eq!(
            DatabaseEndpoint::parse("orna://db.example.test/work").expect("remote endpoint"),
            DatabaseEndpoint::RemoteTls {
                host: "db.example.test".to_owned(),
                port: DEFAULT_REMOTE_PORT,
                database: "work".to_owned(),
            }
        );
        assert_eq!(
            DatabaseEndpoint::parse("orna://[::1]:7444/team%2Fwork").expect("IPv6 endpoint"),
            DatabaseEndpoint::RemoteTls {
                host: "::1".to_owned(),
                port: 7444,
                database: "team/work".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_credentials_fragments_queries_and_ambiguous_local_forms() {
        for value in [
            "orna://user@db.example.test/work",
            "orna://db.example.test/work?sslmode=disable",
            "orna://db.example.test/work#fragment",
            "orna://local:7443/default",
            "orna://local/default/extra",
            "orna://db.example.test/",
            "orna+unix://relative.sock",
            "orna+unix:///tmp/orna.sock?x=1",
            "postgresql://db/work",
        ] {
            assert!(DatabaseEndpoint::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn rejects_malformed_hosts_ports_and_escapes() {
        for value in [
            "orna:///work",
            "orna://db.example.test:0/work",
            "orna://db.example.test:not-a-port/work",
            "orna://2001:db8::1/work",
            "orna://[::1/work",
            "orna://db.example.test/%zz",
            "orna://db.example.test/%FF",
            "orna://db.example.test//work",
        ] {
            assert!(DatabaseEndpoint::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn display_never_contains_userinfo() {
        let endpoint = DatabaseEndpoint::parse("orna://db.example.test/work").expect("endpoint");
        assert_eq!(endpoint.to_string(), "orna://db.example.test:7443/work");
    }
}
