//! What the REPL tests of the `sys.git.*` members share: a REPL over a committed
//! project whose repository a test shapes first, and what that repository
//! records (the database and the commit every reference is pinned to).

use super::*;
use crate::format3::format3_repository_for_table;
use orna_evaluator::ReplValue;
use orna_repository::Repository;
use orna_value::{
    CanonicalSnapshot, GitHash, GitObjectId, OvbRaw, SysRef, branch_reference, commit_reference,
    git_object_reference, git_ref_reference, remote_reference, stash_reference, tag_reference,
};
use tempfile::TempDir;

/// The identity every test commit is written under; no ambient Git identity is
/// assumed.
pub(super) const IDENTITY: [&str; 4] = [
    "-c",
    "user.name=orna-test",
    "-c",
    "user.email=orna-test@example.invalid",
];

pub(super) fn oid_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("a hexadecimal object id"))
        .collect()
}

fn algorithm(bytes: &[u8]) -> GitHash {
    if bytes.len() == 20 {
        GitHash::Sha1
    } else {
        GitHash::Sha256
    }
}

fn object_id(hex: &str) -> GitObjectId {
    let bytes = oid_bytes(hex);
    GitObjectId::new(algorithm(&bytes), bytes).expect("an object id")
}

/// What the repository records: the database and the commit every reference is
/// pinned to.
pub(super) struct Recorded {
    database: [u8; 16],
    pin: CanonicalSnapshot,
}

impl Recorded {
    /// Reads the expectations from the repository, never from the REPL.
    pub(super) fn read(repository: &Repository) -> Self {
        let head = repository
            .head()
            .expect("HEAD is readable")
            .expect("a committed HEAD");
        let database = *repository
            .open_pinned_format_context(head.as_str())
            .expect("a format context at HEAD")
            .require_database_id()
            .expect("the store has a database identity")
            .as_bytes();
        let oid = oid_bytes(head.as_str());
        Self {
            database,
            pin: CanonicalSnapshot::Commit {
                database,
                algorithm: algorithm(&oid),
                oid,
            },
        }
    }

    fn raw(reference: &SysRef) -> OvbRaw {
        reference
            .to_canonical()
            .expect("a reference encodes")
            .raw()
            .clone()
    }

    /// The `sys.BranchRef` of the branch `name`.
    pub(super) fn branch(&self, name: &str) -> OvbRaw {
        let reference =
            branch_reference(self.database, self.pin.clone(), name).expect("a branch reference");
        Self::raw(&reference)
    }

    /// The `sys.CommitRef` of the commit `hex`.
    pub(super) fn commit(&self, hex: &str) -> OvbRaw {
        let reference = commit_reference(self.database, self.pin.clone(), object_id(hex))
            .expect("a commit reference");
        Self::raw(&reference)
    }

    /// The `sys.TagRef` of the tag `name`.
    pub(super) fn tag(&self, name: &str) -> OvbRaw {
        let reference =
            tag_reference(self.database, self.pin.clone(), name).expect("a tag reference");
        Self::raw(&reference)
    }

    /// The `sys.GitObjectRef` of the object `hex`.
    pub(super) fn object(&self, hex: &str) -> OvbRaw {
        let reference = git_object_reference(self.database, self.pin.clone(), object_id(hex))
            .expect("an object reference");
        Self::raw(&reference)
    }

    /// The `sys.RemoteRef` of the remote `name`.
    pub(super) fn remote(&self, name: &str) -> OvbRaw {
        let reference =
            remote_reference(self.database, self.pin.clone(), name).expect("a remote reference");
        Self::raw(&reference)
    }

    /// The `sys.RefRef` of the ref named in full (`refs/heads/main`).
    pub(super) fn git_ref(&self, name: &str) -> OvbRaw {
        let reference =
            git_ref_reference(self.database, self.pin.clone(), name).expect("a ref reference");
        Self::raw(&reference)
    }

    /// The `sys.StashRef` of the stash entry `index`.
    pub(super) fn stash(&self, index: u64) -> OvbRaw {
        let reference =
            stash_reference(self.database, self.pin.clone(), index).expect("a stash reference");
        Self::raw(&reference)
    }
}

/// A REPL session over a committed project, with the transactions it runs
/// writing inputs through.
pub(super) struct Repl {
    _directory: TempDir,
    repository: Option<Repository>,
    session: AdmittedReplSession,
    pub(super) transactions: ReplTransactions,
}

impl Repl {
    /// Opens a REPL over a committed project of `source` (one table, `Music`),
    /// after `prepare` has shaped the repository.
    pub(super) fn open(source: &str, prepare: impl FnOnce(&Repository)) -> Self {
        let (directory, repository, _relation) = format3_repository_for_table(source, "Music");
        prepare(&repository);
        let ReplRuntime {
            repository,
            session,
            transactions,
        } = repl_runtime(Some(repository)).expect("repl runtime");
        Self {
            _directory: directory,
            repository,
            session,
            transactions: transactions.expect("repl transactions"),
        }
    }

    pub(super) fn repository(&self) -> &Repository {
        self.repository.as_ref().expect("a bound repository")
    }

    pub(super) fn submit(&mut self, source: &str) -> ReplValue {
        run_repl_input(
            self.repository.as_ref(),
            Some(&mut self.transactions),
            &mut self.session,
            source,
        )
        .unwrap_or_else(|failure| panic!("{source}: {}", failure.code))
        .value
        .unwrap_or_else(|| panic!("{source} has a value"))
    }

    pub(super) fn rows(&mut self, source: &str) -> Vec<orna_value::CanonicalValue> {
        let ReplValue::Relation(window) = self.submit(source) else {
            panic!("{source} is a relation");
        };
        window.rows
    }

    pub(super) fn value(&mut self, source: &str) -> OvbRaw {
        let ReplValue::Value(value) = self.submit(source) else {
            panic!("{source} is one value");
        };
        value.raw().clone()
    }
}

/// The raw field `name` of a row.
pub(super) fn field<'a>(row: &'a orna_value::CanonicalValue, name: &str) -> &'a OvbRaw {
    raw_field(row.raw(), name)
}

/// The field `name` of a raw record, a row or a record a row holds.
pub(super) fn raw_field<'a>(record: &'a OvbRaw, name: &str) -> &'a OvbRaw {
    let OvbRaw::Map(entries) = record else {
        panic!("a record is a map: {record:?}");
    };
    entries
        .iter()
        .find_map(|(key, value)| matches!(key, OvbRaw::Text(text) if text == name).then_some(value))
        .unwrap_or_else(|| panic!("the record has no `{name}` field"))
}

/// The payload of an optional raw value, or `None` for an absent one.
pub(super) fn present(option: &OvbRaw) -> Option<&OvbRaw> {
    let OvbRaw::Tag(_, inner) = option else {
        panic!("an optional value is a tagged option");
    };
    let OvbRaw::Array(parts) = inner.as_ref() else {
        panic!("an option holds its presence and payload");
    };
    match parts.as_slice() {
        [absent] if *absent == OvbRaw::Int(0.into()) => None,
        [tag, payload] if *tag == OvbRaw::Int(1.into()) => Some(payload),
        other => panic!("not an option: {other:?}"),
    }
}

/// The text of each row of a relation of text.
pub(super) fn texts(rows: &[orna_value::CanonicalValue]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.raw() {
            OvbRaw::Text(text) => text.clone(),
            other => panic!("a row is text, not {other:?}"),
        })
        .collect()
}
