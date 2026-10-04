//! HTTP host bindings are unavailable in the browser evaluator build.

use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpProviderError {
    Denied,
    InvalidRequest,
    HeaderLimit,
    BodyLimit,
    TimedOut,
    Unavailable,
    Cancelled,
}

impl HttpProviderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Denied => "sys.host.http.denied",
            Self::InvalidRequest => "sys.host.http.invalid_request",
            Self::HeaderLimit => "sys.host.http.header_limit",
            Self::BodyLimit => "sys.host.http.body_limit",
            Self::TimedOut => "sys.host.http.timeout",
            Self::Unavailable => "sys.host.http.unavailable",
            Self::Cancelled => "sys.host.http.cancelled",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub type HostHttpHandle = [u8; 16];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HttpProvider;

impl HttpProvider {
    pub fn new(
        maximum_timeout: Duration,
        maximum_header_bytes: usize,
        _maximum_body_bytes: usize,
    ) -> Result<Self, HttpProviderError> {
        if maximum_timeout.is_zero() || maximum_header_bytes == 0 {
            return Err(HttpProviderError::InvalidRequest);
        }
        Ok(Self)
    }

    pub fn allow_origin(&mut self, _origin: &str) -> Result<(), HttpProviderError> {
        Err(HttpProviderError::Unavailable)
    }

    pub fn send(
        &self,
        _method: &str,
        _url: &str,
        _headers: &[(String, String)],
        _request_body: Option<&[u8]>,
        _timeout: Option<Duration>,
        _maximum_header_bytes: usize,
        _maximum_body_bytes: usize,
    ) -> Result<HostHttpResponse, HttpProviderError> {
        Err(HttpProviderError::Unavailable)
    }

    pub fn start(
        &self,
        _method: &str,
        _url: &str,
        _headers: &[(String, String)],
        _request_body: Option<&[u8]>,
        _timeout: Option<Duration>,
        _maximum_header_bytes: usize,
        _maximum_body_bytes: usize,
    ) -> Result<HostHttpHandle, HttpProviderError> {
        Err(HttpProviderError::Unavailable)
    }

    pub fn wait(
        &self,
        _handle: HostHttpHandle,
        _timeout: Option<Duration>,
    ) -> Result<HostHttpResponse, HttpProviderError> {
        Err(HttpProviderError::Unavailable)
    }

    pub fn cancel(&self, _handle: HostHttpHandle) -> Result<bool, HttpProviderError> {
        Err(HttpProviderError::Unavailable)
    }
}
