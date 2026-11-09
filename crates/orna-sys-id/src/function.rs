//! Generated callable identity and name-only lookup.

include!(concat!(env!("OUT_DIR"), "/functions.rs"));

/// Why a name-only callable lookup did not resolve to exactly one identity.
///
/// Overloads are separate catalogue identities (ORNA-SYS-136), so `sys.snapshot`
/// alone is ambiguous. Callers that have only a path must handle this explicitly
/// instead of picking one overload by accident.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmbiguousPath<'a> {
    /// No catalogue callable has this path.
    Unknown(&'a str),
    /// More than one catalogue callable has this path.
    Overloaded {
        /// The requested path.
        path: &'a str,
        /// The distinct identities that share it, in catalogue order.
        candidates: &'static [SysFunction],
    },
}

impl fmt::Display for AmbiguousPath<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AmbiguousPath::Unknown(path) => write!(formatter, "unknown sys callable path `{path}`"),
            AmbiguousPath::Overloaded { path, candidates } => {
                write!(
                    formatter,
                    "sys callable path `{path}` has {} overloads",
                    candidates.len()
                )
            }
        }
    }
}

impl std::error::Error for AmbiguousPath<'_> {}
