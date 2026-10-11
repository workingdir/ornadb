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
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use super::{
    NativeObjectId, OrnaInternalRef, RemoteContinuity, Repository, RepositoryError,
    RequiredInternalRef, scrub_git_routing_environment, trim_output, valid_branch_name,
    valid_remote_name,
};

use crate::native_graph::{sync_all_pack_files, sync_directory};

const MAX_FETCH_REFS: usize = 4096;

/// Names the Gate D fault-injection seam that orders a fetch between its
/// object barrier and its ref CAS. Set to a whole number of seconds to hold
/// the process there so a supervising test can kill it deterministically.
const FAULT_INJECTION_HOLD_ENV: &str = "ORNA_FAULT_INJECTION_FETCH_HOLD_SECONDS";

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
        self.push_locked(&request)
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
    FetchedObjectsNotDurable,
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
            Self::FetchedObjectsNotDurable => {
                "fetched objects could not be made durable before installing refs"
            }
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
        sync_fetched_objects(self, &all_plans)?;
        install_refs(self, &all_plans)?;
        sync_fetched_ref_directories(self, &all_plans)?;

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
        let _lock = self.acquire_coordination_lock()?;
        self.push_locked(request)
    }

    /// Pushes with the repository coordination lock already held.
    fn push_locked(&self, request: &PushRequest) -> Result<(), FetchError> {
        if !self
            .remote_names()?
            .iter()
            .any(|remote| remote == request.remote())
        {
            return Err(FetchError::InvalidRemote);
        }
        let branch_ref = format!("refs/heads/{}", request.branch());
        let mut internal = request.internal.clone();
        internal.sort_by_key(|reference| if reference == ALLOCATOR_REF { 0 } else { 1 });
        let mut ordered = internal
            .into_iter()
            .map(|reference| format!("{reference}:{reference}"))
            .collect::<Vec<_>>();
        ordered.push(format!("{branch_ref}:{branch_ref}"));
        self.preflight_atomic(request.remote(), &branch_ref)?;
        let allocator = ordered
            .iter()
            .position(|refspec| refspec == &format!("{ALLOCATOR_REF}:{ALLOCATOR_REF}"));
        if let Some(index) = allocator {
            let allocator_ref = ordered.remove(index);
            self.push_refspecs(
                request.remote(),
                std::slice::from_ref(&allocator_ref),
                false,
            )?;
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

/// Resolves one repository-relative Git path for this worktree.
fn git_path(repository: &Repository, name: &str) -> Result<PathBuf, FetchError> {
    let mut command = repository.observer_command();
    scrub_git_routing_environment(&mut command);
    let output = command
        .args(["rev-parse", "--git-path", name])
        .output()
        .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
    if !output.status.success() {
        return Err(FetchError::Repository(RepositoryError::GitOperationFailed));
    }
    let value = std::str::from_utf8(&output.stdout)
        .map_err(|_| FetchError::Repository(RepositoryError::GitOperationFailed))?
        .trim();
    let path = Path::new(value);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        repository.worktree().join(path)
    })
}

