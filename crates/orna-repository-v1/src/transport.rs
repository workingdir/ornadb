//! Bounded, repository-local Git fetch transport.
//!
//! This module deliberately stops at fetch.  It downloads the objects named
//! by ordinary requested refs and continuity-approved `refs/orna/*` refs, then
//! installs only compare-and-set-safe local refs.  Its exact-object hydration
//! boundary materializes one already-proven promised object without installing
//! refs. Neither operation changes HEAD, the index, the worktree, or Orna's
//! runtime files, and this module does not claim to implement clone, pull, or
//! push.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use super::{
    NativeObjectId, OrnaInternalRef, RemoteContinuity, Repository, RepositoryError,
    RequiredInternalRef, scrub_git_routing_environment, trim_output, valid_branch_name,
    valid_remote_name,
};

const MAX_FETCH_REFS: usize = 4096;

/// The owner-scoped private edit and capture pins an ordinary push keeps local.
/// These refs record one owner's in-flight protected content, so publishing
/// them would expose private pins (`ORNA-GIT-008`).
const PRIVATE_PIN_REF_PREFIX: &str = "refs/orna/pins/";

/// One ordinary branch or tag requested from a configured remote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestedRef {
    Branch(String),
    Tag(String),
}

impl RequestedRef {
    /// Creates a checked branch request without invoking Git.
    pub fn branch(name: impl Into<String>) -> Result<Self, FetchError> {
        let name = name.into();
        if !valid_fetch_ref_name(&name) || !valid_branch_name(&name) {
            return Err(FetchError::InvalidRef);
        }
        Ok(Self::Branch(name))
    }

    /// Creates a checked tag request without invoking Git.
    pub fn tag(name: impl Into<String>) -> Result<Self, FetchError> {
        let name = name.into();
        if !valid_fetch_ref_name(&name) || !valid_branch_name(&name) {
            return Err(FetchError::InvalidRef);
        }
        Ok(Self::Tag(name))
    }

    fn source(&self) -> String {
        match self {
            Self::Branch(name) => format!("refs/heads/{name}"),
            Self::Tag(name) => format!("refs/tags/{name}"),
        }
    }

    fn destination(&self, remote: &str) -> String {
        match self {
            Self::Branch(name) => format!("refs/remotes/{remote}/{name}"),
            Self::Tag(name) => format!("refs/tags/{name}"),
        }
    }
}

/// A fetch request containing ordinary refs and exact internal continuity
/// witnesses.  The constructor performs all name validation before Git is
/// contacted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchRequest {
    remote: String,
    ordinary: Vec<RequestedRef>,
    continuity: Vec<RequiredInternalRef>,
}

impl FetchRequest {
    /// Creates a bounded fetch request for one configured remote.
    pub fn new(
        remote: impl Into<String>,
        ordinary: impl IntoIterator<Item = RequestedRef>,
        continuity: impl IntoIterator<Item = RequiredInternalRef>,
    ) -> Result<Self, FetchError> {
        let remote = remote.into();
        let ordinary = ordinary.into_iter().collect::<Vec<_>>();
        let continuity = continuity.into_iter().collect::<Vec<_>>();
        if !valid_remote_name(&remote) {
            return Err(FetchError::InvalidRemote);
        }
        if ordinary.len().saturating_add(continuity.len()) == 0 {
            return Err(FetchError::EmptyRequest);
        }
        if ordinary.len().saturating_add(continuity.len()) > MAX_FETCH_REFS {
            return Err(FetchError::TooManyRefs);
        }

        let mut destinations = BTreeSet::new();
        for requested in &ordinary {
            let source = requested.source();
            let destination = requested.destination(&remote);
            if !valid_fetch_full_ref(&source)
                || !valid_fetch_full_ref(&destination)
                || !destinations.insert(destination)
            {
                return Err(FetchError::InvalidRef);
            }
        }
        for required in &continuity {
            let reference = required.reference().as_str();
            if !valid_fetch_full_ref(reference) || !destinations.insert(reference.to_owned()) {
                return Err(FetchError::InvalidContinuity);
            }
        }
        Ok(Self {
            remote,
            ordinary,
            continuity,
        })
    }

    pub fn remote(&self) -> &str {
        &self.remote
    }

    pub fn ordinary(&self) -> &[RequestedRef] {
        &self.ordinary
    }

    pub fn continuity(&self) -> &[RequiredInternalRef] {
        &self.continuity
    }
}

impl Repository {
    /// Fetches requested refs using the repository's current `refs/orna/*`
    /// object IDs as the exact continuity witnesses.
    ///
    /// Witness collection and fetch are protected by one repository
    /// coordination lock so a local allocator/checkpoint ref cannot advance
    /// between observing its expected ID and checking the remote. A
    /// repository with no local Orna refs makes no continuity claim; it does
    /// not synthesize an allocator value or infer one from the remote.
    pub fn fetch_with_local_continuity(
        &self,
        remote: impl Into<String>,
        ordinary: impl IntoIterator<Item = RequestedRef>,
    ) -> Result<FetchReport, FetchError> {
        let remote = remote.into();
        if !valid_remote_name(&remote) {
            return Err(FetchError::InvalidRemote);
        }
        let ordinary = ordinary.into_iter().collect::<Vec<_>>();
        if ordinary.is_empty() {
            return Err(FetchError::EmptyRequest);
        }
        if ordinary.len() > MAX_FETCH_REFS {
            return Err(FetchError::TooManyRefs);
        }

        let _lock = self.acquire_coordination_lock()?;
        let continuity = self.local_continuity_witnesses_locked()?;
        let request = FetchRequest::new(remote, ordinary, continuity)?;
        validate_request(&request)?;
        self.fetch_locked(&request)
    }

