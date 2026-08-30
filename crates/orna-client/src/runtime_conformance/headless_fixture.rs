use super::*;
use std::{error::Error, fmt};

/// Stable status-level classification exposed by the test-only headless
/// fixture. The ABI status remains the machine-readable value; this kind
/// keeps callers from having to infer the outcome from diagnostic text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadlessFixtureErrorKind {
    Validation,
    Unsupported,
    Lifecycle,
    Cancellation,
    Internal,
    StaleRevision,
}

/// A structured error returned by the test-only headless fixture helpers.
///
/// Display retains the existing stable diagnostic text so existing
/// diagnostics remain useful, while callers can inspect status_code,
/// kind, and classification without parsing that text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadlessFixtureError {
    status: StatusCode,
    kind: HeadlessFixtureErrorKind,
    message: String,
}

impl HeadlessFixtureError {
    fn new(status: StatusCode) -> Self {
        let kind = match status {
            StatusCode::InvalidArgument => HeadlessFixtureErrorKind::Validation,
            StatusCode::Unsupported => HeadlessFixtureErrorKind::Unsupported,
            StatusCode::NotFound | StatusCode::Busy | StatusCode::Failed => {
                HeadlessFixtureErrorKind::Lifecycle
            }
            StatusCode::Cancelled => HeadlessFixtureErrorKind::Cancellation,
            StatusCode::Internal => HeadlessFixtureErrorKind::Internal,
            StatusCode::StaleRevision => HeadlessFixtureErrorKind::StaleRevision,
            _ => HeadlessFixtureErrorKind::Internal,
        };
        Self {
            status,
            kind,
            message: String::from_utf8_lossy(status_message(status)).into_owned(),
        }
    }

    /// Returns the stable ABI status code.
    pub const fn status_code(&self) -> StatusCode {
        self.status
    }

    /// Alias for status_code used by status-oriented fixture callers.
    pub const fn code(&self) -> StatusCode {
        self.status
    }

    /// Returns the stable fixture error classification.
    pub const fn kind(&self) -> HeadlessFixtureErrorKind {
        self.kind
    }

    /// Returns the accepted conformance-level error classification.
    pub const fn classification(&self) -> HeadlessFixtureErrorKind {
        self.kind
    }

    /// Returns the stable diagnostic text retained for human diagnostics.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HeadlessFixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for HeadlessFixtureError {}

pub(super) struct HeadlessFixtureState {
    pub(super) surface: Option<SurfaceHandle>,
    node: Option<NodeHandle>,
    revision: u64,
    terminal: bool,
}

pub struct HeadlessFixtureSession {
    pub(super) fixture: FixtureSession,
    pub(super) state: Mutex<HeadlessFixtureState>,
}

impl HeadlessFixtureSession {
    pub fn new() -> Self {
        Self {
            fixture: FixtureSession::new(),
            state: Mutex::new(HeadlessFixtureState {
                surface: None,
                node: None,
                revision: 0,
                terminal: false,
            }),
        }
    }

    pub fn create_surface(&self) -> Result<u64, HeadlessFixtureError> {
        let surface = self
            .fixture
            .create_surface_result(b"Headless fixture")
            .map_err(status_error)?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.surface = Some(surface);
        state.node = None;
        state.revision = 0;
        Ok(surface)
    }

    pub fn apply_ui_payload(&self, payload: &[u8]) -> Result<Vec<u8>, HeadlessFixtureError> {
        if !valid_canonical_frame(payload) {
            return Err(status_error(StatusCode::InvalidArgument));
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(surface) = state.surface else {
            return Err(status_error(StatusCode::NotFound));
        };
        let revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| status_error(StatusCode::InvalidArgument))?;
        let had_node = state.node.is_some();
        let node = state.node.unwrap_or_else(next_unreserved_alias_handle);
        let mut operations = [
            mount(node, 0, view(b"root")),
            set_property(node, view(b"payload")),
        ];
        operations[1].as_.set_property.value = ValueRef {
            handle: 0,
            type_name: view(b"std.ui.UI"),
            canonical_encoding: BytesView {
                data: if payload.is_empty() {
                    ptr::null()
                } else {
                    payload.as_ptr()
                },
                len: payload.len(),
            },
        };
        let first_operation = if had_node { 1 } else { 0 };
        let batch = batch(revision, &operations[first_operation..]);
        let result = self.fixture.apply(surface, &batch);
        if result != StatusCode::Ok {
            return Err(status_error(result));
        }
        state.node = Some(node);
        state.revision = revision;
        self.capture(surface)
    }
    pub fn capture(&self, surface: u64) -> Result<Vec<u8>, HeadlessFixtureError> {
        self.fixture.capture_result(surface).map_err(status_error)
    }

    pub fn destroy_surface(&self, surface: u64) -> Result<(), HeadlessFixtureError> {
        let result = self.fixture.destroy_surface(surface);
        if result != StatusCode::Ok {
            return Err(status_error(result));
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.surface == Some(surface) {
            state.surface = None;
            state.node = None;
            state.revision = 0;
        }
        Ok(())
    }

    pub fn shutdown(&self) -> Result<(), HeadlessFixtureError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.terminal {
            return Err(status_error(StatusCode::Failed));
        }
        let result = self.fixture.shutdown();
        if result == StatusCode::Ok {
            state.terminal = true;
            Ok(())
        } else {
            Err(status_error(result))
        }
    }

    pub(super) fn start_model_request(&self) -> Result<(u64, u64), HeadlessFixtureError> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let Some(surface) = state.surface else {
            return Err(status_error(StatusCode::NotFound));
        };
        self.fixture
            .start_model_request_result(surface)
            .map_err(status_error)
    }

    pub(super) fn complete_model_request(&self, request: u64) -> Result<(), HeadlessFixtureError> {
        let result = self.fixture.apply_model_rows(request);
        if result == StatusCode::Ok {
            Ok(())
        } else {
            Err(status_error(result))
        }
    }

    pub(super) fn cancel_model_request(&self, request: u64) -> Result<(), HeadlessFixtureError> {
        let result = self.fixture.cancel_request(request);
        if result == StatusCode::Ok {
            Ok(())
        } else {
            Err(status_error(result))
        }
    }

    pub fn is_terminal(&self) -> bool {
        let guard = global().lock().unwrap_or_else(|error| error.into_inner());
        guard
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.terminal)
    }

    pub fn last_callback_is_terminal(&self) -> bool {
        self.fixture
            .callback_log()
            .sequence
            .last()
            .is_some_and(|record| record.terminal)
    }
}

fn status_error(code: StatusCode) -> HeadlessFixtureError {
    HeadlessFixtureError::new(code)
}
