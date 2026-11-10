//! Resolved patterns (design section 2.4).
//!
//! Patterns carry resolved constructors: a variant or nominal constructor is a
//! [`DefId`] and its field patterns name field definitions, not strings.
//! Exhaustiveness and constructor-arity checks run once, over these patterns,
//! in `orna-check`; no executor re-validates a pattern's shape.

use crate::{DefId, Lit, LocalId, Name, PatId};

/// One pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pat {
    /// `_`.
    Wild,
    /// A binding.
    Bind {
        /// The slot the value is bound to.
        local: LocalId,
    },
    /// A literal pattern.
    Lit(Lit),
    /// A tuple pattern.
    Tuple(Box<[PatId]>),
    /// A list pattern with an optional rest binding.
    List {
        /// Patterns before the rest binding.
        prefix: Box<[PatId]>,
        /// The rest binding, when written.
        rest: Option<LocalId>,
        /// Patterns after the rest binding.
        suffix: Box<[PatId]>,
    },
    /// A structural record pattern, fields sorted by name.
    Record(Box<[(Name, PatId)]>),
    /// An enum variant or nominal constructor, resolved to a definition.
    Ctor {
        /// The constructor definition.
        def: DefId,
        /// Field patterns, each naming a field definition.
        fields: Box<[(DefId, PatId)]>,
    },
}