    fn local_continuity_witnesses_locked(
        &self,
    ) -> Result<Vec<RequiredInternalRef>, FetchError> {
        let mut command = self.observer_command();
        command.args([
            "for-each-ref",
            "--sort=refname",
            "--count=4097",
            "--format=%(objectname)%09%(refname)",
            "refs/orna/",
        ]);
        let output = command
            .output()
            .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
        if !output.status.success() {
            return Err(FetchError::Repository(
                RepositoryError::GitOperationFailed,
            ));
        }
        let output = String::from_utf8(output.stdout).map_err(|_| FetchError::InvalidContinuity)?;
        if output.is_empty() {
            return Ok(Vec::new());
        }
        let records = output
            .strip_suffix('\n')
            .ok_or(FetchError::InvalidContinuity)?;
        let mut witnesses = Vec::new();
        for record in records.split('\n') {
            if witnesses.len() == MAX_FETCH_REFS {
                return Err(FetchError::TooManyRefs);
            }
            let (object_id, reference) = record
                .split_once('\t')
                .ok_or(FetchError::InvalidContinuity)?;
            let reference = OrnaInternalRef::new(reference.to_owned())
                .map_err(|_| FetchError::InvalidContinuity)?;
            let object_id = NativeObjectId::new(object_id.to_owned())
                .map_err(|_| FetchError::InvalidContinuity)?;
            witnesses.push(RequiredInternalRef::new(reference, object_id));
        }
        Ok(witnesses)
    }

    /// The local `refs/orna/*` references an ordinary push publishes, paired
    /// with the object ID each currently names.
    ///
    /// Allocator and consumer checkpoint refs are the continuity refs an
    /// ordinary push synchronizes, while every private edit and capture pin
    /// under [`PRIVATE_PIN_REF_PREFIX`] MUST NOT be exposed by an ordinary push
    /// (`ORNA-GIT-008`). A repository with no local Orna refs publishes no
    /// continuity ref rather than synthesizing one.
    pub fn ordinary_push_refs(&self) -> Result<Vec<(OrnaInternalRef, String)>, FetchError> {
        let mut command = self.observer_command();
        command.args([
            "for-each-ref",
            "--sort=refname",
            "--count=4097",
            "--format=%(objectname)%09%(refname)",
            "refs/orna/",
        ]);
        let output = command
            .output()
            .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
        if !output.status.success() {
            return Err(FetchError::Repository(
                RepositoryError::GitOperationFailed,
            ));
        }
        let text = String::from_utf8(output.stdout).map_err(|_| FetchError::InvalidContinuity)?;
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let mut selected = Vec::new();
        for record in text.split('\n') {
            if record.is_empty() {
                continue;
            }
            let (object_id, reference) = record
                .split_once('\t')
                .ok_or(FetchError::InvalidContinuity)?;
            if reference.starts_with(PRIVATE_PIN_REF_PREFIX) {
                continue;
            }
            let reference = OrnaInternalRef::new(reference.to_owned())
                .map_err(|_| FetchError::InvalidContinuity)?;
            selected.push((reference, object_id.to_owned()));
        }
        Ok(selected)
    }

    /// Publishes the continuity refs [`Repository::ordinary_push_refs`] selects
    /// together with one ordinary branch ref.
    ///
    /// The selected continuity refs are transferred by pushing `refs/orna/*`
    /// directly, so the remote and the local repository agree on each ref by
    /// name. Every selected continuity ref must already match on the remote:
    /// a ref that the remote is missing or names with a different object ID is
    /// refused as a conflict instead of being republished under a different
    /// reachability, which is the rule a later fetch revalidates
    /// (`ORNA-GIT-008`).
    pub fn push_ordinary_with_local_continuity(
        &self,
        remote: impl Into<String>,
        branch: impl Into<String>,
    ) -> Result<(), FetchError> {
        let remote = remote.into();
        let branch = branch.into();
        if !valid_remote_name(&remote) || !valid_branch_name(&branch) {
            return Err(FetchError::InvalidRemote);
        }
        let _lock = self.acquire_coordination_lock()?;
        let selected = self.ordinary_push_refs()?;
        let mut continuity = Vec::with_capacity(selected.len());
        for (reference, object_id) in selected {
            if object_id.len() != self.native_object_id_length()? {
                return Err(FetchError::InvalidContinuity);
            }
            let advertised = advertise(
                self,
                &remote,
                std::slice::from_ref(&reference.as_str().to_owned()),
                object_id.len(),
            )?;
            match advertised.get(reference.as_str()) {
                Some(remote_id) if remote_id.eq_ignore_ascii_case(&object_id) => {}
                _ => return Err(FetchError::RefConflict),
            }
            continuity.push(reference);
        }
        let request = PushRequest::new(remote, branch, continuity)?;
        self.push(&request)
    }
}

/// A push request whose Orna continuity refs are sent before the ordinary
/// branch ref. The allocator ref, when present, is sent first so a later
/// branch advertisement cannot make an allocator watermark visible too soon.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PushRequest {
    remote: String,
    branch: String,
    internal: Vec<String>,
}

