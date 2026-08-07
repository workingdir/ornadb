//! Authoring source submitted to the compiler.
//!
//! Source units are UTF-8 text. They are compiler inputs only. They do not
//! identify an active revision or represent durable runtime authority.

use std::{collections::HashMap, error::Error, fmt};

/// One UTF-8 source unit submitted to the compiler.
///
/// The logical path labels this source within its submitted bundle. It is not
/// a filesystem path and this type does not access the filesystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceUnit {
    logical_path: String,
    content: String,
}

impl SourceUnit {
    /// Creates a source unit from its logical path and exact UTF-8 content.
    pub fn new(logical_path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            logical_path: logical_path.into(),
            content: content.into(),
        }
    }

    /// Returns the unit's logical path exactly as submitted.
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// Returns the unit's exact UTF-8 content.
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// An ordered set of source units submitted to compiler operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceBundle {
    units: Vec<SourceUnit>,
}

impl SourceBundle {
    /// Validates and creates an ordered source bundle.
    ///
    /// Each unit must have a nonempty logical path. Logical paths compare
    /// exactly and must be unique within this bundle.
    pub fn new(units: impl IntoIterator<Item = SourceUnit>) -> Result<Self, SourceBundleError> {
        let units = units.into_iter().collect::<Vec<_>>();
        let mut paths = HashMap::with_capacity(units.len());

        for (index, unit) in units.iter().enumerate() {
            let logical_path = unit.logical_path();
            if logical_path.is_empty() {
                return Err(SourceBundleError::EmptyLogicalPath { index });
            }

            if let Some(first_index) = paths.insert(logical_path, index) {
                return Err(SourceBundleError::DuplicateLogicalPath {
                    logical_path: logical_path.to_owned(),
                    first_index,
                    duplicate_index: index,
                });
            }
        }

        Ok(Self { units })
    }

    /// Returns source units in their submitted order.
    pub fn units(&self) -> &[SourceUnit] {
        &self.units
    }

    /// Reports whether the bundle has no source units.
    pub const fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Returns the number of source units in the bundle.
    pub const fn len(&self) -> usize {
        self.units.len()
    }
}

/// An error returned when source units cannot form a source bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceBundleError {
    /// A source unit has no logical path.
    EmptyLogicalPath {
        /// The zero-based position of the invalid source unit.
        index: usize,
    },
    /// Two source units use the same logical path.
    DuplicateLogicalPath {
        /// The repeated logical path.
        logical_path: String,
        /// The zero-based position of the first source unit with this path.
        first_index: usize,
        /// The zero-based position of the later source unit with this path.
        duplicate_index: usize,
    },
}

impl fmt::Display for SourceBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLogicalPath { index } => {
                write!(
                    formatter,
                    "source unit at index {index} has an empty logical path"
                )
            }
            Self::DuplicateLogicalPath {
                logical_path,
                first_index,
                duplicate_index,
            } => write!(
                formatter,
                "source units at indexes {first_index} and {duplicate_index} use logical path {logical_path:?}"
            ),
        }
    }
}

impl Error for SourceBundleError {}

#[cfg(test)]
mod tests {
    use super::{SourceBundle, SourceBundleError, SourceUnit};

    #[test]
    fn preserves_source_unit_content_and_bundle_order() {
        let first = SourceUnit::new(
            "crm/schema.orna",
            "CREATE TYPE crm.contact AS OBJECT ();\r\n",
        );
        let second = SourceUnit::new("crm/ui.orna", "-- cafe\u{301}\nSELECT 'tea';\n");
        let bundle = SourceBundle::new([first.clone(), second.clone()]).unwrap();

        assert_eq!(bundle.units(), [first, second]);
        assert_eq!(
            bundle.units()[0].content(),
            "CREATE TYPE crm.contact AS OBJECT ();\r\n"
        );
        assert_eq!(
            bundle.units()[1].content(),
            "-- cafe\u{301}\nSELECT 'tea';\n"
        );
    }

    #[test]
    fn permits_an_empty_bundle_without_inventing_apply_policy() {
        let bundle = SourceBundle::new([]).unwrap();

        assert!(bundle.is_empty());
        assert_eq!(bundle.len(), 0);
    }

    #[test]
    fn rejects_an_empty_logical_path_with_its_source_position() {
        let error = SourceBundle::new([
            SourceUnit::new("crm/schema.orna", "CREATE TYPE crm.contact AS OBJECT ();"),
            SourceUnit::new("", "CREATE TYPE crm.account AS OBJECT ();"),
        ])
        .unwrap_err();

        assert_eq!(error, SourceBundleError::EmptyLogicalPath { index: 1 });
    }

    #[test]
    fn rejects_duplicate_logical_paths_with_both_source_positions() {
        let error = SourceBundle::new([
            SourceUnit::new("crm/schema.orna", "CREATE TYPE crm.contact AS OBJECT ();"),
            SourceUnit::new("crm/schema.orna", "CREATE TYPE crm.account AS OBJECT ();"),
        ])
        .unwrap_err();

        assert_eq!(
            error,
            SourceBundleError::DuplicateLogicalPath {
                logical_path: "crm/schema.orna".to_owned(),
                first_index: 0,
                duplicate_index: 1,
            }
        );
    }
}
