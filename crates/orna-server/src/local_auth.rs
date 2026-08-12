use std::{
    fmt, io,
    mem::{MaybeUninit, size_of},
    os::{fd::AsRawFd, unix::net::UnixStream},
};

use orna_core::security::AuthenticatedSession;
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};

/// A failure while authenticating a connected local client.
#[derive(Debug)]
#[non_exhaustive]
pub enum LocalAuthenticationError {
    /// Linux did not provide credentials for the connected Unix peer.
    PeerCredentials {
        /// The failed `SO_PEERCRED` operation.
        source: io::Error,
    },
    /// The protected kernel security snapshot rejected the peer UID.
    Kernel {
        /// The kernel authentication failure.
        source: PostgresKernelError,
    },
}

impl fmt::Display for LocalAuthenticationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PeerCredentials { .. } => {
                formatter.write_str("could not authenticate the local connection")
            }
            Self::Kernel { source } => source.fmt(formatter),
        }
    }
}

impl std::error::Error for LocalAuthenticationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PeerCredentials { source } => Some(source),
            Self::Kernel { source } => Some(source),
        }
    }
}

/// Authenticates the operating-system peer of a connected Unix stream.
pub async fn authenticate_local_stream(
    kernel: &PostgresKernel,
    stream: &UnixStream,
) -> Result<AuthenticatedSession, LocalAuthenticationError> {
    let uid = peer_uid(stream)?;
    kernel
        .authenticate_local_peer(uid)
        .await
        .map_err(|source| LocalAuthenticationError::Kernel { source })
}

fn peer_uid(stream: &UnixStream) -> Result<u32, LocalAuthenticationError> {
    let mut credentials = MaybeUninit::<nix::libc::ucred>::uninit();
    let mut length = size_of::<nix::libc::ucred>() as nix::libc::socklen_t;
    // SAFETY: `credentials` points to writable storage for exactly `length`
    // bytes, and both pointers remain valid for the duration of getsockopt.
    let status = unsafe {
        nix::libc::getsockopt(
            stream.as_raw_fd(),
            nix::libc::SOL_SOCKET,
            nix::libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(LocalAuthenticationError::PeerCredentials {
            source: io::Error::last_os_error(),
        });
    }
    if length as usize != size_of::<nix::libc::ucred>() {
        return Err(LocalAuthenticationError::PeerCredentials {
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "SO_PEERCRED returned an unexpected credential size",
            ),
        });
    }
    // SAFETY: getsockopt succeeded and reported that it initialised the entire
    // `ucred` value.
    Ok(unsafe { credentials.assume_init() }.uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_identity_only_from_the_connected_unix_peer() {
        let (accepted, _client) = UnixStream::pair().expect("Unix stream pair");

        assert_eq!(
            peer_uid(&accepted).expect("connected peer credentials"),
            nix::unistd::getuid().as_raw(),
        );
    }

    #[test]
    fn peer_credential_errors_hide_operating_system_details() {
        let error = LocalAuthenticationError::PeerCredentials {
            source: io::Error::from_raw_os_error(nix::libc::EBADF),
        };

        assert_eq!(
            error.to_string(),
            "could not authenticate the local connection"
        );
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some(io::Error::from_raw_os_error(nix::libc::EBADF).to_string()),
        );
    }
}
