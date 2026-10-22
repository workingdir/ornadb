//! Host facts threaded through every `orna run` path to the `sys` singletons.
//!
//! The carrier is plain data: this crate has no value-model dependency, so the
//! evaluator builds `sys.RuntimeInfo` and the singleton records from it. Every
//! field is a fact the host observed; none is defaulted or fabricated here.

use std::path::PathBuf;

use uuid::Uuid;

/// The values of `sys.RuntimeMode` (34-system-reference.md, `sys.RuntimeMode`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    Repl,
    Run,
    Serve,
    CommandOwner,
    Embedded,
}

impl RuntimeMode {
    /// The portable label of this mode.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Repl => "repl",
            Self::Run => "run",
            Self::Serve => "serve",
            Self::CommandOwner => "command_owner",
            Self::Embedded => "embedded",
        }
    }
}

/// `language_version`, generated from the sys API declaration at build time.
pub const LANGUAGE_VERSION: &str = env!("ORNA_SYS_LANGUAGE_VERSION");
/// `sys_version`, generated from the sys API declaration at build time.
pub const SYS_VERSION: &str = env!("ORNA_SYS_SYS_VERSION");
/// `build_id`: the git commit of this build, or `<version>+nogit` when the
/// package carries no git metadata. Emitted by `build.rs`.
pub const BUILD_ID: &str = env!("ORNA_SYS_BUILD_ID");
/// `implementation_name` of the running host.
pub const IMPLEMENTATION_NAME: &str = "orna";
/// `implementation_version` of the running host: the package version of this
/// sys crate, the release line the host links.
pub const IMPLEMENTATION_VERSION: &str = env!("CARGO_PKG_VERSION");
/// `presentation_protocol_version`, the value the sys API compatibility test pins.
pub const PRESENTATION_PROTOCOL_VERSION: &str = "1";
/// `supported_profiles`. No release coordinate declares which conformance
/// profiles this build satisfies, so none is claimed.
pub const SUPPORTED_PROFILES: &[&str] = &[];

/// Mints the `sys.RuntimeId` of one activation.
///
/// SYS-027 requires the id to change after an owner restart invalidates
/// handles, so it is a fresh UUIDv4 per invocation rather than a function of
/// the repository identity.
#[must_use]
pub fn new_runtime_id() -> [u8; 16] {
    *Uuid::new_v4().as_bytes()
}

/// The standard library pin of the attached project, the source of
/// `sys.RuntimeInfo.std_snapshot`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StdSnapshotPin {
    /// The project pins no standard profile, so the field is `none`.
    Unpinned,
    /// The raw Git object id of the pinned standard commit: 20 bytes for
    /// SHA-1 or 32 bytes for SHA-256.
    Commit(Vec<u8>),
    /// A standard profile is pinned but its commit id cannot be read back.
    /// The evaluator reports `sys.abi.provider_failed` rather than guessing.
    Unreadable,
}

/// The attached repository identity and the activation facts that
/// `sys.rt.info()` and the other `sys` singletons expose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysHostContext {
    /// `RuntimeIdentity::database_id` of the attached runtime store.
    pub database_id: [u8; 16],
    /// `RuntimeIdentity::repository_id` of the attached runtime store.
    pub repository_id: [u8; 16],
    /// The per-activation runtime id, see [`new_runtime_id`].
    pub runtime_id: [u8; 16],
    /// Absolute repository root as the host resolved it.
    pub root: PathBuf,
    /// Whether the activation refuses repository writes (`sys.RuntimeInfo.read_only`).
    pub read_only: bool,
    /// The activation's `sys.RuntimeMode`.
    pub mode: RuntimeMode,
    /// `repository_layout_version` of the attached repository format.
    pub repository_layout_version: String,
    /// `storage_manifest_version` of the attached repository's storage profile.
    pub storage_manifest_version: String,
    /// The standard library pin of the attached project; see [`StdSnapshotPin`].
    pub std_snapshot: StdSnapshotPin,
}

impl SysHostContext {
    /// Whether this activation is a REPL host; only that host provides `sys.repl`.
    #[must_use]
    pub const fn is_repl(&self) -> bool {
        matches!(self.mode, RuntimeMode::Repl)
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeMode, StdSnapshotPin, SysHostContext, new_runtime_id};
    use std::path::PathBuf;
    use uuid::Uuid;

    #[test]
    fn runtime_mode_labels_match_the_portable_enumeration() {
        let labels = [
            RuntimeMode::Repl,
            RuntimeMode::Run,
            RuntimeMode::Serve,
            RuntimeMode::CommandOwner,
            RuntimeMode::Embedded,
        ]
        .map(RuntimeMode::label);
        assert_eq!(labels, ["repl", "run", "serve", "command_owner", "embedded"]);
    }

    #[test]
    fn activation_ids_are_fresh_uuid_v4() {
        let first = new_runtime_id();
        let second = new_runtime_id();
        assert_ne!(first, second);
        assert_eq!(Uuid::from_bytes(first).get_version_num(), 4);
    }

    fn context_in(mode: RuntimeMode) -> SysHostContext {
        SysHostContext {
            database_id: [1; 16],
            repository_id: [2; 16],
            runtime_id: [0x11; 16],
            root: PathBuf::from("/repo"),
            read_only: false,
            mode,
            repository_layout_version: "3".to_owned(),
            storage_manifest_version: "store-3".to_owned(),
            std_snapshot: StdSnapshotPin::Unpinned,
        }
    }

    #[test]
    fn repl_flag_is_set_only_for_the_repl_mode() {
        assert!(context_in(RuntimeMode::Repl).is_repl());
        for mode in [
            RuntimeMode::Run,
            RuntimeMode::Serve,
            RuntimeMode::CommandOwner,
            RuntimeMode::Embedded,
        ] {
            assert!(!context_in(mode).is_repl());
        }
    }
}