impl PushRequest {
    pub fn new(
        remote: impl Into<String>,
        branch: impl Into<String>,
        internal: impl IntoIterator<Item = OrnaInternalRef>,
    ) -> Result<Self, FetchError> {
        let remote = remote.into();
        let branch = branch.into();
        let internal = internal
            .into_iter()
            .map(|reference| reference.as_str().to_owned())
            .collect::<Vec<_>>();
        if !valid_remote_name(&remote) || !valid_branch_name(&branch) {
            return Err(FetchError::InvalidRemote);
        }
        let mut seen = BTreeSet::new();
        if internal.iter().any(|reference| {
            !valid_fetch_full_ref(reference) || !seen.insert(reference.clone())
        }) {
            return Err(FetchError::InvalidContinuity);
        }
        Ok(Self {
            remote,
            branch,
            internal,
        })
    }

    pub fn remote(&self) -> &str {
        &self.remote
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn internal(&self) -> &[String] {
        &self.internal
    }
}


/// Stable failures from the fetch boundary.  No Git command line, URL,
/// credential, local path, or stderr is retained in this type.
#[derive(Debug)]
pub enum FetchError {
    InvalidRemote,
    InvalidRef,
    EmptyRequest,
    TooManyRefs,
    InvalidContinuity,
    Repository(RepositoryError),
    RemoteUnavailable,
    MalformedAdvertisement,
    RequestedRefMissing,
    RemoteChanged,
    RefConflict,
    ObjectUnavailable,
    InvalidObjectId,
    ObjectNotPromised,
    PromisorUnavailable,
    HydrationFailed,
    PushFailed,
}

impl fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRemote => "invalid configured Git remote",
            Self::InvalidRef => "invalid Git fetch ref",
            Self::EmptyRequest => "Git fetch request is empty",
            Self::TooManyRefs => "Git fetch request exceeds its ref limit",
            Self::InvalidContinuity => "invalid Orna continuity evidence",
            Self::Repository(error) => return error.fmt(formatter),
            Self::RemoteUnavailable => "configured Git remote is unavailable",
            Self::MalformedAdvertisement => "configured Git remote returned malformed refs",
            Self::RequestedRefMissing => "requested Git ref is missing on the remote",
            Self::RemoteChanged => "remote Git refs changed during fetch",
            Self::RefConflict => "local Git ref changed or would overwrite newer state",
            Self::ObjectUnavailable => "fetched Git object is unavailable locally",
            Self::InvalidObjectId => "invalid native Git object ID",
            Self::ObjectNotPromised => "Git object is not a proven promised object",
            Self::PromisorUnavailable => "no usable Git promisor remote is configured",
            Self::HydrationFailed => "Git promised-object hydration failed",
            Self::PushFailed => "Git push failed before the requested refs were published",
        })
    }
}

impl std::error::Error for FetchError {}

impl From<RepositoryError> for FetchError {
    fn from(error: RepositoryError) -> Self {
        Self::Repository(error)
    }
}

/// One ref made available by a successful fetch plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedRef {
    source: String,
    destination: String,
    object_id: NativeObjectId,
    updated: bool,
}

impl FetchedRef {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn destination(&self) -> &str {
        &self.destination
    }

    pub fn object_id(&self) -> &NativeObjectId {
        &self.object_id
    }

    pub const fn updated(&self) -> bool {
        self.updated
    }
}

/// Evidence returned after Git objects were fetched and local refs were
/// installed.  `continuity` is `None` when no internal witness was requested;
/// otherwise it is the exact fail-closed state of those witnesses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchReport {
    ordinary: Vec<FetchedRef>,
    internal: Vec<FetchedRef>,
    continuity: Option<RemoteContinuity>,
}

impl FetchReport {
    pub fn ordinary(&self) -> &[FetchedRef] {
        &self.ordinary
    }

    pub fn internal(&self) -> &[FetchedRef] {
        &self.internal
    }

