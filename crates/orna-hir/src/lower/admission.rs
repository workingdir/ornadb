//! Host admission of the portable `sys` surface.
//!
//! Lowering declares the catalogue; whether a host links a provider for a
//! catalogued member is a fact about the host, not about the language. The
//! host supplies that fact through [`SysAdmission`]. A reference to a
//! catalogued member with no linked provider is rejected at lowering with
//! `sys.version.incompatible`, the same code the admission gate reports.

use std::sync::Arc;

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use super::declare::Builder;
use crate::ModuleId;
use crate::diag;

/// What a host knows about one dotted `sys` path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SysMemberStatus {
    /// The path names no catalogued member; lowering keeps its existing treatment.
    Unknown,
    /// The path names a catalogued member that no production provider serves.
    Unlinked,
    /// The path names a catalogued member with a linked provider.
    Linked,
}

/// The host's view of the portable `sys` catalogue.
pub trait SysAdmission: Send + Sync {
    /// The status of `path`, a `sys`-rooted path such as `["sys", "Run", "as_of"]`.
    fn status(&self, path: &[&str]) -> SysMemberStatus;
}

/// The admission every lowering uses unless a host supplies its own: every
/// member is treated as linked, so no diagnostic changes.
struct AllLinked;

impl SysAdmission for AllLinked {
    fn status(&self, _path: &[&str]) -> SysMemberStatus {
        SysMemberStatus::Linked
    }
}

/// The shared admission handle a lowering carries.
pub type SharedAdmission = Arc<dyn SysAdmission>;

/// The admission every lowering uses unless a host supplies its own.
pub(crate) fn all_linked() -> SharedAdmission {
    Arc::new(AllLinked)
}

/// Reports `sys.version.incompatible` at `span` when the host has not linked
/// the `sys` member named by `path`. Returns `true` when it reported.
pub(crate) fn reject_unlinked(
    builder: &mut Builder,
    module: ModuleId,
    path: &[&str],
    span: SyntaxSpan,
) -> bool {
    if builder.admission.status(path) != SysMemberStatus::Unlinked {
        return false;
    }
    builder.diagnose(
        module,
        diag::SYS_VERSION_INCOMPATIBLE,
        format!(
            "`{}` has no linked production provider in this host",
            path.join(".")
        ),
        span,
    );
    true
}

/// The diagnostic for a `sys` path with no represented declaration. The renamed `sys.runtime`
/// is the spec's ORNA100-E-SYS-RUNTIME (ORNA-SYS-005); any other path stays unrepresented.
pub(crate) fn unrepresented_sys(path: &[&str]) -> (&'static str, String) {
    if path.first() == Some(&"sys") && path.get(1) == Some(&"runtime") {
        return (
            diag::LEGACY_SYS_RUNTIME,
            "`sys.runtime` was renamed to `sys.rt`".to_owned(),
        );
    }
    (
        diag::UNSUPPORTED,
        "the `sys` declarations are not represented yet".to_owned(),
    )
}

/// The diagnostic for a `sys` module used as a callee. `sys.storage` is a grouping namespace
/// (ORNA-SYS-120); any other module stays unrepresented as a callee.
pub(crate) fn module_as_function(path: &[&str]) -> (&'static str, String) {
    if path == ["sys", "storage"] {
        return (
            diag::SYS_STORAGE_CALL,
            "`sys.storage` is a grouping namespace; use `sys.Storage` or `sys.admin` storage functions"
                .to_owned(),
        );
    }
    (diag::UNSUPPORTED, "a module is not a function".to_owned())
}
