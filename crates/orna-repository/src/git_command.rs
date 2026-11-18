//! Extracted from `lib.rs` along its seams; contents are moved verbatim.

use super::*;

pub(crate) fn trim_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

/// One record of a `-z` Git listing (`ls-tree` or `ls-files`): the
/// space-separated header columns before the first tab, and the path Git
/// printed after it. Records are NUL-separated, so a name may hold any byte
/// but NUL; this parser never splits on anything else.
pub(crate) struct GitListRecord<'a> {
    fields: &'a [u8],
    name: &'a [u8],
}

impl<'a> GitListRecord<'a> {
    /// Splits one complete record; `None` when it carries no tab.
    pub(crate) fn parse(record: &'a [u8]) -> Option<Self> {
        let tab = record.iter().position(|byte| *byte == b'\t')?;
        Some(Self {
            fields: &record[..tab],
            name: &record[tab + 1..],
        })
    }

    /// The header columns before the tab, in Git's order.
    pub(crate) fn fields(&self) -> impl Iterator<Item = &'a [u8]> {
        self.fields.split(|byte| *byte == b' ')
    }

    /// The raw header bytes before the tab, for prefix checks.
    pub(crate) fn header(&self) -> &'a [u8] {
        self.fields
    }

    /// The `index`-th header column.
    pub(crate) fn field(&self, index: usize) -> Option<&'a [u8]> {
        self.fields().nth(index)
    }

    /// The path Git printed after the tab.
    pub(crate) fn name(&self) -> &'a [u8] {
        self.name
    }
}

/// One `git ls-tree` record: the entry's mode, object kind and object id, and
/// the path it is filed under. A record with another column count is
/// malformed, so no caller re-checks the header shape.
pub(crate) struct GitTreeRecord<'a> {
    pub(crate) mode: &'a [u8],
    pub(crate) kind: &'a [u8],
    pub(crate) object: &'a [u8],
    pub(crate) path: &'a [u8],
}

impl<'a> GitTreeRecord<'a> {
    pub(crate) fn parse(record: &'a [u8]) -> Option<Self> {
        let record = GitListRecord::parse(record)?;
        let (Some(mode), Some(kind), Some(object), None) = (
            record.field(0),
            record.field(1),
            record.field(2),
            record.field(3),
        ) else {
            return None;
        };
        Some(Self {
            mode,
            kind,
            object,
            path: record.name(),
        })
    }
}

/// Most output a bounded Git listing (history, branches) may produce.
pub(crate) const MAX_LISTING_BYTES: usize = 4 * 1024 * 1024;

/// Runs `command` and returns its stdout, refusing anything past
/// `MAX_LISTING_BYTES`. The child is killed once the bound is crossed, so a
/// runaway listing cannot make this process buffer without limit.
pub(crate) fn run_bounded_listing(mut command: Command) -> Result<Vec<u8>, RepositoryError> {
    command.stdout(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(RepositoryError::GitOperationFailed)?;
    let mut output = Vec::new();
    let read = stdout
        .take(MAX_LISTING_BYTES as u64 + 1)
        .read_to_end(&mut output);
    if read.is_err() || output.len() > MAX_LISTING_BYTES {
        // The child may already have exited, so a failed kill is not an
        // error; the wait reaps it either way.
        let _ = child.kill();
        let _ = child.wait();
        return Err(RepositoryError::GitOperationFailed);
    }
    let status = child
        .wait()
        .map_err(|_| RepositoryError::GitOperationFailed)?;
    if !status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(output)
}

pub(crate) fn run_command(mut command: Command) -> Result<Output, RepositoryError> {
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(RepositoryError::GitOperationFailed)
    }
}

/// Runs `command` with `input` written to its stdin and returns its captured
/// output, as [`run_command`] does. A batch of paths goes through one process
/// this way instead of one process per path.
pub(crate) fn run_command_with_input(
    mut command: Command,
    input: &[u8],
) -> Result<Output, RepositoryError> {
    use std::io::Write as _;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    let written = child.stdin.take().map(|mut stdin| stdin.write_all(input));
    let output = child
        .wait_with_output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if output.status.success() && matches!(written, Some(Ok(()))) {
        Ok(output)
    } else {
        Err(RepositoryError::GitOperationFailed)
    }
}

