//! Standard-library presenter metadata and closed output resolution.
//!
//! `std.present.Presenter` is an ordinary standard-library object type
//! registered as standard-library metadata (work ADR 0057). This module
//! models the queryable registry as a closed core value the sealed
//! `sys.invoke` route can consult: a [`PresenterRegistry`] holds a
//! deterministic set of [`PresenterEntry`] records and resolves an
//! [`InvocationOutputRequirement`] to exactly one presenter function or a
//! closed presentation error.
//!
//! The registry is free of `orna-standard` imports. Entries carry raw
//! [`TypeId`]/[`FunctionId`] identities; the caller (the sealed route, step
//! 7) supplies the actual standard function and type definitions.

use std::{collections::BTreeSet, error::Error, fmt};

use crate::{
    FunctionId, TypeId,
    catalogue::QualifiedSemanticName,
    invocation::{InvocationOutputRequirement, InvocationOutputTypeSelector},
};

/// One failure from presenter-registry construction.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresenterRegistryConstructionError {
    /// The alias text must be non-empty.
    EmptyAlias,
    /// A media type, when present, must be non-empty.
    EmptyMediaType,
    /// Two entries must not share one exact case-sensitive alias.
    DuplicateAlias {
        /// The duplicated alias.
        alias: String,
    },
}

impl fmt::Display for PresenterRegistryConstructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAlias => formatter.write_str("presenter alias must not be empty"),
            Self::EmptyMediaType => formatter.write_str("presenter media type must not be empty"),
            Self::DuplicateAlias { alias } => {
                write!(formatter, "duplicate presenter alias {alias:?}")
            }
        }
    }
}

impl Error for PresenterRegistryConstructionError {}

/// One standard-library presenter metadata record.
///
/// The record mirrors the `std.present.Presenter` standard-library object:
/// a stable CLI alias, the presenter function reference, the accepted input
/// type, the produced output type, an optional media type, the streaming
/// fact, and a deterministic selection priority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresenterEntry {
    alias: String,
    function: FunctionId,
    input_type: TypeId,
    output_type: TypeId,
    media_type: Option<String>,
    streaming: bool,
    priority: u32,
}

impl PresenterEntry {
    /// Creates one checked presenter metadata record.
    pub fn new(
        alias: String,
        function: FunctionId,
        input_type: TypeId,
        output_type: TypeId,
        media_type: Option<String>,
        streaming: bool,
        priority: u32,
    ) -> Result<Self, PresenterRegistryConstructionError> {
        if alias.is_empty() {
            return Err(PresenterRegistryConstructionError::EmptyAlias);
        }
        if media_type.as_deref() == Some("") {
            return Err(PresenterRegistryConstructionError::EmptyMediaType);
        }
        Ok(Self {
            alias,
            function,
            input_type,
            output_type,
            media_type,
            streaming,
            priority,
        })
    }

    /// Returns the stable CLI alias.
    pub fn alias(&self) -> &str {
        &self.alias
    }
    /// Returns the presenter function reference.
    pub const fn function(&self) -> FunctionId {
        self.function
    }
    /// Returns the accepted input type identity.
    pub const fn input_type(&self) -> TypeId {
        self.input_type
    }
    /// Returns the produced output type identity.
    pub const fn output_type(&self) -> TypeId {
        self.output_type
    }
    /// Returns the optional media type.
    pub fn media_type(&self) -> Option<&str> {
        self.media_type.as_deref()
    }
    /// Returns whether the presenter output streams.
    pub const fn streaming(&self) -> bool {
        self.streaming
    }
    /// Returns the deterministic selection priority.
    pub const fn priority(&self) -> u32 {
        self.priority
    }
}

/// One closed presentation failure from output resolution.
///
/// Every variant maps to the presentation error `ORNA0702` (spec exit 5).
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputResolutionError {
    /// No presenter entry carries the requested alias.
    UnresolvedAlias {
        /// The requested alias.
        alias: String,
    },
    /// No presenter entry carries the requested media type.
    UnresolvedMediaType {
        /// The requested media type.
        media_type: String,
    },
    /// No presenter entry accepts the requested type name.
    UnresolvedTypeName {
        /// The requested type name.
        name: String,
    },
    /// Multiple presenter entries tie at the highest priority for one
    /// media-type or type-name selector.
    Ambiguous {
        /// The selector whose resolution is ambiguous.
        selector: AmbiguousOutputSelector,
        /// The aliases tied at the highest priority, in deterministic order.
        aliases: Vec<String>,
    },
}

