//! Index types, dense index tables and interned names (design section 2.2).
//!
//! Every nameable thing has a `DefId`; locals, expressions and patterns are
//! slots inside one [`crate::Body`]. Ids are `u32` newtypes over
//! [`IndexVec`], stable within one [`crate::Program`] and meaningless outside
//! it. A malformed id can therefore only be produced by a lowering bug, which
//! is what [`crate::Program::validate`] exists to catch in debug and test
//! builds.

use core::{
    fmt,
    hash::{Hash, Hasher},
    ops::{Index, IndexMut},
    slice,
};
use std::sync::Arc;

use unicode_normalization::UnicodeNormalization;

/// A dense table index that can only be produced by the type it indexes.
pub trait Idx: Copy + Eq + Ord + Hash + fmt::Debug + 'static {
    /// Builds the id for a table position. Only a table or a test may call
    /// this; callers outside this crate use [`IndexVec::push`].
    fn from_index(index: usize) -> Self;

    /// Returns the table position.
    fn index(self) -> usize;
}

macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl $name {
            /// Wraps a raw index. Callers that are not a table or a test have
            /// no reason to construct an id by hand.
            #[must_use]
            pub const fn new(raw: u32) -> Self {
                Self(raw)
            }

            /// Returns the raw index.
            #[must_use]
            pub const fn as_u32(self) -> u32 {
                self.0
            }
        }

        impl Idx for $name {
            fn from_index(index: usize) -> Self {
                Self(u32::try_from(index).expect("HIR table holds at most u32::MAX entries"))
            }

            fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!(stringify!($name), "({})"), self.0)
            }
        }
    };
}

define_id! {
    /// One module of the program.
    ModuleId
}
define_id! {
    /// One declaration, global across modules.
    DefId
}
define_id! {
    /// One function body, lambda body, initializer or assertion body.
    BodyId
}
define_id! {
    /// One stack slot inside one body.
    LocalId
}
define_id! {
    /// One expression inside one body.
    ExprId
}
define_id! {
    /// One pattern inside one body.
    PatId
}
define_id! {
    /// One source text, referenced by spans for diagnostics and editor queries.
    SourceId
}

/// A `Vec<T>` that can only be indexed by `I`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexVec<I: Idx, T> {
    entries: Vec<T>,
    marker: core::marker::PhantomData<fn(I) -> I>,
}

impl<I: Idx, T> IndexVec<I, T> {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            marker: core::marker::PhantomData,
        }
    }

    /// An empty table with room for `capacity` entries.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            marker: core::marker::PhantomData,
        }
    }

    /// Appends an entry and returns its id.
    pub fn push(&mut self, entry: T) -> I {
        let id = I::from_index(self.entries.len());
        self.entries.push(entry);
        id
    }

    /// Returns the entry for `id`, or `None` when `id` is not in this table.
    #[must_use]
    pub fn get(&self, id: I) -> Option<&T> {
        self.entries.get(id.index())
    }

    /// Returns a mutable entry for `id`, or `None` when `id` is out of range.
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        self.entries.get_mut(id.index())
    }

    /// Whether `id` addresses an entry of this table.
    #[must_use]
    pub fn contains(&self, id: I) -> bool {
        id.index() < self.entries.len()
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The id the next entry pushed will receive.
    #[must_use]
    pub fn next_index(&self) -> Option<I> {
        (self.entries.len() < u32::MAX as usize).then(|| I::from_index(self.entries.len()))
    }

    /// The id of the last entry, when the table is not empty.
    #[must_use]
    pub fn last_index(&self) -> Option<I> {
        (!self.entries.is_empty()).then(|| I::from_index(self.entries.len() - 1))
    }

    /// Entries in table order.
    pub fn iter(&self) -> slice::Iter<'_, T> {
        self.entries.iter()
    }

    /// Entries with their ids, in table order.
    pub fn iter_enumerated(&self) -> impl Iterator<Item = (I, &T)> + '_ {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (I::from_index(index), entry))
    }

    /// Ids of every entry, in table order.
    pub fn indices(&self) -> impl Iterator<Item = I> + '_ {
        (0..self.entries.len()).map(I::from_index)
    }

    /// The entries as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.entries
    }

    /// Consumes the table and returns its entries.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.entries
    }
}

impl<I: Idx, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Idx, T> Index<I> for IndexVec<I, T> {
    type Output = T;

    fn index(&self, id: I) -> &T {
        &self.entries[id.index()]
    }
}

impl<I: Idx, T> IndexMut<I> for IndexVec<I, T> {
    fn index_mut(&mut self, id: I) -> &mut T {
        &mut self.entries[id.index()]
    }
}

impl<'a, I: Idx, T> IntoIterator for &'a IndexVec<I, T> {
    type Item = &'a T;
    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl<I: Idx, T> FromIterator<T> for IndexVec<I, T> {
    fn from_iter<Source: IntoIterator<Item = T>>(entries: Source) -> Self {
        let entries = entries.into_iter().collect();
        Self {
            entries,
            marker: core::marker::PhantomData,
        }
    }
}

/// An NFC-normalised identifier (ORNA-LEX-005).
///
/// Equality, ordering and hashing are the normalised spelling's, so a program
/// that writes a name in decomposed form resolves to the same definition as
/// one that writes it composed.
#[derive(Clone)]
pub struct Name(Arc<str>);

impl Name {
    /// Normalises `text` to NFC and stores it once behind a shared arc.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let normalized = text.nfc().collect::<String>();
        Self(Arc::from(normalized))
    }

    /// The normalised spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Name {}

impl PartialOrd for Name {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Name {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl Hash for Name {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Name {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Name {
    fn from(text: String) -> Self {
        Self::new(&text)
    }
}

/// The module path of a definition, for example `std.bits`.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModulePath(Box<[Name]>);

impl ModulePath {
    /// Builds a path from its segments.
    #[must_use]
    pub fn new(segments: impl IntoIterator<Item = Name>) -> Self {
        Self(segments.into_iter().collect())
    }

    /// The path segments.
    #[must_use]
    pub fn segments(&self) -> &[Name] {
        &self.0
    }

    /// Whether the path has no segments, which only an unparsed source can
    /// produce.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ModulePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str(".")?;
            }
            formatter.write_str(segment.as_str())?;
        }
        Ok(())
    }
}

impl FromIterator<Name> for ModulePath {
    fn from_iter<Source: IntoIterator<Item = Name>>(segments: Source) -> Self {
        Self::new(segments)
    }
}

#[cfg(test)]
mod tests {
    use super::{IndexVec, ModuleId, Name};

    #[test]
    fn names_compare_by_their_normalised_spelling() {
        // U+00E9 LATIN SMALL LETTER E WITH ACUTE composed, then decomposed.
        let composed = Name::new("caf\u{e9}");
        let decomposed = Name::new("cafe\u{301}");
        assert_eq!(composed, decomposed);
        assert_eq!(composed.as_str(), "caf\u{e9}");
    }

    #[test]
    fn index_vectors_return_the_id_of_the_entry_they_hold() {
        let mut modules = IndexVec::<ModuleId, &'static str>::new();
        let first = modules.push("main");
        let second = modules.push("std");
        assert_ne!(first, second);
        assert_eq!(modules[first], "main");
        assert_eq!(modules.get(second), Some(&"std"));
        assert!(!modules.contains(ModuleId::new(9)));
        assert_eq!(modules.len(), 2);
    }
}
