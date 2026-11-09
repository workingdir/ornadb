//! The explicit declaration catalogue and its profiles.

use super::*;

/// Explicit, caller-provided declarations available in addition to source
/// modules.  A catalogue is data, not a loader: this crate never reads a
/// standard-library directory or treats an absent name as a standard name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Catalogue {
    pub(crate) modules: BTreeMap<Namespace, ModuleHeader>,
    pub(crate) attached_symbols: BTreeMap<String, Symbol>,
    /// Aliases of attached databases supplied by the caller. An attached
    /// database keeps its own transaction log, so a write through its alias
    /// cannot be atomic with the current database (ORNA-ATTACH-002,
    /// source/08-relations.md:131).
    pub(crate) attached_databases: BTreeSet<String>,
    /// Declarations retained by an authority for whole-program historical
    /// evaluation.  These are deliberately separate from `modules`: current
    /// declarations must never become historical definitions merely because
    /// they happen to have the same name.
    pub(crate) historical_modules: BTreeMap<Namespace, ModuleHeader>,
    /// The verified source provenance used to admit the source-backed
    /// standard modules, when this catalogue contains any.
    pub(crate) standard_dependency_profile: Option<StandardDependencyProfile>,
    /// Dimension and unit declarations supplied with the caller's environment,
    /// such as the semantic support the reference fixtures require. They
    /// resolve through module exports and attached symbols exactly like
    /// source declarations; nothing is inferred from a unit's spelling.
    pub(crate) source_units: BTreeMap<SourceUnitIdentity, SourceUnitMetadata>,
    pub(crate) source_dimensions: BTreeMap<SourceDimensionIdentity, DimensionBasis>,
}

impl Catalogue {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Declares `alias` as an attached database. A write to a table reached
    /// through the alias cannot be atomic with the current database because
    /// the attached database keeps its own transaction log, so such a write is
    /// rejected at type check (ORNA-ATTACH-002, source/08-relations.md:131).
    pub fn with_attached_database(mut self, alias: impl Into<String>) -> Self {
        self.attached_databases.insert(alias.into());
        self
    }

    /// Returns the pinned standard dependency provenance retained by this
    /// catalogue, if source-backed standard modules were admitted.
    pub fn standard_dependency_profile(&self) -> Option<&StandardDependencyProfile> {
        self.standard_dependency_profile.as_ref()
    }

    /// Returns the canonical revision of the pinned standard dependency
    /// provenance retained by this catalogue.
    pub fn standard_dependency_revision(&self) -> Option<[u8; 32]> {
        self.standard_dependency_profile
            .as_ref()
            .map(StandardDependencyProfile::revision_digest)
    }

    /// Adds an authority-backed projection of declarations retained for
    /// whole-program historical calls.  The projection is intentionally
    /// caller-supplied and never inferred from current modules.
    pub fn with_historical_modules(
        mut self,
        modules: impl IntoIterator<Item = ModuleHeader>,
    ) -> Self {
        for module in modules {
            self.historical_modules
                .insert(module.namespace.clone(), module);
        }
        self
    }

    /// Adds dimension and unit declarations supplied by the environment as
    /// the given modules, so qualified paths such as `std.units.si.kWh`
    /// resolve through ordinary module exports. The public units of a module
    /// marked attached are also visible unqualified, like the attached tables
    /// of a fixture environment; lexical declarations and imports still take
    /// precedence (ORNA-UNIT-008).
    fn with_unit_support(mut self, modules: &[(Namespace, &str, bool)]) -> Self {
        let mut items = Vec::new();
        for (namespace, source, attached) in modules {
            let logical_path = format!("{}.orna", namespace.0.join("/"));
            let parsed = parse_module_with_file(source, &logical_path);
            assert!(
                parsed.diagnostics.is_empty(),
                "catalogue unit support must be valid module source"
            );
            let header =
                collect_header(namespace, &parsed.value, &BTreeSet::new(), &mut Vec::new());
            if *attached {
                for (name, symbol) in &header.exports {
                    if symbol.kind == SymbolKind::Unit {
                        self.attached_symbols.insert(name.clone(), symbol.clone());
                    }
                }
            }
            items.extend(
                parsed
                    .value
                    .items
                    .into_iter()
                    .filter(|item| {
                        matches!(
                            item.declaration,
                            Declaration::Dimension { .. } | Declaration::Unit { .. }
                        )
                    })
                    .map(|item| (namespace.clone(), logical_path.clone(), item)),
            );
            self.modules.insert(namespace.clone(), header);
        }
        let (units, dimensions) = analyze_source_units(&items);
        self.source_units.extend(units);
        self.source_dimensions.extend(dimensions);
        self
    }

