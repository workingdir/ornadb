use super::{
    Diagnostic, RuntimeMode, StdSnapshotPin, SysHostContext, hex_digit, runtime_identity, sys_host,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Endpoint {
    ManagedLocal,
    Path(String),
    UnixSocket(String),
    RemoteTls(String),
}
impl Endpoint {
    pub(crate) fn parse(value: &str) -> Result<Self, Diagnostic> {
        if value.is_empty() {
            return Err(Diagnostic::usage(
                "E1004",
                "database endpoint is empty",
                "provide a local path, socket, or secure Orna URI",
            ));
        }
        if value.starts_with('-') || value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(Diagnostic::usage(
                "E1004",
                "database endpoint is invalid",
                "use a local path, an absolute Orna Unix socket URI, or a secure Orna URI",
            ));
        }
        if value.contains('@') || value.contains('#') || value.contains('?') {
            return Err(Diagnostic::usage(
                "E1005",
                "database endpoint contains unsupported credentials or URI parts",
                "use transport authentication and omit credentials, fragments, and query parameters",
            ));
        }
        if let Some(path) = value.strip_prefix("orna+unix://") {
            return valid_unix_socket_path(path)
                .then(|| Self::UnixSocket(path.to_owned()))
                .ok_or_else(|| {
                    Diagnostic::usage(
                        "E1004",
                        "database endpoint is invalid",
                        "use an absolute Orna Unix socket URI",
                    )
                });
        }
        if let Some(remainder) = value.strip_prefix("orna://") {
            let (authority, database) = remainder.split_once('/').ok_or_else(|| {
                Diagnostic::usage(
                    "E1004",
                    "database endpoint is invalid",
                    "use orna://HOST/DATABASE or orna://local/INSTANCE",
                )
            })?;
            if !valid_orna_authority(authority)
                || !valid_database_path(database)
                || ((authority == "local" || authority.starts_with("local:"))
                    && database.contains('/'))
                || authority.starts_with("local:")
            {
                return Err(Diagnostic::usage(
                    "E1004",
                    "database endpoint is invalid",
                    "use an absolute Orna Unix socket URI or a secure Orna URI with one valid authority",
                ));
            }
            return Ok(if authority == "local" {
                Self::Path(database.to_owned())
            } else {
                Self::RemoteTls(value.to_owned())
            });
        }
        (!value.contains("://") && valid_percent_text(value))
            .then(|| Self::Path(value.to_owned()))
            .ok_or_else(|| {
                Diagnostic::usage(
                    "E1004",
                    "database endpoint is invalid",
                    "use a local path, an absolute Orna Unix socket URI, or a secure Orna URI",
                )
            })
    }
}

pub(crate) fn valid_unix_socket_path(path: &str) -> bool {
    path.starts_with('/') && path.len() > 1 && valid_percent_text(path)
}

pub(crate) fn valid_orna_authority(authority: &str) -> bool {
    if authority.is_empty() || !valid_percent_text(authority) || authority.contains('@') {
        return false;
    }
    if let Some(host) = authority.strip_prefix('[') {
        let Some((host, suffix)) = host.split_once(']') else {
            return false;
        };
        return !host.is_empty()
            && (suffix.is_empty() || suffix.strip_prefix(':').and_then(valid_port).is_some());
    }
    let mut parts = authority.split(':');
    let Some(host) = parts.next() else {
        return false;
    };
    match (parts.next(), parts.next()) {
        (None, _) => !host.is_empty(),
        (Some(port), None) => !host.is_empty() && valid_port(port).is_some(),
        _ => false,
    }
}

pub(crate) fn valid_port(port: &str) -> Option<u16> {
    port.parse().ok().filter(|port| *port != 0)
}

pub(crate) fn valid_database_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|segment| !segment.is_empty())
        && valid_percent_text(path)
}

pub(crate) fn valid_percent_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_control() || byte.is_ascii_whitespace() || matches!(byte, b'?' | b'#') {
            return false;
        }
        if byte == b'%' {
            let Some(high) = bytes.get(index + 1).and_then(|byte| hex_digit(*byte)) else {
                return false;
            };
            let Some(low) = bytes.get(index + 2).and_then(|byte| hex_digit(*byte)) else {
                return false;
            };
            if (high << 4 | low).is_ascii_control() {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

pub(crate) fn local_project_path(endpoint: &Endpoint) -> Result<&str, Diagnostic> {
    match endpoint {
        Endpoint::ManagedLocal => Ok("."),
        Endpoint::Path(path) => Ok(path),
        Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => Err(Diagnostic::target(
            "E2100",
            "project checking requires a local Git worktree",
            "use `check` with the current worktree or `--db PATH check`",
        )),
    }
}

pub(crate) fn pure_sys_host_context(
    endpoint: &Endpoint,
    std_snapshot: StdSnapshotPin,
) -> Option<SysHostContext> {
    let path = local_project_path(endpoint).ok()?;
    let repository = orna_repository::Repository::discover(path).ok()?;
    let (identity, _) = runtime_identity(&repository).ok()?;
    Some(sys_host::sys_host_context(
        repository.worktree(),
        identity,
        RuntimeMode::Run,
        std_snapshot,
    ))
}