/// Stops Git's repository discovery at the parent of a repository root so a
/// corrupt root `.git` cannot fall through to an enclosing repository. Only a
/// directory that carries its own `.git` entry is a root; a plain subdirectory
/// of a worktree (for example `.orna/` or `nested/`) must still discover the
/// enclosing repository, so it is left unbound. The path is canonicalized first
/// so a symlinked root binds to the directory it really resolves to; a relative
/// path cannot yield an empty or relative ceiling.
pub(crate) fn bind_git_discovery_to_worktree(command: &mut Command, worktree: &std::path::Path) {
    let resolved = canonical_or_absolute(worktree);
    if resolved.join(".git").exists() {
        bind_git_ceiling_above(command, &resolved);
    }
}

/// Stops Git's discovery at the parent of `directory` unconditionally. Use this
/// where the caller requires `directory` itself to be a repository root: a plain
/// subdirectory then yields no repository instead of its enclosing one.
pub(crate) fn bind_git_ceiling_above(command: &mut Command, directory: &std::path::Path) {
    let resolved = canonical_or_absolute(directory);
    if let Some(parent) = resolved.parent() {
        command.env("GIT_CEILING_DIRECTORIES", parent);
    }
}

pub(crate) fn canonical_or_absolute(path: &std::path::Path) -> std::path::PathBuf {
    path.canonicalize()
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
thread_local! {
    static GIT_PROCESS_REQUESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Git processes this thread has built through [`git_command`] since the last
/// reset. Every production Git process is built there, so a test can bound the
/// processes one operation starts.
#[cfg(test)]
pub(crate) fn git_process_requests() -> usize {
    GIT_PROCESS_REQUESTS.with(std::cell::Cell::get)
}

/// Clears the count read by [`git_process_requests`].
#[cfg(test)]
pub(crate) fn reset_git_process_requests() {
    GIT_PROCESS_REQUESTS.with(|count| count.set(0));
}

/// A Git command with every inherited routing variable removed, so the
/// caller's environment cannot select another repository, worktree or index.
pub(crate) fn git_command() -> Command {
    #[cfg(test)]
    GIT_PROCESS_REQUESTS.with(|count| count.set(count.get() + 1));
    let mut command = Command::new("git");
    scrub_git_routing_environment(&mut command);
    command
}

/// A Git command for work inside `directory`. Discovery stops above a
/// repository root so a corrupt root cannot fall through to an enclosing
/// repository, while a plain subdirectory of a worktree still discovers the
/// enclosing repository it belongs to.
pub(crate) fn git_command_in(directory: &std::path::Path) -> Command {
    let mut command = git_command();
    command.current_dir(directory);
    bind_git_discovery_to_worktree(&mut command, directory);
    command
}

/// A Git command for a directory that must itself be a repository root:
/// discovery stops at its parent unconditionally, so a directory that is not a
/// root can never reach an enclosing repository's refs or objects.
pub(crate) fn git_command_at_root(directory: &std::path::Path) -> Command {
    let mut command = git_command();
    command.current_dir(directory);
    bind_git_ceiling_above(&mut command, directory);
    command
}

/// Prevent inherited Git routing from selecting a repository, worktree, or
/// index other than the explicitly supplied command directory.
pub(crate) fn scrub_git_routing_environment(command: &mut Command) {
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
        "GIT_REPLACE_REF_BASE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_GRAFT_FILE",
        "GIT_SHALLOW_FILE",
    ] {
        command.env_remove(variable);
    }
    for (variable, _) in std::env::vars_os() {
        if variable.as_encoded_bytes().starts_with(b"GIT_CONFIG_KEY_")
            || variable
                .as_encoded_bytes()
                .starts_with(b"GIT_CONFIG_VALUE_")
        {
            command.env_remove(variable);
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConfigValue {
    Missing,
    Value(String),
    Malformed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromisorReachability {
    Proven,
    NotProven,
    Malformed,
}

pub(crate) fn parse_git_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

pub(crate) fn git_stderr_indicates_malformed_object(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    [
        "corrupt",
        "unable to unpack",
        "inflate:",
        "bad object",
        "invalid object",
    ]
    .iter()
    .any(|marker| stderr.contains(marker))
}

pub(crate) fn valid_remote_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        && !name.contains("..")
        && !name.contains('\\')
        && !name.contains("//")
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
}

pub(crate) fn valid_orna_internal_ref(reference: &str) -> bool {
    let Some(suffix) = reference.strip_prefix("refs/orna/") else {
        return false;
    };
    !suffix.is_empty()
        && suffix.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.ends_with('.')
                && !component.ends_with(".lock")
                && !component.bytes().any(|byte| {
                    byte <= b' '
                        || byte == 0x7f
                        || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
                })
        })
        && !reference.contains("..")
        && !reference.contains("@{")
        && !reference.ends_with('@')
}

/// Splits a successful `git ls-remote` listing into its `(object id, ref
/// name)` records. The listing must end in a newline, and every record must
/// carry one tab and `object_id_length` hexadecimal object-id digits. Whether
/// a ref name is expected or well formed stays with each caller, because the
/// advertisements differ in exactly that rule.
pub(crate) fn remote_ref_records(
    output: &str,
    object_id_length: usize,
) -> Option<Vec<(&str, &str)>> {
    if output.is_empty() {
        return Some(Vec::new());
    }
    output
        .strip_suffix('\n')?
        .split('\n')
        .map(|record| {
            let (object_id, reference) = record.split_once('\t')?;
            (object_id.len() == object_id_length
                && object_id.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some((object_id, reference))
        })
        .collect()
}

pub(crate) fn parse_remote_orna_refs(
    output: &str,
    object_id_length: usize,
    expected: &std::collections::BTreeMap<&str, &str>,
) -> Result<std::collections::BTreeMap<String, String>, RepositoryError> {
    let Some(records) = remote_ref_records(output, object_id_length) else {
        return Err(RepositoryError::GitOperationFailed);
    };
    let mut refs = std::collections::BTreeMap::new();
    for (object_id, reference) in records {
        if !valid_orna_internal_ref(reference)
            || !expected.contains_key(reference)
            || refs
                .insert(reference.to_owned(), object_id.to_ascii_lowercase())
                .is_some()
        {
            return Err(RepositoryError::GitOperationFailed);
        }
    }
    Ok(refs)
}

pub(crate) fn parse_committed_tree_entry(
    raw_entry: &[u8],
    object_id_length: usize,
) -> Result<CommittedTreeEntry, RepositoryError> {
    let record = GitTreeRecord::parse(raw_entry).ok_or(RepositoryError::GitOperationFailed)?;
    let (mode, object_kind, object) = (record.mode, record.kind, record.object);
    if object_kind != b"blob" && object_kind != b"commit"
        || object.len() != object_id_length
        || !object.iter().all(u8::is_ascii_hexdigit)
    {
        return Err(RepositoryError::GitOperationFailed);
    }
    let path = std::str::from_utf8(record.path).map_err(|_| RepositoryError::GitOperationFailed)?;
    let path = ManagedPath::new(path)?;
    let kind = match (mode, object_kind) {
        (b"100644", b"blob") => CommittedTreeEntryKind::File { executable: false },
        (b"100755", b"blob") => CommittedTreeEntryKind::File { executable: true },
        (b"120000", b"blob") => CommittedTreeEntryKind::Symlink,
        (b"160000", b"commit") => CommittedTreeEntryKind::Submodule,
        _ => return Err(RepositoryError::GitOperationFailed),
    };
    Ok(CommittedTreeEntry { path, kind })
}

/// How a local branch has diverged from its upstream, as `for-each-ref` reports
/// it in `%(upstream:track)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpstreamTrack {
    /// The upstream ref is missing, or its commit cannot be read: there is
    /// nothing to count against.
    Gone,
    /// The branch has `ahead` commits its upstream lacks and the upstream has
    /// `behind` commits the branch lacks.
    Diverged { ahead: u64, behind: u64 },
}

/// Reads the text Git prints for `%(upstream:track)`: nothing when the branch
/// is level with its upstream, `[gone]`, or `[ahead N]`, `[behind M]` or
/// `[ahead N, behind M]` with each side printed only when it is nonzero. Any
/// other text is not Git's and is refused.
pub(crate) fn parse_upstream_track(track: &str) -> Option<UpstreamTrack> {
    if track.is_empty() {
        return Some(UpstreamTrack::Diverged {
            ahead: 0,
            behind: 0,
        });
    }
    let inner = track.strip_prefix('[')?.strip_suffix(']')?;
    if inner == "gone" {
        return Some(UpstreamTrack::Gone);
    }
    let (mut ahead, mut behind) = (None, None);
    for side in inner.split(", ") {
        let (label, count) = side.split_once(' ')?;
        let slot = match label {
            "ahead" => &mut ahead,
            "behind" => &mut behind,
            _ => return None,
        };
        if slot.is_some() || count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *slot = Some(count.parse::<u64>().ok()?);
    }
    Some(UpstreamTrack::Diverged {
        ahead: ahead.unwrap_or(0),
        behind: behind.unwrap_or(0),
    })
}

/// Parses one record of the `for-each-ref` listing behind
/// [`Repository::local_branches`]: the `HEAD` marker (`*` or a space), the
/// branch name, its commit, its upstream ref and its upstream tracking text,
/// NUL-separated. A record that does not have exactly that shape, or whose
/// upstream fields contradict one another, is refused.
pub(crate) fn parse_committed_branch(
    record: &[u8],
    object_id_length: usize,
) -> Result<CommittedBranch, RepositoryError> {
    let mut fields = record.split(|byte| *byte == 0);
    let (Some(head), Some(name), Some(object_id), Some(upstream), Some(track), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return Err(RepositoryError::GitOperationFailed);
    };
    let current = match head {
        b"*" => true,
        b" " => false,
        _ => return Err(RepositoryError::GitOperationFailed),
    };
    let name = listing_text(name)?;
    if !valid_branch_name(name) {
        return Err(RepositoryError::GitOperationFailed);
    }
    let commit =
        GitCommitRef::from_verified_commit(listing_text(object_id)?.to_owned(), object_id_length)?;
    let track =
        parse_upstream_track(listing_text(track)?).ok_or(RepositoryError::GitOperationFailed)?;
    let upstream = match (listing_text(upstream)?, track) {
        (
            "",
            UpstreamTrack::Diverged {
                ahead: 0,
                behind: 0,
            },
        )
        | (_, UpstreamTrack::Gone) => None,
        ("", UpstreamTrack::Diverged { .. }) => return Err(RepositoryError::GitOperationFailed),
        (reference, UpstreamTrack::Diverged { ahead, behind }) => {
            if !reference.starts_with("refs/") {
                return Err(RepositoryError::GitOperationFailed);
            }
            Some(BranchUpstream {
                reference: reference.to_owned(),
                ahead,
                behind,
            })
        }
    };
    Ok(CommittedBranch {
        name: name.to_owned(),
        commit,
        current,
        upstream,
    })
}

/// One field of a `for-each-ref` listing as text; a listing that holds anything
/// else is not one Git wrote.
fn listing_text(bytes: &[u8]) -> Result<&str, RepositoryError> {
    std::str::from_utf8(bytes).map_err(|_| RepositoryError::GitOperationFailed)
}

pub(crate) fn valid_native_object_id(object_id: &str) -> bool {
    matches!(object_id.len(), 40 | 64) && object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Checks that one committed listing wrote `object_id` as a Git object id:
/// exactly `object_id_length` hexadecimal digits. Object ids are opaque bytes,
/// so nothing about the kind of object they name is claimed here.
pub(crate) fn require_native_object_id(
    object_id: &str,
    object_id_length: usize,
) -> Result<(), RepositoryError> {
    if object_id.len() == object_id_length && object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(RepositoryError::InvalidObjectId)
    }
}

pub(crate) fn valid_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("..")
        && !name.contains("//")
        && !name.bytes().any(|byte| {
            byte <= b' '
                || byte == 0x7f
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
}

#[cfg(test)]
mod branch_listing_tests {
    use super::{
        BranchUpstream, RepositoryError, UpstreamTrack, parse_committed_branch,
        parse_upstream_track,
    };

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn diverged(ahead: u64, behind: u64) -> Option<UpstreamTrack> {
        Some(UpstreamTrack::Diverged { ahead, behind })
    }

    #[test]
    fn upstream_track_reads_exactly_the_text_git_prints() {
        for (text, expected) in [
            ("", diverged(0, 0)),
            ("[gone]", Some(UpstreamTrack::Gone)),
            ("[ahead 3]", diverged(3, 0)),
            ("[behind 2]", diverged(0, 2)),
            ("[ahead 3, behind 2]", diverged(3, 2)),
            ("[ahead 18446744073709551615]", diverged(u64::MAX, 0)),
        ] {
            assert_eq!(parse_upstream_track(text), expected, "{text:?}");
        }
    }

    #[test]
    fn upstream_track_refuses_text_git_does_not_print() {
        for text in [
            "ahead 3",
            "[]",
            "[ahead]",
            "[ahead ]",
            "[ahead +3]",
            "[ahead -1]",
            "[ahead 3,behind 2]",
            "[ahead 3, ahead 4]",
            "[sideways 3]",
            "[gone, ahead 1]",
            "[ahead 18446744073709551616]",
        ] {
            assert_eq!(parse_upstream_track(text), None, "{text:?}");
        }
    }

    fn record(head: &str, name: &str, object_id: &str, upstream: &str, track: &str) -> Vec<u8> {
        [head, name, object_id, upstream, track]
            .join("\0")
            .into_bytes()
    }

    #[test]
    fn a_branch_record_carries_head_commit_and_upstream() {
        let current = parse_committed_branch(&record("*", "main", OID, "", ""), 40)
            .expect("a branch without an upstream");
        assert_eq!(current.name(), "main");
        assert_eq!(current.commit().as_str(), OID);
        assert!(current.is_current());
        assert_eq!(current.upstream(), None);

        let tracking = parse_committed_branch(
            &record(
                " ",
                "feature/x",
                OID,
                "refs/remotes/origin/feature/x",
                "[ahead 3, behind 2]",
            ),
            40,
        )
        .expect("a branch with a diverged upstream");
        assert_eq!(tracking.name(), "feature/x");
        assert!(!tracking.is_current());
        assert_eq!(
            tracking.upstream(),
            Some(&BranchUpstream {
                reference: "refs/remotes/origin/feature/x".to_owned(),
                ahead: 3,
                behind: 2,
            })
        );

        let level = parse_committed_branch(
            &record(" ", "level", OID, "refs/remotes/origin/level", ""),
            40,
        )
        .expect("a branch level with its upstream");
        assert_eq!(
            level.upstream().map(|up| (up.ahead(), up.behind())),
            Some((0, 0))
        );
    }

    #[test]
    fn a_gone_upstream_is_no_upstream() {
        let gone = parse_committed_branch(
            &record("*", "main", OID, "refs/remotes/origin/main", "[gone]"),
            40,
        )
        .expect("a branch whose upstream is gone");
        assert_eq!(gone.upstream(), None);
    }

    #[test]
    fn a_malformed_branch_record_is_refused() {
        let cases: [(&str, Vec<u8>); 7] = [
            ("too few fields", b"*\0main\0".to_vec()),
            (
                "too many fields",
                [record("*", "main", OID, "", ""), b"\0".to_vec()].concat(),
            ),
            ("unknown HEAD marker", record("x", "main", OID, "", "")),
            ("invalid branch name", record(" ", "a..b", OID, "", "")),
            (
                "upstream text without an upstream",
                record(" ", "main", OID, "", "[ahead 1]"),
            ),
            (
                "upstream that is not a ref",
                record(" ", "main", OID, "origin/main", ""),
            ),
            (
                "unreadable tracking text",
                record(" ", "main", OID, "refs/remotes/origin/main", "[ahead]"),
            ),
        ];
        for (what, bytes) in cases {
            assert!(
                matches!(
                    parse_committed_branch(&bytes, 40),
                    Err(RepositoryError::GitOperationFailed)
                ),
                "{what}"
            );
        }
        assert!(matches!(
            parse_committed_branch(&record(" ", "main", "0123", "", ""), 40),
            Err(RepositoryError::InvalidObjectId)
        ));
    }
}
