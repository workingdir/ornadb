//! Bounded, origin-allowlisted native HTTP calls collected in the sys ABI.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

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

/// Synchronous HTTP provider restricted to explicit URL origins and resource
/// caps. Redirects and ambient proxies are disabled; TLS validation is left
/// enabled by reqwest's rustls configuration.
#[derive(Clone, Debug)]
pub struct HttpProvider {
    allowed_origins: BTreeSet<String>,
    maximum_timeout: Duration,
    maximum_header_bytes: usize,
    maximum_body_bytes: usize,
    requests: Arc<Mutex<BTreeMap<uuid::Uuid, Arc<PendingHttpRequest>>>>,
}

#[derive(Debug)]
struct PendingHttpRequest {
    response: Mutex<Option<Result<HostHttpResponse, HttpProviderError>>>,
    ready: Condvar,
    cancelled: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl PartialEq for HttpProvider {
    fn eq(&self, other: &Self) -> bool {
        self.allowed_origins == other.allowed_origins
            && self.maximum_timeout == other.maximum_timeout
            && self.maximum_header_bytes == other.maximum_header_bytes
            && self.maximum_body_bytes == other.maximum_body_bytes
    }
}

impl Eq for HttpProvider {}

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
            requests: Arc::new(Mutex::new(BTreeMap::new())),
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

    #[sys_host_operation(
        r###"{"name":"std.net.http.start","version":{"major":1,"minor":0},"signature":"fn std.net.http.start(method: Str, url: Str, headers: [(Str, Str)], request_body: Blob?, timeout: Duration?, max_header_bytes: Int, max_body_bytes: Int): Uuid","effects":["invoke"],"preconditions":["the host HTTP provider can allocate a request handle"],"failures":["sys.host.http.unavailable"],"role":"host.std.net.http@1.0","provider":"orna.sys.host.http.v1","implementation":"start"}"###
    )]
    pub fn start(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        request_body: Option<&[u8]>,
        timeout: Option<Duration>,
        max_header_bytes: usize,
        max_body_bytes: usize,
    ) -> Result<HostHttpHandle, HttpProviderError> {
        let pending = Arc::new(PendingHttpRequest {
            response: Mutex::new(None),
            ready: Condvar::new(),
            cancelled: AtomicBool::new(false),
            worker: Mutex::new(None),
        });
        let handle = uuid::Uuid::new_v4();
        self.requests
            .lock()
            .map_err(|_| HttpProviderError::Unavailable)?
            .insert(handle, pending.clone());

        let provider = self.clone();
        let owned_method = method.to_owned();
        let owned_url = url.to_owned();
        let owned_headers = headers.to_vec();
        let owned_body = request_body.map(<[u8]>::to_vec);
        let worker_pending = pending.clone();
        let worker = match thread::Builder::new()
            .name("orna-sys-http".to_owned())
            .spawn(move || {
                let result = provider.send(
                    &owned_method,
                    &owned_url,
                    &owned_headers,
                    owned_body.as_deref(),
                    timeout,
                    max_header_bytes,
                    max_body_bytes,
                );
                let result = if worker_pending.cancelled.load(Ordering::Acquire) {
                    Err(HttpProviderError::Cancelled)
                } else {
                    result
                };
                if let Ok(mut slot) = worker_pending.response.lock()
                    && slot.is_none()
                {
                    *slot = Some(result);
                    worker_pending.ready.notify_all();
                }
            }) {
            Ok(worker) => worker,
            Err(_) => {
                self.requests
                    .lock()
                    .map_err(|_| HttpProviderError::Unavailable)?
                    .remove(&handle);
                return Err(HttpProviderError::Unavailable);
            }
        };
        *pending
            .worker
            .lock()
            .map_err(|_| HttpProviderError::Unavailable)? = Some(worker);
        Ok(*handle.as_bytes())
    }

    #[sys_host_operation(
        r###"{"name":"std.net.http.wait","version":{"major":1,"minor":0},"signature":"fn std.net.http.wait(handle: Uuid, timeout: Duration?): (Int, [(Str, Str)], Blob)","effects":["invoke"],"preconditions":["handle was created by this provider instance","response header and body limits from start are enforced","wait timeout does not cancel the request"],"failures":["sys.host.http.denied","sys.host.http.invalid_request","sys.host.http.header_limit","sys.host.http.body_limit","sys.host.http.timeout","sys.host.http.cancelled","sys.host.http.unavailable"],"role":"host.std.net.http@1.0","provider":"orna.sys.host.http.v1","implementation":"wait"}"###
    )]
    pub fn wait(
        &self,
        handle: HostHttpHandle,
        timeout: Option<Duration>,
    ) -> Result<HostHttpResponse, HttpProviderError> {
        let pending = self
            .requests
            .lock()
            .map_err(|_| HttpProviderError::Unavailable)?
            .get(&uuid::Uuid::from_bytes(handle))
            .cloned()
            .ok_or(HttpProviderError::Denied)?;
        let started = Instant::now();
        let mut response = pending
            .response
            .lock()
            .map_err(|_| HttpProviderError::Unavailable)?;
        loop {
            if let Some(ready) = response.take() {
                drop(response);
                self.requests
                    .lock()
                    .map_err(|_| HttpProviderError::Unavailable)?
                    .remove(&uuid::Uuid::from_bytes(handle));
                return ready;
            }
            if let Some(timeout) = timeout {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(HttpProviderError::TimedOut);
                }
                let (next, result) = pending
                    .ready
                    .wait_timeout(response, remaining)
                    .map_err(|_| HttpProviderError::Unavailable)?;
                response = next;
                if result.timed_out() && response.is_none() {
                    return Err(HttpProviderError::TimedOut);
                }
            } else {
                response = pending
                    .ready
                    .wait(response)
                    .map_err(|_| HttpProviderError::Unavailable)?;
            }
        }
    }

    #[sys_host_operation(
        r###"{"name":"std.net.http.cancel","version":{"major":1,"minor":0},"signature":"fn std.net.http.cancel(handle: Uuid): Bool","effects":["invoke"],"preconditions":["handle was created by this provider instance"],"failures":["sys.host.http.denied","sys.host.http.unavailable"],"role":"host.std.net.http@1.0","provider":"orna.sys.host.http.v1","implementation":"cancel"}"###
    )]
    pub fn cancel(&self, handle: HostHttpHandle) -> Result<bool, HttpProviderError> {
        let handle = uuid::Uuid::from_bytes(handle);
        let pending = self
            .requests
            .lock()
            .map_err(|_| HttpProviderError::Unavailable)?
            .get(&handle)
            .cloned()
            .ok_or(HttpProviderError::Denied)?;
        let mut response = pending
            .response
            .lock()
            .map_err(|_| HttpProviderError::Unavailable)?;
        if response.is_some() || pending.cancelled.swap(true, Ordering::AcqRel) {
            return Ok(false);
        }
        *response = Some(Err(HttpProviderError::Cancelled));
        pending.ready.notify_all();
        drop(response);
        if let Some(worker) = pending
            .worker
            .lock()
            .map_err(|_| HttpProviderError::Unavailable)?
            .take()
        {
            let _ = worker.join();
        }
        self.requests
            .lock()
            .map_err(|_| HttpProviderError::Unavailable)?
            .remove(&handle);
        Ok(true)
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
