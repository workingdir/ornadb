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