/// One selector whose media-type or type-name resolution is ambiguous.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AmbiguousOutputSelector {
    /// The ambiguous media-type selector.
    MediaType(String),
    /// The ambiguous type-name selector.
    TypeName(String),
}

impl OutputResolutionError {
    /// Returns the stable spec code: the presentation error `ORNA0702`.
    pub const fn spec_code(&self) -> &'static str {
        "ORNA0702"
    }
    /// Returns the spec exit code for a presentation error.
    pub const fn exit_code(&self) -> u8 {
        5
    }
}

impl fmt::Display for OutputResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedAlias { alias } => {
                write!(formatter, "no presenter for output alias {alias:?}")
            }
            Self::UnresolvedMediaType { media_type } => {
                write!(
                    formatter,
                    "no presenter for output media type {media_type:?}"
                )
            }
            Self::UnresolvedTypeName { name } => {
                write!(formatter, "no presenter for output type name {name:?}")
            }
            Self::Ambiguous { selector, aliases } => match selector {
                AmbiguousOutputSelector::MediaType(media_type) => write!(
                    formatter,
                    "output media type {media_type:?} is ambiguous between presenters {}",
                    aliases.join(", ")
                ),
                AmbiguousOutputSelector::TypeName(name) => write!(
                    formatter,
                    "output type name {name:?} is ambiguous between presenters {}",
                    aliases.join(", ")
                ),
            },
        }
    }
}

impl Error for OutputResolutionError {}

/// The immutable, deterministic presenter registry.
///
/// Construction rejects duplicate aliases and normalises the entries into
/// canonical order (priority descending, then alias ascending). Lookups are
/// total: every output requirement either resolves to exactly one entry or
/// a closed [`OutputResolutionError`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresenterRegistry {
    entries: Vec<PresenterEntry>,
}

