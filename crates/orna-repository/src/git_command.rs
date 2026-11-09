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

/// A Git command with every inherited routing variable removed, so the
/// caller's environment cannot select another repository, worktree or index.
pub(crate) fn git_command() -> Command {
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