    pub const fn continuity(&self) -> Option<RemoteContinuity> {
        self.continuity
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefKind {
    Branch,
    Tag,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RefPlan {
    source: String,
    destination: String,
    object_id: String,
    old: Option<String>,
    kind: RefKind,
    updated: bool,
}

impl Repository {
    /// Materializes one exact object that the local partial-clone walk has
    /// already proven to be promised. The fetch has an empty destination, so
    /// it may add object bytes but cannot update refs or `FETCH_HEAD`; the
    /// postcondition verifies that the requested ID is locally materialized.
    /// Submodule recursion is disabled: unrelated repositories cannot affect
    /// the requested object's availability or receive incidental ref updates.
    /// A materialized object is an idempotent success. A failed transport remains
    /// `ObjectUnavailable`; a successful transport that does not materialize the
    /// requested object fails as `HydrationFailed`. Unavailable, malformed, or
    /// unproven IDs fail closed before a remote is contacted.
    pub fn hydrate_promised_object(&self, object_id: &str) -> Result<(), FetchError> {
        let object_id =
            NativeObjectId::new(object_id.to_owned()).map_err(|_| FetchError::InvalidObjectId)?;
        let state = self.observe_git_object(object_id.as_str())?;
        if matches!(state, super::GitObjectState::Materialized { .. }) {
            return Ok(());
        }
        if !matches!(state, super::GitObjectState::Promised) {
            return Err(match state {
                super::GitObjectState::Malformed => FetchError::InvalidObjectId,
                super::GitObjectState::Unavailable => FetchError::ObjectNotPromised,
                super::GitObjectState::Materialized { .. } | super::GitObjectState::Promised => {
                    unreachable!()
                }
            });
        }

        let _lock = self.acquire_coordination_lock()?;
        let capabilities = self.observe_git_capabilities()?;
        if capabilities.mode == super::GitRepositoryMode::Malformed
            || capabilities.promisor_remotes().is_empty()
        {
            return Err(FetchError::PromisorUnavailable);
        }

        // Revalidate the promise under the repository lock so a concurrent
        // fetch or maintenance operation cannot turn a stale observation into
        // an unconstrained object request.
        let state = self.observe_git_object(object_id.as_str())?;
        if matches!(state, super::GitObjectState::Materialized { .. }) {
            return Ok(());
        }
        if !matches!(state, super::GitObjectState::Promised) {
            return Err(match state {
                super::GitObjectState::Malformed => FetchError::InvalidObjectId,
                super::GitObjectState::Unavailable => FetchError::ObjectNotPromised,
                super::GitObjectState::Materialized { .. } | super::GitObjectState::Promised => {
                    unreachable!()
                }
            });
        }

        let mut saw_successful_fetch = false;
        for remote in capabilities.promisor_remotes() {
            let mut command = self.observer_command();
            command
                .env("GIT_TERMINAL_PROMPT", "0")
                .args([
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--refmap=",
                    remote,
                ])
                .arg(format!("{}:", object_id.as_str()))
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let Ok(status) = command.status() else {
                continue;
            };
            if !status.success() {
                continue;
            }
            saw_successful_fetch = true;
            match self.observe_git_object(object_id.as_str())? {
                super::GitObjectState::Materialized { .. } => return Ok(()),
                super::GitObjectState::Promised | super::GitObjectState::Unavailable => {}
                super::GitObjectState::Malformed => return Err(FetchError::HydrationFailed),
            }
        }
        if saw_successful_fetch {
            Err(FetchError::HydrationFailed)
        } else {
            Err(FetchError::ObjectUnavailable)
        }
    }

    /// Fetches requested ordinary refs and continuity-approved `refs/orna/*`
    /// refs without changing HEAD, the index, the worktree, or runtime files.
    /// Local ref installation is one compare-and-set transaction; a newer or
    /// divergent local ref is never overwritten.
    pub fn fetch(&self, request: &FetchRequest) -> Result<FetchReport, FetchError> {
        validate_request(request)?;
        let _lock = self.acquire_coordination_lock()?;
        self.fetch_locked(request)
    }

    fn fetch_locked(&self, request: &FetchRequest) -> Result<FetchReport, FetchError> {
        let remotes = self.remote_names()?;
        if !remotes.iter().any(|remote| remote == request.remote()) {
            return Err(FetchError::InvalidRemote);
        }
        let object_id_length = self.native_object_id_length()?;

        let ordinary_sources = request
            .ordinary
            .iter()
            .map(RequestedRef::source)
            .collect::<Vec<_>>();
        let continuity_sources = request
            .continuity
            .iter()
            .map(|required| required.reference().as_str().to_owned())
            .collect::<Vec<_>>();
        let mut requested_sources = ordinary_sources.clone();
        requested_sources.extend(continuity_sources);
        // REMOTE-003 applies to ordinary fetch as well as an explicitly
        // witnessed fetch: every advertised refs/orna/* ref must travel with
        // the requested branch/tag.  Exact witnesses still gate continuity
        // claims below; an unrequested internal ref is only synchronized.
        let advertised =
            advertise_fetch(self, request.remote(), &requested_sources, object_id_length)?;
        let sources = advertised.keys().cloned().collect::<Vec<_>>();

        let mut ordinary_plans = Vec::with_capacity(request.ordinary.len());
        for requested in &request.ordinary {
            let source = requested.source();
            let object_id = advertised
                .get(&source)
                .ok_or(FetchError::RequestedRefMissing)?
                .clone();
            ordinary_plans.push(self.plan_ref(
                source,
                requested.destination(request.remote()),
                object_id,
                match requested {
                    RequestedRef::Branch(_) => RefKind::Branch,
                    RequestedRef::Tag(_) => RefKind::Tag,
                },
            )?);
        }

        let continuity = continuity_state(&request.continuity, &advertised, object_id_length)?;
        let mut internal_plans = Vec::new();
        let required = request
            .continuity
            .iter()
            .map(|witness| {
                (
                    witness.reference().as_str(),
                    witness.expected().as_str(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for (source, object_id) in advertised
            .iter()
            .filter(|(source, _)| source.starts_with("refs/orna/"))
        {
            // An explicitly witnessed ref is installable only when its
            // advertised object matches the expected continuity evidence.
            // Unwitnessed refs are synchronized for REMOTE-003, but cannot
            // establish a continuity claim because `continuity_state` only
            // evaluates caller-supplied witnesses.
            if required
                .get(source.as_str())
                .is_some_and(|expected| !object_id.eq_ignore_ascii_case(expected))
            {
                continue;
            }
            internal_plans.push(self.plan_ref(
                source.clone(),
                source.clone(),
                object_id.clone(),
                RefKind::Internal,
            )?);
        }
        // Keep the internal transaction's order stable regardless of the
        // caller's witness order. Allocator/checkpoint refs are one
        // continuity domain, so deterministic ref ordering makes fetch
        // planning and the subsequent CAS transaction reproducible.
        internal_plans.sort_by(|left, right| left.destination.cmp(&right.destination));

        let mut all_plans = ordinary_plans.clone();
        all_plans.extend(internal_plans.clone());
        fetch_objects(self, request.remote(), &all_plans)?;
        verify_fetched_objects(self, &all_plans)?;
        validate_fast_forward_plans(self, &all_plans)?;
        verify_remote_snapshot(
            self,
            &RemoteSnapshot {
                remote: request.remote().to_owned(),
                sources,
                initial: advertised,
                required: &request.continuity,
                initial_continuity: continuity,
                object_id_length,
            },
            &all_plans,
        )?;
        install_refs(self, &all_plans)?;

        Ok(FetchReport {
            ordinary: ordinary_plans.into_iter().map(FetchedRef::from).collect(),
            internal: internal_plans.into_iter().map(FetchedRef::from).collect(),
            continuity,
        })
    }

    fn plan_ref(
        &self,
        source: String,
        destination: String,
        object_id: String,

        kind: RefKind,
    ) -> Result<RefPlan, FetchError> {
        let old = local_ref_oid(self, &destination)?;
        let updated = match &old {
            None => true,
            Some(old) if old == &object_id => false,
            Some(_) if matches!(kind, RefKind::Branch | RefKind::Internal) => true,
            Some(_) => return Err(FetchError::RefConflict),
        };
        Ok(RefPlan {
            source,
            destination,
            object_id,
            old,
            kind,
            updated,
        })
    }
    /// Creates a partial clone with no checkout, retaining ordinary Git refs
    /// and synchronizing every advertised `refs/orna/*` ref into the new
    /// repository. Missing internal refs are tolerated so a repository moved
    /// with plain Git remains readable; when present, they are available to
    /// the repository/project checkout boundary before CWD selection.
    pub fn clone_from(
        remote: impl AsRef<str>,
        destination: impl AsRef<Path>,
    ) -> Result<Self, FetchError> {
        let mut clone = Command::new("git");
        scrub_git_routing_environment(&mut clone);
        let status = clone
            .args(["clone", "--filter=blob:none", "--no-checkout"])
            .arg(remote.as_ref())
            .arg(destination.as_ref())
            .status()
            .map_err(|_| FetchError::RemoteUnavailable)?;
        if !status.success() {
            return Err(FetchError::RemoteUnavailable);
        }
        let repository = Repository::discover(destination).map_err(FetchError::Repository)?;
        let mut advertised = repository.observer_command();
        advertised
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["ls-remote", "--refs", "origin", "refs/orna/*"]);
        let output = advertised
            .output()
            .map_err(|_| FetchError::RemoteUnavailable)?;
        if !output.status.success() {
            return Err(FetchError::RemoteUnavailable);
        }
        if !output.stdout.is_empty() {
            let mut fetch = repository.observer_command();
            fetch
                .env("GIT_TERMINAL_PROMPT", "0")
                .args([
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--refmap=",
                    "origin",
                    "+refs/orna/*:refs/orna/*",
                ]);
            if !fetch
                .status()
                .map_err(|_| FetchError::RemoteUnavailable)?
                .success()
            {
                return Err(FetchError::RemoteUnavailable);
            }
        }
        Ok(repository)
    }

    /// Pulls the requested branch and continuity refs without changing CWD;
    /// merge/checkout remains an explicit repository operation.
    pub fn pull(&self, request: &FetchRequest) -> Result<FetchReport, FetchError> {
        self.fetch(request)
    }

}
impl Repository {
    /// Pushes Orna continuity refs before the ordinary branch ref. The
    /// allocator ref is isolated in the first push when present, so a later
    /// branch update cannot become visible before its allocation watermark.
    pub fn push(&self, request: &PushRequest) -> Result<(), FetchError> {
        if !self
            .remote_names()?
            .iter()
            .any(|remote| remote == request.remote())
        {
            return Err(FetchError::InvalidRemote);
        }
        let branch_ref = format!("refs/heads/{}", request.branch());
        let mut internal = request.internal.clone();
        internal.sort_by_key(|reference| {
            if reference == ALLOCATOR_REF {
                0
            } else {
                1
            }
        });
        let mut ordered = internal
            .into_iter()
            .map(|reference| format!("{reference}:{reference}"))
            .collect::<Vec<_>>();
        ordered.push(format!("{branch_ref}:{branch_ref}"));
        let _lock = self.acquire_coordination_lock()?;
        self.preflight_atomic(request.remote(), &branch_ref)?;
        let allocator = ordered
            .iter()
            .position(|refspec| refspec == &format!("{ALLOCATOR_REF}:{ALLOCATOR_REF}"));
        if let Some(index) = allocator {
            let allocator_ref = ordered.remove(index);
            self.push_refspecs(request.remote(), std::slice::from_ref(&allocator_ref), false)?;
        }
        self.push_refspecs(request.remote(), &ordered, true)
    }

    fn preflight_atomic(&self, remote: &str, branch_ref: &str) -> Result<(), FetchError> {
        let branch_refspec = format!("{branch_ref}:{branch_ref}");
        let mut command = self.observer_command();
        command.args([
            "push",
            "--atomic",
            "--dry-run",
            "--porcelain",
            "--no-follow-tags",
            remote,
            &branch_refspec,
        ]);
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .status()
            .map_err(|_| FetchError::RemoteUnavailable)?
            .success()
            .then_some(())
            .ok_or(FetchError::PushFailed)
    }

    fn push_refspecs(
        &self,
        remote: &str,
        refspecs: &[String],
        atomic: bool,
    ) -> Result<(), FetchError> {
        let mut command = self.observer_command();
        command.args(["push", "--porcelain", "--no-follow-tags"]);
        if atomic {
            command.arg("--atomic");
        }
        command
            .arg(remote)
            .args(refspecs)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .status()
            .map_err(|_| FetchError::RemoteUnavailable)?
            .success()
            .then_some(())
            .ok_or(FetchError::PushFailed)
    }
}

const ALLOCATOR_REF: &str = "refs/orna/allocator";

impl From<RefPlan> for FetchedRef {
    fn from(plan: RefPlan) -> Self {
        Self {
            source: plan.source,
            destination: plan.destination,
            object_id: NativeObjectId::new(plan.object_id).expect("validated fetch object ID"),
            updated: plan.updated,
        }
    }
}

fn validate_fast_forward_plans(
    repository: &Repository,
    plans: &[RefPlan],
) -> Result<(), FetchError> {
    for plan in plans {
        if !plan.updated || !matches!(plan.kind, RefKind::Branch | RefKind::Internal) {
            continue;
        }
        if let Some(old) = &plan.old
            && !is_fast_forward(repository, old, &plan.object_id)?
        {
            return Err(FetchError::RefConflict);
        }
    }
    Ok(())
}

struct RemoteSnapshot<'a> {
    remote: String,
    sources: Vec<String>,
    initial: BTreeMap<String, String>,
    required: &'a [RequiredInternalRef],
    initial_continuity: Option<RemoteContinuity>,
    object_id_length: usize,
}

fn verify_remote_snapshot(
    repository: &Repository,
    snapshot: &RemoteSnapshot<'_>,
    plans: &[RefPlan],
) -> Result<(), FetchError> {
    let RemoteSnapshot {
        remote,
        sources,
        initial,
        required,
        initial_continuity,
        object_id_length,
    } = snapshot;
    let current = advertise(repository, remote, sources, *object_id_length)?;
    if current != *initial
        || continuity_state(required, &current, *object_id_length)? != *initial_continuity
        || plans.iter().any(|plan| {
            current
                .get(&plan.source)
                .is_none_or(|object_id| object_id != &plan.object_id)
        })
    {
        return Err(FetchError::RemoteChanged);
    }
    Ok(())
}

fn validate_request(request: &FetchRequest) -> Result<(), FetchError> {
    if !valid_remote_name(&request.remote) {
        return Err(FetchError::InvalidRemote);
    }
    let request_len = request
        .ordinary
        .len()
        .saturating_add(request.continuity.len());
    if request_len == 0 {
        return Err(FetchError::EmptyRequest);
    }
    if request_len > MAX_FETCH_REFS {
        return Err(FetchError::TooManyRefs);
    }
    let mut destinations = BTreeSet::new();
    for requested in &request.ordinary {
        let source = requested.source();
        let destination = requested.destination(&request.remote);
        if !valid_fetch_full_ref(&source)
            || !valid_fetch_full_ref(&destination)
            || !destinations.insert(destination)
        {
            return Err(FetchError::InvalidRef);
        }
    }
    for required in &request.continuity {
        let reference = required.reference().as_str();
        if !valid_fetch_full_ref(reference) || !destinations.insert(reference.to_owned()) {
            return Err(FetchError::InvalidContinuity);
        }
    }
    Ok(())
}

fn advertise(
    repository: &Repository,
    remote: &str,
    sources: &[String],
    object_id_length: usize,
) -> Result<BTreeMap<String, String>, FetchError> {
    let mut command = repository.observer_command();
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["ls-remote", "--refs", remote])
        .args(sources);
    let output = command
        .output()
        .map_err(|_| FetchError::RemoteUnavailable)?;
    if !output.status.success() {
        return Err(FetchError::RemoteUnavailable);
    }
    let text =
        std::str::from_utf8(&output.stdout).map_err(|_| FetchError::MalformedAdvertisement)?;
    if text.is_empty() {
        return Ok(BTreeMap::new());
    }
    let records = text
        .strip_suffix('\n')
        .ok_or(FetchError::MalformedAdvertisement)?;
    let expected = sources.iter().cloned().collect::<BTreeSet<_>>();
    let mut advertised = BTreeMap::new();
    for record in records.split('\n') {
        let (object_id, reference) = record
            .split_once('\t')
            .ok_or(FetchError::MalformedAdvertisement)?;
        if !expected.contains(reference)
            || object_id.len() != object_id_length
            || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || advertised
                .insert(reference.to_owned(), object_id.to_ascii_lowercase())
                .is_some()
        {
            return Err(FetchError::MalformedAdvertisement);
        }
    }
    Ok(advertised)
}
fn advertise_fetch(
    repository: &Repository,
    remote: &str,
    sources: &[String],
    object_id_length: usize,
) -> Result<BTreeMap<String, String>, FetchError> {
    let mut command = repository.observer_command();
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["ls-remote", "--refs", remote])
        .args(sources)
        .arg("refs/orna/*");
    let output = command
        .output()
        .map_err(|_| FetchError::RemoteUnavailable)?;
    if !output.status.success() {
        return Err(FetchError::RemoteUnavailable);
    }
    let text =
        std::str::from_utf8(&output.stdout).map_err(|_| FetchError::MalformedAdvertisement)?;
    if text.is_empty() {
        return Ok(BTreeMap::new());
    }
    let records = text
        .strip_suffix('\n')
        .ok_or(FetchError::MalformedAdvertisement)?;
    let expected = sources.iter().cloned().collect::<BTreeSet<_>>();
    let mut advertised = BTreeMap::new();
    for record in records.split('\n') {
        let (object_id, reference) = record
            .split_once('\t')
            .ok_or(FetchError::MalformedAdvertisement)?;
        if (!expected.contains(reference)
            && !reference.starts_with("refs/orna/")
            || !valid_fetch_full_ref(reference)
            || object_id.len() != object_id_length
            || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit()))
            || advertised
                .insert(reference.to_owned(), object_id.to_ascii_lowercase())
                .is_some()
        {
            return Err(FetchError::MalformedAdvertisement);
        }
    }
    Ok(advertised)
}



fn continuity_state(
    required: &[RequiredInternalRef],
    advertised: &BTreeMap<String, String>,
    object_id_length: usize,
) -> Result<Option<RemoteContinuity>, FetchError> {
    if required.is_empty() {
        return Ok(None);
    }
    let mut missing = false;
    let mut stale = false;
    for witness in required {
        let expected = witness.expected().as_str();
        if expected.len() != object_id_length {
            return Err(FetchError::InvalidContinuity);
        }
        match advertised.get(witness.reference().as_str()) {
            None => missing = true,
            Some(actual) if !actual.eq_ignore_ascii_case(expected) => stale = true,
            Some(_) => {}
        }
    }
    Ok(Some(if missing {
        RemoteContinuity::Missing
    } else if stale {
        RemoteContinuity::Stale
    } else {
        RemoteContinuity::Continuous
    }))
}

fn local_ref_oid(repository: &Repository, reference: &str) -> Result<Option<String>, FetchError> {
    let mut command = repository.observer_command();
    command
        .args(["show-ref", "--verify", "--quiet", "--"])
        .arg(reference);
    let output = command
        .output()
        .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
    if output.status.success() {
        let mut command = repository.observer_command();
        command.args(["rev-parse", "--verify", "--quiet", reference]);
        let output = command
            .output()
            .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
        if !output.status.success() {
            return Err(FetchError::Repository(RepositoryError::GitOperationFailed));
        }
        return Ok(Some(trim_output(&output.stdout).to_ascii_lowercase()));
    }
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        Ok(None)
    } else {
        Err(FetchError::Repository(RepositoryError::GitOperationFailed))
    }
}

fn is_fast_forward(repository: &Repository, old: &str, new: &str) -> Result<bool, FetchError> {
    let mut command = repository.observer_command();
    command.args(["merge-base", "--is-ancestor", old, new]);
    let output = command
        .output()
        .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
    if output.status.success() {
        Ok(true)
    } else if output.status.code() == Some(1) {
        Ok(false)
    } else {
        Err(FetchError::Repository(RepositoryError::GitOperationFailed))
    }
}

fn fetch_objects(
    repository: &Repository,
    remote: &str,
    plans: &[RefPlan],
) -> Result<(), FetchError> {
    if plans.is_empty() {
        return Ok(());
    }
    let mut command = repository.observer_command();
    command.env("GIT_TERMINAL_PROMPT", "0").args([
        "fetch",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--refmap=",
        remote,
    ]);
    // An empty destination downloads the source objects without allowing
    // Git's configured remote refspec to mutate a tracking ref.  Ref updates
    // are reserved for the validated CAS transaction below.
    // Configured submodule recursion must not escape this repository's plan.
    let refspecs = plans
        .iter()
        .map(|plan| format!("{}:", plan.source))
        .collect::<Vec<_>>();
    command
        .args(&refspecs)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = command
        .status()
        .map_err(|_| FetchError::RemoteUnavailable)?;
    if status.success() {
        Ok(())
    } else {
        Err(FetchError::RemoteUnavailable)
    }
}

fn verify_fetched_objects(repository: &Repository, plans: &[RefPlan]) -> Result<(), FetchError> {
    for plan in plans {
        let mut command = repository.observer_command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-e", &plan.object_id]);
        let status = command
            .status()
            .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
        if !status.success() {
            return Err(FetchError::ObjectUnavailable);
        }
        if matches!(plan.kind, RefKind::Branch) {
            let mut command = repository.observer_command();
            command
                .env("GIT_NO_LAZY_FETCH", "1")
                .args(["cat-file", "-t", &plan.object_id]);
            let output = command
                .output()
                .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
            if !output.status.success() {
                return Err(FetchError::ObjectUnavailable);
            }
            if trim_output(&output.stdout) != "commit" {
                return Err(FetchError::MalformedAdvertisement);
            }
        }
    }
    Ok(())
}

fn install_refs(repository: &Repository, plans: &[RefPlan]) -> Result<(), FetchError> {
    if plans.is_empty() {
        return Ok(());
    }
    let mut command = repository.command();
    scrub_git_routing_environment(&mut command);
    command
        .args(["update-ref", "--no-deref", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
    let mut input = String::from("start\n");
    for plan in plans {
        if plan.updated {
            match &plan.old {
                Some(old) => {
                    input.push_str(&format!(
                        "update {} {} {}\n",
                        plan.destination, plan.object_id, old
                    ));
                }
                None => {
                    input.push_str(&format!("create {} {}\n", plan.destination, plan.object_id))
                }
            }
        } else {
            let old = plan
                .old
                .as_deref()
                .expect("unchanged plan has an old object ID");
            input.push_str(&format!("verify {} {}\n", plan.destination, old));
        }
    }
    input.push_str("prepare\ncommit\n");
    child
        .stdin
        .take()
        .ok_or(FetchError::Repository(RepositoryError::GitOperationFailed))?
        .write_all(input.as_bytes())
        .map_err(|_| FetchError::Repository(RepositoryError::GitOperationFailed))?;
    let status = child
        .wait()
        .map_err(|_| FetchError::Repository(RepositoryError::GitOperationFailed))?;
    if status.success() {
        Ok(())
    } else {
        Err(FetchError::RefConflict)
    }
}

fn valid_fetch_ref_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && !name.ends_with('@')
        && name.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.starts_with('.')
                && !component.ends_with('.')
                && !component.ends_with(".lock")
                && !component.bytes().any(|byte| {
                    byte <= b' '
                        || byte == 0x7f
                        || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
                })
        })
}

