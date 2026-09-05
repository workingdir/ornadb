//! Bounded in-memory serving state for `orna.present.v1`.
//!
//! This crate deliberately has no transport, clock, persistence, identity
//! provider, or host-I/O implementation.  An integration authenticates an
//! opaque [`Credential`], calls [`Serving::admit`], and reports destructive
//! identity operations through the explicit fail-closed methods.  Protocol
//! envelope validation remains owned by `orna-protocol-v1`; this crate checks
//! the small serving-state subset that it consumes.

use std::collections::{BTreeMap, VecDeque};

use orna_protocol_v1::{Envelope, Message, TargetKind};

pub type Id = [u8; 16];

/// All limits are explicit so a host cannot accidentally create unbounded
/// retained session, patch, page, watch, or action state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_sessions: usize,
    pub max_replay: usize,
    pub max_page_entries: usize,
    pub max_watches_per_session: usize,
    pub max_actions_per_watch: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_sessions: 1_024,
            max_replay: 256,
            max_page_entries: 10_000,
            max_watches_per_session: 64,
            max_actions_per_watch: 1_000_000,
        }
    }
}

impl Limits {
    pub fn validate(self) -> Result<Self> {
        if self.max_sessions == 0
            || self.max_replay == 0
            || self.max_page_entries == 0
            || self.max_watches_per_session == 0
            || self.max_actions_per_watch == 0
        {
            return Err(Error::InvalidLimits);
        }
        Ok(self)
    }
}

/// Opaque, comparable authentication material.  It intentionally has no
/// accessor, `Display`, or derived `Debug`, so diagnostics cannot leak it.
#[derive(Clone, Eq, PartialEq)]
pub struct Credential([u8; 32]);

impl Credential {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl core::fmt::Debug for Credential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Credential(REDACTED)")
    }
}

/// Public, non-secret origin handle retained only to make deletion failures
/// attributable without serialising an origin URL or credential.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Origin(pub Id);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedPin {
    pub revision: u64,
    pub fingerprint: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidLimits,
    MalformedAdmission,
    SessionExists,
    SessionUnknown,
    SessionClosed,
    CredentialRejected,
    CredentialDeletionFailed,
    OriginDeletionFailed,
    SessionLimit,
    WatchLimit,
    WatchUnknown,
    ReplayRequired,
    RevisionMismatch,
    ActionSequenceMismatch,
    ActionLimit,
    RequestUnknown,
    RequestTerminal,
    PatchMalformed,
    PageLimit,
}

impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "serving.invalid_limits",
            Self::MalformedAdmission => "serving.malformed_admission",
            Self::SessionExists => "serving.session_exists",
            Self::SessionUnknown => "serving.session_unknown",
            Self::SessionClosed => "serving.session_closed",
            Self::CredentialRejected => "serving.credential_rejected",
            Self::CredentialDeletionFailed => "serving.credential_deletion_failed",
            Self::OriginDeletionFailed => "serving.origin_deletion_failed",
            Self::SessionLimit => "serving.session_limit",
            Self::WatchLimit => "serving.watch_limit",
            Self::WatchUnknown => "serving.watch_unknown",
            Self::ReplayRequired => "serving.replay_required",
            Self::RevisionMismatch => "serving.revision_mismatch",
            Self::ActionSequenceMismatch => "serving.action_sequence_mismatch",
            Self::ActionLimit => "serving.action_limit",
            Self::RequestUnknown => "serving.request_unknown",
            Self::RequestTerminal => "serving.request_terminal",
            Self::PatchMalformed => "serving.patch_malformed",
            Self::PageLimit => "serving.page_limit",
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Patch {
    Set { key: String, value: String },
    Remove { key: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageRevision {
    pub revision: u64,
    pub page: BTreeMap<String, String>,
    pub pin: RetainedPin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestState {
    Reserved,
    Running,
    Cancelled,
    Completed,
}

impl RequestState {
    const fn terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Completed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Watch {
    revision: u64,
    next_action: u64,
}

#[derive(Clone, Debug)]
struct Session {
    credential: Credential,
    origin: Origin,
    connected: bool,
    closed: bool,
    page: BTreeMap<String, String>,
    retained: VecDeque<PageRevision>,
    watches: BTreeMap<Id, Watch>,
    requests: BTreeMap<Id, RequestState>,
}

/// Pure session state.  The caller owns scheduling, authentication lookup,
/// message encoding, and all external deletion attempts.
#[derive(Debug)]
pub struct Serving {
    limits: Limits,
    sessions: BTreeMap<Id, Session>,
}

impl Serving {
    pub fn new(limits: Limits) -> Result<Self> {
        Ok(Self {
            limits: limits.validate()?,
            sessions: BTreeMap::new(),
        })
    }

    /// Admit only a structurally valid protocol `Subscribe` envelope and bind
    /// its request identity to a newly authenticated session.
    pub fn admit(
        &mut self,
        session_id: Id,
        credential: Credential,
        origin: Origin,
        envelope: &Envelope,
    ) -> Result<()> {
        if !matches!(envelope.message, Message::Subscribe { .. })
            || envelope.request.is_none()
            || envelope.watch.is_some()
        {
            return Err(Error::MalformedAdmission);
        }
        if self.sessions.contains_key(&session_id) {
            return Err(Error::SessionExists);
        }
        if self.sessions.len() == self.limits.max_sessions {
            return Err(Error::SessionLimit);
        }
        self.sessions.insert(
            session_id,
            Session {
                credential,
                origin,
                connected: true,
                closed: false,
                page: BTreeMap::new(),
                retained: VecDeque::new(),
                watches: BTreeMap::new(),
                requests: BTreeMap::new(),
            },
        );
        Ok(())
    }

    pub fn disconnect(&mut self, session_id: Id) -> Result<()> {
        self.session_mut(session_id)?.connected = false;
        Ok(())
    }

    /// Reconnect has no implicit re-admission: the exact credential remains
    /// required and a deletion failure leaves this session closed.
    pub fn reconnect(&mut self, session_id: Id, credential: &Credential) -> Result<()> {
        let session = self.session_mut(session_id)?;
        if &session.credential != credential {
            return Err(Error::CredentialRejected);
        }
        session.connected = true;
        Ok(())
    }

    /// Replace a session credential only after checking the credential that
    /// currently protects the serving state.  A host uses this while rotating
    /// one shared session credential across the security and serving layers.
    pub fn rotate_credential(
        &mut self,
        session_id: Id,
        current: &Credential,
        replacement: Credential,
    ) -> Result<()> {
        let session = self.session_mut(session_id)?;
        if &session.credential != current {
            return Err(Error::CredentialRejected);
        }
        session.credential = replacement;
        Ok(())
    }

    pub fn retain_pin(&self, session_id: Id) -> Result<Option<RetainedPin>> {
        Ok(self
            .session(session_id)?
            .retained
            .back()
            .map(|entry| entry.pin))
    }

    /// Returns every retained revision strictly after `after`; gaps fail
    /// closed, requiring the transport layer to request a fresh snapshot.
    pub fn resync(&mut self, session_id: Id, after: u64) -> Result<Vec<PageRevision>> {
        let session = self.session_mut(session_id)?;
        if !session.connected {
            return Err(Error::SessionClosed);
        }
        let first = session.retained.front().map(|entry| entry.revision);
        if first.is_some_and(|revision| after.saturating_add(1) < revision) {
            return Err(Error::ReplayRequired);
        }
        Ok(session
            .retained
            .iter()
            .filter(|entry| entry.revision > after)
            .cloned()
            .collect())
    }

    /// Validate the full patch against a copy, then publish exactly one new
    /// page revision.  Any error leaves both visible page and replay state
    /// unchanged.
    pub fn apply_patch(
        &mut self,
        session_id: Id,
        base_revision: u64,
        new_revision: u64,
        patches: &[Patch],
        pin: RetainedPin,
    ) -> Result<()> {
        let limit = self.limits.max_page_entries;
        let max_replay = self.limits.max_replay;
        let session = self.session_mut(session_id)?;
        let current = session.retained.back().map_or(0, |entry| entry.revision);
        if base_revision != current || new_revision <= base_revision {
            return Err(Error::RevisionMismatch);
        }
        let mut candidate = session.page.clone();
        let mut touched = BTreeMap::<&str, ()>::new();
        for patch in patches {
            let key = match patch {
                Patch::Set { key, .. } | Patch::Remove { key } => key,
            };
            if key.is_empty() || touched.insert(key, ()).is_some() {
                return Err(Error::PatchMalformed);
            }
            match patch {
                Patch::Set { key, value } => {
                    candidate.insert(key.clone(), value.clone());
                }
                Patch::Remove { key } => {
                    candidate.remove(key);
                }
            }
        }
        if candidate.len() > limit {
            return Err(Error::PageLimit);
        }
        let revision = PageRevision {
            revision: new_revision,
            page: candidate.clone(),
            pin,
        };
        session.page = candidate;
        session.retained.push_back(revision);
        for watch in session.watches.values_mut() {
            watch.revision = new_revision;
        }
        if session.retained.len() > max_replay {
            let _ = session.retained.pop_front();
        }
        Ok(())
    }

    pub fn open_watch(&mut self, session_id: Id, watch_id: Id, revision: u64) -> Result<()> {
        let max_watches = self.limits.max_watches_per_session;
        let session = self.session_mut(session_id)?;
        if session.watches.contains_key(&watch_id) || session.watches.len() == max_watches {
            return Err(Error::WatchLimit);
        }
        let current = session.retained.back().map_or(0, |entry| entry.revision);
        if revision != current {
            return Err(Error::RevisionMismatch);
        }
        session.watches.insert(
            watch_id,
            Watch {
                revision,
                next_action: 0,
            },
        );
        Ok(())
    }

    /// Closing a watch is idempotent so repeated unsubscribe or cancellation
    /// delivery cannot create a new serving-side effect.
    pub fn close_watch(&mut self, session_id: Id, watch_id: Id) -> Result<()> {
        self.session_mut(session_id)?.watches.remove(&watch_id);
        Ok(())
    }

    pub fn action(
        &mut self,
        session_id: Id,
        watch_id: Id,
        revision: u64,
        sequence: u64,
    ) -> Result<()> {
        let action_limit = self.limits.max_actions_per_watch;
        let session = self.session_mut(session_id)?;
        let watch = session
            .watches
            .get_mut(&watch_id)
            .ok_or(Error::WatchUnknown)?;
        if revision != watch.revision {
            return Err(Error::RevisionMismatch);
        }
        if sequence != watch.next_action {
            return Err(Error::ActionSequenceMismatch);
        }
        if sequence >= action_limit {
            return Err(Error::ActionLimit);
        }
        watch.next_action += 1;
        Ok(())
    }

    pub fn reserve_request(&mut self, session_id: Id, request_id: Id) -> Result<()> {
        let session = self.session_mut(session_id)?;
        if session.requests.contains_key(&request_id) {
            return Err(Error::RequestTerminal);
        }
        session.requests.insert(request_id, RequestState::Reserved);
        Ok(())
    }

    pub fn start_request(&mut self, session_id: Id, request_id: Id) -> Result<()> {
        let state = self
            .session_mut(session_id)?
            .requests
            .get_mut(&request_id)
            .ok_or(Error::RequestUnknown)?;
        if *state != RequestState::Reserved {
            return Err(Error::RequestTerminal);
        }
        *state = RequestState::Running;
        Ok(())
    }

    /// Completion is terminal and may conclude either a reserved request that
    /// never began execution or a running request.
    pub fn complete_request(&mut self, session_id: Id, request_id: Id) -> Result<()> {
        let state = self
            .session_mut(session_id)?
            .requests
            .get_mut(&request_id)
            .ok_or(Error::RequestUnknown)?;
        if !matches!(*state, RequestState::Reserved | RequestState::Running) {
            return Err(Error::RequestTerminal);
        }
        *state = RequestState::Completed;
        Ok(())
    }

    /// Cancellation is terminal and idempotent only at the boundary: a second
    /// cancel is rejected, preventing duplicate cancellation effects.
    pub fn cancel_request(&mut self, session_id: Id, request_id: Id) -> Result<()> {
        let state = self
            .session_mut(session_id)?
            .requests
            .get_mut(&request_id)
            .ok_or(Error::RequestUnknown)?;
        if state.terminal() {
            return Err(Error::RequestTerminal);
        }
        *state = RequestState::Cancelled;
        Ok(())
    }

    pub fn request_state(&self, session_id: Id, request_id: Id) -> Result<RequestState> {
        self.session(session_id)?
            .requests
            .get(&request_id)
            .copied()
            .ok_or(Error::RequestUnknown)
    }

    /// An integration calls this after attempting credential deletion.  A
    /// failure is terminal: no reconnect is permitted afterwards.
    pub fn credential_deleted(&mut self, session_id: Id, deleted: bool) -> Result<()> {
        let session = self.session_mut(session_id)?;
        if !deleted {
            session.closed = true;
            session.connected = false;
            return Err(Error::CredentialDeletionFailed);
        }
        session.closed = true;
        session.connected = false;
        Ok(())
    }

    /// Origin deletion follows the same fail-closed rule.  Origin identity is
    /// retained solely internally and never included in an error.
    pub fn origin_deleted(&mut self, session_id: Id, origin: Origin, deleted: bool) -> Result<()> {
        let session = self.session_mut(session_id)?;
        if session.origin != origin || !deleted {
            session.closed = true;
            session.connected = false;
            return Err(Error::OriginDeletionFailed);
        }
        session.closed = true;
        session.connected = false;
        Ok(())
    }

    /// Recognise cancellation requests in the protocol vocabulary without
    /// treating arbitrary protocol messages as state transitions.
    pub fn cancel_envelope(&mut self, session_id: Id, envelope: &Envelope) -> Result<()> {
        match &envelope.message {
            Message::Cancel {
                target_kind: TargetKind::Request,
                target,
            } if envelope.request.is_some() && envelope.watch.is_none() => {
                self.cancel_request(session_id, *target)
            }
            _ => Err(Error::MalformedAdmission),
        }
    }

    fn session(&self, session_id: Id) -> Result<&Session> {
        let session = self
            .sessions
            .get(&session_id)
            .ok_or(Error::SessionUnknown)?;
        if session.closed {
            return Err(Error::SessionClosed);
        }
        Ok(session)
    }

    fn session_mut(&mut self, session_id: Id) -> Result<&mut Session> {
        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or(Error::SessionUnknown)?;
        if session.closed {
            return Err(Error::SessionClosed);
        }
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_protocol_v1::{PresentationContext, TargetKind};

    fn id(value: u8) -> Id {
        [value; 16]
    }
    fn credential() -> Credential {
        Credential::new([7; 32])
    }
    fn pin(revision: u64) -> RetainedPin {
        RetainedPin {
            revision,
            fingerprint: [revision as u8; 32],
        }
    }
    fn subscribe(request: Id) -> Envelope {
        Envelope {
            request: Some(request),
            watch: None,
            message: Message::Subscribe {
                resource: id(9),
                presentation: PresentationContext {
                    locale: "en-GB".into(),
                    timezone: None,
                    width: None,
                    theme: "dark".into(),
                    supported_kinds: vec![],
                },
            },
            extensions: BTreeMap::new(),
        }
    }
    fn serving() -> Serving {
        Serving::new(Limits {
            max_replay: 2,
            ..Limits::default()
        })
        .unwrap()
    }
    fn admitted() -> Serving {
        let mut state = serving();
        state
            .admit(id(1), credential(), Origin(id(2)), &subscribe(id(3)))
            .unwrap();
        state
    }

    #[test]
    fn malformed_admission_and_bad_reconnect_are_rejected() {
        let mut state = serving();
        let mut malformed = subscribe(id(3));
        malformed.watch = Some(id(4));
        assert_eq!(
            state.admit(id(1), credential(), Origin(id(2)), &malformed),
            Err(Error::MalformedAdmission)
        );
        state
            .admit(id(1), credential(), Origin(id(2)), &subscribe(id(3)))
            .unwrap();
        state.disconnect(id(1)).unwrap();
        assert_eq!(
            state.reconnect(id(1), &Credential::new([8; 32])),
            Err(Error::CredentialRejected)
        );
        let replacement = Credential::new([9; 32]);
        state
            .rotate_credential(id(1), &credential(), replacement.clone())
            .unwrap();
        assert_eq!(
            state.rotate_credential(id(1), &credential(), Credential::new([10; 32])),
            Err(Error::CredentialRejected)
        );
        state.reconnect(id(1), &replacement).unwrap();
    }

    #[test]
    fn replay_is_bounded_and_gaps_require_resync() {
        let mut state = admitted();
        for revision in 1..=3 {
            state
                .apply_patch(
                    id(1),
                    revision - 1,
                    revision,
                    &[Patch::Set {
                        key: format!("k{revision}"),
                        value: "v".into(),
                    }],
                    pin(revision),
                )
                .unwrap();
        }
        assert_eq!(state.retain_pin(id(1)).unwrap(), Some(pin(3)));
        assert_eq!(state.resync(id(1), 0), Err(Error::ReplayRequired));
        assert_eq!(
            state
                .resync(id(1), 1)
                .unwrap()
                .into_iter()
                .map(|x| x.revision)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn cancellation_is_terminal_and_protocol_bound() {
        let mut state = admitted();
        state.reserve_request(id(1), id(4)).unwrap();
        state.start_request(id(1), id(4)).unwrap();
        let envelope = Envelope {
            request: Some(id(8)),
            watch: None,
            message: Message::Cancel {
                target_kind: TargetKind::Request,
                target: id(4),
            },
            extensions: BTreeMap::new(),
        };
        state.cancel_envelope(id(1), &envelope).unwrap();
        assert_eq!(
            state.request_state(id(1), id(4)).unwrap(),
            RequestState::Cancelled
        );
        assert_eq!(
            state.cancel_request(id(1), id(4)),
            Err(Error::RequestTerminal)
        );
    }

    #[test]
    fn completion_is_terminal_from_reserved_or_running() {
        let mut state = admitted();
        state.reserve_request(id(1), id(4)).unwrap();
        state.complete_request(id(1), id(4)).unwrap();
        assert_eq!(
            state.request_state(id(1), id(4)).unwrap(),
            RequestState::Completed
        );
        assert_eq!(
            state.complete_request(id(1), id(4)),
            Err(Error::RequestTerminal)
        );

        state.reserve_request(id(1), id(5)).unwrap();
        state.start_request(id(1), id(5)).unwrap();
        state.complete_request(id(1), id(5)).unwrap();
        assert_eq!(
            state.request_state(id(1), id(5)).unwrap(),
            RequestState::Completed
        );
        assert_eq!(
            state.complete_request(id(1), id(6)),
            Err(Error::RequestUnknown)
        );
    }

    #[test]
    fn patch_failures_never_publish_a_partial_page() {
        let mut state = admitted();
        state
            .apply_patch(
                id(1),
                0,
                1,
                &[Patch::Set {
                    key: "a".into(),
                    value: "one".into(),
                }],
                pin(1),
            )
            .unwrap();
        let invalid = [
            Patch::Set {
                key: "b".into(),
                value: "two".into(),
            },
            Patch::Set {
                key: "b".into(),
                value: "three".into(),
            },
        ];
        assert_eq!(
            state.apply_patch(id(1), 1, 2, &invalid, pin(2)),
            Err(Error::PatchMalformed)
        );
        let replay = state.resync(id(1), 0).unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].page.get("b"), None);
        assert_eq!(replay[0].page.get("a"), Some(&"one".to_owned()));
    }

    #[test]
    fn watch_actions_are_ordered_and_bounded() {
        let mut state = admitted();
        state.apply_patch(id(1), 0, 1, &[], pin(1)).unwrap();
        state.open_watch(id(1), id(5), 1).unwrap();
        state.action(id(1), id(5), 1, 0).unwrap();
        assert_eq!(
            state.action(id(1), id(5), 1, 2),
            Err(Error::ActionSequenceMismatch)
        );
        assert_eq!(
            state.action(id(1), id(5), 2, 1),
            Err(Error::RevisionMismatch)
        );
    }

    #[test]
    fn closing_a_watch_is_idempotent() {
        let mut state = admitted();
        state.apply_patch(id(1), 0, 1, &[], pin(1)).unwrap();
        state.open_watch(id(1), id(5), 1).unwrap();
        state.close_watch(id(1), id(5)).unwrap();
        state.close_watch(id(1), id(5)).unwrap();
        assert_eq!(state.action(id(1), id(5), 1, 0), Err(Error::WatchUnknown));
    }

    #[test]
    fn destructive_deletion_failure_is_redacted_and_closed() {
        let mut state = admitted();
        assert_eq!(
            state.credential_deleted(id(1), false),
            Err(Error::CredentialDeletionFailed)
        );
        assert_eq!(
            state.reconnect(id(1), &credential()),
            Err(Error::SessionClosed)
        );
        assert_eq!(format!("{:?}", credential()), "Credential(REDACTED)");
    }

    #[test]
    fn origin_deletion_failure_is_terminal() {
        let mut state = admitted();
        assert_eq!(
            state.origin_deleted(id(1), Origin(id(2)), false),
            Err(Error::OriginDeletionFailed)
        );
        assert_eq!(state.disconnect(id(1),), Err(Error::SessionClosed));
    }
}