/// Makes the objects a fetch just wrote durable before any ref names them.
///
/// `git fetch` reports success with the new loose objects and packs still in
/// the page cache. Installing refs first would let a crash publish a ref whose
/// objects never reached the disk, so this barrier runs between verification
/// and the ref CAS: every fetched object is fsynced, the pack set is fsynced
/// when an object is not loose, and both the loose-object shards and the object
/// directory are persisted so the names survive with the bytes.
///
/// A mirrored object database is refused rather than half-synced: when
/// `objects/info/alternates` is non-empty the fetched closure may live outside
/// this repository, and silently skipping those objects would turn an
/// unverifiable fetch into a silent one.
fn sync_fetched_objects(repository: &Repository, plans: &[RefPlan]) -> Result<(), FetchError> {
    if plans.is_empty() {
        return Ok(());
    }
    let mut objects = BTreeSet::new();
    for plan in plans {
        objects
            .insert(NativeObjectId::new(plan.object_id.clone()).map_err(FetchError::Repository)?);
    }
    let objects_dir = git_path(repository, "objects")?;
    let alternates = objects_dir.join("info").join("alternates");
    match std::fs::symlink_metadata(&alternates) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.len() != 0 {
                return Err(FetchError::FetchedObjectsNotDurable);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(FetchError::FetchedObjectsNotDurable),
    }
    let mut needs_pack_sync = false;
    for object_id in &objects {
        let hex = object_id.as_str();
        let shard = hex.get(..2).ok_or(FetchError::FetchedObjectsNotDurable)?;
        let path = objects_dir
            .join(shard)
            .join(hex.get(2..).ok_or(FetchError::FetchedObjectsNotDurable)?);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(FetchError::FetchedObjectsNotDurable);
                }
                File::open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| FetchError::FetchedObjectsNotDurable)?;
                sync_directory(path.parent().ok_or(FetchError::FetchedObjectsNotDurable)?)
                    .map_err(|_| FetchError::FetchedObjectsNotDurable)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                needs_pack_sync = true;
            }
            Err(_) => return Err(FetchError::FetchedObjectsNotDurable),
        }
    }
    if needs_pack_sync {
        sync_all_pack_files(&objects_dir.join("pack"))
            .map_err(|_| FetchError::FetchedObjectsNotDurable)?;
    }
    sync_directory(&objects_dir).map_err(|_| FetchError::FetchedObjectsNotDurable)?;
    fault_injection_seam();
    Ok(())
}