    /// Builds a declaration catalogue from source bytes that match one
    /// explicitly pinned standard dependency profile. The source is parsed to
    /// derive declarations, but bodies are not executed and the bytes are not
    /// retained after this call.
    pub fn from_standard_sources(
        profile: &StandardDependencyProfile,
        sources: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, StandardCatalogueError> {
        if profile.module_prelude_exports.keys().any(|path| {
            !is_standard_module_path(path) || !profile.module_digests.contains_key(path)
        }) {
            return Err(StandardCatalogueError::Profile(
                StandardProfileError::InvalidPreludeExport,
            ));
        }
        let mut catalogue = Self::empty();
        catalogue.standard_dependency_profile = Some(profile.clone());
        let mut seen = BTreeSet::new();
        let mut supplied = BTreeSet::new();
        let mut trees = Vec::new();
        for (logical_path, source) in sources {
            profile.verify_source(&logical_path, &source)?;
            if !seen.insert(logical_path.clone()) {
                return Err(StandardCatalogueError::DuplicateModule);
            }
            let namespace = namespace_for_path(&logical_path, &mut Vec::new())
                .ok_or(StandardCatalogueError::InvalidModulePath)?;
            if namespace.0.first().map(String::as_str) != Some("std") {
                return Err(StandardCatalogueError::InvalidModulePath);
            }
            let parsed = parse_module_with_file(&source, &logical_path);
            if !parsed.is_ok() {
                return Err(StandardCatalogueError::InvalidSource);
            }
            let mut diagnostics = Vec::new();
            let empty_prelude_exports = BTreeSet::new();
            let prelude_exports = if namespace == Namespace(vec!["std".into()]) {
                &profile.prelude_exports
            } else {
                profile
                    .module_prelude_exports
                    .get(&logical_path)
                    .unwrap_or(&empty_prelude_exports)
            };
            let header =
                collect_header(&namespace, &parsed.value, prelude_exports, &mut diagnostics);
            if diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code() == DIAG_DUPLICATE)
            {
                return Err(StandardCatalogueError::DuplicateDeclaration);
            }
            if !prelude_exports
                .iter()
                .all(|name| header.exports.contains_key(name))
            {
                return Err(StandardCatalogueError::Profile(
                    StandardProfileError::InvalidPreludeExport,
                ));
            }
            trees.push((namespace.clone(), parsed.value));
            if catalogue.modules.insert(namespace, header).is_some() {
                return Err(StandardCatalogueError::DuplicateModule);
            }
            supplied.insert(logical_path);
        }
        if supplied != profile.module_digests.keys().cloned().collect() {
            return Err(StandardCatalogueError::MissingModule);
        }
        if !profile.prelude_exports.is_empty()
            && !catalogue
                .modules
                .contains_key(&Namespace(vec!["std".into()]))
        {
            return Err(StandardCatalogueError::Profile(
                StandardProfileError::InvalidPreludeExport,
            ));
        }
        summarize_standard_function_effects(&mut catalogue.modules, &trees);
        Ok(catalogue)
    }

    /// Replaces the matching source-backed standard modules in an existing
    /// core catalogue after the source bundle has passed profile verification.
    /// Core declarations remain available for system and language surfaces;
    /// source-backed modules provide the executable standard declarations.
    pub fn with_standard_sources(
        mut self,
        profile: &StandardDependencyProfile,
        sources: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, StandardCatalogueError> {
        let standard = Self::from_standard_sources(profile, sources)?;
        let provenance = standard.standard_dependency_profile.clone();
        for (namespace, header) in standard.modules {
            self.modules.insert(namespace, header);
        }
        self.standard_dependency_profile = provenance;
        Ok(self)
    }

    /// The stable core surface represented by the authoritative `stdlib/std`
    /// corpus.  It is intentionally a small declaration catalogue rather than
    /// executable standard-library source.
    pub fn authoritative_core() -> Self {
        let mut modules = BTreeMap::new();
        core_root_modules(&mut modules);
        core_std_modules(&mut modules);
        Self {
            modules,
            attached_symbols: BTreeMap::new(),
            attached_databases: BTreeSet::new(),
            historical_modules: BTreeMap::new(),
            standard_dependency_profile: None,
            source_units: BTreeMap::new(),
            source_dimensions: BTreeMap::new(),
        }
    }

    /// The explicit core catalogue plus the attached environment used by the
    /// reference conformance examples.  Attached tables and connectors are
    /// declarations supplied by the caller; this profile does not execute or
    /// discover them from the host.
    pub fn authoritative_fixture() -> Self {
        let mut catalogue = Self::authoritative_core();
        let rows = fixture_rows();
        fixture_attached_symbols(&mut catalogue, &rows);
        fixture_modules(&mut catalogue, &rows);
        catalogue
            .with_unit_support(&[
                (
                    Namespace(vec!["fixture".into(), "units".into()]),
                    FIXTURE_UNIT_SUPPORT,
                    true,
                ),
                (
                    Namespace(vec!["std".into(), "units".into(), "si".into()]),
                    "pub unit kWh : Energy = 3.6e6.J;",
                    false,
                ),
                (
                    Namespace(vec!["work".into(), "units".into()]),
                    "pub unit kWh : Energy = 3.6e6.J;",
                    false,
                ),
            ])
            // `examples/invalid/cross-database-write.orna` writes through this
            // alias; `work` is a pinned database attached alongside the fixture
            // database (ORNA-ATTACH-002).
            .with_attached_database("work")
    }
}
