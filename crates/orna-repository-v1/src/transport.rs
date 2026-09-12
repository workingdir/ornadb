//! Bounded, repository-local Git fetch transport.
//!
//! This module deliberately stops at fetch.  It downloads the objects named
//! by ordinary requested refs and continuity-approved `refs/orna/*` refs, then
//! installs only compare-and-set-safe local refs.  It does not change HEAD,
//! the index, the worktree, or Orna's runtime files, and it does not claim to
//! implement clone, pull, or push.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Write,
    process::Stdio,
};

use super::{
    NativeObjectId, RemoteContinuity, Repository, RepositoryError,
    RequiredInternalRef, scrub_git_routing_environment, valid_branch_name, valid_remote_name,
};

const MAX_FETCH_REFS: usize = 4096;

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
    /// Fetches requested ordinary refs and continuity-approved `refs/orna/*`
    /// refs without changing HEAD, the index, the worktree, or runtime files.
    /// Local ref installation is one compare-and-set transaction; a newer or
    /// divergent local ref is never overwritten.
    pub fn fetch(&self, request: &FetchRequest) -> Result<FetchReport, FetchError> {
        validate_request(request)?;
        let _lock = self.acquire_coordination_lock()?;
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
        let mut sources = ordinary_sources.clone();
        sources.extend(continuity_sources.iter().cloned());
        let advertised = advertise(self, request.remote(), &sources, object_id_length)?;

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
        if continuity == Some(RemoteContinuity::Continuous) {
            for required in &request.continuity {
                let source = required.reference().as_str().to_owned();
                let object_id = advertised
                    .get(&source)
                    .ok_or(FetchError::RequestedRefMissing)?
                    .clone();
                internal_plans.push(self.plan_ref(
                    source.clone(),
                    source,
                    object_id,
                    RefKind::Internal,
                )?);
            }
        }

        let mut all_plans = ordinary_plans.clone();
        all_plans.extend(internal_plans.clone());
        fetch_objects(self, request.remote(), &all_plans)?;
        verify_fetched_objects(self, &all_plans)?;
        validate_fast_forward_plans(self, &all_plans)?;
        verify_remote_snapshot(
            self,
            request.remote(),
            &sources,
            &advertised,
            &request.continuity,
            continuity,
            object_id_length,
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
}

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
        if let Some(old) = &plan.old {
            if !is_fast_forward(repository, old, &plan.object_id)? {
                return Err(FetchError::RefConflict);
            }
        }
    }
    Ok(())
}

fn verify_remote_snapshot(
    repository: &Repository,
    remote: &str,
    sources: &[String],
    initial: &BTreeMap<String, String>,
    required: &[RequiredInternalRef],
    initial_continuity: Option<RemoteContinuity>,
    object_id_length: usize,
    plans: &[RefPlan],
) -> Result<(), FetchError> {
    let current = advertise(repository, remote, sources, object_id_length)?;
    if current != *initial
        || continuity_state(required, &current, object_id_length)? != initial_continuity
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
    let request_len = request.ordinary.len().saturating_add(request.continuity.len());
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
    let text = std::str::from_utf8(&output.stdout).map_err(|_| FetchError::MalformedAdvertisement)?;
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
    let mut command = repository.command();
    command
        .args(["show-ref", "--verify", "--quiet", "--"])
        .arg(reference);
    let output = command.output().map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
    if output.status.success() {
        return repository
            .git(["rev-parse", "--verify", "--quiet", reference])
            .map(|oid| Some(oid.to_ascii_lowercase()))
            .map_err(FetchError::from);
    }
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        Ok(None)
    } else {
        Err(FetchError::Repository(RepositoryError::GitOperationFailed))
    }
}

fn is_fast_forward(repository: &Repository, old: &str, new: &str) -> Result<bool, FetchError> {
    let mut command = repository.command();
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
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--refmap=",
            remote,
        ]);
    // An empty destination downloads the source objects without allowing
    // Git's configured remote refspec to mutate a tracking ref.  Ref updates
    // are reserved for the validated CAS transaction below.
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
    }
    Ok(())
}

fn install_refs(repository: &Repository, plans: &[RefPlan]) -> Result<(), FetchError> {
    let updates = plans.iter().filter(|plan| plan.updated).collect::<Vec<_>>();
    if updates.is_empty() {
        return Ok(());
    }
    let mut command = repository.command();
    scrub_git_routing_environment(&mut command);
    command
        .args(["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| FetchError::Repository(RepositoryError::GitUnavailable))?;
    let mut input = String::from("start\n");
    for plan in updates {
        match &plan.old {
            Some(old) => {
                input.push_str(&format!("update {} {} {}\n", plan.destination, plan.object_id, old));
            }
            None => input.push_str(&format!("create {} {}\n", plan.destination, plan.object_id)),
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
