use crate::*;

impl Repository {
    /// Creates `name` at the selected commit using Git's nonexistence CAS.
    /// It makes no commit and does not modify the index or worktree. An
    /// existing branch is never moved: it is refused with `BranchExists`, and a
    /// HEAD that moved during the create is refused with `StaleHead`.
    pub fn create_branch_at_head(&self, name: &str) -> Result<GitCommitRef, RepositoryError> {
        if !valid_branch_name(name) || name == "HEAD" || name == "@" {
            return Err(RepositoryError::InvalidBranchName);
        }
        let _lock = self.acquire_coordination_lock()?;
        let head = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        let reference = format!("refs/heads/{name}");
        if self.ref_exists(&reference)? {
            return Err(RepositoryError::BranchExists);
        }
        match self.update_ref_transaction(&head, &reference) {
            Ok(()) => Ok(head),
            Err(RepositoryError::StaleHead) => {
                // One failed `update-ref` transaction covers both a HEAD that
                // moved and a ref that appeared concurrently; the state now
                // present says which one it was.
                if self.head()?.as_ref() != Some(&head) {
                    Err(RepositoryError::StaleHead)
                } else if self.ref_exists(&reference)? {
                    Err(RepositoryError::BranchExists)
                } else {
                    Err(RepositoryError::GitOperationFailed)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Lists ordinary configured remote names. Internal refs are not published
    /// here; publication is deliberately owned by a later layer.
    pub fn remote_names(&self) -> Result<Vec<String>, RepositoryError> {
        let output = self.git(["remote"])?;
        Ok(output
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Passively verifies that a configured ordinary remote has exactly the
    /// required `refs/orna/*` object identities. This invokes only
    /// `git ls-remote --refs`; it never fetches, pushes, hydrates promised
    /// objects, or changes local configuration, refs, index, or worktree.
    ///
    /// A plain-Git transfer that lacks a required ref is `Missing`; a present
    /// ref at another object is `Stale`. Invalid input, unexpected internal
    /// refs, malformed command output, unsupported local object format, or an
    /// unreachable remote are all `Unverifiable` rather than guessed at.
    pub fn observe_remote_continuity(
        &self,
        remote: &str,
        required: &[RequiredInternalRef],
    ) -> RemoteContinuity {
        if required.is_empty() {
            return RemoteContinuity::InvalidEvidence;
        }
        if !valid_remote_name(remote) {
            return RemoteContinuity::Unverifiable;
        }
        let native_length = match self.observer_native_object_id_length() {
            Ok(length) => length,
            Err(_) => return RemoteContinuity::Unverifiable,
        };
        let mut expected = std::collections::BTreeMap::new();
        for requirement in required {
            if requirement.expected.as_str().len() != native_length
                || expected
                    .insert(
                        requirement.reference.as_str(),
                        requirement.expected.as_str(),
                    )
                    .is_some()
            {
                return RemoteContinuity::InvalidEvidence;
            }
        }
        let remotes = match self.observer_remote_names() {
            Ok(remotes) => remotes,
            Err(_) => return RemoteContinuity::Unverifiable,
        };
        if !remotes.iter().any(|candidate| candidate == remote) {
            return RemoteContinuity::Unverifiable;
        }
        let observed = match self.observer_remote_orna_refs(remote, native_length, &expected) {
            Ok(observed) => observed,
            Err(_) => return RemoteContinuity::Unverifiable,
        };
        if expected
            .keys()
            .any(|reference| !observed.contains_key(*reference))
        {
            return RemoteContinuity::Missing;
        }
        if expected.iter().any(|(reference, expected_id)| {
            observed
                .get(*reference)
                .is_none_or(|observed_id| !observed_id.eq_ignore_ascii_case(expected_id))
        }) {
            return RemoteContinuity::Stale;
        }
        RemoteContinuity::Continuous
    }

    /// Observes sparse-checkout and partial/promisor configuration without
    /// changing Git configuration, refs, the index, or the worktree.
    ///
    /// This is the repository-local half of ORNA-GIT-004: sparse checkout is
    /// a working-tree property, while partial clone is an object-materiality
    /// property. The observation does not fetch, hydrate, or infer allocator
    /// or transport semantics.
    pub fn observe_git_capabilities(&self) -> Result<GitRepositoryCapabilities, RepositoryError> {
        let remotes = match self.observer_remote_names() {
            Ok(remotes) => remotes,
            Err(RepositoryError::GitUnavailable) => {
                return Err(RepositoryError::GitUnavailable);
            }
            Err(_) => {
                return Ok(GitRepositoryCapabilities {
                    mode: GitRepositoryMode::Malformed,
                    sparse_checkout: false,
                    partial_clone: false,
                    promisor_remotes: Vec::new(),
                });
            }
        };
        if remotes.iter().any(|remote| !valid_remote_name(remote)) {
            return Ok(GitRepositoryCapabilities {
                mode: GitRepositoryMode::Malformed,
                sparse_checkout: false,
                partial_clone: false,
                promisor_remotes: Vec::new(),
            });
        }

        let mut malformed = false;
        let sparse_checkout = match self.worktree_or_local_config_value("core.sparseCheckout")? {
            ConfigValue::Missing => false,
            ConfigValue::Value(value) => match parse_git_bool(&value) {
                Some(value) => value,
                None => {
                    malformed = true;
                    false
                }
            },
            ConfigValue::Malformed => {
                malformed = true;
                false
            }
        };

        let mut promisor_remotes = Vec::new();
        for remote in &remotes {
            let key = format!("remote.{remote}.promisor");
            match self.local_config_value(&key)? {
                ConfigValue::Missing => {}
                ConfigValue::Value(value) => match parse_git_bool(&value) {
                    Some(true) => promisor_remotes.push(remote.clone()),
                    Some(false) => {}
                    None => malformed = true,
                },
                ConfigValue::Malformed => malformed = true,
            }
        }

        let partial_clone_remote = match self.local_config_value("extensions.partialClone")? {
            ConfigValue::Missing => None,
            ConfigValue::Value(value)
                if valid_remote_name(&value) && remotes.iter().any(|remote| remote == &value) =>
            {
                Some(value)
            }
            ConfigValue::Value(_) | ConfigValue::Malformed => {
                malformed = true;
                None
            }
        };
        if let Some(remote) = &partial_clone_remote
            && !promisor_remotes.iter().any(|candidate| candidate == remote)
        {
            malformed = true;
        }

        let partial_clone = partial_clone_remote.is_some() || !promisor_remotes.is_empty();
        let mode = if malformed {
            GitRepositoryMode::Malformed
        } else {
            match (sparse_checkout, partial_clone) {
                (false, false) => GitRepositoryMode::Ordinary,
                (true, false) => GitRepositoryMode::SparseCheckout,
                (false, true) => GitRepositoryMode::PartialClone,
                (true, true) => GitRepositoryMode::Combined,
            }
        };
        Ok(GitRepositoryCapabilities {
            mode,
            sparse_checkout,
            partial_clone,
            promisor_remotes,
        })
    }

    /// Observes one complete native Git object ID without invoking Git's
    /// fetch or filter machinery. Missing objects are classified as promised
    /// only when valid local promisor configuration and reachability evidence
    /// support that interpretation; otherwise they remain unavailable.
    pub fn observe_git_object(&self, object_id: &str) -> Result<GitObjectState, RepositoryError> {
        // Native Git object IDs are hexadecimal and Git reports them in their
        // canonical lowercase form. Normalize the caller spelling before
        // comparing command output so this observer agrees with
        // `NativeObjectId::new` without weakening exact-ID validation.
        let object_id = object_id.to_ascii_lowercase();
        let object_id_length = self.observer_native_object_id_length()?;
        if object_id.len() != object_id_length
            || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(GitObjectState::Malformed);
        }

        let capabilities = self.observe_git_capabilities()?;
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1");
        command
            .args([
                "cat-file",
                "--batch-check=%(objectname) %(objecttype) %(objectsize)",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        child
            .stdin
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?
            .write_all(format!("{object_id}\n").as_bytes())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !output.status.success() {
            // `cat-file` reports corrupt loose/packed objects through its
            // exit status.  Keep the observation redacted while preserving
            // the materialisation distinction for callers.
            return Ok(GitObjectState::Malformed);
        }
        let line = trim_output(&output.stdout);
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() == 2 && fields[0] == object_id && fields[1] == "missing" {
            if git_stderr_indicates_malformed_object(&output.stderr) {
                return Ok(GitObjectState::Malformed);
            }
            return Ok(if capabilities.mode == GitRepositoryMode::Malformed {
                GitObjectState::Malformed
            } else if capabilities.partial_clone {
                match self.promisor_reachability(&object_id)? {
                    PromisorReachability::Proven => GitObjectState::Promised,
                    PromisorReachability::NotProven => GitObjectState::Unavailable,
                    PromisorReachability::Malformed => GitObjectState::Malformed,
                }
            } else {
                GitObjectState::Unavailable
            });
        }
        if fields.len() != 3 || fields[0] != object_id {
            return Ok(GitObjectState::Malformed);
        }
        let kind = match fields[1] {
            "commit" => GitObjectKind::Commit,
            "tree" => GitObjectKind::Tree,
            "blob" => GitObjectKind::Blob,
            "tag" => GitObjectKind::Tag,
            _ => return Ok(GitObjectState::Malformed),
        };
        let Ok(size) = fields[2].parse::<u64>() else {
            return Ok(GitObjectState::Malformed);
        };
        Ok(GitObjectState::Materialized { kind, size })
    }

    /// Classifies a caller-declared object set without fetching, hydrating, or
    /// changing local Git state. A promised object preserves logical
    /// completeness; an unavailable or malformed object never does.
    pub fn observe_declared_git_object_set(
        &self,
        object_ids: &[&str],
    ) -> Result<GitDeclaredObjectSetState, RepositoryError> {
        let mut incomplete = false;
        for object_id in object_ids {
            match self.observe_git_object(object_id)? {
                GitObjectState::Materialized { .. } | GitObjectState::Promised => {}
                GitObjectState::Unavailable => incomplete = true,
                GitObjectState::Malformed => return Ok(GitDeclaredObjectSetState::Malformed),
            }
        }
        Ok(if incomplete {
            GitDeclaredObjectSetState::Incomplete
        } else {
            GitDeclaredObjectSetState::Complete
        })
    }

    /// Classifies a manifest-declared set of immutable storage segment blob
    /// IDs without fetching, hydrating, or changing local Git state.
    ///
    /// Unlike [`Self::observe_declared_git_object_set`], this boundary is for
    /// objects that a verified storage manifest has already declared to be
    /// segment blobs. A locally materialized commit, tree, or tag is therefore
    /// invalid evidence for such a declaration. A missing object remains
    /// logically available only when the local promisor walk proves its
    /// declared ID reachable; the manifest retains the segment's blob role
    /// while its bytes are absent.
    pub fn observe_declared_segment_blob_set(
        &self,
        object_ids: &[&str],
    ) -> Result<GitDeclaredObjectSetState, RepositoryError> {
        let mut incomplete = false;
        for object_id in object_ids {
            match self.observe_git_object(object_id)? {
                GitObjectState::Materialized {
                    kind: GitObjectKind::Blob,
                    ..
                }
                | GitObjectState::Promised => {}
                GitObjectState::Materialized { .. } | GitObjectState::Malformed => {
                    return Ok(GitDeclaredObjectSetState::Malformed);
                }
                GitObjectState::Unavailable => incomplete = true,
            }
        }
        Ok(if incomplete {
            GitDeclaredObjectSetState::Incomplete
        } else {
            GitDeclaredObjectSetState::Complete
        })
    }

    /// Hydrates only the promised segment blobs declared by a verified
    /// compact manifest. The complete manifest and shard witness is checked
    /// before any fetch is attempted; materialized segments are checked for
    /// their physical identity and promised segments are fetched through the
    /// exact-object transport boundary. A final verification prevents a
    /// partially hydrated or mismatched segment from being reported as ready.
    ///
    /// Git refs, `HEAD`, `FETCH_HEAD`, the index, worktree, runtime files and
    /// objects outside this manifest are not installation targets of this
    /// operation. Hydration may add the requested immutable object bytes to
    /// the local object store, but it never changes logical repository state.
    pub fn hydrate_compact_manifest_segments(
        &self,
        commit: &GitCommitRef,
        table: Uuid,
    ) -> Result<BTreeMap<String, GitObjectState>, FetchError> {
        let plan = self
            .plan_compact_manifest_hydration(commit, table)
            .map_err(FetchError::Repository)?;
        for (object_id, state) in &plan {
            if matches!(state, GitObjectState::Promised) {
                self.hydrate_promised_object(object_id)?;
            }
        }
        self.plan_compact_manifest_hydration(commit, table)
            .map_err(FetchError::Repository)
    }

    /// Hydrates only the promised compact-segment blobs whose verified
    /// manifest bounds overlap the inclusive key range. The range planner is
    /// the authority for selection and validation; each selected promise is
    /// materialized through the exact-object transport boundary, then the
    /// same range is planned again to verify the postcondition.
    ///
    /// This operation may add selected immutable object bytes to the local
    /// object store, but it never installs refs, changes `HEAD`, writes
    /// `FETCH_HEAD`, updates the index or worktree, or considers segments
    /// outside the requested range.
    pub fn hydrate_compact_manifest_segments_for_key_range(
        &self,
        commit: &GitCommitRef,
        table: Uuid,
        lower_key: &[u8],
        upper_key: &[u8],
    ) -> Result<BTreeMap<String, GitObjectState>, FetchError> {
        let plan = self
            .plan_compact_manifest_hydration_for_key_range(commit, table, lower_key, upper_key)
            .map_err(FetchError::Repository)?;
        for (object_id, state) in &plan {
            if matches!(state, GitObjectState::Promised) {
                self.hydrate_promised_object(object_id)?;
            }
        }
        self.plan_compact_manifest_hydration_for_key_range(commit, table, lower_key, upper_key)
            .map_err(FetchError::Repository)
    }

    pub(crate) fn local_config_value(&self, key: &str) -> Result<ConfigValue, RepositoryError> {
        self.config_value("--local", key)
    }

    pub(crate) fn worktree_or_local_config_value(
        &self,
        key: &str,
    ) -> Result<ConfigValue, RepositoryError> {
        match self.config_value("--worktree", key)? {
            ConfigValue::Missing => self.local_config_value(key),
            value => Ok(value),
        }
    }

    pub(crate) fn config_value(
        &self,
        scope: &str,
        key: &str,
    ) -> Result<ConfigValue, RepositoryError> {
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1");
        command.args(["config", scope, "--get", key]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            return Ok(ConfigValue::Value(trim_output(&output.stdout)));
        }
        if output.status.code() == Some(1) {
            return Ok(ConfigValue::Missing);
        }
        Ok(ConfigValue::Malformed)
    }

    pub(crate) fn observer_remote_names(&self) -> Result<Vec<String>, RepositoryError> {
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1");
        command.arg("remote");
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(trim_output(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    pub(crate) fn observer_remote_orna_refs(
        &self,
        remote: &str,
        object_id_length: usize,
        expected: &std::collections::BTreeMap<&str, &str>,
    ) -> Result<std::collections::BTreeMap<String, String>, RepositoryError> {
        let mut command = self.observer_command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["ls-remote", "--refs", remote])
            .args(expected.keys());
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let output =
            std::str::from_utf8(&output.stdout).map_err(|_| RepositoryError::GitOperationFailed)?;
        parse_remote_orna_refs(output, object_id_length, expected)
    }

    pub(crate) fn observer_native_object_id_length(&self) -> Result<usize, RepositoryError> {
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1");
        command.args(["rev-parse", "--show-object-format"]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        match trim_output(&output.stdout).as_str() {
            "sha1" => Ok(40),
            "sha256" => Ok(64),
            _ => Err(RepositoryError::UnsupportedObjectFormat),
        }
    }

    pub(crate) fn promisor_reachability(
        &self,
        object_id: &str,
    ) -> Result<PromisorReachability, RepositoryError> {
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1").args([
            "rev-list",
            "--objects",
            "--missing=print",
            "--no-object-names",
            "--all",
        ]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            // A local reachability walk cannot safely distinguish an absent
            // object from a malformed object database when Git rejects it.
            return Ok(PromisorReachability::Malformed);
        }
        let promised = format!("?{object_id}");
        Ok(
            if trim_output(&output.stdout)
                .lines()
                .any(|line| line.eq_ignore_ascii_case(&promised))
            {
                PromisorReachability::Proven
            } else {
                PromisorReachability::NotProven
            },
        )
    }
}