fn valid_fetch_full_ref(reference: &str) -> bool {
    reference.starts_with("refs/")
        && valid_fetch_ref_name(reference.strip_prefix("refs/").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path, process::Command};

    fn git(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn ordinary_push_refs_selects_continuity_refs_and_keeps_private_pins_local() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("local");
        fs::create_dir(&local).unwrap();
        git(root.path(), &["init", "-b", "main", local.to_str().unwrap()]);
        git(&local, &["config", "user.name", "kierandrewett"]);
        git(&local, &["config", "user.email", "kieran@drewett.dev"]);
        git(&local, &["config", "commit.gpgsign", "false"]);
        fs::write(local.join("state.txt"), "allocator state\n").unwrap();
        git(&local, &["add", "state.txt"]);
        git(&local, &["commit", "-m", "initial"]);
        let head = git(&local, &["rev-parse", "HEAD"]);
        git(&local, &["update-ref", ALLOCATOR_REF, &head]);
        git(
            &local,
            &["update-ref", "refs/orna/checkpoints/0123456789abcdef", &head],
        );
        git(
            &local,
            &[
                "update-ref",
                "refs/orna/pins/0123456789abcdef/scratch/row-1",
                &head,
            ],
        );
        git(
            &local,
            &[
                "update-ref",
                "refs/orna/pins/ffffffffffffffff/pending/row-2",
                &head,
            ],
        );
        let repository = Repository::discover(&local).unwrap();
        let selected = repository.ordinary_push_refs().unwrap();
        let names = selected
            .iter()
            .map(|(reference, _)| reference.as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                ALLOCATOR_REF.to_owned(),
                "refs/orna/checkpoints/0123456789abcdef".to_owned(),
            ],
            "continuity refs are selected and no private pin ref is exposed"
        );
        assert_eq!(selected[0].1, head, "each ref keeps its own object ID");
    }

    #[test]
    fn push_request_validates_internal_refs_and_branch() {
        let allocator = OrnaInternalRef::new("refs/orna/allocator").unwrap();
        let checkpoint = OrnaInternalRef::new("refs/orna/checkpoint").unwrap();
        let request =
            PushRequest::new("origin", "main", [checkpoint.clone(), allocator.clone()]).unwrap();
        assert_eq!(request.remote(), "origin");
        assert_eq!(request.branch(), "main");
        assert_eq!(
            request.internal(),
            &[
                "refs/orna/checkpoint".to_owned(),
                "refs/orna/allocator".to_owned()
            ]
        );
        assert!(matches!(
            PushRequest::new("origin", "bad branch", [allocator.clone()]),
            Err(FetchError::InvalidRemote)
        ));
        assert!(matches!(
            PushRequest::new("origin", "main", [allocator.clone(), allocator]),
            Err(FetchError::InvalidContinuity)
        ));
    }

    #[test]
    fn fetch_with_local_continuity_produces_allocator_witness() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("local");
        let remote = root.path().join("remote.git");
        fs::create_dir(&local).unwrap();
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(&local, &["init", "-b", "main"]);
        git(&local, &["config", "user.name", "kierandrewett"]);
        git(&local, &["config", "user.email", "kieran@drewett.dev"]);
        git(&local, &["config", "commit.gpgsign", "false"]);
        fs::write(local.join("state.txt"), "allocator state\n").unwrap();
        git(&local, &["add", "state.txt"]);
        git(&local, &["commit", "-m", "initial"]);
        let head = git(&local, &["rev-parse", "HEAD"]);
        git(&local, &["update-ref", ALLOCATOR_REF, &head]);
        git(
            &local,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&local, &["push", "origin", "refs/heads/main"]);

        let repository = Repository::discover(&local).unwrap();
        let request = || {
            repository.fetch_with_local_continuity(
                "origin",
                [RequestedRef::branch("main").unwrap()],
            )
        };

        let missing = request().unwrap();
        assert_eq!(missing.continuity(), Some(RemoteContinuity::Missing));
        assert!(missing.internal().is_empty());

        git(
            &local,
            &["push", "origin", &format!("{ALLOCATOR_REF}:{ALLOCATOR_REF}")],
        );
        let continuous = request().unwrap();
        assert_eq!(continuous.continuity(), Some(RemoteContinuity::Continuous));
        assert_eq!(continuous.internal().len(), 1);
        assert_eq!(continuous.internal()[0].source(), ALLOCATOR_REF);
        assert_eq!(continuous.internal()[0].object_id().as_str(), head);
    }
}