/// Orders this process between the object barrier and the ref CAS.
///
/// Gate D injects a fault at each durability boundary by killing the process
/// exactly here: the fetched bytes are already on disk, but no ref names them
/// yet, so a fault child that dies at this seam must leave the refs unwritten.
/// An unset variable makes it a no-op.
fn fault_injection_seam() {
    if let Some(seconds) = std::env::var_os(FAULT_INJECTION_HOLD_ENV) {
        let seconds = seconds
            .to_str()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
}

/// Persists the ref namespace the CAS just named, so the refs survive a crash
/// together with the objects they name.
///
/// The refs are installed first because their directories only exist once
/// `install_refs` has created them; this pass then syncs each ref's directory
/// and every existing directory above it up to the worktree, which covers the
/// components `update-ref` had to create for the ref it wrote.
fn sync_fetched_ref_directories(
    repository: &Repository,
    plans: &[RefPlan],
) -> Result<(), FetchError> {
    for plan in plans {
        let ref_path = git_path(repository, &plan.destination)?;
        let mut directory = ref_path
            .parent()
            .ok_or(FetchError::FetchedObjectsNotDurable)?
            .to_path_buf();
        loop {
            match sync_directory(&directory) {
                Ok(()) => {}
                // A component this pass cannot reach needs no durability: the
                // ref was never written through it.
                Err(_) if !directory.exists() => {}
                Err(_) => return Err(FetchError::FetchedObjectsNotDurable),
            }
            let Some(parent) = directory.parent() else {
                break;
            };
            if !parent.starts_with(repository.worktree()) {
                break;
            }
            directory = parent.to_path_buf();
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

    /// Builds a bare remote holding one commit that `local` has never seen and
    /// points `local` at it, so fetching `main` is a real transfer rather than a
    /// no-op against objects the fixture already had.
    fn remote_with_one_unseen_commit(root: &Path, local: &Path) -> String {
        let author = root.join("author");
        let remote = root.join("remote.git");
        fs::create_dir(&author).unwrap();
        fs::create_dir(local).unwrap();
        git(root, &["init", "--bare", remote.to_str().unwrap()]);
        git(&author, &["init", "-b", "main"]);
        git(&author, &["config", "user.name", "kierandrewett"]);
        git(&author, &["config", "user.email", "kieran@drewett.dev"]);
        git(&author, &["config", "commit.gpgsign", "false"]);
        fs::write(author.join("state.txt"), "fetched payload\n").unwrap();
        git(&author, &["add", "state.txt"]);
        git(&author, &["commit", "-m", "initial"]);
        let head = git(&author, &["rev-parse", "HEAD"]);
        git(
            &author,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&author, &["push", "origin", "refs/heads/main"]);
        assert_eq!(
            git(&remote, &["rev-parse", "refs/heads/main"]),
            head,
            "the sealed remote must advertise the commit under test"
        );
        git(local, &["init", "-b", "main"]);
        git(
            local,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        head
    }

    /// The durability barrier is ordered between object verification and the
    /// ref CAS, and a barrier that cannot make the fetched closure durable must
    /// leave the ref unpublished.
    ///
    /// A mirrored object database is the one shape the barrier refuses: the
    /// fetched closure may live outside this repository, so claiming durability
    /// for it would be a silent lie. The refusal also makes the ordering
    /// observable. If the barrier ran after `install_refs`, the ref below would
    /// already be installed when the barrier failed, and this test would see it.
    #[test]
    fn fetch_leaves_refs_unpublished_when_it_cannot_make_fetched_objects_durable() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("local");
        let head = remote_with_one_unseen_commit(root.path(), &local);

        let repository = Repository::discover(&local).unwrap();
        let request =
            FetchRequest::new("origin", [RequestedRef::branch("main").unwrap()], []).unwrap();
        assert_eq!(
            local_ref_oid(&repository, "refs/remotes/origin/main").unwrap(),
            None
        );
        let objects = local.join(".git").join("objects");
        assert!(
            !fetched_object_on_disk(&objects, &head),
            "the local repository must not already hold the commit under test"
        );

        // A non-empty alternates file marks a mirrored object database, which
        // the barrier must refuse rather than half-sync.
        let alternates = local
            .join(".git")
            .join("objects")
            .join("info")
            .join("alternates");
        fs::create_dir_all(alternates.parent().unwrap()).unwrap();
        fs::write(
            &alternates,
            format!("{}\n", root.path().join("remote.git").join("objects").display()),
        )
        .unwrap();

        assert!(matches!(
            repository.fetch(&request),
            Err(FetchError::FetchedObjectsNotDurable)
        ));
        assert_eq!(
            local_ref_oid(&repository, "refs/remotes/origin/main").unwrap(),
            None,
            "a failed durability barrier must not leave the ref published"
        );
    }

    /// A real fetch arrives as a pack, so the barrier's pack-set path is the
    /// one that runs on the normal fetch route. The fetched commit must be on
    /// disk, and the ref that names it installed.
    #[test]
    fn fetch_installs_refs_for_objects_the_barrier_made_durable() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("local");
        let remote = root.path().join("remote.git");
        fs::create_dir(&local).unwrap();
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(&local, &["init", "-b", "main"]);
        git(&local, &["config", "user.name", "kierandrewett"]);
        git(&local, &["config", "user.email", "kieran@drewett.dev"]);
        git(&local, &["config", "commit.gpgsign", "false"]);
        fs::write(local.join("state.txt"), "durable fetch payload\n").unwrap();
        git(&local, &["add", "state.txt"]);
        git(&local, &["commit", "-m", "initial"]);
        let head = git(&local, &["rev-parse", "HEAD"]);
        git(
            &local,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&local, &["push", "origin", "refs/heads/main"]);

        let repository = Repository::discover(&local).unwrap();
        let request =
            FetchRequest::new("origin", [RequestedRef::branch("main").unwrap()], []).unwrap();
        repository.fetch(&request).unwrap();

        assert_eq!(
            local_ref_oid(&repository, "refs/remotes/origin/main").unwrap(),
            Some(head.clone())
        );
        // The commit the published ref names must physically exist in the local
        // object database the barrier synced: loose, or inside a pack set.
        let objects = local.join(".git").join("objects");
        let loose = objects.join(&head[..2]).join(&head[2..]);
        let packed = fs::read_dir(objects.join("pack"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .any(|entry| entry.path().extension().is_some_and(|ext| ext == "idx"))
            })
            .unwrap_or(false);
        assert!(
            loose.is_file() || packed,
            "fetched commit {head} is neither loose nor packed in the object database"
        );
    }

    /// The barrier never claims durability for an object it cannot see: with no
    /// loose copy and no pack set, the pass is rejected.
    #[test]
    fn durability_barrier_rejects_objects_it_cannot_find() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("local");
        fs::create_dir(&local).unwrap();
        git(&local, &["init", "-b", "main"]);
        let repository = Repository::discover(&local).unwrap();

        let absent = "0123456789abcdef0123456789abcdef01234567".to_owned();
        let plan = RefPlan {
            source: "refs/heads/main".to_owned(),
            destination: "refs/remotes/origin/main".to_owned(),
            object_id: absent,
            old: None,
            kind: RefKind::Branch,
            updated: true,
        };
        assert!(matches!(
            sync_fetched_objects(&repository, &[plan]),
            Err(FetchError::FetchedObjectsNotDurable)
        ));
    }

    const FETCH_CRASH_ROOT: &str = "ORNA_FETCH_CRASH_ROOT";
    const FETCH_CRASH_TEST: &str =
        "transport::tests::crashed_fetch_holds_objects_durable_before_any_ref_names_them";

    fn fetched_object_on_disk(objects: &Path, hex: &str) -> bool {
        if objects.join(&hex[..2]).join(&hex[2..]).is_file() {
            return true;
        }
        fs::read_dir(objects.join("pack"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .any(|entry| entry.path().extension().is_some_and(|ext| ext == "idx"))
            })
            .unwrap_or(false)
    }

    /// Fetches the one ref its repository advertises, which parks this process
    /// inside the durability barrier's fault-injection seam until the
    /// supervisor kills it.
    fn crashed_fetch_child_body(root: &Path) {
        let repository =
            Repository::discover(&root.join("local")).expect("discover crash repository");
        let request =
            FetchRequest::new("origin", [RequestedRef::branch("main").unwrap()], []).unwrap();
        let _ = repository.fetch(&request);
    }

    /// Kills a fetch between its object barrier and its ref CAS, which is the
    /// one boundary Gate D must survive. The fetched commit is on disk when the
    /// process dies, and no ref names it: the objects the barrier made durable
    /// authorize nothing, because the publisher never ran.
    ///
    /// The child holds the seam instead of racing a real crash, so the object
    /// appearing with the ref still unwritten and the child still alive is the
    /// ordering itself, not a timing coincidence.
    #[test]
    fn crashed_fetch_holds_objects_durable_before_any_ref_names_them() {
        use std::time::{Duration, Instant};

        if let Some(root) = std::env::var_os(FETCH_CRASH_ROOT) {
            crashed_fetch_child_body(Path::new(&root));
            return;
        }

        let directory = tempfile::tempdir().expect("create crash boundary root");
        let root = directory.path();
        let local = root.join("local");
        let head = remote_with_one_unseen_commit(root, &local);

        let repository = Repository::discover(&local).unwrap();
        assert_eq!(
            local_ref_oid(&repository, "refs/remotes/origin/main").unwrap(),
            None
        );
        let objects = local.join(".git").join("objects");
        assert!(
            !fetched_object_on_disk(&objects, &head),
            "the parent repository must not already hold the fetched commit"
        );

        let mut child = Command::new(std::env::current_exe().expect("resolve test binary"))
            .args([
                "--exact",
                FETCH_CRASH_TEST,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(FETCH_CRASH_ROOT, root.as_os_str())
            .env(FAULT_INJECTION_HOLD_ENV, "30")
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn the held fetch");

        let deadline = Instant::now() + Duration::from_secs(20);
        while !fetched_object_on_disk(&objects, &head) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            fetched_object_on_disk(&objects, &head),
            "the barrier must persist the fetched commit before the seam"
        );
        assert_eq!(
            local_ref_oid(&repository, "refs/remotes/origin/main").unwrap(),
            None,
            "no ref may name the fetched commit while the fetch is held before its CAS"
        );
        assert!(
            child.try_wait().expect("poll the held fetch").is_none(),
            "the fetch must still be held in the seam, not finished"
        );

        child.kill().expect("kill the held fetch");
        let status = child.wait().expect("await the killed fetch");
        assert!(
            !status.success(),
            "the held fetch must die by signal, not publish an exit status"
        );
        assert_eq!(
            local_ref_oid(&repository, "refs/remotes/origin/main").unwrap(),
            None,
            "a crash before the ref CAS must leave the ref unpublished"
        );
        assert!(
            fetched_object_on_disk(&objects, &head),
            "the killed fetch must leave the durable bytes its barrier synced"
        );
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
