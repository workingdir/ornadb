//! Removed `sys` names and the closed replacement identities they must suggest.
//!
//! `api/sys.json` `removed_names` is heterogeneous: an entry either names a
//! replacement member and the diagnostic that must be raised, or retires the name
//! with a user-visible reason. Those are different vocabularies (a source rule code
//! versus a runtime failure code), so [`DiagnosticIdentity`] keeps them apart.
//!
//! ORNA-SYS-005 requires `sys.runtime` to report `ORNA100-E-SYS-RUNTIME` and suggest
//! `sys.rt`. The suggestion is a [`SysMemberPath`], not a string, so the diagnostic
//! and the replacement cannot drift apart.

use core::fmt;

use crate::{
    SysFailureCode, SysField, SysFunction, SysGroupedHandle, SysRelation, SysSingleton,
    UnknownSysMember,
};

/// A closed ORNA-family rule code.
///
/// Seeded here from the diagnostics the catalogue itself names. The full
/// `source/33-diagnostics.md` table is a separate follow-up, so this type stays small
/// rather than pretending to be complete.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuleCode(&'static str);

impl RuleCode {
    /// Wraps one rule spelling; generated constants are the only intended callers.
    #[must_use]
    pub const fn new(text: &'static str) -> Self {
        Self(text)
    }

    /// The exact rule spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for RuleCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

/// The identity a removed name reports: a source rule or a runtime failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticIdentity {
    /// An `ORNA*` source diagnostic rule.
    Rule(RuleCode),
    /// A closed `sys.*` failure code.
    Failure(SysFailureCode),
}

impl DiagnosticIdentity {
    /// The exact spelling the diagnostic must carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DiagnosticIdentity::Rule(rule) => rule.as_str(),
            DiagnosticIdentity::Failure(code) => code.as_str(),
        }
    }
}

impl fmt::Display for DiagnosticIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The closed path of a member a removed name can be replaced by.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SysMemberPath {
    /// A catalogue callable.
    Callable(SysFunction),
    /// A canonical relation.
    Relation(SysRelation),
    /// A grouped catalogue or history handle.
    GroupedHandle(SysGroupedHandle),
    /// A singleton record.
    Singleton(SysSingleton),
    /// One field of a singleton's declared view record.
    SingletonField(SysSingleton, SysField),
}

impl SysMemberPath {
    /// Builds a singleton-field path only when the view record declares the field.
    ///
    /// This is the build-time check of section 4.4 expressed at run time: a field the
    /// catalogue does not declare cannot become a suggestion.
    #[must_use]
    pub fn singleton_field(singleton: SysSingleton, field: SysField) -> Option<Self> {
        singleton
            .fields()
            .contains(&field)
            .then_some(SysMemberPath::SingletonField(singleton, field))
    }
}

impl fmt::Display for SysMemberPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SysMemberPath::Callable(function) => formatter.write_str(function.label()),
            SysMemberPath::Relation(relation) => formatter.write_str(relation.name()),
            SysMemberPath::GroupedHandle(handle) => formatter.write_str(handle.name()),
            SysMemberPath::Singleton(singleton) => formatter.write_str(singleton.name()),
            SysMemberPath::SingletonField(singleton, field) => {
                write!(formatter, "{}.{}", singleton.name(), field.name())
            }
        }
    }
}

/// What the catalogue says about one removed name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortableRemoval {
    /// Still resolvable through another portable member; never an alias.
    Replaced {
        /// The closed replacement path.
        by: SysMemberPath,
        /// The diagnostic the removed spelling must raise.
        diagnostic: DiagnosticIdentity,
    },
    /// Retired with no replacement; the reason is user-visible.
    Retired {
        /// The diagnostic the removed spelling must raise.
        diagnostic: DiagnosticIdentity,
        /// The catalogue's user-visible reason.
        reason: &'static str,
    },
}

impl PortableRemoval {
    /// The diagnostic every use of the removed name must raise.
    #[must_use]
    pub const fn diagnostic(self) -> DiagnosticIdentity {
        match self {
            PortableRemoval::Replaced { diagnostic, .. }
            | PortableRemoval::Retired { diagnostic, .. } => diagnostic,
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/removed.rs"));
