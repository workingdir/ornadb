//! The host effect vocabulary: the closed `sys.EffectKind` enum of the Orna
//! 1.1.0 system API (`api/sys.json`).
//!
//! Every host-bound callee the semantic layer classifies maps to exactly one
//! kind through [`host_effect`]; table access maps to the table kinds at its
//! table-operation sites. Effect names are the spec spellings, so diagnostics
//! and the controlled-activation admission share one vocabulary.

/// One host effect kind, in `sys.EffectKind` declaration order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EffectKind {
    TableRead,
    TableWrite,
    Network,
    Filesystem,
    Process,
    Clock,
    Randomness,
    SecretAccess,
    Ui,
    Git,
    StorageAdmin,
    RuntimeAdmin,
}

impl EffectKind {
    /// Every `sys.EffectKind` value, in declaration order.
    pub const ALL: [EffectKind; 12] = [
        EffectKind::TableRead,
        EffectKind::TableWrite,
        EffectKind::Network,
        EffectKind::Filesystem,
        EffectKind::Process,
        EffectKind::Clock,
        EffectKind::Randomness,
        EffectKind::SecretAccess,
        EffectKind::Ui,
        EffectKind::Git,
        EffectKind::StorageAdmin,
        EffectKind::RuntimeAdmin,
    ];

    /// The spec spelling of this kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            EffectKind::TableRead => "table_read",
            EffectKind::TableWrite => "table_write",
            EffectKind::Network => "network",
            EffectKind::Filesystem => "filesystem",
            EffectKind::Process => "process",
            EffectKind::Clock => "clock",
            EffectKind::Randomness => "randomness",
            EffectKind::SecretAccess => "secret_access",
            EffectKind::Ui => "ui",
            EffectKind::Git => "git",
            EffectKind::StorageAdmin => "storage_admin",
            EffectKind::RuntimeAdmin => "runtime_admin",
        }
    }
}

/// The host effect of a callee by its full path. Paths that touch no host
/// resource, including `sys` reads and the in-memory blob writer, have none.
pub fn host_effect(path: &[&str]) -> Option<EffectKind> {
    use EffectKind::{Clock, Filesystem, Git, Network, Process, Randomness, RuntimeAdmin};
    use EffectKind::{SecretAccess, StorageAdmin, Ui};
    Some(match path {
        ["std", "net", ..] => Network,
        ["std", "io", "fs", ..] => Filesystem,
        ["std", "io", "process" | "environment", ..] => Process,
        ["std", "concurrent", "sleep"] => Clock,
        ["std", "random", ..] => Randomness,
        ["std", "secret", "open"] => SecretAccess,
        ["std", "ui", ..] => Ui,
        ["sys", "io", ..] => Filesystem,
        ["sys", "net", ..] => Network,
        ["sys", "process", ..] => Process,
        ["sys", "ui", ..] => Ui,
        ["sys", "clock", ..] => Clock,
        ["sys", "random", ..] => Randomness,
        ["sys", "blob", "capture_file"] => Filesystem,
        ["sys", "admin", "commit" | "commit_reviewed" | "checkout" | "create_branch"] => Git,
        ["sys", "admin", "flush" | "compact" | "verify"] => StorageAdmin,
        [
            "sys",
            "admin",
            "cancel_run"
            | "pause_stream"
            | "resume_stream"
            | "reset_checkpoint"
            | "retry_failure"
            | "skip_failure"
            | "replay_failure"
            | "resolve_failure",
        ] => RuntimeAdmin,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{EffectKind, host_effect};
    use std::collections::BTreeSet;

    /// Known answer: the spec `sys.EffectKind` values, in spec order.
    #[test]
    fn vocabulary_is_the_sys_effect_kind_enum() {
        let names: Vec<&str> = EffectKind::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(
            names,
            [
                "table_read",
                "table_write",
                "network",
                "filesystem",
                "process",
                "clock",
                "randomness",
                "secret_access",
                "ui",
                "git",
                "storage_admin",
                "runtime_admin",
            ]
        );
    }

    /// Known answer: every host-bound operation classified by the spec maps to
    /// one kind, and together the host classes cover every non-table kind.
    #[test]
    fn host_classification_covers_every_host_kind() {
        let cases: [(&[&str], EffectKind); 26] = [
            (&["std", "net", "http", "get"], EffectKind::Network),
            (&["std", "io", "fs", "read"], EffectKind::Filesystem),
            (&["std", "io", "process", "run"], EffectKind::Process),
            (&["std", "io", "environment", "get"], EffectKind::Process),
            (&["std", "concurrent", "sleep"], EffectKind::Clock),
            (&["std", "random", "dice"], EffectKind::Randomness),
            (&["std", "secret", "open"], EffectKind::SecretAccess),
            (&["std", "ui", "text"], EffectKind::Ui),
            (&["sys", "io", "read"], EffectKind::Filesystem),
            (&["sys", "net", "get"], EffectKind::Network),
            (&["sys", "process", "run"], EffectKind::Process),
            (&["sys", "ui", "text"], EffectKind::Ui),
            (&["sys", "clock", "now"], EffectKind::Clock),
            (&["sys", "random", "int"], EffectKind::Randomness),
            (&["sys", "blob", "capture_file"], EffectKind::Filesystem),
            (&["sys", "admin", "commit"], EffectKind::Git),
            (&["sys", "admin", "commit_reviewed"], EffectKind::Git),
            (&["sys", "admin", "checkout"], EffectKind::Git),
            (&["sys", "admin", "create_branch"], EffectKind::Git),
            (&["sys", "admin", "flush"], EffectKind::StorageAdmin),
            (&["sys", "admin", "compact"], EffectKind::StorageAdmin),
            (&["sys", "admin", "verify"], EffectKind::StorageAdmin),
            (&["sys", "admin", "cancel_run"], EffectKind::RuntimeAdmin),
            (&["sys", "admin", "pause_stream"], EffectKind::RuntimeAdmin),
            (&["sys", "admin", "reset_checkpoint"], EffectKind::RuntimeAdmin),
            (&["sys", "admin", "resolve_failure"], EffectKind::RuntimeAdmin),
        ];
        let mut covered = BTreeSet::new();
        for (path, kind) in cases {
            assert_eq!(host_effect(path), Some(kind), "{path:?}");
            covered.insert(kind);
        }
        let table_kinds = BTreeSet::from([EffectKind::TableRead, EffectKind::TableWrite]);
        let host_kinds: BTreeSet<EffectKind> = EffectKind::ALL
            .into_iter()
            .filter(|kind| !table_kinds.contains(kind))
            .collect();
        assert_eq!(covered, host_kinds);
    }

    /// Pure `sys` and `std` operations, including read-class admin plans and
    /// the in-memory blob writer, touch no host resource.
    #[test]
    fn pure_operations_have_no_host_effect() {
        let paths: [&[&str]; 8] = [
            &["sys", "admin", "plan_checkout"],
            &["sys", "admin", "review_commit"],
            &["sys", "blob", "begin"],
            &["sys", "blob", "append"],
            &["sys", "blob", "read"],
            &["sys", "git", "commits"],
            &["std", "secret", "ref"],
            &["user", "fetch"],
        ];
        for path in paths {
            assert_eq!(host_effect(path), None, "{path:?}");
        }
    }
}