impl PresenterRegistry {
    /// Creates one checked registry, rejecting duplicate aliases.
    ///
    /// Entries are stored in canonical deterministic order: priority
    /// descending, then alias ascending. A duplicate alias (exact,
    /// case-sensitive) is a construction error.
    pub fn new(
        mut entries: Vec<PresenterEntry>,
    ) -> Result<Self, PresenterRegistryConstructionError> {
        let mut seen = BTreeSet::new();
        for entry in &entries {
            if !seen.insert(entry.alias.as_str()) {
                return Err(PresenterRegistryConstructionError::DuplicateAlias {
                    alias: entry.alias.clone(),
                });
            }
        }
        entries.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.alias.cmp(&right.alias))
        });
        Ok(Self { entries })
    }

    /// Returns the entries in canonical deterministic order.
    pub fn entries(&self) -> &[PresenterEntry] {
        &self.entries
    }
    /// Returns whether the registry holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Returns the number of registered entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Resolves one exact case-sensitive alias to its presenter entry.
    ///
    /// Aliases are unique in a checked registry, so an alias match is never
    /// ambiguous.
    pub fn resolve_alias(&self, alias: &str) -> Option<&PresenterEntry> {
        self.entries.iter().find(|entry| entry.alias == alias)
    }

    /// Resolves one exact media type to its presenter entry.
    ///
    /// Matching is exact string equality (no wildcards in this slice). Among
    /// the entries carrying the media type, the highest priority wins; a tie
    /// at the highest priority is [`OutputResolutionError::Ambiguous`].
    pub fn resolve_media_type(
        &self,
        media_type: &str,
    ) -> Result<&PresenterEntry, OutputResolutionError> {
        let mut matching = self
            .entries
            .iter()
            .filter(|entry| entry.media_type.as_deref() == Some(media_type));
        let Some(first) = matching.next() else {
            return Err(OutputResolutionError::UnresolvedMediaType {
                media_type: media_type.to_owned(),
            });
        };
        let best_priority = first.priority;
        let mut aliases = vec![first.alias.clone()];
        for entry in matching {
            if entry.priority < best_priority {
                break;
            }
            aliases.push(entry.alias.clone());
        }
        if aliases.len() > 1 {
            return Err(OutputResolutionError::Ambiguous {
                selector: AmbiguousOutputSelector::MediaType(media_type.to_owned()),
                aliases,
            });
        }
        Ok(first)
    }

    /// Resolves one exact input type to its presenter entry.
    ///
    /// Among the entries accepting the input type, the highest priority
    /// wins; a tie at the highest priority is
    /// [`OutputResolutionError::Ambiguous`].
    pub fn resolve_input_type(
        &self,
        input_type: TypeId,
    ) -> Result<&PresenterEntry, OutputResolutionError> {
        let mut matching = self
            .entries
            .iter()
            .filter(|entry| entry.input_type == input_type);
        let Some(first) = matching.next() else {
            return Err(OutputResolutionError::UnresolvedTypeName {
                name: input_type.canonical(),
            });
        };
        let best_priority = first.priority;
        let mut aliases = vec![first.alias.clone()];
        for entry in matching {
            if entry.priority < best_priority {
                break;
            }
            aliases.push(entry.alias.clone());
        }
        if aliases.len() > 1 {
            return Err(OutputResolutionError::Ambiguous {
                selector: AmbiguousOutputSelector::TypeName(input_type.canonical()),
                aliases,
            });
        }
        Ok(first)
    }

    /// Resolves one checked output requirement to exactly one presenter.
    ///
    /// Selection precedence is deterministic: an exact alias match wins over
    /// a media-type match, which wins over a type-name match. A qualified
    /// type-name selector is resolved through `resolve_name` — the sealed
    /// route supplies catalogue resolution, because a closed registry cannot
    /// consult the catalogue. An un-resolvable or unregistered name is
    /// [`OutputResolutionError::UnresolvedTypeName`].
    pub fn resolve_requirement(
        &self,
        requirement: &InvocationOutputRequirement,
        resolve_name: impl Fn(&QualifiedSemanticName) -> Option<TypeId>,
    ) -> Result<&PresenterEntry, OutputResolutionError> {
        match (
            requirement.alias(),
            requirement.media_type(),
            requirement.type_selector(),
        ) {
            (Some(alias), _, _) => {
                self.resolve_alias(alias)
                    .ok_or_else(|| OutputResolutionError::UnresolvedAlias {
                        alias: alias.to_owned(),
                    })
            }
            (None, Some(media_type), _) => self.resolve_media_type(media_type),
            (None, None, Some(selector)) => match selector {
                InvocationOutputTypeSelector::TypeId(type_id) => self.resolve_input_type(*type_id),
                InvocationOutputTypeSelector::QualifiedName(name) => {
                    let Some(input_type) = resolve_name(name) else {
                        return Err(OutputResolutionError::UnresolvedTypeName {
                            name: name.to_string(),
                        });
                    };
                    self.resolve_input_type(input_type)
                        .map_err(|error| match error {
                            OutputResolutionError::UnresolvedTypeName { .. } => {
                                OutputResolutionError::UnresolvedTypeName {
                                    name: name.to_string(),
                                }
                            }
                            other => other,
                        })
                }
            },
            (None, None, None) => {
                unreachable!("InvocationOutputRequirement::new rejects an empty requirement")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_id(byte: u8) -> TypeId {
        TypeId::from_bytes([byte; 16])
    }

    fn function_id(byte: u8) -> FunctionId {
        FunctionId::from_bytes([byte; 16])
    }

    fn entry(
        alias: &str,
        function_byte: u8,
        input_byte: u8,
        output_byte: u8,
        media_type: Option<&str>,
        streaming: bool,
        priority: u32,
    ) -> PresenterEntry {
        PresenterEntry::new(
            alias.to_owned(),
            function_id(function_byte),
            type_id(input_byte),
            type_id(output_byte),
            media_type.map(str::to_owned),
            streaming,
            priority,
        )
        .expect("a valid test entry")
    }

    /// The `std.json.encode` presenter: values to a JSON byte stream.
    fn json_encode() -> PresenterEntry {
        entry(
            "json",
            1,
            1, // std.json.Value
            2, // std.io.ByteStream
            Some("application/json"),
            false,
            100,
        )
    }

    /// The `std.terminal.present_table` presenter: rows to a document.
    fn present_table() -> PresenterEntry {
        entry(
            "table",
            2,
            3, // std.data.Rows
            4, // std.terminal.Document
            Some("text/plain"),
            false,
            100,
        )
    }

    fn standard_registry() -> PresenterRegistry {
        PresenterRegistry::new(vec![json_encode(), present_table()])
            .expect("the standard registry must construct")
    }

    fn requirement(
        alias: Option<&str>,
        media_type: Option<&str>,
        type_id: Option<TypeId>,
    ) -> InvocationOutputRequirement {
        InvocationOutputRequirement::new(
            alias.map(str::to_owned),
            media_type.map(str::to_owned),
            type_id.map(InvocationOutputTypeSelector::type_id),
            crate::invocation::InvocationStreamingRequirement::Unspecified,
        )
        .expect("a valid test requirement")
    }

    #[test]
    fn entry_construction_rejects_empty_alias() {
        assert_eq!(
            PresenterEntry::new(
                String::new(),
                function_id(1),
                type_id(1),
                type_id(2),
                None,
                false,
                100,
            ),
            Err(PresenterRegistryConstructionError::EmptyAlias)
        );
    }

    #[test]
    fn entry_construction_rejects_empty_media_type() {
        assert_eq!(
            PresenterEntry::new(
                "json".to_owned(),
                function_id(1),
                type_id(1),
                type_id(2),
                Some(String::new()),
                false,
                100,
            ),
            Err(PresenterRegistryConstructionError::EmptyMediaType)
        );
    }

    #[test]
    fn registry_rejects_duplicate_alias() {
        let error = PresenterRegistry::new(vec![
            json_encode(),
            PresenterEntry::new(
                "json".to_owned(),
                function_id(9),
                type_id(9),
                type_id(2),
                Some("application/x-other".to_owned()),
                false,
                200,
            )
            .expect("a valid duplicate-alias entry"),
        ])
        .expect_err("a duplicate alias must be rejected");
        assert_eq!(
            error,
            PresenterRegistryConstructionError::DuplicateAlias {
                alias: "json".to_owned()
            }
        );
    }

    #[test]
    fn empty_registry_resolves_nothing() {
        let registry = PresenterRegistry::new(Vec::new()).expect("an empty registry constructs");
        assert!(registry.is_empty());
        assert_eq!(registry.resolve_alias("json"), None);
        assert_eq!(
            registry.resolve_media_type("application/json"),
            Err(OutputResolutionError::UnresolvedMediaType {
                media_type: "application/json".to_owned()
            })
        );
        assert_eq!(
            registry.resolve_input_type(type_id(1)),
            Err(OutputResolutionError::UnresolvedTypeName {
                name: type_id(1).canonical()
            })
        );
    }

    #[test]
    fn entries_are_normalised_into_deterministic_order() {
        let low = entry("low", 1, 1, 2, None, false, 10);
        let high = entry("high", 2, 1, 2, None, false, 200);
        let middle = entry("middle", 3, 1, 2, None, false, 100);
        let registry = PresenterRegistry::new(vec![middle, low, high])
            .expect("distinct aliases must construct");
        let aliases = registry
            .entries()
            .iter()
            .map(PresenterEntry::alias)
            .collect::<Vec<_>>();
        assert_eq!(aliases, ["high", "middle", "low"]);
    }

    #[test]
    fn alias_resolution_is_exact_and_case_sensitive() {
        let registry = standard_registry();
        let resolved = registry
            .resolve_alias("json")
            .expect("the json alias must resolve");
        assert_eq!(resolved.alias(), "json");
        assert_eq!(resolved.function(), function_id(1));
        assert_eq!(resolved.input_type(), type_id(1));
        assert_eq!(resolved.output_type(), type_id(2));
        assert_eq!(resolved.media_type(), Some("application/json"));
        assert!(!resolved.streaming());
        assert_eq!(resolved.priority(), 100);

        let table = registry
            .resolve_alias("table")
            .expect("the table alias must resolve");
        assert_eq!(table.function(), function_id(2));
        assert_eq!(table.input_type(), type_id(3));
        assert_eq!(table.output_type(), type_id(4));

        assert_eq!(registry.resolve_alias("JSON"), None);
        assert_eq!(registry.resolve_alias("csv"), None);
    }

    #[test]
    fn media_type_resolution_is_exact() {
        let registry = standard_registry();
        let resolved = registry
            .resolve_media_type("application/json")
            .expect("application/json must resolve");
        assert_eq!(resolved.alias(), "json");
        let table = registry
            .resolve_media_type("text/plain")
            .expect("text/plain must resolve");
        assert_eq!(table.alias(), "table");
        assert_eq!(
            registry.resolve_media_type("application/xml"),
            Err(OutputResolutionError::UnresolvedMediaType {
                media_type: "application/xml".to_owned()
            })
        );
    }

    #[test]
    fn type_name_resolution_is_exact() {
        let registry = standard_registry();
        let resolved = registry
            .resolve_input_type(type_id(1))
            .expect("the json value type must resolve");
        assert_eq!(resolved.alias(), "json");
        let table = registry
            .resolve_input_type(type_id(3))
            .expect("the rows type must resolve");
        assert_eq!(table.alias(), "table");
        assert_eq!(
            registry.resolve_input_type(type_id(9)),
            Err(OutputResolutionError::UnresolvedTypeName {
                name: type_id(9).canonical()
            })
        );
    }

    #[test]
    fn media_type_priority_ordering_picks_highest() {
        let entries = vec![
            json_encode(),
            entry(
                "json-canonical",
                7,
                1,
                2,
                Some("application/json"),
                false,
                200,
            ),
        ];
        let registry = PresenterRegistry::new(entries).expect("distinct aliases must construct");
        let resolved = registry
            .resolve_media_type("application/json")
            .expect("application/json must resolve");
        assert_eq!(resolved.alias(), "json-canonical");
    }

    #[test]
    fn media_type_tie_at_highest_priority_is_ambiguous() {
        let registry = PresenterRegistry::new(vec![
            json_encode(),
            entry("json-stream", 8, 1, 2, Some("application/json"), true, 100),
        ])
        .expect("distinct aliases must construct");
        assert_eq!(
            registry.resolve_media_type("application/json"),
            Err(OutputResolutionError::Ambiguous {
                selector: AmbiguousOutputSelector::MediaType("application/json".to_owned()),
                aliases: vec!["json".to_owned(), "json-stream".to_owned()],
            })
        );
    }

    #[test]
    fn input_type_tie_at_highest_priority_is_ambiguous() {
        let registry = PresenterRegistry::new(vec![
            json_encode(),
            entry("json-canonical", 7, 1, 2, None, false, 100),
        ])
        .expect("distinct aliases must construct");
        assert_eq!(
            registry.resolve_input_type(type_id(1)),
            Err(OutputResolutionError::Ambiguous {
                selector: AmbiguousOutputSelector::TypeName(type_id(1).canonical()),
                aliases: vec!["json".to_owned(), "json-canonical".to_owned()],
            })
        );
    }

    #[test]
    fn requirement_resolution_prefers_alias_over_media_type() {
        let registry = PresenterRegistry::new(vec![
            json_encode(),
            present_table(),
            entry("json-doc", 9, 1, 4, Some("application/json"), false, 200),
        ])
        .expect("distinct aliases must construct");
        // The alias json wins even though json-doc has a higher priority on
        // the same media type.
        let resolved = registry
            .resolve_requirement(
                &requirement(Some("json"), Some("application/json"), None),
                |_| None,
            )
            .expect("the alias must resolve");
        assert_eq!(resolved.alias(), "json");
    }

    #[test]
    fn requirement_resolution_prefers_media_type_over_type_name() {
        let registry = standard_registry();
        let resolved = registry
            .resolve_requirement(
                &requirement(None, Some("application/json"), Some(type_id(3))),
                |_| None,
            )
            .expect("the media type must resolve");
        assert_eq!(resolved.alias(), "json");
    }

    #[test]
    fn requirement_resolution_resolves_qualified_type_names() {
        let registry = standard_registry();
        let json_value =
            QualifiedSemanticName::new(["std", "json", "Value"]).expect("a valid qualified name");
        let resolved = registry
            .resolve_requirement(
                &InvocationOutputRequirement::new(
                    None,
                    None,
                    Some(
                        InvocationOutputTypeSelector::qualified_name(json_value.clone())
                            .expect("a valid type-name selector"),
                    ),
                    crate::invocation::InvocationStreamingRequirement::Unspecified,
                )
                .expect("a valid requirement"),
                |name| (name == &json_value).then(|| type_id(1)),
            )
            .expect("the type name must resolve");
        assert_eq!(resolved.alias(), "json");
    }

    #[test]
    fn requirement_resolution_reports_unresolved_selectors() {
        let registry = standard_registry();
        assert_eq!(
            registry.resolve_requirement(&requirement(Some("csv"), None, None), |_| None),
            Err(OutputResolutionError::UnresolvedAlias {
                alias: "csv".to_owned()
            })
        );
        assert_eq!(
            registry
                .resolve_requirement(&requirement(None, Some("application/xml"), None), |_| None),
            Err(OutputResolutionError::UnresolvedMediaType {
                media_type: "application/xml".to_owned()
            })
        );
        assert_eq!(
            registry.resolve_requirement(&requirement(None, None, Some(type_id(9))), |_| None),
            Err(OutputResolutionError::UnresolvedTypeName {
                name: type_id(9).canonical()
            })
        );
    }

    #[test]
    fn requirement_resolution_reports_unresolved_qualified_names() {
        let registry = standard_registry();
        let unknown =
            QualifiedSemanticName::new(["std", "xml", "Value"]).expect("a valid qualified name");
        let requirement = InvocationOutputRequirement::new(
            None,
            None,
            Some(
                InvocationOutputTypeSelector::qualified_name(unknown.clone())
                    .expect("a valid type-name selector"),
            ),
            crate::invocation::InvocationStreamingRequirement::Unspecified,
        )
        .expect("a valid requirement");
        // The name is not resolvable to a TypeId at all.
        assert_eq!(
            registry.resolve_requirement(&requirement, |_| None),
            Err(OutputResolutionError::UnresolvedTypeName {
                name: unknown.to_string()
            })
        );
        // The name resolves to a type with no presenter.
        assert_eq!(
            registry.resolve_requirement(&requirement, |_| Some(type_id(9))),
            Err(OutputResolutionError::UnresolvedTypeName {
                name: unknown.to_string()
            })
        );
    }

    #[test]
    fn errors_map_to_spec_code_and_exit_code() {
        for error in [
            OutputResolutionError::UnresolvedAlias {
                alias: "csv".to_owned(),
            },
            OutputResolutionError::UnresolvedMediaType {
                media_type: "application/xml".to_owned(),
            },
            OutputResolutionError::UnresolvedTypeName {
                name: "std.xml.Value".to_owned(),
            },
            OutputResolutionError::Ambiguous {
                selector: AmbiguousOutputSelector::MediaType("application/json".to_owned()),
                aliases: vec!["json".to_owned(), "json-stream".to_owned()],
            },
        ] {
            assert_eq!(error.spec_code(), "ORNA0702");
            assert_eq!(error.exit_code(), 5);
        }
    }

    #[test]
    fn errors_display_human_readable_messages() {
        assert_eq!(
            OutputResolutionError::UnresolvedAlias {
                alias: "csv".to_owned()
            }
            .to_string(),
            "no presenter for output alias \"csv\""
        );
        assert_eq!(
            OutputResolutionError::Ambiguous {
                selector: AmbiguousOutputSelector::TypeName("std.json.Value".to_owned()),
                aliases: vec!["json".to_owned(), "json-stream".to_owned()],
            }
            .to_string(),
            "output type name \"std.json.Value\" is ambiguous between presenters json, json-stream"
        );
    }
}
