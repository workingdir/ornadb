//! The source-local scope and the analysis inputs that build it.

use super::*;

#[derive(Clone, Default)]
pub(crate) struct Scope {
    pub(crate) names: BTreeMap<String, Symbol>,
    pub(crate) current_namespace: Option<Namespace>,
    /// Every dimension and unit declared by the analysed sources or supplied
    /// by the catalogue, keyed by identity. Names reach them only through
    /// `names` and module exports (`lookup_unit`).
    pub(crate) source_units: Rc<BTreeMap<SourceUnitIdentity, SourceUnitMetadata>>,
    pub(crate) source_dimensions: Rc<BTreeMap<SourceDimensionIdentity, DimensionBasis>>,
    /// Per-module summaries used while resolving table provenance through
    /// pure helper calls. This remains semantic-loader state rather than a
    /// public Symbol field so existing catalogue/value contracts stay stable.
    pub(crate) function_dependencies: RefCell<BTreeMap<String, BTreeSet<String>>>,
    /// Diagnostic messages for the portable `sys` members a production
    /// admission requires without a linked provider; `None` when only the
    /// declared surface is checked. Inference only records them and the
    /// admission boundary reports them after checking, so provider linkage
    /// never alters inferred types or other diagnostics. Clones share the
    /// record: nested scopes belong to the same admission.
    pub(crate) unlinked_system_members: Option<Rc<RefCell<BTreeSet<String>>>>,
    pub(crate) ambiguous: BTreeSet<String>,
    /// Source spellings mapped to compiler-local nominal identities. The
    /// values are qualified by defining module and are never a substitute for
    /// catalogue/revision object identity.
    pub(crate) nominal_identities: BTreeMap<String, String>,
    /// Direct `use module [as alias]` bindings. These are deliberately kept
    /// apart from ordinary values so only explicitly imported module roots can
    /// begin qualified module-member lookup.
    pub(crate) modules: BTreeMap<String, Namespace>,
    /// The closed source/catalogue module set used by qualified lookup. This
    /// is data assembled by the caller, never a filesystem loader.
    pub(crate) available_modules: BTreeMap<Namespace, ModuleHeader>,
    /// Authority-backed declarations available only through a historical
    /// database snapshot. Current modules are never implicitly projected.
    pub(crate) historical_modules: BTreeMap<Namespace, ModuleHeader>,
    /// Aliases of databases attached alongside the current one. Their
    /// independent transaction logs rule out atomic writes through them
    /// (ORNA-ATTACH-002, source/08-relations.md:131).
    pub(crate) attached_databases: BTreeSet<String>,
    /// Local table rows are separate from public table identities: a table
    /// expression produces its relation row shape, while table operations
    /// retain the declared nominal table result.
    pub(crate) table_rows: BTreeMap<String, Type>,
    /// Tables without declared keys use the implicit automatic key and cannot
    /// be explicitly re-keyed by source code.
    pub(crate) auto_key_tables: BTreeSet<String>,
    /// Local nominal record constructors elaborate to their declared row shape
    /// so field access remains structural inside inferred stream pipelines.
    pub(crate) nominal_rows: BTreeMap<String, Type>,
    /// Private representation fields keyed by canonical nominal identity.
    /// They are visible only while checking that type's nested implementation.
    pub(crate) nominal_private_fields: BTreeMap<String, BTreeSet<String>>,
    /// Imported types whose full definition holds a secret, private members included
    /// (ORNA-SECRET-001), keyed by the name a type reference resolves to.
    pub(crate) secret_types: BTreeSet<String>,
    /// The nominal owner whose nested implementation body is currently being
    /// checked. Ordinary module code has no private-field owner.
    pub(crate) private_field_owner: Option<String>,
    /// A same-module nominal constructor used as an argument to a bounded
    /// generic call may supply that type's private representation. This is
    /// scoped to argument inference and does not grant ordinary field access.
    pub(crate) allow_local_private_nominal_construction: bool,
    /// Names whose nominal rows belong to this source module. Imported rows
    /// are selected from the explicitly resolved export instead of by short
    /// name alone.
    pub(crate) local_nominal_names: BTreeSet<String>,
    /// Local refined types mapped to the type they refine, so the closed
    /// `Port.from(value)` constructor can check the source value without
    /// erasing the refined nominal result.
    pub(crate) refined_types: BTreeMap<String, Type>,
    /// Canonical identities of the local, imported and attached nominal types
    /// that implement the core `Currency` protocol (ORNA-MONEY-004). Never
    /// inferred from names or static members.
    pub(crate) currency_types: BTreeSet<String>,
    /// Local enum payloads retained for closed constructor-pattern checking.
    pub(crate) enum_variants: BTreeMap<String, BTreeMap<String, BTreeMap<String, Type>>>,
    /// Local or explicitly imported enum types whose variants carry payloads.
    /// This compact admission metadata is kept separately from public Symbols.
    pub(crate) payload_enum_types: BTreeSet<String>,
    /// Payload rows of explicitly imported source enums, keyed by the
    /// spelling that names the enum here (`Level`, or `alias.Level` through a
    /// module binding). Construction checks qualified variants against them.
    pub(crate) imported_enum_variants: BTreeMap<String, EnumVariantRows>,
    /// Local transparent aliases. Refined and nominal type declarations are
    /// deliberately excluded so alias resolution cannot erase identity.
    pub(crate) type_aliases: BTreeMap<String, Type>,
    /// Local protocols with type parameters. Their substitution is outside
    /// this validator and therefore fails closed at implementation sites.
    pub(crate) generic_protocols: BTreeSet<String>,
    /// Locally declared protocol member surfaces used to check non-generic
    /// nested implementation members without inventing generic dispatch.
    pub(crate) local_protocols: BTreeMap<String, Vec<ProtocolMember>>,
    /// Generic type parameters bound by the function currently being checked.
    /// These names are local semantic witnesses, not globally declared types.
    pub(crate) generic_type_parameters: BTreeSet<String>,
    /// Local generic function signatures. Imported generic functions are
    /// intentionally absent because their declaration-level bounds are not
    /// part of this source-local semantic scope.
    pub(crate) generic_functions: BTreeMap<String, orna_syntax_ast::FunctionSignature>,
    /// Bounds available for generic parameters in the function currently
    /// being checked.
    pub(crate) generic_type_bounds: BTreeMap<String, Vec<TypeExpr>>,
    /// Nested protocol implementations keyed by nominal target. This is the
    /// closed-world evidence used by local generic bound checks.
    pub(crate) protocol_implementations: BTreeMap<String, Vec<Type>>,
    /// Source types accepted by each locally declared nominal `From` surface.
    pub(crate) nominal_conversions: BTreeMap<String, Vec<Type>>,
    /// Effects/failure summaries of each conversion body.
    pub(crate) nominal_conversion_effects: BTreeMap<String, Vec<(Type, EffectSummary)>>,
    /// Function bodies being inferred, innermost last. Each frame checks or
    /// joins the `return` statements that it owns.
    pub(crate) return_frames: RefCell<Vec<ReturnFrame>>,
}

/// The dimension and unit declarations an analysis resolves units against:
/// those of the analysed sources plus those supplied by the catalogue.
#[derive(Clone, Default)]
pub(crate) struct UnitEnvironment {
    pub(crate) units: Rc<BTreeMap<SourceUnitIdentity, SourceUnitMetadata>>,
    pub(crate) dimensions: Rc<BTreeMap<SourceDimensionIdentity, DimensionBasis>>,
}

/// The module graph and catalogue projections one module scope resolves
/// against. They are views of one analysis input, so they travel together
/// instead of as a positional list beside the per-module arguments.
pub(crate) struct ImportInputs<'a> {
    pub(crate) modules: &'a BTreeMap<Namespace, ModuleHeader>,
    pub(crate) historical_modules: &'a BTreeMap<Namespace, ModuleHeader>,
    pub(crate) attached_symbols: &'a BTreeMap<String, Symbol>,
    pub(crate) attached_databases: &'a BTreeSet<String>,
    pub(crate) dependency_summaries: &'a BTreeMap<Namespace, BTreeMap<String, BTreeSet<String>>>,
    pub(crate) units: &'a UnitEnvironment,
}
