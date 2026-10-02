//! Bounded, origin-allowlisted native HTTP calls collected in the sys ABI.

use std::{collections::BTreeSet, io::Read, time::Duration};

use orna_sys_macros::sys_host_operation;
use reqwest::{
    blocking::Client,
    header::{HeaderName, HeaderValue},
    redirect::Policy,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpProviderError {
    Denied,
    InvalidRequest,
    HeaderLimit,
    BodyLimit,
    TimedOut,
    Unavailable,
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
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Synchronous HTTP provider restricted to explicit URL origins and resource
/// caps. Redirects and ambient proxies are disabled; TLS validation is left
/// enabled by reqwest's rustls configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpProvider {
    allowed_origins: BTreeSet<String>,
    maximum_timeout: Duration,
    maximum_header_bytes: usize,
    maximum_body_bytes: usize,
}

impl HttpProvider {
    pub fn new(
        maximum_timeout: Duration,
        maximum_header_bytes: usize,
        maximum_body_bytes: usize,
    ) -> Result<Self, HttpProviderError> {
        if maximum_timeout.is_zero() || maximum_header_bytes == 0 {
            return Err(HttpProviderError::InvalidRequest);
        }
        Ok(Self {
            allowed_origins: BTreeSet::new(),
            maximum_timeout,
            maximum_header_bytes,
            maximum_body_bytes,
        })
    }

    /// Adds a canonical `http` or `https` origin, with no credentials, path,
    /// query, or fragment, to the host's outbound allowlist.
    pub fn allow_origin(&mut self, origin: &str) -> Result<(), HttpProviderError> {
        let parsed = reqwest::Url::parse(origin).map_err(|_| HttpProviderError::InvalidRequest)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(HttpProviderError::InvalidRequest);
        }
        self.allowed_origins
            .insert(parsed.origin().ascii_serialization());
        Ok(())
    }

    #[sys_host_operation(
        r###"{"name":"std.net.http.send","version":{"major":1,"minor":0},"signature":"fn std.net.http.send(method: Str, url: Str, headers: [(Str, Str)], request_body: Blob?, timeout: Duration?, max_header_bytes: Int, max_body_bytes: Int): (Int, [(Str, Str)], Blob)","effects":["invoke"],"preconditions":["origin is explicitly allowlisted by the host","only http and https are accepted","redirects and ambient proxy settings are disabled","timeout and response byte limits are within provider limits"],"failures":["sys.host.http.denied","sys.host.http.invalid_request","sys.host.http.header_limit","sys.host.http.body_limit","sys.host.http.timeout","sys.host.http.unavailable"],"role":"host.std.net.http@1.0","provider":"orna.sys.host.http.v1","implementation":"send"}"###
    )]
    pub fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        request_body: Option<&[u8]>,
        timeout: Option<Duration>,
        max_header_bytes: usize,
        max_body_bytes: usize,
    ) -> Result<HostHttpResponse, HttpProviderError> {
        let url = reqwest::Url::parse(url).map_err(|_| HttpProviderError::InvalidRequest)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(HttpProviderError::Denied);
        }
        if !self
            .allowed_origins
            .contains(&url.origin().ascii_serialization())
        {
            return Err(HttpProviderError::Denied);
        }
        if max_header_bytes == 0
            || max_header_bytes > self.maximum_header_bytes
            || max_body_bytes > self.maximum_body_bytes
            || request_body.is_some_and(|body| body.len() > max_body_bytes)
            || timeout.is_some_and(|value| value.is_zero() || value > self.maximum_timeout)
        {
            return Err(HttpProviderError::Denied);
        }
        let effective_timeout = timeout.unwrap_or(self.maximum_timeout);
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| HttpProviderError::InvalidRequest)?;
        let mut request_headers = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            if value.contains(['\r', '\n', '\0']) {
                return Err(HttpProviderError::InvalidRequest);
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| HttpProviderError::InvalidRequest)?;
            let value =
                HeaderValue::from_str(value).map_err(|_| HttpProviderError::InvalidRequest)?;
            request_headers.append(name, value);
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(effective_timeout)
            .build()
            .map_err(|_| HttpProviderError::Unavailable)?;
        let mut request = client.request(method, url).headers(request_headers);
        if let Some(body) = request_body {
            request = request.body(body.to_vec());
        }
        let mut response = request.send().map_err(map_http_error)?;
        let status = response.status().as_u16();
        let mut response_headers = Vec::new();
        let mut header_bytes = 0usize;
        for (name, value) in response.headers() {
            let value = value
                .to_str()
                .map_err(|_| HttpProviderError::InvalidRequest)?
                .to_owned();
            header_bytes = header_bytes
                .checked_add(name.as_str().len() + value.len() + 4)
                .ok_or(HttpProviderError::HeaderLimit)?;
            if header_bytes > max_header_bytes {
                return Err(HttpProviderError::HeaderLimit);
            }
            response_headers.push((name.as_str().to_owned(), value));
        }
        let mut body = Vec::new();
        response
            .by_ref()
            .take(max_body_bytes.saturating_add(1) as u64)
            .read_to_end(&mut body)
            .map_err(map_read_error)?;
        if body.len() > max_body_bytes {
            return Err(HttpProviderError::BodyLimit);
        }
        Ok(HostHttpResponse {
            status,
            headers: response_headers,
            body,
        })
    }
}

fn map_http_error(error: reqwest::Error) -> HttpProviderError {
    if error.is_timeout() {
        HttpProviderError::TimedOut
    } else {
        HttpProviderError::Unavailable
    }
}

fn map_read_error(error: std::io::Error) -> HttpProviderError {
    if error.kind() == std::io::ErrorKind::TimedOut {
        HttpProviderError::TimedOut
    } else {
        HttpProviderError::Unavailable
    }
}
