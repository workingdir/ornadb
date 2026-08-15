//! Orna-owned source parsing at the compiler boundary.
//!
//! This crate accepts an ordered source bundle and returns lossless parsed
//! units with compiler diagnostics. Semantic analysis and revision construction
//! are separate stages.

use orna_core::{catalogue::QualifiedSemanticName, source::SourceBundle};
use orna_syntax::{
    Diagnostic as SyntaxDiagnostic, NamePart, Parse as SyntaxParse, QualifiedName, SourceSpan,
};

mod mutation;
mod prepare;
pub(crate) mod relational;
mod resolver;

pub use prepare::{
    PrepareError, PrepareStandardApplicationError, PrepareStandardUpgradeError,
    PreparedStandardUpgrade, StandardUpgradeIdentity, prepare, prepare_checked_standard_upgrade,
    prepare_standard_application,
};

pub use orna_core::revision::EMPTY_APPLICATION_CATALOGUE_REVISION_ID;
pub use resolver::{
    CheckReport, CheckedApplicationTypeUse, CheckedBundle, CheckedClientFunction, CheckedDefault,
    CheckedDefinitionReference, CheckedDefinitionReferenceTarget, CheckedExpressionId,
    CheckedField, CheckedFieldId, CheckedFunctionId, CheckedObjectReferenceUse, CheckedObjectType,
    CheckedParameterId, CheckedSchema, CheckedSchemaId, CheckedServerFunction,
    CheckedServerFunctionParameter, CheckedServerFunctionReturnColumn,
    CheckedStandardApplicationBundle, CheckedStandardApplicationClientFunction,
    CheckedStandardApplicationField, CheckedStandardApplicationObjectType,
    CheckedStandardApplicationParameter, CheckedStandardApplicationRecordValueField,
    CheckedStandardApplicationRecordValueType, CheckedStandardApplicationReturnColumn,
    CheckedStandardApplicationServerFunction, CheckedStandardLibrary, CheckedStandardSchema,
    CheckedStandardTypeBinding, CheckedStandardTypeReference, CheckedStandardValueType,
    CheckedTypeId, CheckedTypeUseKind, CheckedValueTypeUse, ConstantValue,
    NewApplicationCheckError, ProvisionalExpressionId, ProvisionalFieldId, ProvisionalFunctionId,
    ProvisionalParameterId, ProvisionalSchemaId, ProvisionalTypeId, SemanticType,
    StandardApplicationCheckContext, StandardApplicationCheckReport,
    StandardApplicationContextError, StandardLibraryCheckError, check, check_new_application,
    check_standard_application, check_standard_library_source,
};

/// Resolves an identifier component with Orna quoted-name rules.
///
/// Unquoted identifiers are case-insensitive. Quoted identifiers retain their
/// case and unescape doubled double quotes.
pub(crate) fn normalise_name_part(part: &NamePart) -> String {
    if part.text.starts_with('"') {
        part.text[1..part.text.len() - 1].replace("\"\"", "\"")
    } else {
        part.text.to_lowercase()
    }
}

/// Resolves a qualified source name with Orna identifier rules.
pub(crate) fn normalise_qualified_name(name: &QualifiedName) -> QualifiedSemanticName {
    QualifiedSemanticName::new(name.parts.iter().map(normalise_name_part))
        .expect("parser produced a non-empty qualified name")
}

pub(crate) fn semantic_diagnostic(
    code: DiagnosticCode,
    message: impl Into<String>,
    logical_path: &str,
    span: &SourceSpan,
) -> CompilerDiagnostic {
    DiagnosticCode::semantic(
        code,
        message,
        SourceLocation::from_syntax(logical_path, span),
    )
}

/// Parses every source unit in a bundle without changing compiler state.
pub fn parse_bundle(bundle: &SourceBundle) -> ParseReport {
    let mut units = Vec::with_capacity(bundle.len());
    let mut diagnostics = Vec::new();

    for source_unit in bundle.units() {
        let parsed = orna_syntax::parse(source_unit.content());
        let logical_path = source_unit.logical_path().to_owned();

        diagnostics.extend(
            parsed
                .diagnostics()
                .iter()
                .map(|diagnostic| CompilerDiagnostic::from_syntax(&logical_path, diagnostic)),
        );
        units.push(ParsedSourceUnit {
            logical_path,
            source_text: source_unit.content().to_owned(),
            parsed,
        });
    }

    ParseReport { units, diagnostics }
}

/// The closed set of syntax diagnostic codes produced by this compiler stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    /// The source contains a token that is invalid at its current position.
    UnexpectedToken,
    /// The source ends before a comment, quoted identifier, or string closes.
    UnterminatedSourceConstruct,
    /// A referenced qualified name does not exist in the checking context.
    UnknownQualifiedName,
    /// More than one declaration has the same resolved semantic name.
    DuplicateDefinition,
    /// A declaration or constant does not satisfy its resolved type.
    TypeMismatch,
    /// A `REF` declaration targets a scalar rather than an object type.
    InvalidReferenceTarget,
    /// A declaration uses a valid construct outside this compiler domain.
    DomainIncompatible,
}

impl DiagnosticCode {
    /// Returns the stable Orna diagnostic code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnexpectedToken => "ORNA0001",
            Self::UnterminatedSourceConstruct => "ORNA0002",
            Self::UnknownQualifiedName => "ORNA0101",
            Self::DuplicateDefinition => "ORNA0103",
            Self::TypeMismatch => "ORNA0201",
            Self::InvalidReferenceTarget => "ORNA0203",
            Self::DomainIncompatible => "ORNA0303",
        }
    }

    fn from_syntax_code(code: &str) -> Self {
        match code {
            "ORNA0001" => Self::UnexpectedToken,
            "ORNA0002" => Self::UnterminatedSourceConstruct,
            _ => panic!("orna-syntax emitted a diagnostic outside the compiler syntax contract"),
        }
    }

    fn semantic(
        code: Self,
        message: impl Into<String>,
        location: SourceLocation,
    ) -> CompilerDiagnostic {
        CompilerDiagnostic {
            code,
            message: message.into(),
            location,
        }
    }
}

/// An owned byte span within one logical source unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteSpan {
    start: usize,
    end: usize,
}

impl ByteSpan {
    fn from_syntax_span(span: &SourceSpan) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
    }

    /// Returns the first byte in this span.
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Returns the first byte after this span.
    pub const fn end(&self) -> usize {
        self.end
    }
}

/// An owned location for a compiler diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    logical_path: String,
    span: ByteSpan,
}

impl SourceLocation {
    pub(crate) fn from_syntax(logical_path: &str, span: &SourceSpan) -> Self {
        Self {
            logical_path: logical_path.to_owned(),
            span: ByteSpan::from_syntax_span(span),
        }
    }

    /// Returns the logical path submitted with the source unit.
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// Returns the byte span within the source unit.
    pub fn span(&self) -> &ByteSpan {
        &self.span
    }
}

/// A compiler diagnostic with an Orna-owned code and source location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerDiagnostic {
    code: DiagnosticCode,
    message: String,
    location: SourceLocation,
}

impl CompilerDiagnostic {
    fn from_syntax(logical_path: &str, diagnostic: &SyntaxDiagnostic) -> Self {
        Self {
            code: DiagnosticCode::from_syntax_code(diagnostic.code),
            message: diagnostic.message.clone(),
            location: SourceLocation::from_syntax(logical_path, &diagnostic.span),
        }
    }

    /// Returns the stable diagnostic code.
    pub const fn code(&self) -> DiagnosticCode {
        self.code
    }

    /// Returns a description of the source error.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the source unit and byte span that produced this diagnostic.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One lossless source unit parsed by the compiler.
#[derive(Clone, Debug)]
pub struct ParsedSourceUnit {
    logical_path: String,
    source_text: String,
    parsed: SyntaxParse,
}

impl ParsedSourceUnit {
    /// Returns the submitted logical path for this source unit.
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// Returns the exact submitted UTF-8 source text.
    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    /// Returns the exact source text represented by the private lossless CST.
    pub fn syntax_text(&self) -> String {
        self.parsed.syntax().text()
    }

    pub(crate) fn parsed(&self) -> &SyntaxParse {
        &self.parsed
    }

    #[cfg(test)]
    pub(crate) fn replace_source_text_for_test(&mut self, source_text: impl Into<String>) {
        self.source_text = source_text.into();
    }
}

/// The result of parsing one ordered source bundle.
#[derive(Clone, Debug)]
pub struct ParseReport {
    units: Vec<ParsedSourceUnit>,
    diagnostics: Vec<CompilerDiagnostic>,
}

impl ParseReport {
    /// Returns parsed source units in submitted bundle order.
    pub fn units(&self) -> &[ParsedSourceUnit] {
        &self.units
    }

    /// Returns compiler diagnostics in source-unit and source order.
    pub fn diagnostics(&self) -> &[CompilerDiagnostic] {
        &self.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use orna_core::{
        CatalogueRevisionId, ExpressionId, FunctionId, FunctionRevisionId, SchemaId,
        SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeId,
        canonical_hash::{
            artifact_payload_digest, catalogue_digest, catalogue_digest_with_context,
            function_semantic_digest, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest, verify_standard_library_snapshot,
        },
        catalogue::{
            CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
            PreludeTypeName, QualifiedSemanticName, SchemaDefinition, TypeBinding,
            ValueTypeDefinition, ValueTypeKind, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
            DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision,
            ExecutableArtifact, ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord,
            FunctionSemanticHashVersion, RevisionPair, Sha256Digest, SourceOrigin,
            StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
            StoredSourceUnit,
        },
        source::{SourceBundle, SourceUnit},
        types::{ResolvedType, StandardScalar},
    };

    use super::{
        CheckedFunctionId, CheckedParameterId, CheckedStandardLibrary, CheckedTypeId,
        CheckedTypeUseKind, CheckedValueTypeUse, DiagnosticCode,
        EMPTY_APPLICATION_CATALOGUE_REVISION_ID, PrepareError, PrepareStandardApplicationError,
        PrepareStandardUpgradeError, PreparedStandardUpgrade, StandardApplicationCheckContext,
        StandardApplicationCheckReport, StandardApplicationContextError, StandardLibraryCheckError,
        check, check_new_application, check_standard_application, check_standard_library_source,
        parse_bundle, prepare, prepare_checked_standard_upgrade, prepare_standard_application,
    };
    use crate::prepare::{
        CandidateAllocator, CandidateIdSource, ReservedStandardIds,
        prepare_checked_standard_upgrade_with_allocator,
        prepare_standard_application_with_allocator,
    };

    const SUCCESS_STANDARD_DIGEST: [u8; 32] = [
        0x72, 0x4b, 0x41, 0xcf, 0x68, 0x5c, 0x93, 0xa8, 0xc9, 0x8d, 0xf9, 0x3d, 0x96, 0x77, 0x98,
        0x98, 0x12, 0x34, 0xc0, 0x98, 0xf6, 0xc1, 0x00, 0xfa, 0x57, 0xe9, 0xac, 0x00, 0xdd, 0x03,
        0xfb, 0x6d,
    ];

    const NON_STD_SCHEMA_STANDARD_DIGEST: [u8; 32] = [
        0x9c, 0xbf, 0x15, 0x96, 0xd6, 0x6d, 0xf7, 0xe9, 0x70, 0xcc, 0x24, 0x31, 0x86, 0x71, 0xe1,
        0x06, 0xeb, 0x06, 0x3d, 0x39, 0x2b, 0x8c, 0xf4, 0xe1, 0xe3, 0x88, 0xfa, 0x1f, 0x41, 0xc3,
        0x5e, 0x23,
    ];

    const CANONICAL_STANDARD_SOURCE: &str = include_str!("../../../stdlib/std/types.orna");

    static PREPARE_CATALOGUE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static PREPARE_BUNDLE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static PREPARE_REVISION_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static PREPARE_UNIT_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static PREPARE_FUNCTION_REVISION_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static PREPARE_SCHEMA_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static PREPARE_TYPE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static PREPARE_ALLOCATION_LOCK: Mutex<()> = Mutex::new(());

    fn retry_id(counter: &AtomicUsize, reserved: u8, safe: u8) -> u8 {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            reserved
        } else {
            safe
        }
    }

    fn retry_catalogue_id() -> CatalogueRevisionId {
        CatalogueRevisionId::from_bytes([retry_id(&PREPARE_CATALOGUE_ALLOCATIONS, 8, 0x81); 16])
    }

    fn retry_bundle_id() -> SourceBundleId {
        SourceBundleId::from_bytes([retry_id(&PREPARE_BUNDLE_ALLOCATIONS, 5, 0x82); 16])
    }

    fn retry_revision_id() -> SourceRevisionId {
        SourceRevisionId::from_bytes([retry_id(&PREPARE_REVISION_ALLOCATIONS, 6, 0x83); 16])
    }

    fn retry_unit_id() -> SourceUnitId {
        let allocation = PREPARE_UNIT_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        let byte = match allocation {
            0 => 4,
            1 => 0x84,
            index => 0x84 + u8::try_from(index - 1).unwrap(),
        };
        SourceUnitId::from_bytes([byte; 16])
    }

    fn retry_schema_id() -> SchemaId {
        SchemaId::from_bytes([retry_id(&PREPARE_SCHEMA_ALLOCATIONS, 1, 0x85); 16])
    }

    fn retry_type_id() -> TypeId {
        TypeId::from_bytes([retry_id(&PREPARE_TYPE_ALLOCATIONS, 3, 0x86); 16])
    }

    fn next_function_revision_id() -> FunctionRevisionId {
        let allocation = PREPARE_FUNCTION_REVISION_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        FunctionRevisionId::from_bytes([0x90 + u8::try_from(allocation).unwrap(); 16])
    }

    fn retrying_standard_allocator(
        verified: &orna_core::revision::VerifiedStandardLibrarySnapshot,
    ) -> CandidateAllocator {
        for counter in [
            &PREPARE_CATALOGUE_ALLOCATIONS,
            &PREPARE_BUNDLE_ALLOCATIONS,
            &PREPARE_REVISION_ALLOCATIONS,
            &PREPARE_UNIT_ALLOCATIONS,
            &PREPARE_FUNCTION_REVISION_ALLOCATIONS,
            &PREPARE_SCHEMA_ALLOCATIONS,
            &PREPARE_TYPE_ALLOCATIONS,
        ] {
            counter.store(0, Ordering::SeqCst);
        }
        CandidateAllocator::with_source(
            ReservedStandardIds::from_snapshot(verified),
            CandidateIdSource {
                catalogue_revision: retry_catalogue_id,
                source_bundle: retry_bundle_id,
                source_revision: retry_revision_id,
                source_unit: retry_unit_id,
                schema: retry_schema_id,
                type_id: retry_type_id,
                function_revision: next_function_revision_id,
            },
        )
    }

    fn assert_no_standard_upgrade_allocations() {
        for counter in [
            &PREPARE_CATALOGUE_ALLOCATIONS,
            &PREPARE_BUNDLE_ALLOCATIONS,
            &PREPARE_REVISION_ALLOCATIONS,
            &PREPARE_UNIT_ALLOCATIONS,
            &PREPARE_FUNCTION_REVISION_ALLOCATIONS,
            &PREPARE_SCHEMA_ALLOCATIONS,
            &PREPARE_TYPE_ALLOCATIONS,
        ] {
            assert_eq!(
                counter.load(Ordering::SeqCst),
                0,
                "standard upgrade gate must reject before candidate allocation"
            );
        }
    }

    fn checked_type_use_kind_tag(kind: CheckedTypeUseKind) -> &'static str {
        match kind {
            CheckedTypeUseKind::Field { .. } => "field",
            CheckedTypeUseKind::Parameter { .. } => "parameter",
            CheckedTypeUseKind::Return { .. } => "return",
            CheckedTypeUseKind::Expression { .. } => "expression",
            CheckedTypeUseKind::Result { .. } => "result",
        }
    }

    fn assert_declaration_evidence_mismatch(
        error: PrepareStandardApplicationError,
        expected_kind: CheckedTypeUseKind,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch { .. }
        ));
        if let PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch { kind } = &error {
            assert_eq!(*kind, expected_kind);
        }
        assert_eq!(
            error.to_string(),
            format!(
                "the checked declaration type evidence does not match its {} type use",
                checked_type_use_kind_tag(expected_kind)
            )
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_body_evidence_mismatch(
        error: PrepareStandardApplicationError,
        expected_function: CheckedFunctionId,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::BodyTypeEvidenceMismatch { .. }
        ));
        if let PrepareStandardApplicationError::BodyTypeEvidenceMismatch { function } = &error {
            assert_eq!(*function, expected_function);
        }
        assert_eq!(
            error.to_string(),
            format!("the checked body type evidence does not match function {expected_function}")
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_function_reference_evidence_mismatch(
        error: PrepareStandardApplicationError,
        expected_function: CheckedFunctionId,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::FunctionTypeReferenceMismatch { .. }
        ));
        if let PrepareStandardApplicationError::FunctionTypeReferenceMismatch { function } = &error
        {
            assert_eq!(*function, expected_function);
        }
        assert_eq!(
            error.to_string(),
            format!(
                "the checked function type references do not match function {expected_function}"
            )
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_check_not_complete(error: PrepareStandardApplicationError, expected_count: usize) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::CheckNotComplete { .. }
        ));
        if let PrepareStandardApplicationError::CheckNotComplete { diagnostic_count } = &error {
            assert_eq!(*diagnostic_count, expected_count);
        }
        assert_eq!(
            error.to_string(),
            format!("the standard application check has {expected_count} diagnostics")
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_expected_base_mismatch(
        error: PrepareStandardApplicationError,
        expected_expected: RevisionPair,
        expected_active: RevisionPair,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::ExpectedBaseMismatch { .. }
        ));
        if let PrepareStandardApplicationError::ExpectedBaseMismatch { expected, active } = &error {
            assert_eq!(*expected, expected_expected);
            assert_eq!(*active, expected_active);
        }
        assert_eq!(
            error.to_string(),
            "the expected application base does not match the active revision"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_checked_base_mismatch(
        error: PrepareStandardApplicationError,
        expected_checked: CatalogueRevisionId,
        expected_active: CatalogueRevisionId,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::CheckedBaseMismatch { .. }
        ));
        if let PrepareStandardApplicationError::CheckedBaseMismatch { checked, active } = &error {
            assert_eq!(*checked, expected_checked);
            assert_eq!(*active, expected_active);
        }
        assert_eq!(
            error.to_string(),
            "the checked application base does not match the active revision"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_standard_library_unavailable(error: PrepareStandardApplicationError) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::StandardLibraryUnavailable
        ));
        assert_eq!(
            error.to_string(),
            "the active database has no standard library"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_standard_catalogue_mismatch(
        error: PrepareStandardApplicationError,
        expected_checked: CatalogueRevisionId,
        expected_active: CatalogueRevisionId,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::StandardCatalogueMismatch { .. }
        ));
        if let PrepareStandardApplicationError::StandardCatalogueMismatch { checked, active } =
            &error
        {
            assert_eq!(*checked, expected_checked);
            assert_eq!(*active, expected_active);
        }
        assert_eq!(
            error.to_string(),
            "the checked standard catalogue does not match the active standard catalogue"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_standard_revision_mismatch(
        error: PrepareStandardApplicationError,
        expected_checked: StandardLibraryRevisionId,
        expected_active: StandardLibraryRevisionId,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::StandardRevisionMismatch { .. }
        ));
        if let PrepareStandardApplicationError::StandardRevisionMismatch { checked, active } =
            &error
        {
            assert_eq!(*checked, expected_checked);
            assert_eq!(*active, expected_active);
        }
        assert_eq!(
            error.to_string(),
            "the checked standard library revision does not match the active standard library revision"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    fn assert_standard_digest_mismatch(
        error: PrepareStandardApplicationError,
        expected_checked: Sha256Digest,
        expected_active: Sha256Digest,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::StandardDigestMismatch { .. }
        ));
        if let PrepareStandardApplicationError::StandardDigestMismatch { checked, active } = &error
        {
            assert_eq!(*checked, expected_checked);
            assert_eq!(*active, expected_active);
        }
        assert_eq!(
            error.to_string(),
            "the checked standard library digest does not match the active standard library digest"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn exposes_the_standard_application_preparation_interface() {
        let _: fn(
            &StandardApplicationCheckReport,
            RevisionPair,
            &ActiveDatabaseRevision,
        ) -> Result<DeployableRevision, PrepareStandardApplicationError> =
            prepare_standard_application;
    }

    #[test]
    fn prepares_a_standard_backed_server_only_application_as_version_two() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle =
            SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA app;")]).unwrap();
        let report = check_standard_application(&bundle, &context);

        assert!(report.diagnostics().is_empty());
        assert!(report.checked_bundle().is_some());

        let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();

        assert_eq!(
            prepared.catalogue_hash_context().version(),
            CatalogueHashVersion::Version2
        );
        assert_eq!(
            prepared
                .catalogue_hash_context()
                .standard()
                .unwrap()
                .digest(),
            verified.digest()
        );
        assert_eq!(prepared.current_function_revisions(), Some([].as_slice()));
        assert_eq!(prepared.candidate().schemas().len(), 1);
    }

    #[test]
    fn prepares_nullable_and_required_unique_text_fields_as_version_two_values() {
        let verified = verified_canonical_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.contact AS OBJECT (email TEXT UNIQUE, name TEXT NOT NULL UNIQUE);",
        )])
        .unwrap();

        let report = check_standard_application(&bundle, &context);
        assert!(report.diagnostics().is_empty());

        let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
        let contact = prepared
            .candidate()
            .object_type_by_name(&semantic_name(["crm", "contact"]))
            .unwrap();
        let email = contact.field_by_name("email").unwrap();
        let name = contact.field_by_name("name").unwrap();
        let text = ResolvedType::Value(TypeId::from_bytes(CANONICAL_TYPE_IDS[5]));

        assert_eq!(email.resolved_type(), text);
        assert!(email.nullable());
        assert!(email.unique());
        assert_eq!(name.resolved_type(), text);
        assert!(!name.nullable());
        assert!(name.unique());
    }

    #[test]
    fn prepares_a_checked_client_boolean_constant() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        assert_eq!(standard.value_types().len(), 1);
        assert_ne!(
            standard.value_types()[0].id(),
            TypeId::from_bytes(CANONICAL_TYPE_IDS[0]),
            "the fixture must retain a self-consistent non-golden Boolean identity"
        );
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let source =
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let report = check_standard_application(&bundle, &context);

        assert!(report.diagnostics().is_empty());
        assert!(report.checked_bundle().is_some());
        let prepared = prepare_standard_application_with_allocator(
            &report,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap();
        assert_eq!(prepared.candidate().functions().len(), 1);
        let function = &prepared.candidate().functions()[0];
        assert_eq!(function.name().to_string(), "app.enabled");
        assert_eq!(function.domain(), FunctionDomain::Client);
        assert_eq!(function.parameters(), []);
        assert_eq!(
            function.return_type(),
            &FunctionReturn::Single(ResolvedType::Value(TypeId::from_bytes([3; 16])))
        );
        assert_eq!(prepared.new_function_revisions().len(), 1);
        let revision = &prepared.new_function_revisions()[0];
        assert_eq!(revision.function(), function.id());
        assert_eq!(function.current_revision(), revision.id());
        assert_eq!(
            revision.semantic_hash_version(),
            FunctionSemanticHashVersion::Version2
        );
        assert_eq!(revision.language_version(), "orna.language/1");
        assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Client);
        assert_eq!(revision.artifact().format(), "orna.client-plan");
        assert_eq!(revision.artifact().version(), 1);
        assert_eq!(
            revision.artifact().payload(),
            b"ORNACP\0\0\0\0\0\x01\x01\x01"
        );
        assert_eq!(prepared.references().len(), 1);
        let reference = &prepared.references()[0];
        assert_eq!(reference.source_function(), function.id());
        assert_eq!(reference.source_revision(), revision.id());
        assert_eq!(reference.ordinal(), 0);
        assert_eq!(reference.kind(), DefinitionReferenceKind::NamedType);
        assert_eq!(
            reference.target(),
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
        );
        let return_start = source.find("BOOLEAN");
        assert!(return_start.is_some());
        let return_start = return_start.unwrap_or_default();
        assert_eq!(
            reference.source_origin().byte_start(),
            u32::try_from(return_start).unwrap_or_default()
        );
        assert_eq!(
            reference.source_origin().byte_end(),
            u32::try_from(return_start + "BOOLEAN".len()).unwrap_or_default()
        );
        assert_eq!(
            reference.source_origin().source_unit(),
            prepared.source().units()[0].id()
        );

        let declaration_start = source.find("CREATE CLIENT FUNCTION").unwrap_or_default();
        let client_origins = prepared
            .origins()
            .iter()
            .filter(|origin| origin.identity() == DefinitionIdentity::Function(function.id()))
            .collect::<Vec<_>>();
        assert_eq!(client_origins.len(), 1);
        assert_eq!(
            client_origins[0].source(),
            SourceOrigin::new(
                prepared.source().units()[0].id(),
                u32::try_from(declaration_start).unwrap_or_default(),
                u32::try_from(source.len()).unwrap_or_default(),
            )
            .unwrap(),
        );
        assert!(prepared.origins().iter().all(|origin| {
            !matches!(
                origin.identity(),
                DefinitionIdentity::Parameter { owner, .. }
                    | DefinitionIdentity::FunctionReturnColumn { owner, .. }
                    if owner == function.id()
            )
        }));
    }

    #[test]
    fn standard_preparation_reuses_the_lowest_historical_client_true_revision() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let true_source =
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
        let false_source =
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN FALSE;";
        let true_bundle =
            SourceBundle::new([SourceUnit::new("application.orna", true_source)]).unwrap();
        let false_bundle =
            SourceBundle::new([SourceUnit::new("application.orna", false_source)]).unwrap();

        let initial = empty_version_two_active(&verified);
        let initial_context =
            StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
        let first_true_report = check_standard_application(&true_bundle, &initial_context);
        assert_eq!(first_true_report.diagnostics(), &[]);
        let first_true =
            prepare_standard_application(&first_true_report, initial.pair(), &initial).unwrap();
        assert_eq!(first_true.new_function_revisions().len(), 1);
        let true_revision = first_true.new_function_revisions()[0].clone();
        assert_eq!(
            true_revision.semantic_hash_version(),
            FunctionSemanticHashVersion::Version2
        );
        assert_eq!(
            first_true.references()[0].target(),
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
        );
        assert_eq!(
            true_revision.artifact().payload(),
            b"ORNACP\0\0\0\0\0\x01\x01\x01"
        );

        let true_active = active_from_prepared_standard_candidate(&first_true, Vec::new());
        let true_context =
            StandardApplicationCheckContext::try_new(true_active.catalogue(), &standard).unwrap();
        let false_report = check_standard_application(&false_bundle, &true_context);
        assert_eq!(false_report.diagnostics(), &[]);
        let false_prepared =
            prepare_standard_application(&false_report, true_active.pair(), &true_active).unwrap();
        assert_eq!(false_prepared.new_function_revisions().len(), 1);
        assert_ne!(
            false_prepared.new_function_revisions()[0].id(),
            true_revision.id()
        );
        assert_eq!(
            false_prepared.references()[0].target(),
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
        );
        assert_ne!(
            false_prepared.new_function_revisions()[0].semantic_hash(),
            true_revision.semantic_hash()
        );

        let false_active =
            active_from_prepared_standard_candidate(&false_prepared, vec![true_revision.clone()]);
        let false_context =
            StandardApplicationCheckContext::try_new(false_active.catalogue(), &standard).unwrap();
        let reused_true_report = check_standard_application(&true_bundle, &false_context);
        assert_eq!(reused_true_report.diagnostics(), &[]);
        let reused_true =
            prepare_standard_application(&reused_true_report, false_active.pair(), &false_active)
                .unwrap();

        assert_eq!(reused_true.new_function_revisions(), &[]);
        let current_revisions = reused_true.current_function_revisions();
        assert!(current_revisions.is_some());
        let current_revisions = current_revisions.unwrap_or_default();
        assert_eq!(current_revisions.len(), 1);
        assert_eq!(current_revisions[0].id(), true_revision.id());
        assert_eq!(current_revisions[0].revision_number(), 1);
        assert_eq!(
            current_revisions[0].artifact().payload(),
            b"ORNACP\0\0\0\0\0\x01\x01\x01"
        );
        assert_eq!(
            current_revisions[0].semantic_hash(),
            true_revision.semantic_hash()
        );
        let function = &reused_true.candidate().functions()[0];
        assert_eq!(function.id(), true_revision.function());
        assert_eq!(function.current_revision(), true_revision.id());
        assert_eq!(reused_true.references().len(), 1);
        assert_eq!(reused_true.references()[0].source_function(), function.id());
        assert_eq!(
            reused_true.references()[0].source_revision(),
            true_revision.id()
        );
    }

    #[test]
    fn standard_preparation_reuses_client_boolean_across_formatting_and_spelling() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let initial = empty_version_two_active(&verified);
        let initial_context =
            StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
        let canonical = SourceBundle::new([SourceUnit::new(
            "canonical.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let canonical_report = check_standard_application(&canonical, &initial_context);
        assert_eq!(canonical_report.diagnostics(), &[]);
        let initial_prepared =
            prepare_standard_application(&canonical_report, initial.pair(), &initial).unwrap();
        let initial_revision = initial_prepared.new_function_revisions()[0].clone();
        let active = active_from_prepared_standard_candidate(&initial_prepared, Vec::new());
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let equivalent = SourceBundle::new([SourceUnit::new(
            "formatted.orna",
            "CREATE SCHEMA app;\n\nCREATE CLIENT FUNCTION app.enabled() RETURNS std.BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = check_standard_application(&equivalent, &context);
        assert_eq!(report.diagnostics(), &[]);
        let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();

        assert_eq!(prepared.new_function_revisions(), &[]);
        let current = prepared.current_function_revisions().unwrap_or_default();
        assert_eq!(current, [initial_revision]);
        assert_eq!(prepared.references().len(), 1);
        assert_eq!(
            prepared.references()[0].target(),
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
        );
    }

    fn assert_no_standard_preparation_allocations() {
        for counter in [
            &PREPARE_CATALOGUE_ALLOCATIONS,
            &PREPARE_BUNDLE_ALLOCATIONS,
            &PREPARE_REVISION_ALLOCATIONS,
            &PREPARE_UNIT_ALLOCATIONS,
            &PREPARE_SCHEMA_ALLOCATIONS,
            &PREPARE_TYPE_ALLOCATIONS,
        ] {
            assert_eq!(
                counter.load(Ordering::SeqCst),
                0,
                "CLIENT Gate 11 must reject before allocating a candidate identity"
            );
        }
    }

    fn assert_client_gate_eleven_failure(
        error: PrepareStandardApplicationError,
        reason: &'static str,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::Prepare {
                source: PrepareError::InvalidCheckedBundle { .. }
            }
        ));
        if let PrepareStandardApplicationError::Prepare { source } = &error {
            assert!(matches!(
                source,
                PrepareError::InvalidCheckedBundle { reason: actual } if *actual == reason
            ));
            assert_eq!(source.to_string(), reason);
        }
        assert_eq!(
            error.to_string(),
            format!("the standard application could not be prepared: {reason}")
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    fn assert_existing_function_mismatch(
        error: PrepareStandardApplicationError,
        expected: FunctionId,
    ) {
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::Prepare {
                source: PrepareError::ExistingDefinitionMismatch {
                    definition: DefinitionIdentity::Function(id),
                }
            } if *id == expected
        ));
        assert_eq!(
            error.to_string(),
            "the standard application could not be prepared: existing checked definition differs from active catalogue"
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    #[derive(Clone, Copy)]
    enum HostileClientFact {
        Domain,
        Parameter,
        Return,
        Security,
        Transaction,
        Volatility,
        Body,
        Reference,
    }

    impl HostileClientFact {
        const fn reason(self) -> &'static str {
            match self {
                Self::Domain => "checked CLIENT function has an unsupported domain",
                Self::Parameter => "checked CLIENT function declares parameters",
                Self::Return => {
                    "checked CLIENT function does not return BOOLEAN from the checked standard library"
                }
                Self::Security => "checked CLIENT function has an unsupported security mode",
                Self::Transaction => "checked CLIENT function has an unsupported transaction mode",
                Self::Volatility => "checked CLIENT function has an unsupported volatility mode",
                Self::Body => "checked CLIENT function has an unsupported body",
                Self::Reference => {
                    "checked CLIENT function contains unsupported application definition references"
                }
            }
        }

        fn apply(self, report: &mut StandardApplicationCheckReport) -> bool {
            match self {
                Self::Domain => report.replace_first_client_domain_for_test(FunctionDomain::Server),
                Self::Parameter => report.append_first_client_parameter_for_test(),
                Self::Return => report.replace_first_client_return_with_integer_for_test(),
                Self::Security => {
                    report.replace_first_client_security_for_test(FunctionSecurity::Definer)
                }
                Self::Transaction => report
                    .replace_first_client_transaction_for_test(Some(FunctionTransaction::Atomic)),
                Self::Volatility => {
                    report.replace_first_client_volatility_for_test(FunctionVolatility::Stable)
                }
                Self::Body => report.replace_first_client_body_with_unsupported_for_test(),
                Self::Reference => report.append_first_client_reference_for_test(),
            }
        }
    }

    #[test]
    fn standard_preparation_rejects_every_client_gate_eleven_fact_before_allocation() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);

        for fact in [
            HostileClientFact::Domain,
            HostileClientFact::Parameter,
            HostileClientFact::Return,
            HostileClientFact::Security,
            HostileClientFact::Transaction,
            HostileClientFact::Volatility,
            HostileClientFact::Body,
            HostileClientFact::Reference,
        ] {
            let mut hostile = report.clone();
            let changed = fact.apply(&mut hostile);
            assert!(changed);

            let error = prepare_standard_application_with_allocator(
                &hostile,
                active.pair(),
                &active,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();
            assert_client_gate_eleven_failure(error, fact.reason());
            assert_no_standard_preparation_allocations();
        }
    }

    #[test]
    fn standard_preparation_orders_every_adjacent_client_gate_eleven_pair_before_allocation() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);
        let facts = [
            HostileClientFact::Domain,
            HostileClientFact::Parameter,
            HostileClientFact::Return,
            HostileClientFact::Security,
            HostileClientFact::Transaction,
            HostileClientFact::Volatility,
            HostileClientFact::Body,
            HostileClientFact::Reference,
        ];

        for pair in facts.windows(2) {
            let mut hostile = report.clone();
            assert!(pair[0].apply(&mut hostile));
            assert!(pair[1].apply(&mut hostile));
            let error = prepare_standard_application_with_allocator(
                &hostile,
                active.pair(),
                &active,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();
            assert_client_gate_eleven_failure(error, pair[0].reason());
            assert_no_standard_preparation_allocations();
        }
    }

    #[test]
    fn standard_preparation_orders_gate_ten_and_common_preflight_before_client_semantics() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);

        let mut gate_ten = report.clone();
        assert!(
            gate_ten.replace_standard_type_reference_for_test(
                0,
                CheckedFunctionId::Existing(FunctionId::from_bytes([0xc1; 16])),
                0,
                TypeId::from_bytes([3; 16]),
                report.checked_bundle().unwrap().standard_type_references()[0]
                    .location()
                    .clone(),
            )
        );
        assert!(gate_ten.replace_first_client_body_with_unsupported_for_test());
        let error = prepare_standard_application_with_allocator(
            &gate_ten,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_function_reference_evidence_mismatch(
            error,
            report.checked_bundle().unwrap().standard_type_references()[0].owner(),
        );
        assert_no_standard_preparation_allocations();

        let retained_owner = CheckedFunctionId::Existing(FunctionId::from_bytes([0xc2; 16]));
        let mut retained_client_return = report.clone();
        assert!(retained_client_return.replace_first_client_id_for_test(retained_owner));
        assert!(retained_client_return.replace_first_client_body_with_unsupported_for_test());
        let error = prepare_standard_application_with_allocator(
            &retained_client_return,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_function_reference_evidence_mismatch(error, retained_owner);
        assert_no_standard_preparation_allocations();

        let mut common_preflight = report.clone();
        assert!(common_preflight.replace_first_client_location_for_test(
            super::SourceLocation::from_syntax(
                "missing.orna",
                &orna_syntax::SourceSpan { start: 0, end: 1 },
            ),
        ));
        assert!(common_preflight.replace_first_client_body_with_unsupported_for_test());
        let error = prepare_standard_application_with_allocator(
            &common_preflight,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::Prepare {
                source: PrepareError::InvalidSourceLocation {
                    logical_path,
                    byte_start: 0,
                    byte_end: 1,
                }
            } if logical_path == "missing.orna"
        ));
        assert_eq!(
            error.to_string(),
            "the standard application could not be prepared: checked source location is invalid"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert_no_standard_preparation_allocations();
    }

    #[test]
    fn standard_preparation_materialises_exact_client_return_evidence_at_gate_ten() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);
        let checked = report.checked_bundle().unwrap();
        let client = checked.client_functions().next().unwrap();
        let owner = client.id();
        let reference = checked.standard_type_references()[0].clone();
        let return_location = reference.location().clone();

        let mut hostile_cases = Vec::new();
        let mut missing = report.clone();
        assert!(missing.replace_standard_type_references_for_test(Vec::new()));
        hostile_cases.push((missing, owner));

        let mut extra = report.clone();
        assert!(
            extra.replace_standard_type_references_for_test(vec![
                reference.clone(),
                reference.clone(),
            ])
        );
        hostile_cases.push((extra, owner));

        let mut wrong_owner = report.clone();
        assert!(wrong_owner.replace_standard_type_reference_for_test(
            0,
            CheckedFunctionId::Existing(FunctionId::from_bytes([0xc3; 16])),
            0,
            reference.target(),
            return_location.clone(),
        ));
        hostile_cases.push((wrong_owner, owner));

        let mut wrong_ordinal = report.clone();
        assert!(wrong_ordinal.replace_standard_type_reference_for_test(
            0,
            owner,
            1,
            reference.target(),
            return_location.clone(),
        ));
        hostile_cases.push((wrong_ordinal, owner));

        let mut wrong_target = report.clone();
        assert!(wrong_target.replace_standard_type_reference_for_test(
            0,
            owner,
            0,
            TypeId::from_bytes([0xc4; 16]),
            return_location.clone(),
        ));
        hostile_cases.push((wrong_target, owner));

        let mut wrong_reference_location = report.clone();
        assert!(
            wrong_reference_location.replace_standard_type_reference_for_test(
                0,
                owner,
                0,
                reference.target(),
                super::SourceLocation::from_syntax(
                    "other.orna",
                    &orna_syntax::SourceSpan { start: 0, end: 1 },
                ),
            )
        );
        hostile_cases.push((wrong_reference_location, owner));

        let mut wrong_class = report.clone();
        assert!(wrong_class.replace_first_client_return_kind_for_test(
            CheckedTypeUseKind::Parameter {
                owner,
                parameter: CheckedParameterId::Existing(orna_core::ParameterId::from_bytes(
                    [0xc5; 16]
                )),
            },
        ));
        hostile_cases.push((wrong_class, owner));

        let mut wrong_kind_ordinal = report.clone();
        assert!(
            wrong_kind_ordinal.replace_first_client_return_kind_for_test(
                CheckedTypeUseKind::Return { owner, ordinal: 1 },
            )
        );
        hostile_cases.push((wrong_kind_ordinal, owner));

        let mut wrong_retained_target = report.clone();
        assert!(
            wrong_retained_target
                .replace_first_client_return_type_id_for_test(TypeId::from_bytes([0xc6; 16]))
        );
        hostile_cases.push((wrong_retained_target, owner));

        let mut wrong_retained_location = report.clone();
        assert!(
            wrong_retained_location.replace_first_client_return_use_location_for_test(
                super::SourceLocation::from_syntax(
                    "other.orna",
                    &orna_syntax::SourceSpan { start: 0, end: 1 },
                ),
            )
        );
        hostile_cases.push((wrong_retained_location, owner));

        for (hostile, expected_owner) in hostile_cases {
            let error = prepare_standard_application_with_allocator(
                &hostile,
                active.pair(),
                &active,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();
            assert_function_reference_evidence_mismatch(error, expected_owner);
            assert_no_standard_preparation_allocations();
        }

        let two_clients = SourceBundle::new([SourceUnit::new(
            "two-clients.orna",
            "CREATE SCHEMA app; \
             CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN TRUE; \
             CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN FALSE;",
        )])
        .unwrap();
        let ordered_report = check_standard_application(&two_clients, &context);
        assert_eq!(ordered_report.diagnostics(), &[]);
        let ordered = ordered_report.checked_bundle().unwrap();
        let expected_owner = ordered.standard_type_references()[0].owner();
        let mut reordered = ordered_report.clone();
        let mut references = ordered.standard_type_references().to_vec();
        references.swap(0, 1);
        assert!(reordered.replace_standard_type_references_for_test(references));
        let error = prepare_standard_application_with_allocator(
            &reordered,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_function_reference_evidence_mismatch(error, expected_owner);
        assert_no_standard_preparation_allocations();
    }

    #[test]
    fn standard_preparation_validates_every_gate_eleven_location_in_nested_order() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (flag BOOLEAN NOT NULL DEFAULT TRUE); \
            CREATE SERVER FUNCTION app.read(p_ref REF app.item) RETURNS ROWS (item REF app.item) \
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT REF(i) FROM app.item i WHERE REF(i) = p_ref; \
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);
        let invalid_location = super::SourceLocation::from_syntax(
            "missing.orna",
            &orna_syntax::SourceSpan { start: 0, end: 1 },
        );

        for selector in [
            "schema",
            "object",
            "field",
            "default",
            "server",
            "server parameter",
            "server return",
            "server reference",
            "client",
            "client parameter",
            "client return",
            "client body",
            "client reference",
        ] {
            let mut hostile = report.clone();
            if selector == "client parameter" {
                assert!(hostile.append_first_client_parameter_for_test());
            }
            if selector == "client reference" {
                assert!(hostile.append_first_client_reference_for_test());
            }
            assert!(hostile.replace_standard_preparation_location_for_test(
                selector,
                invalid_location.clone(),
            ));
            let error = prepare_standard_application_with_allocator(
                &hostile,
                active.pair(),
                &active,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();
            assert!(matches!(
                &error,
                PrepareStandardApplicationError::Prepare {
                    source: PrepareError::InvalidSourceLocation {
                        logical_path,
                        byte_start: 0,
                        byte_end: 1,
                    }
                } if logical_path == "missing.orna"
            ));
            assert_eq!(
                error.to_string(),
                "the standard application could not be prepared: checked source location is invalid"
            );
            assert!(std::error::Error::source(&error).is_some());
            assert_no_standard_preparation_allocations();
        }

        let selectors = [
            "schema",
            "object",
            "field",
            "default",
            "server",
            "server parameter",
            "server return",
            "server reference",
            "client",
            "client parameter",
            "client return",
            "client body",
            "client reference",
        ];
        for (index, pair) in selectors.windows(2).enumerate() {
            let mut hostile = report.clone();
            if pair.contains(&"client parameter") {
                assert!(hostile.append_first_client_parameter_for_test());
            }
            if pair.contains(&"client reference") {
                assert!(hostile.append_first_client_reference_for_test());
            }
            let first_location = super::SourceLocation::from_syntax(
                "first-missing.orna",
                &orna_syntax::SourceSpan {
                    start: index,
                    end: index + 1,
                },
            );
            let second_location = super::SourceLocation::from_syntax(
                "second-missing.orna",
                &orna_syntax::SourceSpan {
                    start: index + 20,
                    end: index + 21,
                },
            );
            assert!(
                hostile.replace_standard_preparation_location_for_test(pair[0], first_location,)
            );
            assert!(
                hostile.replace_standard_preparation_location_for_test(pair[1], second_location,)
            );
            let error = prepare_standard_application_with_allocator(
                &hostile,
                active.pair(),
                &active,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();
            assert!(matches!(
                &error,
                PrepareStandardApplicationError::Prepare {
                    source: PrepareError::InvalidSourceLocation {
                        logical_path,
                        byte_start,
                        byte_end,
                    }
                } if logical_path == "first-missing.orna"
                    && *byte_start == index
                    && *byte_end == index + 1
            ));
            assert_eq!(
                error.to_string(),
                "the standard application could not be prepared: checked source location is invalid"
            );
            assert!(std::error::Error::source(&error).is_some());
            assert_no_standard_preparation_allocations();
        }

        let mut nested_precedence = report;
        assert!(
            nested_precedence.replace_standard_preparation_location_for_test(
                "schema",
                invalid_location.clone(),
            )
        );
        assert!(nested_precedence.replace_first_client_body_with_unsupported_for_test());
        let error = prepare_standard_application_with_allocator(
            &nested_precedence,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::Prepare {
                source: PrepareError::InvalidSourceLocation { logical_path, .. }
            } if logical_path == "missing.orna"
        ));
        assert_no_standard_preparation_allocations();
    }

    #[test]
    fn standard_preparation_orders_server_continuity_client_order_and_owner_completeness() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) RETURNS ROWS (item REF app.item) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT REF(item) FROM app.item item WHERE REF(item) = p_ref; \
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);
        let server_id = FunctionId::from_bytes([0xc2; 16]);

        let mut server_before_client = report.clone();
        assert!(
            server_before_client
                .replace_first_server_id_for_test(CheckedFunctionId::Existing(server_id))
        );
        assert!(server_before_client.replace_first_client_body_with_unsupported_for_test());
        let error = prepare_standard_application_with_allocator(
            &server_before_client,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_existing_function_mismatch(error, server_id);
        assert_no_standard_preparation_allocations();

        let checked = report.checked_bundle().unwrap();
        let client_id = checked.client_functions().next().unwrap().id();
        let mut client_semantics_before_duplicate = report.clone();
        assert!(client_semantics_before_duplicate.replace_first_server_id_for_test(client_id));
        assert!(
            client_semantics_before_duplicate.replace_first_client_body_with_unsupported_for_test()
        );
        let error = prepare_standard_application_with_allocator(
            &client_semantics_before_duplicate,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_client_gate_eleven_failure(error, "checked CLIENT function has an unsupported body");
        assert_no_standard_preparation_allocations();

        let client_source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN TRUE; \
            CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN FALSE;";
        let client_bundle =
            SourceBundle::new([SourceUnit::new("clients.orna", client_source)]).unwrap();
        let client_report = check_standard_application(&client_bundle, &context);
        assert_eq!(client_report.diagnostics(), &[]);
        let mut first_before_second = client_report.clone();
        assert!(first_before_second.replace_first_client_body_with_unsupported_for_test());
        assert!(first_before_second.replace_client_domain_for_test(1, FunctionDomain::Server));
        let error = prepare_standard_application_with_allocator(
            &first_before_second,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_client_gate_eleven_failure(error, "checked CLIENT function has an unsupported body");
        assert_no_standard_preparation_allocations();

        let mut duplicate_domain = report.clone();
        assert!(duplicate_domain.replace_first_server_id_for_test(client_id));
        let error = prepare_standard_application_with_allocator(
            &duplicate_domain,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_client_gate_eleven_failure(error, "duplicate checked function");
        assert_no_standard_preparation_allocations();

        let server_only_source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) RETURNS ROWS (item REF app.item) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT REF(item) FROM app.item item WHERE REF(item) = p_ref;";
        let server_only_bundle =
            SourceBundle::new([SourceUnit::new("server.orna", server_only_source)]).unwrap();
        let server_only = check_standard_application(&server_only_bundle, &context);
        assert_eq!(server_only.diagnostics(), &[]);
        let mut owner_mismatch = server_only.clone();
        assert!(owner_mismatch.remove_first_server_declaration_evidence_for_test());
        let error = prepare_standard_application_with_allocator(
            &owner_mismatch,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_client_gate_eleven_failure(
            error,
            "checked standard function owners do not match declaration evidence",
        );
        assert_no_standard_preparation_allocations();
    }

    #[test]
    fn standard_preparation_validates_existing_server_parameters_before_client_semantics() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let initial = empty_version_two_active(&verified);
        let initial_context =
            StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
        let server_source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN) \
            RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
            AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);";
        let server_bundle =
            SourceBundle::new([SourceUnit::new("server.orna", server_source)]).unwrap();
        let initial_report = check_standard_application(&server_bundle, &initial_context);
        assert_eq!(initial_report.diagnostics(), &[]);
        let prepared =
            prepare_standard_application(&initial_report, initial.pair(), &initial).unwrap();
        let active = active_from_prepared_standard_candidate(&prepared, Vec::new());
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let mixed_source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN) \
            RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
            AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made); \
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
        let mixed_bundle =
            SourceBundle::new([SourceUnit::new("mixed.orna", mixed_source)]).unwrap();
        let report = check_standard_application(&mixed_bundle, &context);
        assert_eq!(report.diagnostics(), &[]);

        let function = active.catalogue().functions()[0].id();
        let parameter = active.catalogue().functions()[0].parameters()[0].id();
        let mut hostile = report;
        assert!(hostile.replace_server_parameter_name_for_test(0, "first-renamed".to_owned()));
        assert!(hostile.replace_server_parameter_name_for_test(1, "second-renamed".to_owned()));
        assert!(hostile.replace_first_client_body_with_unsupported_for_test());
        let error = prepare_standard_application_with_allocator(
            &hostile,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::Prepare {
                source: PrepareError::ExistingDefinitionMismatch {
                    definition: DefinitionIdentity::Parameter { owner, parameter: actual },
                }
            } if *owner == function && *actual == parameter
        ));
        assert_eq!(
            error.to_string(),
            "the standard application could not be prepared: existing checked definition differs from active catalogue"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert_no_standard_preparation_allocations();
    }

    #[test]
    fn standard_preparation_checks_both_active_function_domain_directions_and_name_continuity() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let initial = empty_version_two_active(&verified);
        let initial_context =
            StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
        let client_source =
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
        let client_bundle =
            SourceBundle::new([SourceUnit::new("client.orna", client_source)]).unwrap();
        let client_report = check_standard_application(&client_bundle, &initial_context);
        assert_eq!(client_report.diagnostics(), &[]);
        let prepared_client =
            prepare_standard_application(&client_report, initial.pair(), &initial).unwrap();
        let client_active = active_from_prepared_standard_candidate(&prepared_client, Vec::new());
        let active_client_id = client_active.catalogue().functions()[0].id();

        let server_source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) RETURNS ROWS (item REF app.item) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT REF(item) FROM app.item item WHERE REF(item) = p_ref;";
        let server_bundle =
            SourceBundle::new([SourceUnit::new("server.orna", server_source)]).unwrap();
        let client_context =
            StandardApplicationCheckContext::try_new(client_active.catalogue(), &standard).unwrap();
        let server_report = check_standard_application(&server_bundle, &client_context);
        assert_eq!(server_report.diagnostics(), &[]);
        let mut server_as_client = server_report.clone();
        assert!(
            server_as_client
                .replace_first_server_id_for_test(CheckedFunctionId::Existing(active_client_id))
        );
        let error = prepare_standard_application_with_allocator(
            &server_as_client,
            client_active.pair(),
            &client_active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_existing_function_mismatch(error, active_client_id);
        assert_no_standard_preparation_allocations();

        let prepared_server =
            prepare_standard_application(&server_report, client_active.pair(), &client_active)
                .unwrap();
        let server_active = active_from_prepared_standard_candidate(&prepared_server, Vec::new());
        let active_server_id = server_active.catalogue().functions()[0].id();
        let server_context =
            StandardApplicationCheckContext::try_new(server_active.catalogue(), &standard).unwrap();
        let client_report = check_standard_application(&client_bundle, &server_context);
        assert_eq!(client_report.diagnostics(), &[]);
        let mut client_as_server = client_report.clone();
        assert!(
            client_as_server.replace_first_client_id_with_evidence_for_test(
                CheckedFunctionId::Existing(active_server_id)
            )
        );
        let error = prepare_standard_application_with_allocator(
            &client_as_server,
            server_active.pair(),
            &server_active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_existing_function_mismatch(error, active_server_id);
        assert_no_standard_preparation_allocations();

        let client_context =
            StandardApplicationCheckContext::try_new(client_active.catalogue(), &standard).unwrap();
        let existing_client_report = check_standard_application(&client_bundle, &client_context);
        assert_eq!(existing_client_report.diagnostics(), &[]);
        let mut renamed_client = existing_client_report.clone();
        assert!(
            renamed_client.replace_first_client_name_for_test(semantic_name(["app", "renamed",]))
        );
        let error = prepare_standard_application_with_allocator(
            &renamed_client,
            client_active.pair(),
            &client_active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_existing_function_mismatch(error, active_client_id);
        assert_no_standard_preparation_allocations();

        let ordered_client_source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE; \
            CREATE CLIENT FUNCTION app.later() RETURNS BOOLEAN RETURN FALSE;";
        let ordered_client_bundle = SourceBundle::new([SourceUnit::new(
            "ordered-clients.orna",
            ordered_client_source,
        )])
        .unwrap();
        let ordered_client_report =
            check_standard_application(&ordered_client_bundle, &client_context);
        assert_eq!(ordered_client_report.diagnostics(), &[]);
        let mut first_continuity_before_second = ordered_client_report.clone();
        assert!(
            first_continuity_before_second
                .replace_first_client_name_for_test(semantic_name(["app", "renamed",]))
        );
        assert!(
            first_continuity_before_second
                .replace_client_domain_for_test(1, FunctionDomain::Server)
        );
        let error = prepare_standard_application_with_allocator(
            &first_continuity_before_second,
            client_active.pair(),
            &client_active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_existing_function_mismatch(error, active_client_id);
        assert_no_standard_preparation_allocations();
    }

    #[test]
    fn standard_preparation_orders_the_first_seven_gates() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let valid_bundle =
            SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA app;")]).unwrap();
        let valid_report = check_standard_application(&valid_bundle, &context);
        assert!(valid_report.diagnostics().is_empty());

        let incomplete_bundle =
            SourceBundle::new([SourceUnit::new("invalid.orna", "CREATE SCHEMA ;")]).unwrap();
        let incomplete = check_standard_application(&incomplete_bundle, &context);
        let incomplete_expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes([0xe1; 16]),
            active.pair().catalogue(),
        );
        let error = prepare_standard_application(&incomplete, incomplete_expected_base, &active)
            .unwrap_err();
        assert_check_not_complete(error, incomplete.diagnostics().len());

        let mut wrong_base_after_expected = valid_report.clone();
        assert!(
            wrong_base_after_expected.replace_base_catalogue_revision_for_test(
                CatalogueRevisionId::from_bytes([0xe2; 16])
            )
        );
        let wrong_expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes([0xe3; 16]),
            active.pair().catalogue(),
        );
        let error =
            prepare_standard_application(&wrong_base_after_expected, wrong_expected_base, &active)
                .unwrap_err();
        assert_expected_base_mismatch(error, wrong_expected_base, active.pair());

        let mut wrong_base = valid_report.clone();
        assert!(
            wrong_base.replace_base_catalogue_revision_for_test(CatalogueRevisionId::from_bytes(
                [0xe4; 16]
            ))
        );
        let no_standard = empty_version_one_active();
        let error = prepare_standard_application(&wrong_base, no_standard.pair(), &no_standard)
            .unwrap_err();
        assert_checked_base_mismatch(
            error,
            CatalogueRevisionId::from_bytes([0xe4; 16]),
            no_standard.pair().catalogue(),
        );

        let mut report_without_standard = valid_report.clone();
        assert!(
            report_without_standard
                .replace_base_catalogue_revision_for_test(no_standard.pair().catalogue())
        );
        let error = prepare_standard_application(
            &report_without_standard,
            no_standard.pair(),
            &no_standard,
        )
        .unwrap_err();
        assert_standard_library_unavailable(error);

        let mut wrong_catalogue = valid_report.clone();
        assert!(wrong_catalogue.replace_standard_context_for_test(
            CatalogueRevisionId::from_bytes([0xe5; 16]),
            StandardLibraryRevisionId::from_bytes([0xe6; 16]),
            Sha256Digest::from_bytes([0xe7; 32]),
        ));
        let error =
            prepare_standard_application(&wrong_catalogue, active.pair(), &active).unwrap_err();
        assert_standard_catalogue_mismatch(
            error,
            CatalogueRevisionId::from_bytes([0xe5; 16]),
            verified.catalogue().revision(),
        );

        let mut wrong_revision = valid_report.clone();
        assert!(wrong_revision.replace_standard_context_for_test(
            verified.catalogue().revision(),
            StandardLibraryRevisionId::from_bytes([0xe8; 16]),
            Sha256Digest::from_bytes([0xe9; 32]),
        ));
        let error =
            prepare_standard_application(&wrong_revision, active.pair(), &active).unwrap_err();
        assert_standard_revision_mismatch(
            error,
            StandardLibraryRevisionId::from_bytes([0xe8; 16]),
            verified.revision(),
        );

        let mut wrong_digest = valid_report;
        assert!(wrong_digest.replace_standard_context_for_test(
            verified.catalogue().revision(),
            verified.revision(),
            Sha256Digest::from_bytes([0xea; 32]),
        ));
        let error =
            prepare_standard_application(&wrong_digest, active.pair(), &active).unwrap_err();
        assert_standard_digest_mismatch(
            error,
            Sha256Digest::from_bytes([0xea; 32]),
            verified.digest(),
        );
    }

    #[test]
    fn standard_preparation_retries_reserved_candidate_ids_before_hash_construction() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; \
             CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL); \
             CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN) \
             TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f;",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);

        let prepared = prepare_standard_application_with_allocator(
            &report,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap();

        for counter in [
            &PREPARE_CATALOGUE_ALLOCATIONS,
            &PREPARE_BUNDLE_ALLOCATIONS,
            &PREPARE_REVISION_ALLOCATIONS,
            &PREPARE_UNIT_ALLOCATIONS,
            &PREPARE_SCHEMA_ALLOCATIONS,
            &PREPARE_TYPE_ALLOCATIONS,
        ] {
            assert_eq!(counter.load(Ordering::SeqCst), 2);
        }
        assert_eq!(prepared.candidate().revision().to_bytes(), [0x81; 16]);
        assert_eq!(prepared.source().bundle().to_bytes(), [0x82; 16]);
        assert_eq!(prepared.source().id().to_bytes(), [0x83; 16]);
        assert_eq!(prepared.source().units()[0].id().to_bytes(), [0x84; 16]);
        assert_eq!(
            prepared.candidate().schemas()[0].id().to_bytes(),
            [0x85; 16]
        );
        assert_eq!(
            prepared.candidate().object_types()[0].id().to_bytes(),
            [0x86; 16]
        );
    }

    #[test]
    fn standard_preparation_keeps_reference_only_function_hashes_at_version_one() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; \
             CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL); \
             CREATE SERVER FUNCTION app.read(p_flag REF app.flag) RETURNS ROWS (value REF app.flag) \
             TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT REF(f) FROM app.flag f WHERE REF(f) = p_flag;",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);

        let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();

        assert_eq!(prepared.new_function_revisions().len(), 1);
        assert_eq!(
            prepared.new_function_revisions()[0].semantic_hash_version(),
            orna_core::revision::FunctionSemanticHashVersion::Version1
        );
        assert!(prepared.references().iter().all(|reference| {
            !matches!(
                reference.target(),
                orna_core::revision::DefinitionReferenceTarget::ValueType(_)
            )
        }));
        assert_eq!(
            prepared.references()[..2]
                .iter()
                .map(|reference| reference.kind())
                .collect::<Vec<_>>(),
            vec![
                orna_core::revision::DefinitionReferenceKind::ObjectReference,
                orna_core::revision::DefinitionReferenceKind::ObjectReference,
            ]
        );
        assert_eq!(
            prepared.references()[..2]
                .iter()
                .map(|reference| reference.ordinal())
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn standard_preparation_retains_checked_value_type_references() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f WHERE f.value = TRUE;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let report = check_standard_application(&bundle, &context);

        assert!(report.diagnostics().is_empty());
        let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
        let standard_boolean = TypeId::from_bytes([3; 16]);
        assert_eq!(
            prepared.candidate().object_types()[0].fields()[0].resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        assert_eq!(
            (
                prepared.references()[0].kind(),
                prepared.references()[0].target(),
            ),
            (
                orna_core::revision::DefinitionReferenceKind::NamedType,
                orna_core::revision::DefinitionReferenceTarget::ValueType(standard_boolean),
            )
        );
        assert_eq!(
            prepared.new_function_revisions()[0].semantic_hash_version(),
            orna_core::revision::FunctionSemanticHashVersion::Version2
        );
    }

    #[test]
    fn standard_preparation_lowers_sealed_signature_references_before_body_references() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let first_source = "CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN, p_alias std.BOOLEAN) \
             RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
             AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);";
        let second_source = "CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) \
             RETURNS ROWS (visible std.BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE \
             AS SELECT TRUE FROM app.item item WHERE REF(item) = p_ref;";
        let declarations_source =
            "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);";
        let bundle = SourceBundle::new([
            SourceUnit::new("z-first-server.orna", first_source),
            SourceUnit::new("a-second-server.orna", second_source),
            SourceUnit::new("m-declarations.orna", declarations_source),
        ])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);
        let checked_bundle = report.checked_bundle();
        assert!(checked_bundle.is_some());
        let checked = checked_bundle.expect("a diagnostic-free report has a checked bundle");
        let checked_functions = checked.server_functions().collect::<Vec<_>>();
        assert_eq!(checked_functions.len(), 2);
        let standard_boolean = TypeId::from_bytes([3; 16]);
        let first_boolean = first_source.find("p_boolean BOOLEAN").unwrap() + "p_boolean ".len();
        let first_alias = first_source.find("p_alias std.BOOLEAN").unwrap() + "p_alias ".len();
        let second_boolean = second_source.find("visible std.BOOLEAN").unwrap() + "visible ".len();
        let sealed_references = checked.standard_type_references();
        assert_eq!(sealed_references.len(), 3);
        assert_eq!(
            sealed_references
                .iter()
                .map(|reference| {
                    (
                        reference.owner(),
                        reference.ordinal(),
                        reference.target(),
                        reference.location().logical_path(),
                        reference.location().span().start(),
                        reference.location().span().end(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    checked_functions[0].id(),
                    1,
                    standard_boolean,
                    "z-first-server.orna",
                    first_boolean,
                    first_boolean + "BOOLEAN".len(),
                ),
                (
                    checked_functions[0].id(),
                    2,
                    standard_boolean,
                    "z-first-server.orna",
                    first_alias,
                    first_alias + "std.BOOLEAN".len(),
                ),
                (
                    checked_functions[1].id(),
                    1,
                    standard_boolean,
                    "a-second-server.orna",
                    second_boolean,
                    second_boolean + "std.BOOLEAN".len(),
                ),
            ]
        );

        let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
        assert_eq!(
            prepared.catalogue_hash_context().version(),
            CatalogueHashVersion::Version2
        );
        assert!(orna_core::revision::validate_persistable_catalogue(&prepared).is_ok());
        let candidate_functions = prepared.candidate().functions();
        assert_eq!(candidate_functions.len(), 2);
        assert_eq!(
            candidate_functions
                .iter()
                .map(|function| function.name().to_string())
                .collect::<Vec<_>>(),
            vec!["app.create", "app.by_ref"]
        );
        let checked_object_types = checked.object_types().collect::<Vec<_>>();
        assert_eq!(checked_object_types.len(), 1);
        let durable_item = prepared.candidate().object_types()[0].id();
        let source_unit = |path| {
            prepared
                .source()
                .units()
                .iter()
                .find(|unit| unit.logical_path() == path)
                .expect("the prepared source retains every submitted unit")
                .id()
        };
        let first_unit = source_unit("z-first-server.orna");
        let second_unit = source_unit("a-second-server.orna");
        let first_ref = first_source.find("p_ref REF app.item").unwrap() + "p_ref REF ".len();
        let first_return =
            first_source.find("created REF app.item").unwrap() + "created REF ".len();
        let first_body = first_source.find("INSERT INTO app.item").unwrap() + "INSERT INTO ".len();
        let second_ref = second_source.find("p_ref REF app.item").unwrap() + "p_ref REF ".len();
        let second_body = second_source.find("FROM app.item").unwrap() + "FROM ".len();
        let object_target =
            orna_core::revision::DefinitionReferenceTarget::ObjectType(durable_item);
        let value_target =
            orna_core::revision::DefinitionReferenceTarget::ValueType(standard_boolean);
        let object_kind = orna_core::revision::DefinitionReferenceKind::ObjectReference;
        let value_kind = orna_core::revision::DefinitionReferenceKind::NamedType;
        let byte = |offset: usize| u32::try_from(offset).unwrap();
        let reference_details = |function: FunctionId| {
            prepared
                .references()
                .iter()
                .filter(|reference| reference.source_function() == function)
                .map(|reference| {
                    (
                        reference.ordinal(),
                        reference.target(),
                        reference.kind(),
                        reference.source_origin().source_unit(),
                        reference.source_origin().byte_start(),
                        reference.source_origin().byte_end(),
                    )
                })
                .collect::<Vec<_>>()
        };

        let first_references = reference_details(candidate_functions[0].id());
        let first_prefix = vec![
            (
                0,
                object_target,
                object_kind,
                first_unit,
                byte(first_ref),
                byte(first_ref + "app.item".len()),
            ),
            (
                1,
                value_target,
                value_kind,
                first_unit,
                byte(first_boolean),
                byte(first_boolean + "BOOLEAN".len()),
            ),
            (
                2,
                value_target,
                value_kind,
                first_unit,
                byte(first_alias),
                byte(first_alias + "std.BOOLEAN".len()),
            ),
            (
                3,
                object_target,
                object_kind,
                first_unit,
                byte(first_return),
                byte(first_return + "app.item".len()),
            ),
        ];
        assert!(first_references.len() > first_prefix.len());
        assert_eq!(first_references[..first_prefix.len()], first_prefix);
        assert_eq!(
            (
                first_references[first_prefix.len()].0,
                first_references[first_prefix.len()].3,
                first_references[first_prefix.len()].4,
                first_references[first_prefix.len()].5,
            ),
            (
                u32::try_from(first_prefix.len()).unwrap(),
                first_unit,
                byte(first_body),
                byte(first_body + "app.item".len()),
            ),
            "body references must begin after the complete interleaved signature prefix"
        );

        let second_references = reference_details(candidate_functions[1].id());
        let second_prefix = vec![
            (
                0,
                object_target,
                object_kind,
                second_unit,
                byte(second_ref),
                byte(second_ref + "app.item".len()),
            ),
            (
                1,
                value_target,
                value_kind,
                second_unit,
                byte(second_boolean),
                byte(second_boolean + "std.BOOLEAN".len()),
            ),
        ];
        assert!(second_references.len() > second_prefix.len());
        assert_eq!(second_references[..second_prefix.len()], second_prefix);
        assert_eq!(
            (
                second_references[second_prefix.len()].0,
                second_references[second_prefix.len()].3,
                second_references[second_prefix.len()].4,
                second_references[second_prefix.len()].5,
            ),
            (
                u32::try_from(second_prefix.len()).unwrap(),
                second_unit,
                byte(second_body),
                byte(second_body + "app.item".len()),
            ),
            "the second function body must begin after its signature prefix"
        );
    }

    #[test]
    fn standard_preparation_drives_declaration_body_and_reference_evidence_gates() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app;\
             CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);\
             CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN, p_alias std.BOOLEAN) \
             RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
             AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);
        let checked = report.checked_bundle().unwrap();
        let canonical_uses = checked.uses().to_vec();
        let canonical_references = checked.standard_type_references().to_vec();
        let server_functions = checked.server_functions().collect::<Vec<_>>();
        assert_eq!(server_functions.len(), 1);
        let body_function = server_functions[0].id();
        let declaration_value_index = canonical_uses
            .iter()
            .position(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Field { .. }
                        | CheckedTypeUseKind::Parameter { .. }
                        | CheckedTypeUseKind::Return { .. }
                ) && type_use.value().is_some()
            })
            .unwrap();
        let declaration_reference_index = canonical_uses
            .iter()
            .position(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Field { .. }
                        | CheckedTypeUseKind::Parameter { .. }
                        | CheckedTypeUseKind::Return { .. }
                ) && type_use.object_reference().is_some()
            })
            .unwrap();
        let body_value_index = canonical_uses
            .iter()
            .position(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
                ) && type_use.value().is_some()
            })
            .unwrap();
        let body_reference_index = canonical_uses
            .iter()
            .position(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
                ) && type_use.object_reference().is_some()
            })
            .unwrap();

        let mut declaration_and_body_hostile = report.clone();
        let mut uses = canonical_uses.clone();
        let direct_index = uses
            .iter()
            .position(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Field { .. }
                        | CheckedTypeUseKind::Parameter { .. }
                        | CheckedTypeUseKind::Return { .. }
                )
            })
            .unwrap();
        uses.remove(direct_index);
        let mut body_indices = uses
            .iter()
            .enumerate()
            .filter_map(|(index, type_use)| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
                )
                .then_some(index)
            })
            .collect::<Vec<_>>();
        body_indices.reverse();
        assert!(body_indices.len() >= 2);
        uses.swap(body_indices[0], body_indices[1]);
        assert!(declaration_and_body_hostile.replace_type_uses_for_test(uses));
        let mut references = canonical_references.clone();
        references.swap(0, 1);
        assert!(declaration_and_body_hostile.replace_standard_type_references_for_test(references));
        let error =
            prepare_standard_application(&declaration_and_body_hostile, active.pair(), &active)
                .unwrap_err();
        assert_declaration_evidence_mismatch(error, canonical_uses[direct_index].kind());

        let mut reordered_declarations = report.clone();
        let mut uses = canonical_uses.clone();
        uses.swap(declaration_value_index, declaration_reference_index);
        assert!(reordered_declarations.replace_type_uses_for_test(uses));
        let error = prepare_standard_application(&reordered_declarations, active.pair(), &active)
            .unwrap_err();
        assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

        let mut wrong_declaration_kind = report.clone();
        assert!(wrong_declaration_kind.replace_type_use_kind_for_test(
            declaration_value_index,
            canonical_uses[body_value_index].kind(),
        ));
        let error = prepare_standard_application(&wrong_declaration_kind, active.pair(), &active)
            .unwrap_err();
        assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

        let mut wrong_declaration_type = report.clone();
        assert!(wrong_declaration_type.replace_value_type_id_for_test(
            declaration_value_index,
            TypeId::from_bytes([0xd1; 16]),
        ));
        let error = prepare_standard_application(&wrong_declaration_type, active.pair(), &active)
            .unwrap_err();
        assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

        let mut gate_seven_before_eight = wrong_declaration_type.clone();
        let hostile_digest = Sha256Digest::from_bytes([0xd0; 32]);
        assert!(gate_seven_before_eight.replace_standard_context_for_test(
            verified.catalogue().revision(),
            verified.revision(),
            hostile_digest,
        ));
        let error = prepare_standard_application(&gate_seven_before_eight, active.pair(), &active)
            .unwrap_err();
        assert_standard_digest_mismatch(error, hostile_digest, verified.digest());

        let mut wrong_declaration_target = report.clone();
        assert!(
            wrong_declaration_target.replace_object_reference_target_for_test(
                declaration_reference_index,
                CheckedTypeId::Existing(TypeId::from_bytes([0xd2; 16])),
            )
        );
        let error = prepare_standard_application(&wrong_declaration_target, active.pair(), &active)
            .unwrap_err();
        assert_declaration_evidence_mismatch(
            error,
            canonical_uses[declaration_reference_index].kind(),
        );

        let mut wrong_declaration_location = report.clone();
        assert!(
            wrong_declaration_location.replace_type_use_location_for_test(
                declaration_value_index,
                canonical_uses[declaration_reference_index]
                    .location()
                    .clone(),
            )
        );
        let error =
            prepare_standard_application(&wrong_declaration_location, active.pair(), &active)
                .unwrap_err();
        assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

        let mut wrong_declaration_class = report.clone();
        assert!(
            wrong_declaration_class.replace_value_with_object_reference_for_test(
                declaration_value_index,
                CheckedTypeId::Existing(TypeId::from_bytes([0xd3; 16])),
            )
        );
        let error = prepare_standard_application(&wrong_declaration_class, active.pair(), &active)
            .unwrap_err();
        assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

        let mut duplicate_declaration = report.clone();
        let mut uses = canonical_uses.clone();
        uses.push(canonical_uses[direct_index].clone());
        assert!(duplicate_declaration.replace_type_uses_for_test(uses));
        let error = prepare_standard_application(&duplicate_declaration, active.pair(), &active)
            .unwrap_err();
        assert_declaration_evidence_mismatch(error, canonical_uses[direct_index].kind());

        let mut body_hostile = report.clone();
        let mut uses = canonical_uses.clone();
        let mut body_indices = uses
            .iter()
            .enumerate()
            .filter_map(|(index, type_use)| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
                )
                .then_some(index)
            })
            .collect::<Vec<_>>();
        body_indices.reverse();
        assert!(body_indices.len() >= 2);
        uses.swap(body_indices[0], body_indices[1]);
        assert!(body_hostile.replace_type_uses_for_test(uses));
        let error =
            prepare_standard_application(&body_hostile, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut body_before_declaration = report.clone();
        let mut uses = canonical_uses.clone();
        let body = uses.remove(body_value_index);
        uses.insert(direct_index, body);
        assert!(body_before_declaration.replace_type_uses_for_test(uses));
        let error = prepare_standard_application(&body_before_declaration, active.pair(), &active)
            .unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut declaration_after_body = report.clone();
        let mut uses = canonical_uses.clone();
        let last_declaration_index = uses
            .iter()
            .enumerate()
            .filter_map(|(index, type_use)| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Field { .. }
                        | CheckedTypeUseKind::Parameter { .. }
                        | CheckedTypeUseKind::Return { .. }
                )
                .then_some(index)
            })
            .next_back()
            .unwrap();
        let declaration = uses.remove(last_declaration_index);
        uses.insert(body_value_index, declaration);
        assert!(declaration_after_body.replace_type_uses_for_test(uses));
        let error = prepare_standard_application(&declaration_after_body, active.pair(), &active)
            .unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut gate_nine_before_ten = body_hostile.clone();
        let mut references = canonical_references.clone();
        references.swap(0, 1);
        assert!(gate_nine_before_ten.replace_standard_type_references_for_test(references));
        let error = prepare_standard_application(&gate_nine_before_ten, active.pair(), &active)
            .unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut wrong_body_type = report.clone();
        assert!(
            wrong_body_type
                .replace_value_type_id_for_test(body_value_index, TypeId::from_bytes([0xd4; 16]),)
        );
        let error =
            prepare_standard_application(&wrong_body_type, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut wrong_body_target = report.clone();
        assert!(wrong_body_target.replace_object_reference_target_for_test(
            body_reference_index,
            CheckedTypeId::Existing(TypeId::from_bytes([0xd5; 16])),
        ));
        let error =
            prepare_standard_application(&wrong_body_target, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut wrong_body_location = report.clone();
        assert!(wrong_body_location.replace_type_use_location_for_test(
            body_value_index,
            canonical_uses[body_reference_index].location().clone(),
        ));
        let error =
            prepare_standard_application(&wrong_body_location, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut wrong_body_class = report.clone();
        assert!(
            wrong_body_class.replace_value_with_object_reference_for_test(
                body_value_index,
                CheckedTypeId::Existing(TypeId::from_bytes([0xd6; 16])),
            )
        );
        let error =
            prepare_standard_application(&wrong_body_class, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut wrong_body_kind = report.clone();
        assert!(wrong_body_kind.replace_type_use_kind_for_test(
            body_value_index,
            canonical_uses[body_reference_index].kind(),
        ));
        let error =
            prepare_standard_application(&wrong_body_kind, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut empty_body = report.clone();
        let uses = canonical_uses
            .iter()
            .filter(|type_use| {
                !matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
                )
            })
            .cloned()
            .collect();
        assert!(empty_body.replace_type_uses_for_test(uses));
        let error = prepare_standard_application(&empty_body, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut extra_body = report.clone();
        let mut uses = canonical_uses.clone();
        let extra = uses
            .iter()
            .find(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
                )
            })
            .cloned()
            .unwrap();
        uses.push(extra);
        assert!(extra_body.replace_type_uses_for_test(uses));
        let error = prepare_standard_application(&extra_body, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, body_function);

        let mut references_hostile = report.clone();
        let mut references = canonical_references.clone();
        assert_eq!(references.len(), 2);
        references.swap(0, 1);
        assert!(references_hostile.replace_standard_type_references_for_test(references));
        let error =
            prepare_standard_application(&references_hostile, active.pair(), &active).unwrap_err();
        assert_function_reference_evidence_mismatch(error, canonical_references[0].owner());

        let mut wrong_reference = report.clone();
        let first_reference = &checked.standard_type_references()[0];
        assert!(wrong_reference.replace_standard_type_reference_for_test(
            0,
            CheckedFunctionId::Existing(FunctionId::from_bytes([0xd7; 16])),
            first_reference.ordinal() + 1,
            TypeId::from_bytes([0xd8; 16]),
            canonical_uses[body_value_index].location().clone(),
        ));
        let error =
            prepare_standard_application(&wrong_reference, active.pair(), &active).unwrap_err();
        assert_function_reference_evidence_mismatch(error, first_reference.owner());

        let mut missing_reference = report.clone();
        let mut references = checked.standard_type_references().to_vec();
        references.pop();
        assert!(missing_reference.replace_standard_type_references_for_test(references));
        let error =
            prepare_standard_application(&missing_reference, active.pair(), &active).unwrap_err();
        assert_function_reference_evidence_mismatch(error, first_reference.owner());
    }

    #[test]
    fn standard_preparation_checks_relational_mutation_and_client_body_uses_before_client_staging()
    {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; \
             CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL); \
             CREATE SERVER FUNCTION app.read(p_task REF app.task) RETURNS ROWS (done BOOLEAN) \
             TRANSACTION READ ONLY VOLATILITY STABLE \
             AS SELECT task.done FROM app.task task WHERE REF(task) = p_task; \
             CREATE SERVER FUNCTION app.create(p_done BOOLEAN) RETURNS ROWS (created REF app.task) \
             TRANSACTION ATOMIC \
             AS INSERT INTO app.task AS made (done) VALUES (p_done) RETURNING REF(made); \
             CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);
        let checked_bundle = report.checked_bundle();
        assert!(
            checked_bundle.is_some(),
            "a diagnostic-free standard application report must contain a checked bundle"
        );
        let checked = checked_bundle.expect("the asserted checked bundle must be present");
        let servers = checked.server_functions().collect::<Vec<_>>();
        let clients = checked.client_functions().collect::<Vec<_>>();
        assert_eq!(servers.len(), 2);
        assert_eq!(clients.len(), 1);
        let read = servers[0];
        let create = servers[1];
        let enabled = clients[0];
        let uses = checked.uses();
        let value_body_index = |owner| {
            uses.iter().position(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Expression { owner: actual, .. }
                        | CheckedTypeUseKind::Result { owner: actual, .. }
                        if actual == owner
                ) && type_use.value().is_some()
            })
        };
        let read_body = value_body_index(read.id())
            .expect("the relational function must retain a value body use");
        let create_body = value_body_index(create.id())
            .expect("the mutation function must retain a value body use");
        let client_body = value_body_index(enabled.id())
            .expect("the CLIENT function must retain a value body use");

        for (index, owner) in [
            (read_body, read.id()),
            (create_body, create.id()),
            (client_body, enabled.id()),
        ] {
            let mut hostile = report.clone();
            assert!(hostile.replace_value_type_id_for_test(index, TypeId::from_bytes([0xdd; 16])));
            let error = prepare_standard_application(&hostile, active.pair(), &active).unwrap_err();
            assert_body_evidence_mismatch(error, owner);
        }
    }

    #[test]
    fn standard_preparation_preserves_multi_unit_signature_references_and_mixed_owner_order() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        assert_eq!(standard.value_types()[0].id(), TypeId::from_bytes([3; 16]));
        assert_ne!(
            standard.value_types()[0].id(),
            TypeId::from_bytes(CANONICAL_TYPE_IDS[0])
        );
        let active = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let first_server = "CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN, p_alias std.BOOLEAN) \
            RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
            AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);";
        let client = "CREATE CLIENT FUNCTION app.enabled() RETURNS std.BOOLEAN RETURN TRUE;";
        let second_server = "CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) \
            RETURNS ROWS (value std.BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT TRUE FROM app.item item WHERE REF(item) = p_ref;";
        let declarations = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);";
        let bundle = SourceBundle::new([
            SourceUnit::new("z-first-server.orna", first_server),
            SourceUnit::new("a-client.orna", client),
            SourceUnit::new("y-second-server.orna", second_server),
            SourceUnit::new("m-declarations.orna", declarations),
        ])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        assert_eq!(report.diagnostics(), &[]);
        let checked_bundle = report.checked_bundle();
        assert!(
            checked_bundle.is_some(),
            "a diagnostic-free standard application report must contain a checked bundle"
        );
        let checked = checked_bundle.expect("the asserted checked bundle must be present");
        let references = checked.standard_type_references().to_vec();
        assert_eq!(references.len(), 4);
        assert_eq!(
            references
                .iter()
                .map(|reference| (reference.ordinal(), reference.location().logical_path()))
                .collect::<Vec<_>>(),
            vec![
                (1, "z-first-server.orna"),
                (2, "z-first-server.orna"),
                (0, "a-client.orna"),
                (1, "y-second-server.orna"),
            ],
            "reference order follows source-unit insertion order and preserves REF ordinal gaps"
        );
        let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
        assert_eq!(
            prepared
                .candidate()
                .functions()
                .iter()
                .map(|function| (function.name().to_string(), function.domain()))
                .collect::<Vec<_>>(),
            vec![
                ("app.create".to_owned(), FunctionDomain::Server),
                ("app.enabled".to_owned(), FunctionDomain::Client),
                ("app.by_ref".to_owned(), FunctionDomain::Server),
            ],
            "CLIENT and SERVER lowering follows canonical declaration-evidence owner order"
        );
        assert_eq!(
            prepared.catalogue_hash_context().version(),
            CatalogueHashVersion::Version2
        );
        let candidate_object = &prepared.candidate().object_types()[0];
        let candidate_item = candidate_object.id();
        assert_eq!(
            candidate_object.fields()[0].resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        let candidate_functions = prepared.candidate().functions();
        let create = &candidate_functions[0];
        assert!(matches!(
            create.parameters()[0].resolved_type(),
            ResolvedType::Reference { target } if target == candidate_item
        ));
        assert_eq!(
            create.parameters()[1].resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        assert_eq!(
            create.parameters()[2].resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        let FunctionReturn::Rows(create_columns) = create.return_type() else {
            panic!("the mutation fixture must retain a ROWS return")
        };
        assert!(matches!(
            create_columns[0].resolved_type(),
            ResolvedType::Reference { target } if target == candidate_item
        ));
        assert_eq!(
            candidate_functions[1].return_type(),
            &FunctionReturn::Single(ResolvedType::Value(TypeId::from_bytes([3; 16])))
        );
        let by_ref = &candidate_functions[2];
        assert!(matches!(
            by_ref.parameters()[0].resolved_type(),
            ResolvedType::Reference { target } if target == candidate_item
        ));
        let FunctionReturn::Rows(by_ref_columns) = by_ref.return_type() else {
            panic!("the SERVER fixture must retain a ROWS return")
        };
        assert_eq!(
            by_ref_columns[0].resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        let value_reference_targets = prepared
            .references()
            .iter()
            .filter_map(|reference| {
                if let DefinitionReferenceTarget::ValueType(type_id) = reference.target() {
                    Some(type_id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(value_reference_targets.len(), 4);
        assert!(
            value_reference_targets
                .iter()
                .all(|type_id| *type_id == TypeId::from_bytes([3; 16]))
        );
        assert_eq!(
            prepared
                .references()
                .iter()
                .filter_map(|reference| {
                    if !matches!(reference.target(), DefinitionReferenceTarget::ValueType(_)) {
                        return None;
                    }
                    Some((
                        reference.source_function(),
                        reference.ordinal(),
                        reference.kind(),
                        reference.target(),
                    ))
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    candidate_functions[0].id(),
                    1,
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16])),
                ),
                (
                    candidate_functions[0].id(),
                    2,
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16])),
                ),
                (
                    candidate_functions[1].id(),
                    0,
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16])),
                ),
                (
                    candidate_functions[2].id(),
                    1,
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16])),
                ),
            ]
        );
        assert!(orna_core::revision::validate_persistable_catalogue(&prepared).is_ok());
        let candidate_function_ids = prepared
            .candidate()
            .functions()
            .iter()
            .map(|function| function.id())
            .collect::<Vec<_>>();
        let function_origins = prepared
            .origins()
            .iter()
            .filter_map(|origin| match origin.identity() {
                DefinitionIdentity::Function(id) => Some(id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(function_origins, candidate_function_ids);
        let current = prepared.current_function_revisions().unwrap_or_default();
        assert_eq!(
            current
                .iter()
                .map(|revision| revision.function())
                .collect::<Vec<_>>(),
            candidate_function_ids
        );
        let mut reference_groups = Vec::new();
        for reference in prepared.references() {
            if reference_groups.last().copied() != Some(reference.source_function()) {
                reference_groups.push(reference.source_function());
            }
        }
        assert_eq!(reference_groups, candidate_function_ids);

        let first = &references[0];
        for (owner, ordinal, target, location) in [
            (
                CheckedFunctionId::Existing(FunctionId::from_bytes([0xde; 16])),
                first.ordinal(),
                first.target(),
                first.location().clone(),
            ),
            (
                first.owner(),
                first.ordinal() + 1,
                first.target(),
                first.location().clone(),
            ),
            (
                first.owner(),
                first.ordinal(),
                TypeId::from_bytes([0xdf; 16]),
                first.location().clone(),
            ),
            (
                first.owner(),
                first.ordinal(),
                first.target(),
                references[1].location().clone(),
            ),
        ] {
            let mut hostile = report.clone();
            assert!(
                hostile
                    .replace_standard_type_reference_for_test(0, owner, ordinal, target, location,)
            );
            let error = prepare_standard_application(&hostile, active.pair(), &active).unwrap_err();
            assert_function_reference_evidence_mismatch(error, first.owner());
        }

        let mut reordered = report.clone();
        let mut hostile_references = references.clone();
        hostile_references.swap(0, 2);
        assert!(reordered.replace_standard_type_references_for_test(hostile_references));
        let error = prepare_standard_application(&reordered, active.pair(), &active).unwrap_err();
        assert_function_reference_evidence_mismatch(error, first.owner());

        let mut missing = report.clone();
        let mut hostile_references = references.clone();
        hostile_references.pop();
        assert!(missing.replace_standard_type_references_for_test(hostile_references));
        let error = prepare_standard_application(&missing, active.pair(), &active).unwrap_err();
        assert_function_reference_evidence_mismatch(error, references[3].owner());

        let mut extra = report.clone();
        let mut hostile_references = references.clone();
        hostile_references.push(hostile_references[0].clone());
        assert!(extra.replace_standard_type_references_for_test(hostile_references));
        let error = prepare_standard_application(&extra, active.pair(), &active).unwrap_err();
        assert_function_reference_evidence_mismatch(error, references[0].owner());
    }

    const CANONICAL_RESERVED_ID: [u8; 16] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01,
    ];
    const CANONICAL_SCHEMA_IDS: [[u8; 16]; 2] = [
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x02,
        ],
    ];
    const CANONICAL_TYPE_IDS: [[u8; 16]; 14] = [
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x02,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x03,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x04,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x05,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x06,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x07,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x08,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x09,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0a,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0b,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0c,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0d,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0e,
        ],
    ];
    const CANONICAL_STANDARD_DIGEST: [u8; 32] = [
        0xbe, 0x61, 0x9c, 0xaa, 0xf6, 0xb2, 0x0b, 0xb7, 0xf8, 0xbc, 0x8d, 0xf9, 0x56, 0xd4, 0x89,
        0xad, 0xe4, 0x9b, 0xc8, 0xdf, 0xe0, 0x3c, 0xd6, 0xd9, 0x64, 0x70, 0x5b, 0x30, 0x23, 0x5b,
        0x08, 0x1d,
    ];

    #[derive(Clone, Copy)]
    struct CanonicalValueTypeFact {
        name: &'static str,
        kind: ValueTypeKind,
        representation_contract: &'static str,
        persistence: ValueTypePersistence,
    }

    const CANONICAL_VALUE_TYPE_FACTS: [CanonicalValueTypeFact; 14] = [
        CanonicalValueTypeFact {
            name: "std.types.boolean",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.boolean@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.integer",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.integer@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.bigint",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.bigint@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.float",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.float@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.decimal",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.decimal@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.character_large_object",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.character-large-object@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.binary_large_object",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.binary-large-object@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.uuid",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.uuid@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.date",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.date@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.time",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.time@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.timestamp",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.timestamp@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.duration",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.duration@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.void",
            kind: ValueTypeKind::Primitive,
            representation_contract: "orna.kernel.value.void@1",
            persistence: ValueTypePersistence::Transient,
        },
        CanonicalValueTypeFact {
            name: "std.types.opaque_token",
            kind: ValueTypeKind::Opaque,
            representation_contract: "orna.std.value.opaque-token@1",
            persistence: ValueTypePersistence::Transient,
        },
    ];

    #[derive(Clone, Copy)]
    enum CanonicalBindingKind {
        Qualified,
        Prelude,
    }

    impl CanonicalBindingKind {
        const fn catalogue_kind(self) -> orna_core::catalogue::TypeBindingKind {
            match self {
                Self::Qualified => orna_core::catalogue::TypeBindingKind::Qualified,
                Self::Prelude => orna_core::catalogue::TypeBindingKind::Prelude,
            }
        }
    }

    #[derive(Clone, Copy)]
    struct CanonicalBindingFact {
        kind: CanonicalBindingKind,
        name: &'static str,
        target_type_index: usize,
    }

    const CANONICAL_BINDING_FACTS: [CanonicalBindingFact; 31] = [
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.boolean",
            target_type_index: 0,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "boolean",
            target_type_index: 0,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "bool",
            target_type_index: 0,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.integer",
            target_type_index: 1,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "integer",
            target_type_index: 1,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "int",
            target_type_index: 1,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.bigint",
            target_type_index: 2,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "bigint",
            target_type_index: 2,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.float",
            target_type_index: 3,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "float",
            target_type_index: 3,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.decimal",
            target_type_index: 4,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "decimal",
            target_type_index: 4,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.character_large_object",
            target_type_index: 5,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "character large object",
            target_type_index: 5,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "text",
            target_type_index: 5,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.binary_large_object",
            target_type_index: 6,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "binary large object",
            target_type_index: 6,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "bytes",
            target_type_index: 6,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.uuid",
            target_type_index: 7,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "uuid",
            target_type_index: 7,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.date",
            target_type_index: 8,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "date",
            target_type_index: 8,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.time",
            target_type_index: 9,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "time",
            target_type_index: 9,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.timestamp",
            target_type_index: 10,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "timestamp",
            target_type_index: 10,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.duration",
            target_type_index: 11,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "duration",
            target_type_index: 11,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.void",
            target_type_index: 12,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "void",
            target_type_index: 12,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.opaque_token",
            target_type_index: 13,
        },
    ];

    const CANONICAL_BINDING_IDS: [[u8; 16]; 31] = [
        [
            0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
            0x4d, 0x31,
        ],
        [
            0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8,
            0x15, 0xaa,
        ],
        [
            0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61, 0x11,
            0x0c, 0xad,
        ],
        [
            0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2, 0xdf,
            0x25, 0x93,
        ],
        [
            0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf, 0x18,
            0x57, 0x78,
        ],
        [
            0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e, 0x7c,
            0x58, 0x89,
        ],
        [
            0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed, 0x5b,
            0x0d, 0x1e,
        ],
        [
            0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b, 0xaa,
            0x5e, 0x88,
        ],
        [
            0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71, 0x9e,
            0xfc, 0xb5,
        ],
        [
            0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87, 0x97,
            0xb5, 0xe6,
        ],
        [
            0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8, 0x6b,
            0x45, 0xf9,
        ],
        [
            0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3, 0x10,
            0x6e, 0xd5,
        ],
        [
            0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b, 0xc2,
            0xd8, 0x70,
        ],
        [
            0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6,
            0x89, 0x1a,
        ],
        [
            0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99, 0xf0,
            0xdb, 0xc2,
        ],
        [
            0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c, 0x7d,
            0x81, 0x34,
        ],
        [
            0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d, 0x33,
            0xaf, 0xbf,
        ],
        [
            0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51, 0xb6,
            0x9d, 0xb7,
        ],
        [
            0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f, 0x31,
            0x66, 0x00,
        ],
        [
            0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6, 0x7c,
            0x79, 0x7f,
        ],
        [
            0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1, 0x71,
            0xf7, 0xac,
        ],
        [
            0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e, 0x1f,
            0x93, 0x56,
        ],
        [
            0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8, 0xa0,
            0x34, 0x8d,
        ],
        [
            0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72, 0xd2,
            0x33, 0xd4,
        ],
        [
            0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30, 0xfc,
            0xff, 0x04,
        ],
        [
            0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97, 0x9c,
            0x8a, 0xa1,
        ],
        [
            0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb, 0x62,
            0x14, 0x9a,
        ],
        [
            0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80, 0x2d,
            0xdc, 0x37,
        ],
        [
            0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc, 0xbf,
            0x94, 0x0f,
        ],
        [
            0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a, 0x01,
            0x73, 0xfb,
        ],
        [
            0x4d, 0xab, 0x42, 0x83, 0x03, 0x1f, 0xcd, 0x81, 0xb5, 0x8d, 0x09, 0xd8, 0x87, 0x63,
            0x46, 0xae,
        ],
    ];

    #[derive(Clone, Copy)]
    enum CanonicalDeclarationIdentity {
        Schema(usize),
        ValueType(usize),
        TypeBinding(usize),
    }

    #[derive(Clone, Copy)]
    struct CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity,
        byte_start: u32,
        byte_end: u32,
    }

    const CANONICAL_DECLARATION_FACTS: [CanonicalDeclarationFact; 47] = [
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::Schema(0),
            byte_start: 0,
            byte_end: 18,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::Schema(1),
            byte_start: 19,
            byte_end: 43,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(0),
            byte_start: 45,
            byte_end: 174,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(0),
            byte_start: 176,
            byte_end: 221,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(1),
            byte_start: 223,
            byte_end: 269,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(2),
            byte_start: 270,
            byte_end: 313,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(1),
            byte_start: 315,
            byte_end: 444,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(3),
            byte_start: 446,
            byte_end: 491,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(4),
            byte_start: 493,
            byte_end: 539,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(5),
            byte_start: 540,
            byte_end: 582,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(2),
            byte_start: 584,
            byte_end: 711,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(6),
            byte_start: 713,
            byte_end: 756,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(7),
            byte_start: 758,
            byte_end: 802,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(3),
            byte_start: 804,
            byte_end: 929,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(8),
            byte_start: 931,
            byte_end: 972,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(9),
            byte_start: 974,
            byte_end: 1016,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(4),
            byte_start: 1018,
            byte_end: 1147,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(10),
            byte_start: 1149,
            byte_end: 1194,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(11),
            byte_start: 1196,
            byte_end: 1242,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(5),
            byte_start: 1244,
            byte_end: 1403,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(12),
            byte_start: 1405,
            byte_end: 1480,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(13),
            byte_start: 1482,
            byte_end: 1558,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(14),
            byte_start: 1559,
            byte_end: 1617,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(6),
            byte_start: 1619,
            byte_end: 1772,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(15),
            byte_start: 1774,
            byte_end: 1843,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(16),
            byte_start: 1845,
            byte_end: 1915,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(17),
            byte_start: 1916,
            byte_end: 1972,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(7),
            byte_start: 1974,
            byte_end: 2097,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(18),
            byte_start: 2099,
            byte_end: 2138,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(19),
            byte_start: 2140,
            byte_end: 2180,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(8),
            byte_start: 2182,
            byte_end: 2305,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(20),
            byte_start: 2307,
            byte_end: 2346,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(21),
            byte_start: 2348,
            byte_end: 2388,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(9),
            byte_start: 2390,
            byte_end: 2513,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(22),
            byte_start: 2515,
            byte_end: 2554,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(23),
            byte_start: 2556,
            byte_end: 2596,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(10),
            byte_start: 2598,
            byte_end: 2731,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(24),
            byte_start: 2733,
            byte_end: 2782,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(25),
            byte_start: 2784,
            byte_end: 2834,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(11),
            byte_start: 2836,
            byte_end: 2967,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(26),
            byte_start: 2969,
            byte_end: 3016,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(27),
            byte_start: 3018,
            byte_end: 3066,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(12),
            byte_start: 3068,
            byte_end: 3189,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(28),
            byte_start: 3191,
            byte_end: 3230,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(29),
            byte_start: 3232,
            byte_end: 3272,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(13),
            byte_start: 3274,
            byte_end: 3405,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(30),
            byte_start: 3407,
            byte_end: 3462,
        },
    ];
    const CANONICAL_VALUE_TYPE_ORIGIN_INDICES: [usize; 14] =
        [2, 6, 10, 13, 16, 19, 23, 27, 30, 33, 36, 39, 42, 45];
    const CANONICAL_BINDING_ORIGIN_INDICES: [usize; 31] = [
        3, 4, 5, 7, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22, 24, 25, 26, 28, 29, 31, 32, 34, 35,
        37, 38, 40, 41, 43, 44, 46,
    ];

    #[test]
    fn checks_a_core_verified_non_golden_standard_source() {
        let snapshot = verified_standard_source_fixture();

        let checked = check_standard_library_source(&snapshot).unwrap();

        assert_eq!(
            checked.verified_snapshot().revision(),
            StandardLibraryRevisionId::from_bytes([7; 16])
        );
        assert_eq!(
            checked.verified_snapshot().digest(),
            Sha256Digest::from_bytes(SUCCESS_STANDARD_DIGEST)
        );
        assert_eq!(
            checked.verified_snapshot().source().id(),
            SourceRevisionId::from_bytes([6; 16])
        );
        assert_eq!(
            checked.verified_snapshot().catalogue().revision(),
            CatalogueRevisionId::from_bytes([8; 16])
        );
        assert_eq!(
            checked.verified_snapshot().source().units()[0].content(),
            "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;"
        );

        assert_eq!(checked.schemas().len(), 2);
        assert_eq!(checked.schemas()[0].id(), SchemaId::from_bytes([1; 16]));
        assert_eq!(checked.schemas()[0].name(), &semantic_name(["std"]));
        assert_eq!(checked.schemas()[0].origin(), source_origin(0, 18));
        assert_eq!(checked.schemas()[1].id(), SchemaId::from_bytes([2; 16]));
        assert_eq!(
            checked.schemas()[1].name(),
            &semantic_name(["std", "types"])
        );
        assert_eq!(checked.schemas()[1].origin(), source_origin(18, 42));

        assert_eq!(checked.value_types().len(), 1);
        let value_type = &checked.value_types()[0];
        assert_eq!(value_type.id(), TypeId::from_bytes([3; 16]));
        assert_eq!(
            value_type.name(),
            &semantic_name(["std", "types", "boolean"])
        );
        assert_eq!(
            value_type.kind(),
            orna_core::catalogue::ValueTypeKind::Primitive
        );
        assert_eq!(value_type.mutability(), ValueTypeMutability::Immutable);
        assert_eq!(value_type.persistence(), ValueTypePersistence::Persistable);
        assert_eq!(
            value_type.representation_contract(),
            "orna.kernel.value.boolean@1"
        );
        assert_eq!(value_type.origin(), source_origin(42, 159));

        assert_eq!(checked.type_bindings().len(), 2);
        let qualified = &checked.type_bindings()[0];
        assert_eq!(
            qualified.id().to_bytes(),
            [
                0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
                0x4d, 0x31,
            ]
        );
        assert_eq!(
            qualified.kind(),
            orna_core::catalogue::TypeBindingKind::Qualified
        );
        assert_eq!(
            qualified.name(),
            &orna_core::catalogue::TypeLookupName::qualified(semantic_name(["std", "boolean"]))
        );
        assert_eq!(qualified.target(), TypeId::from_bytes([3; 16]));
        assert_eq!(qualified.origin(), source_origin(159, 204));

        let prelude = &checked.type_bindings()[1];
        assert_eq!(
            prelude.id().to_bytes(),
            [
                0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8,
                0x15, 0xaa,
            ]
        );
        assert_eq!(
            prelude.kind(),
            orna_core::catalogue::TypeBindingKind::Prelude
        );
        assert_eq!(
            prelude.name(),
            &orna_core::catalogue::TypeLookupName::prelude(
                PreludeTypeName::new(["boolean"]).unwrap()
            )
        );
        assert_eq!(prelude.target(), TypeId::from_bytes([3; 16]));
        assert_eq!(prelude.origin(), source_origin(204, 250));

        assert_eq!(checked.schemas()[0].clone(), checked.schemas()[0]);
        assert_eq!(value_type.clone(), *value_type);
        assert_eq!(qualified.clone(), *qualified);
    }

    #[test]
    fn checks_the_accepted_canonical_standard_source() {
        let snapshot = verified_canonical_standard_source_fixture();

        let checked = check_standard_library_source(&snapshot).unwrap();

        assert_eq!(
            checked.verified_snapshot().revision().to_bytes(),
            CANONICAL_RESERVED_ID
        );
        assert_eq!(
            checked
                .verified_snapshot()
                .catalogue()
                .revision()
                .to_bytes(),
            CANONICAL_RESERVED_ID
        );
        assert_eq!(
            checked.verified_snapshot().source().id().to_bytes(),
            CANONICAL_RESERVED_ID
        );
        assert_eq!(
            checked.verified_snapshot().source().units()[0]
                .id()
                .to_bytes(),
            CANONICAL_RESERVED_ID
        );
        assert_eq!(
            checked.verified_snapshot().source().units()[0].logical_path(),
            "std/types.orna"
        );
        assert_eq!(
            checked.verified_snapshot().source().units()[0].content(),
            CANONICAL_STANDARD_SOURCE
        );
        assert_eq!(
            checked.verified_snapshot().digest().to_bytes(),
            CANONICAL_STANDARD_DIGEST
        );
        assert_eq!(CANONICAL_STANDARD_SOURCE.len(), 3463);

        assert_eq!(checked.schemas().len(), 2);
        assert_eq!(checked.value_types().len(), 14);
        assert_eq!(
            checked
                .type_bindings()
                .iter()
                .filter(|binding| binding.kind() == orna_core::catalogue::TypeBindingKind::Qualified)
                .count(),
            14
        );
        assert_eq!(
            checked
                .type_bindings()
                .iter()
                .filter(|binding| binding.kind() == orna_core::catalogue::TypeBindingKind::Prelude)
                .count(),
            17
        );

        for (index, schema) in checked.schemas().iter().enumerate() {
            let expected_name = ["std", "std.types"][index];
            assert_eq!(schema.id().to_bytes(), CANONICAL_SCHEMA_IDS[index]);
            assert_eq!(schema.name().to_string(), expected_name);
            assert_eq!(
                schema.origin(),
                canonical_source_origin(CANONICAL_DECLARATION_FACTS[index])
            );
        }
        for (index, value_type) in checked.value_types().iter().enumerate() {
            let expected = CANONICAL_VALUE_TYPE_FACTS[index];
            assert_eq!(value_type.id().to_bytes(), CANONICAL_TYPE_IDS[index]);
            assert_eq!(value_type.name().to_string(), expected.name);
            assert_eq!(value_type.kind(), expected.kind);
            assert_eq!(value_type.mutability(), ValueTypeMutability::Immutable);
            assert_eq!(value_type.persistence(), expected.persistence);
            assert_eq!(
                value_type.representation_contract(),
                expected.representation_contract
            );
            assert_eq!(
                value_type.origin(),
                canonical_source_origin(
                    CANONICAL_DECLARATION_FACTS[CANONICAL_VALUE_TYPE_ORIGIN_INDICES[index]]
                )
            );
        }
        for (index, binding) in checked.type_bindings().iter().enumerate() {
            let expected = CANONICAL_BINDING_FACTS[index];
            assert_eq!(binding.id().to_bytes(), CANONICAL_BINDING_IDS[index]);
            assert_eq!(binding.kind(), expected.kind.catalogue_kind());
            assert_eq!(binding.name().to_string(), expected.name);
            assert_eq!(
                binding.target().to_bytes(),
                CANONICAL_TYPE_IDS[expected.target_type_index]
            );
            assert_eq!(
                binding.origin(),
                canonical_source_origin(
                    CANONICAL_DECLARATION_FACTS[CANONICAL_BINDING_ORIGIN_INDICES[index]]
                )
            );
        }
    }

    #[test]
    fn checks_an_empty_application_against_a_checked_standard_library() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x2a; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", "")]).unwrap();

        let report = check_standard_application(&bundle, &context);

        assert_eq!(report.diagnostics(), &[]);
        assert_eq!(
            report.standard_library().verified_snapshot().digest(),
            standard.verified_snapshot().digest()
        );
        let checked = report.checked_bundle().unwrap();
        assert_eq!(
            checked.base_catalogue_revision(),
            CatalogueRevisionId::from_bytes([0x2a; 16])
        );
        assert!(checked.uses().is_empty());
        assert!(checked.standard_type_references().is_empty());
    }

    #[test]
    fn checks_one_new_application_against_the_empty_sentinel_catalogue() {
        let standard =
            check_standard_library_source(&verified_canonical_standard_source_fixture()).unwrap();
        let source = "CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_new_application(&bundle, &standard).unwrap();

        assert_eq!(report.diagnostics(), &[]);
        let checked = report.checked_bundle().unwrap();
        assert_eq!(
            checked.base_catalogue_revision(),
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID
        );
        assert_eq!(EMPTY_APPLICATION_CATALOGUE_REVISION_ID.to_bytes(), [0; 16]);
        assert_eq!(checked.object_types().count(), 1);
        assert_eq!(checked.uses().len(), 1);
    }

    #[test]
    fn checks_new_application_source_diagnostics_after_authority_establishment() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let bundle =
            SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA ;")]).unwrap();
        let expected_diagnostics = parse_bundle(&bundle).diagnostics().to_vec();

        let report = check_new_application(&bundle, &standard).unwrap();

        assert_eq!(report.diagnostics(), expected_diagnostics);
        assert!(report.checked_bundle().is_none());
    }

    #[test]
    fn resolves_a_prelude_standard_value_type_and_retains_its_durable_identity() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x2b; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let source = "CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_standard_application(&bundle, &context);

        assert_eq!(report.diagnostics(), &[]);
        let checked = report.checked_bundle().unwrap();
        let objects = checked.object_types().collect::<Vec<_>>();
        assert_eq!(objects.len(), 1);
        let fields = objects[0].fields().collect::<Vec<_>>();
        assert_eq!(fields.len(), 1);
        let resolved = fields[0].resolved_type();
        assert!(resolved.value().is_some());
        let Some(value) = resolved.value() else {
            return;
        };
        assert_eq!(value.type_id(), TypeId::from_bytes([3; 16]));
        assert_eq!(
            value.kind(),
            CheckedTypeUseKind::Field {
                owner: objects[0].id(),
                field: fields[0].id(),
            }
        );
        let start = source.find("BOOLEAN").unwrap();
        assert_eq!(value.location().logical_path(), "application.orna");
        assert_eq!(value.location().span().start(), start);
        assert_eq!(value.location().span().end(), start + "BOOLEAN".len());
    }

    #[test]
    fn records_standard_server_and_client_signature_uses_without_accepting_client_parameters() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x2c; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.flag AS OBJECT (value BOOLEAN);\
            CREATE SERVER FUNCTION app.read(p_flag REF app.flag) RETURNS ROWS (value BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f \
            WHERE REF(f) = p_flag;\
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_standard_application(&bundle, &context);

        assert_eq!(report.diagnostics(), &[]);
        let checked = report.checked_bundle().unwrap();

        let objects = checked.object_types().collect::<Vec<_>>();
        let servers = checked.server_functions().collect::<Vec<_>>();
        let clients = checked.client_functions().collect::<Vec<_>>();
        assert_eq!(objects.len(), 1);
        assert_eq!(servers.len(), 1);
        assert_eq!(clients.len(), 1);

        let fields = objects[0].fields().collect::<Vec<_>>();
        let parameters = servers[0].parameters().collect::<Vec<_>>();
        let columns = servers[0].return_columns().collect::<Vec<_>>();
        assert_eq!(fields.len(), 1);
        assert_eq!(parameters.len(), 1);
        assert_eq!(columns.len(), 1);
        assert_eq!(clients[0].parameters().count(), 0);

        let declaration_uses = checked
            .uses()
            .iter()
            .filter(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Field { .. }
                        | CheckedTypeUseKind::Parameter { .. }
                        | CheckedTypeUseKind::Return { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(declaration_uses.len(), 4);
        assert_eq!(
            declaration_uses
                .iter()
                .map(|type_use| type_use.kind())
                .collect::<Vec<_>>(),
            [
                CheckedTypeUseKind::Field {
                    owner: objects[0].id(),
                    field: fields[0].id(),
                },
                CheckedTypeUseKind::Parameter {
                    owner: servers[0].id(),
                    parameter: parameters[0].id(),
                },
                CheckedTypeUseKind::Return {
                    owner: servers[0].id(),
                    ordinal: 0,
                },
                CheckedTypeUseKind::Return {
                    owner: clients[0].id(),
                    ordinal: 0,
                },
            ]
        );

        assert_eq!(
            fields[0]
                .resolved_type()
                .value()
                .map(CheckedValueTypeUse::type_id),
            Some(TypeId::from_bytes([3; 16]))
        );
        assert!(parameters[0].resolved_type().object_reference().is_some());
        assert_eq!(
            columns[0]
                .resolved_type()
                .value()
                .map(CheckedValueTypeUse::type_id),
            Some(TypeId::from_bytes([3; 16]))
        );
        assert_eq!(
            clients[0]
                .return_type()
                .value()
                .map(CheckedValueTypeUse::type_id),
            Some(TypeId::from_bytes([3; 16]))
        );
        let reference_start = source.find("app.flag) RETURNS").unwrap();
        assert_eq!(
            declaration_uses[1].location().span().start(),
            reference_start
        );

        let debug_values = [
            format!("{report:?}"),
            format!("{checked:?}"),
            format!("{:?}", objects[0]),
            format!("{:?}", fields[0]),
            format!("{:?}", servers[0]),
            format!("{:?}", parameters[0]),
            format!("{:?}", columns[0]),
            format!("{:?}", clients[0]),
        ];
        for rendered in debug_values {
            assert!(!rendered.contains("SemanticType"));
            assert!(!rendered.contains("Scalar"));
        }
    }

    #[test]
    fn preserves_the_client_parameter_diagnostic_with_standard_authority() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x2d; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let source = "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled(value BOOLEAN) RETURNS BOOLEAN RETURN TRUE;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_standard_application(&bundle, &context);
        let legacy_report = check(&bundle, &application);

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].message(),
            "this CLIENT function cannot declare parameters yet"
        );
        assert_eq!(report.diagnostics(), legacy_report.diagnostics());
        assert!(report.checked_bundle().is_none());
    }

    #[test]
    fn resolves_every_accepted_canonical_standard_spelling_to_its_checked_type_id() {
        let standard =
            check_standard_library_source(&verified_canonical_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x2e; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let prelude = [
            ("BOOLEAN", 0),
            ("BOOL", 0),
            ("INTEGER", 1),
            ("INT", 1),
            ("BIGINT", 2),
            ("FLOAT", 3),
            ("DECIMAL", 4),
            ("CHARACTER LARGE OBJECT", 5),
            ("TEXT", 5),
            ("BINARY LARGE OBJECT", 6),
            ("BYTES", 6),
            ("UUID", 7),
            ("DATE", 8),
            ("TIME", 9),
            ("TIMESTAMP", 10),
            ("DURATION", 11),
            ("VOID", 12),
        ];
        let primaries = [
            ("std.types.BOOLEAN", 0),
            ("std.types.INTEGER", 1),
            ("std.types.BIGINT", 2),
            ("std.types.FLOAT", 3),
            ("std.types.DECIMAL", 4),
            ("std.types.CHARACTER_LARGE_OBJECT", 5),
            ("std.types.BINARY_LARGE_OBJECT", 6),
            ("std.types.UUID", 7),
            ("std.types.DATE", 8),
            ("std.types.TIME", 9),
            ("std.types.TIMESTAMP", 10),
            ("std.types.DURATION", 11),
            ("std.types.VOID", 12),
        ];
        let qualified = [
            ("std.BOOLEAN", 0),
            ("std.INTEGER", 1),
            ("std.BIGINT", 2),
            ("std.FLOAT", 3),
            ("std.DECIMAL", 4),
            ("std.CHARACTER_LARGE_OBJECT", 5),
            ("std.BINARY_LARGE_OBJECT", 6),
            ("std.UUID", 7),
            ("std.DATE", 8),
            ("std.TIME", 9),
            ("std.TIMESTAMP", 10),
            ("std.DURATION", 11),
            ("std.VOID", 12),
        ];
        let spellings = prelude
            .into_iter()
            .chain(primaries)
            .chain(qualified)
            .collect::<Vec<_>>();
        let fields = spellings
            .iter()
            .enumerate()
            .map(|(index, (spelling, _))| format!("f{index} {spelling}"))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!("CREATE SCHEMA app; CREATE TYPE app.all_types AS OBJECT ({fields});");
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_standard_application(&bundle, &context);

        assert_eq!(report.diagnostics(), &[]);
        let checked = report.checked_bundle().unwrap();
        assert_eq!(checked.uses().len(), 43);
        for (type_use, (_, expected_type_index)) in checked.uses().iter().zip(spellings) {
            assert!(type_use.value().is_some());
            let Some(value) = type_use.value() else {
                return;
            };
            assert_eq!(
                value.type_id(),
                TypeId::from_bytes(CANONICAL_TYPE_IDS[expected_type_index])
            );
        }
    }

    #[test]
    fn standard_application_context_checks_every_adjacent_gate_precedence_and_error_contract() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();

        let schema_identity_before_schema_name = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x31; 16]),
            vec![
                SchemaDefinition::new(SchemaId::from_bytes([0x32; 16]), semantic_name(["std"])),
                SchemaDefinition::new(
                    SchemaId::from_bytes([2; 16]),
                    semantic_name(["application"]),
                ),
            ],
            Vec::new(),
        )
        .unwrap();
        assert_standard_context_error(
            StandardApplicationCheckContext::try_new(
                &schema_identity_before_schema_name,
                &standard,
            ),
            StandardApplicationContextError::SchemaIdentityConflict {
                id: SchemaId::from_bytes([2; 16]),
            },
            format!(
                "the application catalogue conflicts with standard schema identity {}",
                SchemaId::from_bytes([2; 16])
            ),
        );

        let type_after_schema_name = ValueTypeDefinition::primitive(
            TypeId::from_bytes([3; 16]),
            semantic_name(["application", "flag"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let schema_name_before_type_identity = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x32; 16]),
            vec![
                SchemaDefinition::new(
                    SchemaId::from_bytes([0x33; 16]),
                    semantic_name(["std", "types"]),
                ),
                SchemaDefinition::new(
                    SchemaId::from_bytes([0x34; 16]),
                    semantic_name(["application"]),
                ),
            ],
            Vec::new(),
            vec![type_after_schema_name],
            Vec::new(),
        )
        .unwrap();
        assert_standard_context_error(
            StandardApplicationCheckContext::try_new(&schema_name_before_type_identity, &standard),
            StandardApplicationContextError::SchemaNameConflict {
                name: semantic_name(["std", "types"]),
            },
            "the application catalogue conflicts with standard schema name std.types".to_owned(),
        );

        let type_identity = ValueTypeDefinition::primitive(
            TypeId::from_bytes([3; 16]),
            semantic_name(["application", "flag"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let binding_after_type_identity = TypeBinding::prelude(
            PreludeTypeName::new(["boolean"]).unwrap(),
            type_identity.id(),
        )
        .unwrap();
        let type_identity_before_binding_identity = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x34; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x35; 16]),
                semantic_name(["application"]),
            )],
            Vec::new(),
            vec![type_identity],
            vec![binding_after_type_identity],
        )
        .unwrap();
        assert_standard_context_error(
            StandardApplicationCheckContext::try_new(
                &type_identity_before_binding_identity,
                &standard,
            ),
            StandardApplicationContextError::TypeIdentityConflict {
                id: TypeId::from_bytes([3; 16]),
            },
            format!(
                "the application catalogue conflicts with standard type identity {}",
                TypeId::from_bytes([3; 16])
            ),
        );

        let binding_target = ValueTypeDefinition::primitive(
            TypeId::from_bytes([0x36; 16]),
            semantic_name(["application", "flag"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let binding = TypeBinding::prelude(
            PreludeTypeName::new(["boolean"]).unwrap(),
            binding_target.id(),
        )
        .unwrap();
        let binding_identity_before_unsupported_contract = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x37; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x38; 16]),
                semantic_name(["application"]),
            )],
            Vec::new(),
            vec![binding_target],
            vec![binding.clone()],
        )
        .unwrap();
        let unsupported_standard =
            super::resolver::checked_standard_library_with_contract_overrides_for_test(
                &verified_standard_source_fixture(),
                &[(0, "unsupported@1")],
            )
            .unwrap();
        assert_standard_context_error(
            StandardApplicationCheckContext::try_new(
                &binding_identity_before_unsupported_contract,
                &unsupported_standard,
            ),
            StandardApplicationContextError::TypeBindingIdentityConflict { id: binding.id() },
            format!(
                "the application catalogue conflicts with standard type binding identity {}",
                binding.id()
            ),
        );

        let unsupported_before_duplicate =
            super::resolver::checked_standard_library_with_contract_overrides_for_test(
                &verified_canonical_standard_source_fixture(),
                &[
                    (0, "unsupported-first@1"),
                    (1, "orna.kernel.value.boolean@1"),
                ],
            )
            .unwrap();
        let empty_application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x38; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        assert_standard_context_error(
            StandardApplicationCheckContext::try_new(
                &empty_application,
                &unsupported_before_duplicate,
            ),
            StandardApplicationContextError::UnsupportedCompatibilityContract {
                type_id: TypeId::from_bytes(CANONICAL_TYPE_IDS[0]),
                contract: "unsupported-first@1".to_owned(),
            },
            format!(
                "the standard value type {} uses unsupported compatibility contract unsupported-first@1",
                TypeId::from_bytes(CANONICAL_TYPE_IDS[0])
            ),
        );

        let duplicate_contract =
            super::resolver::checked_standard_library_with_contract_overrides_for_test(
                &verified_canonical_standard_source_fixture(),
                &[(1, "orna.kernel.value.boolean@1")],
            )
            .unwrap();
        assert_standard_context_error(
            StandardApplicationCheckContext::try_new(&empty_application, &duplicate_contract),
            StandardApplicationContextError::CompatibilityContractConflict {
                contract: "orna.kernel.value.boolean@1".to_owned(),
            },
            "the standard library uses compatibility contract orna.kernel.value.boolean@1 for more than one type".to_owned(),
        );
    }

    #[test]
    fn quoted_names_do_not_acquire_standard_prelude_meaning() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x39; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let source = "CREATE SCHEMA app; CREATE TYPE app.flag AS OBJECT (value \"BOOLEAN\");";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_standard_application(&bundle, &context);

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnknownQualifiedName
        );
        assert_eq!(
            report.diagnostics()[0].message(),
            "unknown type name BOOLEAN"
        );
        assert!(report.checked_bundle().is_none());
    }

    #[test]
    fn unknown_qualified_aliases_and_quoted_counterparts_do_not_resolve_through_standard() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x3a; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let source = "CREATE SCHEMA app; CREATE TYPE app.flag AS OBJECT (alias std.ALIAS, quoted std.\"BOOLEAN\");";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_standard_application(&bundle, &context);

        assert_eq!(report.diagnostics().len(), 2);
        let expected = [
            ("std.ALIAS", "unknown type name std.alias"),
            ("std.\"BOOLEAN\"", "unknown type name std.BOOLEAN"),
        ];
        for (diagnostic, (spelling, message)) in report.diagnostics().iter().zip(expected) {
            assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
            assert_eq!(diagnostic.message(), message);
            let start = source.find(spelling).unwrap();
            assert_eq!(diagnostic.location().span().start(), start);
            assert_eq!(diagnostic.location().span().end(), start + spelling.len());
        }
        assert!(report.checked_bundle().is_none());
    }

    #[test]
    fn standard_declaration_type_uses_are_ordered_by_written_source_not_declaration_family() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x3a; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let source = "CREATE SCHEMA app;\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN) \
            AS SELECT f.value FROM app.flag f;\
            CREATE TYPE app.flag AS OBJECT (value BOOLEAN);";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_standard_application(&bundle, &context);

        assert_eq!(report.diagnostics(), &[]);
        let checked = report.checked_bundle().unwrap();
        let declaration_uses = checked
            .uses()
            .iter()
            .filter(|type_use| {
                matches!(
                    type_use.kind(),
                    CheckedTypeUseKind::Field { .. }
                        | CheckedTypeUseKind::Parameter { .. }
                        | CheckedTypeUseKind::Return { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(declaration_uses.len(), 2);
        assert!(matches!(
            declaration_uses[0].kind(),
            CheckedTypeUseKind::Return { .. }
        ));
        assert!(matches!(
            declaration_uses[1].kind(),
            CheckedTypeUseKind::Field { .. }
        ));
        assert!(
            declaration_uses[0].location().span().start()
                < declaration_uses[1].location().span().start()
        );
    }

    #[test]
    fn standard_values_are_not_valid_ref_targets() {
        let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
        let application = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x3b; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
        let source = "CREATE SCHEMA app; CREATE TYPE app.flag AS OBJECT (value REF std.BOOLEAN);";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

        let report = check_standard_application(&bundle, &context);

        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::InvalidReferenceTarget
        );
        assert_eq!(
            report.diagnostics()[0].message(),
            "REF target std.boolean is a scalar type"
        );
        let start = source.find("std.BOOLEAN").unwrap();
        assert_eq!(report.diagnostics()[0].location().span().start(), start);
        assert_eq!(
            report.diagnostics()[0].location().span().end(),
            start + "std.BOOLEAN".len()
        );
        assert!(report.checked_bundle().is_none());
    }

    #[test]
    fn counts_source_units_before_parsing_or_reconciling() {
        let empty = verified_empty_catalogue_fixture(
            &[],
            [
                0x82, 0xba, 0xf7, 0xee, 0xe1, 0x53, 0x2a, 0x90, 0xa8, 0x10, 0x14, 0x71, 0x0d, 0xa6,
                0xb7, 0x07, 0xb0, 0x37, 0x24, 0xe6, 0x62, 0xce, 0x4b, 0x3a, 0xe7, 0x07, 0xa4, 0xc7,
                0x78, 0xd3, 0xe9, 0xb1,
            ],
        );
        let two = verified_empty_catalogue_fixture(
            &[
                ("std/one.orna", "CREATE SCHEMA std.;"),
                ("std/two.orna", ""),
            ],
            [
                0x71, 0xf0, 0x0f, 0x75, 0x50, 0x39, 0xd7, 0xa5, 0x54, 0xc4, 0xec, 0xb1, 0x80, 0x92,
                0xdf, 0x21, 0xe3, 0x30, 0xc0, 0x28, 0x69, 0xd7, 0xa1, 0xbd, 0xa9, 0xbc, 0x8e, 0xb2,
                0xbe, 0x32, 0x3e, 0xba,
            ],
        );

        assert_eq!(
            check_standard_library_source(&empty).unwrap_err(),
            StandardLibraryCheckError::SourceUnitCount { actual: 0 }
        );
        assert_eq!(
            check_standard_library_source(&two).unwrap_err(),
            StandardLibraryCheckError::SourceUnitCount { actual: 2 }
        );
    }

    #[test]
    fn returns_parser_diagnostics_before_reconciliation() {
        const SOURCE: &str = "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;";
        let parsed = parse_bundle(
            &SourceBundle::new([SourceUnit::new("std/malformed.orna", SOURCE)]).unwrap(),
        );
        assert_eq!(parsed.units()[0].parsed().schemas().len(), 1);
        assert_eq!(parsed.diagnostics().len(), 2);

        let snapshot = verified_empty_catalogue_fixture(
            &[("std/malformed.orna", SOURCE)],
            [
                0x6d, 0x3f, 0xaa, 0x32, 0x82, 0x0e, 0xeb, 0x73, 0x77, 0xc5, 0xbd, 0xfa, 0x3e, 0x8d,
                0x6c, 0xaf, 0xdc, 0x95, 0xa6, 0x7c, 0xbd, 0xef, 0x5b, 0x02, 0x63, 0x1f, 0x29, 0x1d,
                0x14, 0xcc, 0x68, 0xae,
            ],
        );

        let error = check_standard_library_source(&snapshot).unwrap_err();

        assert!(matches!(
            error,
            StandardLibraryCheckError::Diagnostics { .. }
        ));
        let StandardLibraryCheckError::Diagnostics { diagnostics } = error else {
            return;
        };
        assert_eq!(diagnostics, parsed.diagnostics());
    }

    #[test]
    fn standard_source_check_errors_have_the_documented_messages_and_no_causes() {
        let errors = [
            StandardLibraryCheckError::SourceUnitCount { actual: 9 },
            StandardLibraryCheckError::Diagnostics {
                diagnostics: Vec::new(),
            },
            StandardLibraryCheckError::SourceMismatch,
        ];
        let messages = [
            "the verified standard library has 9 source units, expected exactly one",
            "the verified standard library source has compiler diagnostics",
            "the verified standard library source does not match its catalogue and origins",
        ];

        for (error, message) in errors.iter().zip(messages) {
            assert_eq!(error.to_string(), message);
            assert!(std::error::Error::source(error).is_none());
        }
    }

    fn empty_version_two_active(
        standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
    ) -> ActiveDatabaseRevision {
        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x41; 16]),
            0,
            "active.orna",
            "",
            source_unit_content_digest("").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x42; 16]),
            SourceRevisionId::from_bytes([0x43; 16]),
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x42; 16]),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x44; 16]),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard.clone());
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source.id(), catalogue.revision()),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            ),
            context,
        )
        .unwrap()
    }

    fn empty_version_one_active() -> ActiveDatabaseRevision {
        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0xf1; 16]),
            0,
            "active.orna",
            "",
            source_unit_content_digest("").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0xf2; 16]),
            SourceRevisionId::from_bytes([0xf3; 16]),
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0xf2; 16]),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0xf4; 16]),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap();
        ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn version_one_active_with(
        source_text: &str,
        catalogue: CatalogueSnapshot,
    ) -> ActiveDatabaseRevision {
        version_one_active_with_origins(source_text, catalogue, Vec::new())
    }

    fn version_one_active_with_origins(
        source_text: &str,
        catalogue: CatalogueSnapshot,
        origins: Vec<DefinitionOrigin>,
    ) -> ActiveDatabaseRevision {
        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0xa1; 16]),
            0,
            "active.orna",
            source_text,
            source_unit_content_digest(source_text).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0xa2; 16]),
            SourceRevisionId::from_bytes([0xa3; 16]),
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0xa2; 16]),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            Vec::new(),
            origins,
            Vec::new(),
        )
        .unwrap()
    }

    fn stored_source_with_ids(
        source_text: &str,
        unit_id: SourceUnitId,
        bundle_id: SourceBundleId,
        revision_id: SourceRevisionId,
    ) -> StoredSourceRevision {
        let source_unit = StoredSourceUnit::new(
            unit_id,
            0,
            "active.orna",
            source_text,
            source_unit_content_digest(source_text).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        StoredSourceRevision::new(
            bundle_id,
            revision_id,
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(bundle_id, None, bundle_hash).unwrap(),
        )
        .unwrap()
    }

    fn version_one_active_with_source(
        source: StoredSourceRevision,
        catalogue: CatalogueSnapshot,
        origins: Vec<DefinitionOrigin>,
    ) -> ActiveDatabaseRevision {
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            Vec::new(),
            origins,
            Vec::new(),
        )
        .unwrap()
    }

    fn active_from_prepared_standard_candidate(
        prepared: &DeployableRevision,
        historical_function_revisions: Vec<orna_core::revision::FunctionRevisionRecord>,
    ) -> ActiveDatabaseRevision {
        let current_function_revisions = prepared
            .current_function_revisions()
            .map_or_else(Vec::new, ToOwned::to_owned);
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                prepared.candidate_pair(),
                prepared.source().clone(),
                prepared.candidate().clone(),
                prepared.catalogue_hash(),
                ActiveRevisionContent::new(
                    prepared.expressions().to_vec(),
                    current_function_revisions,
                    prepared.origins().to_vec(),
                    prepared.references().to_vec(),
                )
                .with_history(historical_function_revisions),
            ),
            prepared.catalogue_hash_context().clone(),
        )
        .unwrap()
    }

    fn active_from_prepared_version_one_candidate(
        prepared: &DeployableRevision,
    ) -> ActiveDatabaseRevision {
        ActiveDatabaseRevision::new(
            prepared.candidate_pair(),
            prepared.source().clone(),
            prepared.candidate().clone(),
            prepared.catalogue_hash(),
            prepared.expressions().to_vec(),
            prepared.new_function_revisions().to_vec(),
            prepared.origins().to_vec(),
            prepared.references().to_vec(),
        )
        .unwrap()
    }

    fn version_one_client_active_from_standard_candidate(
        prepared: &DeployableRevision,
    ) -> ActiveDatabaseRevision {
        const STANDARD_BOOLEAN: TypeId = TypeId::from_bytes([3; 16]);
        let candidate = prepared.candidate();
        assert!(candidate.object_types().is_empty());
        assert_eq!(candidate.functions().len(), 1);
        let function = &candidate.functions()[0];
        assert_eq!(function.domain(), FunctionDomain::Client);
        assert!(function.parameters().is_empty());
        assert_eq!(
            function.return_type(),
            &FunctionReturn::Single(ResolvedType::Value(STANDARD_BOOLEAN))
        );
        let legacy_function = FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            function.domain(),
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            function.current_revision(),
            function.security(),
            function.transaction(),
            function.volatility(),
        );
        let legacy_catalogue = CatalogueSnapshot::new_with_functions_and_types(
            candidate.revision(),
            candidate.schemas().to_vec(),
            Vec::new(),
            candidate.value_types().to_vec(),
            candidate.type_bindings().to_vec(),
            vec![legacy_function],
        )
        .unwrap();
        let legacy_function = &legacy_catalogue.functions()[0];
        let current = prepared.current_function_revisions().unwrap_or_default();
        assert_eq!(current.len(), 1);
        let revision = &current[0];
        let version_one_revision = FunctionRevisionRecord::new(
            revision.function(),
            revision.id(),
            revision.revision_number(),
            revision.declaration_origin(),
            revision.declaration_content_hash(),
            function_semantic_digest(
                legacy_function,
                revision.language_version(),
                revision.artifact(),
                prepared.expressions(),
                &[],
            )
            .unwrap(),
            revision.language_version(),
            revision.artifact().clone(),
        )
        .unwrap();
        let catalogue_hash = catalogue_digest(
            &legacy_catalogue,
            std::slice::from_ref(&version_one_revision),
            prepared.expressions(),
            prepared.origins(),
            &[],
        )
        .unwrap();
        ActiveDatabaseRevision::new(
            prepared.candidate_pair(),
            prepared.source().clone(),
            legacy_catalogue,
            catalogue_hash,
            prepared.expressions().to_vec(),
            vec![version_one_revision],
            prepared.origins().to_vec(),
            Vec::new(),
        )
        .unwrap()
    }

    fn active_with_history(
        active: &ActiveDatabaseRevision,
        historical_function_revisions: Vec<FunctionRevisionRecord>,
    ) -> ActiveDatabaseRevision {
        ActiveDatabaseRevision::new_with_history(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            active.expressions().to_vec(),
            active.function_revisions().to_vec(),
            historical_function_revisions,
            active.origins().to_vec(),
            active.references().to_vec(),
        )
        .unwrap()
    }

    fn verified_standard_source_fixture() -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        const SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;";

        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([4; 16]),
            0,
            "std/types.orna",
            SOURCE,
            source_unit_content_digest(SOURCE).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([5; 16]),
            SourceRevisionId::from_bytes([6; 16]),
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
                .unwrap(),
        )
        .unwrap();

        let boolean = ValueTypeDefinition::primitive(
            TypeId::from_bytes([3; 16]),
            semantic_name(["std", "types", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let qualified =
            TypeBinding::qualified(semantic_name(["std", "boolean"]), boolean.id()).unwrap();
        let prelude =
            TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([8; 16]),
            vec![
                SchemaDefinition::new(SchemaId::from_bytes([1; 16]), semantic_name(["std"])),
                SchemaDefinition::new(
                    SchemaId::from_bytes([2; 16]),
                    semantic_name(["std", "types"]),
                ),
            ],
            vec![],
            vec![boolean],
            vec![qualified.clone(), prelude.clone()],
        )
        .unwrap();
        let origins = vec![
            origin(
                DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
                0,
                18,
            ),
            origin(
                DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
                18,
                42,
            ),
            origin(
                DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
                42,
                159,
            ),
            origin(DefinitionIdentity::TypeBinding(qualified.id()), 159, 204),
            origin(DefinitionIdentity::TypeBinding(prelude.id()), 204, 250),
        ];
        let snapshot = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([7; 16]),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            catalogue,
            origins,
            Sha256Digest::from_bytes(SUCCESS_STANDARD_DIGEST),
        )
        .unwrap();

        verify_standard_library_snapshot(snapshot).unwrap()
    }

    fn verified_non_std_schema_standard_source_fixture()
    -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        const SOURCE: &str = "CREATE SCHEMA library;";
        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([90; 16]),
            0,
            "library.orna",
            SOURCE,
            source_unit_content_digest(SOURCE).unwrap(),
        )
        .unwrap();
        let source_bundle = SourceBundleId::from_bytes([91; 16]);
        let source_revision = SourceRevisionId::from_bytes([92; 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            source_bundle,
            source_revision,
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
        )
        .unwrap();
        let schema_id = SchemaId::from_bytes([93; 16]);
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([94; 16]),
            vec![SchemaDefinition::new(schema_id, semantic_name(["library"]))],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let origins = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            SourceOrigin::new(SourceUnitId::from_bytes([90; 16]), 0, SOURCE.len() as u32).unwrap(),
        )];
        let snapshot = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([95; 16]),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            catalogue,
            origins,
            Sha256Digest::from_bytes(NON_STD_SCHEMA_STANDARD_DIGEST),
        )
        .unwrap();

        verify_standard_library_snapshot(snapshot).unwrap()
    }

    fn verified_canonical_standard_source_fixture()
    -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
            0,
            "std/types.orna",
            CANONICAL_STANDARD_SOURCE,
            source_unit_content_digest(CANONICAL_STANDARD_SOURCE).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes(CANONICAL_RESERVED_ID),
            SourceRevisionId::from_bytes(CANONICAL_RESERVED_ID),
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes(CANONICAL_RESERVED_ID),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let value_types = CANONICAL_VALUE_TYPE_FACTS
            .iter()
            .enumerate()
            .map(|(index, fact)| {
                let id = TypeId::from_bytes(CANONICAL_TYPE_IDS[index]);
                let name = semantic_name_from_dotted(fact.name);
                match fact.kind {
                    ValueTypeKind::Primitive => ValueTypeDefinition::primitive(
                        id,
                        name,
                        ValueTypeMutability::Immutable,
                        fact.persistence,
                        fact.representation_contract,
                    ),
                    ValueTypeKind::Opaque => {
                        ValueTypeDefinition::opaque(id, name, fact.representation_contract)
                    }
                    _ => {
                        unreachable!(
                            "the canonical fixture contains only primitive and opaque values"
                        )
                    }
                }
            })
            .collect();
        let type_bindings = CANONICAL_BINDING_FACTS
            .iter()
            .map(|fact| {
                let target = TypeId::from_bytes(CANONICAL_TYPE_IDS[fact.target_type_index]);
                match fact.kind {
                    CanonicalBindingKind::Qualified => {
                        TypeBinding::qualified(semantic_name_from_dotted(fact.name), target)
                    }
                    CanonicalBindingKind::Prelude => TypeBinding::prelude(
                        PreludeTypeName::new(fact.name.split(' ')).unwrap(),
                        target,
                    ),
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(CANONICAL_RESERVED_ID),
            vec![
                SchemaDefinition::new(
                    SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[0]),
                    semantic_name_from_dotted("std"),
                ),
                SchemaDefinition::new(
                    SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[1]),
                    semantic_name_from_dotted("std.types"),
                ),
            ],
            vec![],
            value_types,
            type_bindings,
        )
        .unwrap();
        let snapshot = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes(CANONICAL_RESERVED_ID),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            catalogue,
            canonical_origins(),
            Sha256Digest::from_bytes(CANONICAL_STANDARD_DIGEST),
        )
        .unwrap();

        verify_standard_library_snapshot(snapshot).unwrap()
    }

    fn canonical_origins() -> Vec<DefinitionOrigin> {
        CANONICAL_DECLARATION_FACTS
            .iter()
            .map(|fact| {
                DefinitionOrigin::new(
                    canonical_definition_identity(fact.identity),
                    canonical_source_origin(*fact),
                )
            })
            .collect()
    }

    fn canonical_definition_identity(identity: CanonicalDeclarationIdentity) -> DefinitionIdentity {
        match identity {
            CanonicalDeclarationIdentity::Schema(index) => {
                DefinitionIdentity::Schema(SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[index]))
            }
            CanonicalDeclarationIdentity::ValueType(index) => {
                DefinitionIdentity::ValueType(TypeId::from_bytes(CANONICAL_TYPE_IDS[index]))
            }
            CanonicalDeclarationIdentity::TypeBinding(index) => DefinitionIdentity::TypeBinding(
                orna_core::TypeBindingId::from_bytes(CANONICAL_BINDING_IDS[index]),
            ),
        }
    }

    fn canonical_source_origin(fact: CanonicalDeclarationFact) -> SourceOrigin {
        SourceOrigin::new(
            SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
            fact.byte_start,
            fact.byte_end,
        )
        .unwrap()
    }

    fn semantic_name_from_dotted(name: &str) -> QualifiedSemanticName {
        QualifiedSemanticName::new(name.split('.')).unwrap()
    }

    fn semantic_name<const COUNT: usize>(parts: [&str; COUNT]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts).unwrap()
    }

    fn assert_standard_context_error(
        result: Result<StandardApplicationCheckContext<'_>, StandardApplicationContextError>,
        expected: StandardApplicationContextError,
        message: String,
    ) {
        assert!(result.is_err());
        let Err(error) = result else {
            return;
        };
        assert_eq!(error, expected);
        assert_eq!(error.clone(), error);
        assert_eq!(error.to_string(), message);
        assert!(std::error::Error::source(&error).is_none());
    }

    fn origin(identity: DefinitionIdentity, byte_start: u32, byte_end: u32) -> DefinitionOrigin {
        DefinitionOrigin::new(identity, source_origin(byte_start, byte_end))
    }

    fn source_origin(byte_start: u32, byte_end: u32) -> SourceOrigin {
        SourceOrigin::new(SourceUnitId::from_bytes([4; 16]), byte_start, byte_end).unwrap()
    }

    fn verified_empty_catalogue_fixture(
        units: &[(&str, &str)],
        standard_digest: [u8; 32],
    ) -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        let stored_units = units
            .iter()
            .enumerate()
            .map(|(ordinal, (logical_path, content))| {
                StoredSourceUnit::new(
                    SourceUnitId::from_bytes([u8::try_from(ordinal + 4).unwrap(); 16]),
                    u32::try_from(ordinal).unwrap(),
                    *logical_path,
                    *content,
                    source_unit_content_digest(content).unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let bundle_hash = source_bundle_digest(&stored_units).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([5; 16]),
            SourceRevisionId::from_bytes([6; 16]),
            None,
            stored_units,
            bundle_hash,
            source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
                .unwrap(),
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([8; 16]),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        let snapshot = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([7; 16]),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            catalogue,
            vec![],
            Sha256Digest::from_bytes(standard_digest),
        )
        .unwrap();

        verify_standard_library_snapshot(snapshot).unwrap()
    }

    #[test]
    fn retains_exact_source_and_syntax_text_in_bundle_order() {
        let first_source = "-- customer source\r\nCREATE SCHEMA crm;  \r\n";
        let second_source = "/* task source */\nCREATE SCHEMA tasks;\n";
        let bundle = SourceBundle::new([
            SourceUnit::new("crm/schema.orna", first_source),
            SourceUnit::new("tasks/schema.orna", second_source),
        ])
        .unwrap();

        let report = parse_bundle(&bundle);

        assert!(report.diagnostics().is_empty());
        assert_eq!(report.units().len(), 2);
        assert_eq!(report.units()[0].logical_path(), "crm/schema.orna");
        assert_eq!(report.units()[0].source_text(), first_source);
        assert_eq!(report.units()[0].syntax_text(), first_source);
        assert_eq!(report.units()[1].logical_path(), "tasks/schema.orna");
        assert_eq!(report.units()[1].source_text(), second_source);
        assert_eq!(report.units()[1].syntax_text(), second_source);
    }

    #[test]
    fn parses_later_units_after_an_earlier_syntax_error() {
        let bundle = SourceBundle::new([
            SourceUnit::new("broken.orna", "CREATE SCHEMA crm.;"),
            SourceUnit::new("valid.orna", "-- retained\nCREATE SCHEMA tasks;"),
        ])
        .unwrap();

        let report = parse_bundle(&bundle);

        assert_eq!(report.units().len(), 2);
        assert_eq!(
            report.units()[1].syntax_text(),
            "-- retained\nCREATE SCHEMA tasks;"
        );
        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnexpectedToken
        );
    }

    #[test]
    fn maps_syntax_codes_and_retains_owned_paths_and_byte_spans() {
        let bundle = SourceBundle::new([
            SourceUnit::new("syntax.orna", "CREATE SCHEMA crm.;"),
            SourceUnit::new("comment.orna", "/* unfinished"),
        ])
        .unwrap();

        let report = parse_bundle(&bundle);

        assert_eq!(report.diagnostics().len(), 2);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnexpectedToken
        );
        assert_eq!(report.diagnostics()[0].code().as_str(), "ORNA0001");
        assert_eq!(
            report.diagnostics()[0].location().logical_path(),
            "syntax.orna"
        );
        assert_eq!(report.diagnostics()[0].location().span().start(), 18);
        assert_eq!(report.diagnostics()[0].location().span().end(), 19);
        assert_eq!(
            report.diagnostics()[1].code(),
            DiagnosticCode::UnterminatedSourceConstruct
        );
        assert_eq!(report.diagnostics()[1].code().as_str(), "ORNA0002");
        assert_eq!(
            report.diagnostics()[1].location().logical_path(),
            "comment.orna"
        );
        assert_eq!(report.diagnostics()[1].location().span().start(), 0);
        assert_eq!(report.diagnostics()[1].location().span().end(), 13);
    }

    #[test]
    fn exposes_checked_standard_upgrade_preparation_seam() {
        let _: fn(
            &CheckedStandardLibrary,
            &ActiveDatabaseRevision,
        ) -> Result<PreparedStandardUpgrade, PrepareStandardUpgradeError> =
            prepare_checked_standard_upgrade;
    }

    #[test]
    fn standard_upgrade_rejects_an_already_installed_standard_before_source_work() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let source_text = "CREATE SCHEMA std;";
        let source_unit = SourceUnitId::from_bytes([0xc1; 16]);
        let source = stored_source_with_ids(
            source_text,
            source_unit,
            SourceBundleId::from_bytes([0xc2; 16]),
            SourceRevisionId::from_bytes([0xc3; 16]),
        );
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0xc4; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0xc5; 16]),
                semantic_name(["std"]),
            )],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let origins = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0xc5; 16])),
            SourceOrigin::new(source_unit, 0, source_text.len() as u32).unwrap(),
        )];
        let context = CatalogueHashContext::version_two(verified.clone());
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source.id(), catalogue.revision()),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
            ),
            context,
        )
        .unwrap();

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision }
                if revision == verified.revision()
        ));
        assert_eq!(
            error.to_string(),
            format!(
                "standard library {} is already installed",
                verified.revision()
            )
        );
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn prepares_an_empty_version_one_application_for_a_checked_standard_upgrade() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_one_active();

        let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();

        assert_eq!(
            prepared.standard_library().verified_snapshot().digest(),
            verified.digest()
        );
        assert_eq!(
            prepared.application_revision().expected_base(),
            active.pair()
        );
        assert_eq!(
            prepared
                .application_revision()
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.digest()),
            Some(verified.digest())
        );
    }

    #[test]
    fn standard_upgrade_retries_every_companion_identity_before_constructing_version_two() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let active = empty_version_one_active();

        let prepared = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap();

        for counter in [
            &PREPARE_CATALOGUE_ALLOCATIONS,
            &PREPARE_BUNDLE_ALLOCATIONS,
            &PREPARE_REVISION_ALLOCATIONS,
            &PREPARE_UNIT_ALLOCATIONS,
        ] {
            assert_eq!(counter.load(Ordering::SeqCst), 2);
        }
        assert_eq!(PREPARE_SCHEMA_ALLOCATIONS.load(Ordering::SeqCst), 0);
        assert_eq!(PREPARE_TYPE_ALLOCATIONS.load(Ordering::SeqCst), 0);
        let application = prepared.application_revision();
        assert_eq!(application.candidate().revision().to_bytes(), [0x81; 16]);
        assert_eq!(application.source().bundle().to_bytes(), [0x82; 16]);
        assert_eq!(application.source().id().to_bytes(), [0x83; 16]);
        assert_eq!(application.source().units()[0].id().to_bytes(), [0x84; 16]);
    }

    #[test]
    fn standard_upgrade_retries_and_copies_every_retained_source_unit() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let bundle = SourceBundle::new([
            SourceUnit::new("first.orna", "CREATE SCHEMA app;"),
            SourceUnit::new("second.orna", "-- retained empty unit\n"),
        ])
        .unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);

        let prepared = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap();

        assert_eq!(PREPARE_UNIT_ALLOCATIONS.load(Ordering::SeqCst), 3);
        assert_eq!(
            prepared
                .application_revision()
                .source()
                .units()
                .iter()
                .map(|unit| unit.id().to_bytes())
                .collect::<Vec<_>>(),
            vec![[0x84; 16], [0x85; 16]]
        );
    }

    #[test]
    fn matches_nonempty_version_one_source_before_preparing_the_upgrade() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let bundle =
            SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA app;")]).unwrap();
        let report = check(&bundle, empty.catalogue());
        let version_one = prepare(&report, empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);

        let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();

        assert_eq!(
            prepared.application_revision().expected_base(),
            active.pair()
        );
        assert_eq!(
            prepared.application_revision().candidate().schemas().len(),
            1
        );
        assert_eq!(
            prepared.application_revision().candidate().schemas()[0].name(),
            &semantic_name(["app"])
        );
    }

    #[test]
    fn standard_upgrade_requires_an_exact_allocation_free_version_one_match() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let bundle =
            SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA app;")]).unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let origins = version_one
            .origins()
            .iter()
            .map(|origin| {
                DefinitionOrigin::new(
                    origin.identity(),
                    SourceOrigin::new(
                        SourceUnitId::from_bytes([0xa1; 16]),
                        origin.source().byte_start(),
                        origin.source().byte_end(),
                    )
                    .unwrap(),
                )
            })
            .collect();
        let active = version_one_active_with_origins(
            "CREATE SCHEMA changed;",
            version_one.candidate().clone(),
            origins,
        );

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PrepareStandardUpgradeError::ActiveSourceMismatch
        ));
        assert_eq!(
            error.to_string(),
            "the active application source does not match the active catalogue"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn standard_upgrade_compares_the_complete_current_function_revision_record() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);
        let current = active.function_revisions()[0].clone();
        let origin = current.declaration_origin();
        let changed_origin = FunctionRevisionRecord::new(
            current.function(),
            current.id(),
            current.revision_number(),
            SourceOrigin::new(
                origin.source_unit(),
                origin.byte_start() + 1,
                origin.byte_end(),
            )
            .unwrap(),
            current.declaration_content_hash(),
            current.semantic_hash(),
            current.language_version(),
            current.artifact().clone(),
        )
        .unwrap();
        let changed_declaration_hash = FunctionRevisionRecord::new(
            current.function(),
            current.id(),
            current.revision_number(),
            current.declaration_origin(),
            Sha256Digest::from_bytes([0xe1; 32]),
            current.semantic_hash(),
            current.language_version(),
            current.artifact().clone(),
        )
        .unwrap();
        let changed_language = "orna.language/changed";
        let changed_language_version = FunctionRevisionRecord::new(
            current.function(),
            current.id(),
            current.revision_number(),
            current.declaration_origin(),
            current.declaration_content_hash(),
            function_semantic_digest(
                active
                    .catalogue()
                    .function_by_id(current.function())
                    .unwrap(),
                changed_language,
                current.artifact(),
                active.expressions(),
                active.references(),
            )
            .unwrap(),
            changed_language,
            current.artifact().clone(),
        )
        .unwrap();
        for (label, changed) in [
            ("origin", changed_origin),
            ("declaration hash", changed_declaration_hash),
            ("language version", changed_language_version),
        ] {
            let catalogue_hash = catalogue_digest(
                active.catalogue(),
                std::slice::from_ref(&changed),
                active.expressions(),
                active.origins(),
                active.references(),
            )
            .unwrap();
            let hostile = ActiveDatabaseRevision::new(
                active.pair(),
                active.source().clone(),
                active.catalogue().clone(),
                catalogue_hash,
                active.expressions().to_vec(),
                vec![changed],
                active.origins().to_vec(),
                active.references().to_vec(),
            )
            .unwrap();

            let error = prepare_checked_standard_upgrade_with_allocator(
                &standard,
                &hostile,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();

            assert!(
                matches!(&error, PrepareStandardUpgradeError::ActiveSourceMismatch),
                "Gate 6 accepted changed {label}"
            );
            assert_eq!(
                error.to_string(),
                "the active application source does not match the active catalogue"
            );
            assert!(std::error::Error::source(&error).is_none());
            assert_no_standard_upgrade_allocations();
        }

        let function = active
            .catalogue()
            .function_by_id(current.function())
            .unwrap();
        let record_with_artifact = |artifact: ExecutableArtifact| {
            FunctionRevisionRecord::new(
                current.function(),
                current.id(),
                current.revision_number(),
                current.declaration_origin(),
                current.declaration_content_hash(),
                function_semantic_digest(
                    function,
                    current.language_version(),
                    &artifact,
                    active.expressions(),
                    active.references(),
                )
                .unwrap(),
                current.language_version(),
                artifact,
            )
            .unwrap()
        };
        let changed_format = ExecutableArtifact::new(
            current.artifact().kind(),
            "orna.hostile-format",
            current.artifact().version(),
            current.artifact().payload().to_vec(),
            current.artifact().content_hash(),
        )
        .unwrap();
        let changed_version = ExecutableArtifact::new(
            current.artifact().kind(),
            current.artifact().format(),
            current.artifact().version() + 1,
            current.artifact().payload().to_vec(),
            current.artifact().content_hash(),
        )
        .unwrap();
        let mut changed_payload = current.artifact().payload().to_vec();
        changed_payload.push(0xff);
        let changed_payload = ExecutableArtifact::new(
            current.artifact().kind(),
            current.artifact().format(),
            current.artifact().version(),
            changed_payload.clone(),
            artifact_payload_digest(&changed_payload).unwrap(),
        )
        .unwrap();
        for (label, changed) in [
            ("artifact format", record_with_artifact(changed_format)),
            ("artifact version", record_with_artifact(changed_version)),
            ("artifact payload", record_with_artifact(changed_payload)),
        ] {
            let catalogue_hash = catalogue_digest(
                active.catalogue(),
                std::slice::from_ref(&changed),
                active.expressions(),
                active.origins(),
                active.references(),
            )
            .unwrap();
            let hostile = ActiveDatabaseRevision::new(
                active.pair(),
                active.source().clone(),
                active.catalogue().clone(),
                catalogue_hash,
                active.expressions().to_vec(),
                vec![changed],
                active.origins().to_vec(),
                active.references().to_vec(),
            )
            .unwrap();
            let error = prepare_checked_standard_upgrade_with_allocator(
                &standard,
                &hostile,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();
            assert!(
                matches!(&error, PrepareStandardUpgradeError::ActiveSourceMismatch),
                "Gate 6 accepted changed {label}"
            );
            assert_no_standard_upgrade_allocations();
        }

        let assert_content_mismatch =
            |catalogue: CatalogueSnapshot,
             expressions: Vec<ExpressionArtifact>,
             origins: Vec<DefinitionOrigin>,
             references: Vec<orna_core::revision::DefinitionReference>| {
                let catalogue_hash = catalogue_digest(
                    &catalogue,
                    active.function_revisions(),
                    &expressions,
                    &origins,
                    &references,
                )
                .unwrap();
                let hostile = ActiveDatabaseRevision::new(
                    active.pair(),
                    active.source().clone(),
                    catalogue,
                    catalogue_hash,
                    expressions,
                    active.function_revisions().to_vec(),
                    origins,
                    references,
                )
                .unwrap();
                let error = prepare_checked_standard_upgrade_with_allocator(
                    &standard,
                    &hostile,
                    retrying_standard_allocator(&verified),
                )
                .unwrap_err();
                assert!(matches!(
                    error,
                    PrepareStandardUpgradeError::ActiveSourceMismatch
                ));
                assert_no_standard_upgrade_allocations();
            };

        let mut changed_origins = active.origins().to_vec();
        let first_origin = changed_origins[0].source();
        changed_origins[0] = DefinitionOrigin::new(
            changed_origins[0].identity(),
            SourceOrigin::new(
                first_origin.source_unit(),
                first_origin.byte_start() + 1,
                first_origin.byte_end(),
            )
            .unwrap(),
        );
        assert_content_mismatch(
            active.catalogue().clone(),
            active.expressions().to_vec(),
            changed_origins,
            active.references().to_vec(),
        );

        let first_reference = active.references()[0].clone();
        let changed_reference = orna_core::revision::DefinitionReference::new(
            first_reference.source_function(),
            first_reference.source_revision(),
            first_reference.ordinal(),
            first_reference.target(),
            first_reference.kind(),
            SourceOrigin::new(
                first_reference.source_origin().source_unit(),
                first_reference.source_origin().byte_start() + 1,
                first_reference.source_origin().byte_end(),
            )
            .unwrap(),
        );
        assert_content_mismatch(
            active.catalogue().clone(),
            active.expressions().to_vec(),
            active.origins().to_vec(),
            vec![changed_reference]
                .into_iter()
                .chain(active.references()[1..].iter().cloned())
                .collect(),
        );

        let expression_payload = b"hostile-expression".to_vec();
        let expression = ExpressionArtifact::new(
            ExpressionId::from_bytes([0xe9; 16]),
            "orna.constant-expression",
            1,
            expression_payload.clone(),
            artifact_payload_digest(&expression_payload).unwrap(),
        )
        .unwrap();
        let mut expression_origins = active.origins().to_vec();
        expression_origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Expression(expression.id()),
            SourceOrigin::new(active.source().units()[0].id(), 0, 1).unwrap(),
        ));
        assert_content_mismatch(
            active.catalogue().clone(),
            vec![expression],
            expression_origins,
            active.references().to_vec(),
        );

        let mut changed_object_types = active.catalogue().object_types().to_vec();
        changed_object_types[0] = ObjectTypeDefinition::new(
            changed_object_types[0].id(),
            semantic_name(["app", "changed"]),
            changed_object_types[0].fields().to_vec(),
        );
        let changed_catalogue = CatalogueSnapshot::new_with_functions_and_types(
            active.catalogue().revision(),
            active.catalogue().schemas().to_vec(),
            changed_object_types,
            active.catalogue().value_types().to_vec(),
            active.catalogue().type_bindings().to_vec(),
            active.catalogue().functions().to_vec(),
        )
        .unwrap();
        assert_content_mismatch(
            changed_catalogue,
            active.expressions().to_vec(),
            active.origins().to_vec(),
            active.references().to_vec(),
        );
    }

    #[test]
    fn standard_upgrade_rejects_active_source_mismatch_before_revision_exhaustion() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL); \
             CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN) \
             TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;",
        )])
        .unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);
        let original = &active.source().units()[0];
        let changed_content = format!(
            "-- shifts every declaration location\n{}",
            original.content()
        );
        let shifted_unit = StoredSourceUnit::new(
            original.id(),
            original.ordinal(),
            original.logical_path(),
            &changed_content,
            source_unit_content_digest(&changed_content).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&shifted_unit)).unwrap();
        let shifted_source = StoredSourceRevision::new(
            active.source().bundle(),
            active.source().id(),
            active.source().parent(),
            vec![shifted_unit],
            bundle_hash,
            source_revision_record_digest(
                active.source().bundle(),
                active.source().parent(),
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let current = &active.function_revisions()[0];
        let exhausted = FunctionRevisionRecord::new(
            current.function(),
            current.id(),
            u64::MAX,
            current.declaration_origin(),
            current.declaration_content_hash(),
            current.semantic_hash(),
            current.language_version(),
            current.artifact().clone(),
        )
        .unwrap();
        let catalogue_hash = catalogue_digest(
            active.catalogue(),
            std::slice::from_ref(&exhausted),
            active.expressions(),
            active.origins(),
            active.references(),
        )
        .unwrap();
        let hostile = ActiveDatabaseRevision::new(
            active.pair(),
            shifted_source,
            active.catalogue().clone(),
            catalogue_hash,
            active.expressions().to_vec(),
            vec![exhausted],
            active.origins().to_vec(),
            active.references().to_vec(),
        )
        .unwrap();

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &hostile,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PrepareStandardUpgradeError::ActiveSourceMismatch
        ));
        assert_eq!(
            error.to_string(),
            "the active application source does not match the active catalogue"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn prepares_version_two_server_semantics_after_matching_version_one_source() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let report = check(&bundle, empty.catalogue());
        assert!(report.diagnostics().is_empty());
        let version_one = prepare(&report, empty.pair(), &empty).unwrap();
        assert_eq!(
            version_one.candidate().object_types()[0].fields()[0].resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        let version_one_function = &version_one.candidate().functions()[0];
        let FunctionReturn::Rows(version_one_columns) = version_one_function.return_type() else {
            panic!("the legacy server fixture must retain a ROWS return")
        };
        assert_eq!(
            version_one_columns[0].resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        let active = active_from_prepared_version_one_candidate(&version_one);
        assert_eq!(
            active.function_revisions()[0].semantic_hash_version(),
            FunctionSemanticHashVersion::Version1
        );
        let legacy_payload = active.function_revisions()[0].artifact().payload().to_vec();
        let legacy_payload_hash = active.function_revisions()[0].artifact().content_hash();

        let public_prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();
        assert_eq!(
            public_prepared
                .application_revision()
                .catalogue_hash_context()
                .version(),
            CatalogueHashVersion::Version2
        );
        assert_eq!(
            public_prepared
                .application_revision()
                .candidate()
                .object_types()[0]
                .fields()[0]
                .resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        let public_function = &public_prepared
            .application_revision()
            .candidate()
            .functions()[0];
        let FunctionReturn::Rows(public_columns) = public_function.return_type() else {
            panic!("server fixture must retain a ROWS return")
        };
        assert_eq!(public_columns.len(), 1);
        assert_eq!(
            public_columns[0].resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        assert!(
            orna_core::revision::validate_persistable_catalogue(
                public_prepared.application_revision()
            )
            .is_ok()
        );
        assert!(
            public_prepared
                .application_revision()
                .references()
                .iter()
                .any(|reference| {
                    reference.target()
                        == DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
                        && reference.kind() == DefinitionReferenceKind::NamedType
                })
        );

        let prepared = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap();

        assert_eq!(
            prepared
                .application_revision()
                .new_function_revisions()
                .len(),
            1
        );
        assert_eq!(
            prepared.application_revision().new_function_revisions()[0].semantic_hash_version(),
            FunctionSemanticHashVersion::Version2
        );
        assert_eq!(
            prepared.application_revision().new_function_revisions()[0]
                .artifact()
                .payload(),
            legacy_payload
        );
        assert_eq!(
            prepared.application_revision().new_function_revisions()[0]
                .artifact()
                .content_hash(),
            legacy_payload_hash
        );
        assert!(
            prepared
                .application_revision()
                .references()
                .iter()
                .any(|reference| {
                    reference.target()
                        == DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
                        && reference.kind() == DefinitionReferenceKind::NamedType
                })
        );
        assert_eq!(
            PREPARE_FUNCTION_REVISION_ALLOCATIONS.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            prepared.application_revision().new_function_revisions()[0]
                .id()
                .to_bytes(),
            [0x90; 16]
        );
    }

    #[test]
    fn prepares_version_two_mutation_parameter_and_reference_return_with_value_identity() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.create(p_done BOOLEAN) RETURNS ROWS (created REF app.item)\
            TRANSACTION ATOMIC AS INSERT INTO app.item AS made (done) VALUES (p_done) RETURNING REF(made);";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);

        let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();
        assert_eq!(
            prepared
                .application_revision()
                .catalogue_hash_context()
                .version(),
            CatalogueHashVersion::Version2
        );
        let candidate = prepared.application_revision().candidate();
        assert_eq!(
            candidate.object_types()[0].fields()[0].resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        let function = &candidate.functions()[0];
        assert_eq!(
            function.parameters()[0].resolved_type(),
            ResolvedType::Value(TypeId::from_bytes([3; 16]))
        );
        let item = candidate.object_types()[0].id();
        let FunctionReturn::Rows(columns) = function.return_type() else {
            panic!("the mutation fixture must retain a ROWS return")
        };
        assert!(matches!(
            columns[0].resolved_type(),
            ResolvedType::Reference { target } if target == item
        ));
        assert!(
            prepared
                .application_revision()
                .references()
                .iter()
                .any(|reference| {
                    reference.target()
                        == DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
                })
        );
        assert!(
            orna_core::revision::validate_persistable_catalogue(prepared.application_revision())
                .is_ok()
        );
        let revision = &prepared.application_revision().new_function_revisions()[0];
        assert_eq!(
            revision.semantic_hash_version(),
            FunctionSemanticHashVersion::Version2
        );
        assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
        assert_eq!(
            revision.artifact().payload(),
            active.function_revisions()[0].artifact().payload()
        );
        assert_eq!(
            revision.artifact().content_hash(),
            active.function_revisions()[0].artifact().content_hash()
        );
    }

    #[test]
    fn standard_upgrade_checks_function_revision_exhaustion_before_allocation() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);
        let current = active.function_revisions()[0].clone();
        let exhausted = FunctionRevisionRecord::new(
            current.function(),
            current.id(),
            u64::MAX,
            current.declaration_origin(),
            current.declaration_content_hash(),
            current.semantic_hash(),
            current.language_version(),
            current.artifact().clone(),
        )
        .unwrap();
        let catalogue_hash = catalogue_digest(
            active.catalogue(),
            std::slice::from_ref(&exhausted),
            active.expressions(),
            active.origins(),
            active.references(),
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            catalogue_hash,
            active.expressions().to_vec(),
            vec![exhausted],
            active.origins().to_vec(),
            active.references().to_vec(),
        )
        .unwrap();

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PrepareStandardUpgradeError::FunctionRevisionNumberExhausted { function }
                if function == current.function()
        ));
        assert_eq!(error.to_string(), "function revision number is exhausted");
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn prepares_version_two_client_semantics_after_matching_version_one_source() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let initial = empty_version_two_active(&verified);
        let context =
            StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
        let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = check_standard_application(&bundle, &context);
        let seeded = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
        let active = version_one_client_active_from_standard_candidate(&seeded);
        assert_eq!(
            active.catalogue().functions()[0].return_type(),
            &FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean))
        );
        assert_eq!(
            active.function_revisions()[0].semantic_hash_version(),
            FunctionSemanticHashVersion::Version1
        );

        let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();
        assert_eq!(
            prepared
                .application_revision()
                .catalogue_hash_context()
                .version(),
            CatalogueHashVersion::Version2
        );

        let function = &prepared.application_revision().candidate().functions()[0];
        let revision = &prepared.application_revision().new_function_revisions()[0];
        assert_eq!(function.domain(), FunctionDomain::Client);
        assert_eq!(
            function.return_type(),
            &FunctionReturn::Single(ResolvedType::Value(TypeId::from_bytes([3; 16])))
        );
        assert!(
            orna_core::revision::validate_persistable_catalogue(prepared.application_revision())
                .is_ok()
        );
        assert_eq!(
            revision.semantic_hash_version(),
            FunctionSemanticHashVersion::Version2
        );
        assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Client);
        assert_eq!(
            revision.artifact().payload(),
            b"ORNACP\0\0\0\0\0\x01\x01\x01"
        );
        assert_eq!(
            revision.artifact().payload(),
            active.function_revisions()[0].artifact().payload()
        );
        assert_eq!(
            revision.artifact().content_hash(),
            active.function_revisions()[0].artifact().content_hash()
        );
        assert_eq!(prepared.application_revision().references().len(), 1);
        assert_eq!(
            prepared.application_revision().references()[0].target(),
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
        );
    }

    #[test]
    fn standard_upgrade_reuses_an_exact_historical_version_two_revision() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);
        let first = prepare_checked_standard_upgrade(&standard, &active).unwrap();
        let historical = first.application_revision().new_function_revisions()[0].clone();
        let active = active_with_history(&active, vec![historical.clone()]);

        let reused = prepare_checked_standard_upgrade(&standard, &active).unwrap();

        assert!(
            reused
                .application_revision()
                .new_function_revisions()
                .is_empty()
        );
        assert_eq!(
            reused.application_revision().candidate().functions()[0].current_revision(),
            historical.id()
        );
    }

    #[test]
    fn standard_upgrade_rejects_near_matching_historical_version_two_revisions_for_reuse() {
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);
        let first = prepare_checked_standard_upgrade(&standard, &active).unwrap();
        let historical = first.application_revision().new_function_revisions()[0].clone();
        let mut payload = historical.artifact().payload().to_vec();
        payload.push(0);
        let wrong_artifact = ExecutableArtifact::new(
            historical.artifact().kind(),
            historical.artifact().format(),
            historical.artifact().version(),
            payload.clone(),
            artifact_payload_digest(&payload).unwrap(),
        )
        .unwrap();
        let revisions = [
            FunctionRevisionRecord::new(
                historical.function(),
                historical.id(),
                historical.revision_number(),
                historical.declaration_origin(),
                historical.declaration_content_hash(),
                historical.semantic_hash(),
                "orna.language/changed",
                historical.artifact().clone(),
            )
            .unwrap()
            .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
            FunctionRevisionRecord::new(
                historical.function(),
                historical.id(),
                historical.revision_number(),
                historical.declaration_origin(),
                historical.declaration_content_hash(),
                historical.semantic_hash(),
                historical.language_version(),
                wrong_artifact,
            )
            .unwrap()
            .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
            FunctionRevisionRecord::new(
                historical.function(),
                historical.id(),
                historical.revision_number(),
                historical.declaration_origin(),
                historical.declaration_content_hash(),
                Sha256Digest::from_bytes([0xf1; 32]),
                historical.language_version(),
                historical.artifact().clone(),
            )
            .unwrap()
            .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
        ];

        for historical in revisions {
            let active = active_with_history(&active, vec![historical.clone()]);
            let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();

            assert_eq!(
                prepared
                    .application_revision()
                    .new_function_revisions()
                    .len(),
                1
            );
            assert_ne!(
                prepared.application_revision().new_function_revisions()[0].id(),
                historical.id()
            );
        }
    }

    #[test]
    fn standard_upgrade_checks_history_for_reuse_before_revision_number_exhaustion() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let empty = empty_version_one_active();
        let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
        let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
        let version_one =
            prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
        let active = active_from_prepared_version_one_candidate(&version_one);
        let first = prepare_checked_standard_upgrade(&standard, &active).unwrap();
        let historical = first.application_revision().new_function_revisions()[0].clone();
        let exact_maximum = FunctionRevisionRecord::new(
            historical.function(),
            historical.id(),
            u64::MAX,
            historical.declaration_origin(),
            historical.declaration_content_hash(),
            historical.semantic_hash(),
            historical.language_version(),
            historical.artifact().clone(),
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let reusable = active_with_history(&active, vec![exact_maximum.clone()]);

        let prepared = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &reusable,
            retrying_standard_allocator(&verified),
        )
        .unwrap();

        assert!(
            prepared
                .application_revision()
                .new_function_revisions()
                .is_empty()
        );
        assert_eq!(
            prepared.application_revision().candidate().functions()[0].current_revision(),
            exact_maximum.id()
        );
        assert_eq!(
            PREPARE_FUNCTION_REVISION_ALLOCATIONS.load(Ordering::SeqCst),
            0
        );

        let non_reusable_maximum = FunctionRevisionRecord::new(
            historical.function(),
            historical.id(),
            u64::MAX,
            historical.declaration_origin(),
            historical.declaration_content_hash(),
            historical.semantic_hash(),
            "orna.language/changed",
            historical.artifact().clone(),
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
        let exhausted = active_with_history(&active, vec![non_reusable_maximum]);

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &exhausted,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PrepareStandardUpgradeError::FunctionRevisionNumberExhausted { function }
                if function == historical.function()
        ));
        assert_eq!(error.to_string(), "function revision number is exhausted");
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn standard_upgrade_rejects_std_namespace_before_reserved_identities() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let catalogue = CatalogueSnapshot::new(
            verified.catalogue().revision(),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0xb1; 16]),
                semantic_name(["std"]),
            )],
            Vec::new(),
        )
        .unwrap();
        let active = version_one_active_with_origins(
            "CREATE SCHEMA std;",
            catalogue,
            vec![DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes([0xb1; 16])),
                SourceOrigin::new(SourceUnitId::from_bytes([0xa1; 16]), 0, 18).unwrap(),
            )],
        );

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            PrepareStandardUpgradeError::NamespaceOccupied { name }
                if name == &semantic_name(["std"])
        ));
        assert_eq!(
            error.to_string(),
            "the application catalogue already uses the reserved std namespace"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn standard_upgrade_namespace_gate_uses_snapshot_family_order_and_first_name() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let first_schema = SchemaDefinition::new(
            SchemaId::from_bytes([0xd1; 16]),
            semantic_name(["std", "first"]),
        );
        let second_schema = SchemaDefinition::new(
            SchemaId::from_bytes([0xd2; 16]),
            semantic_name(["std", "second"]),
        );
        let object = ObjectTypeDefinition::new(
            TypeId::from_bytes([0xd3; 16]),
            semantic_name(["std", "first", "object"]),
            Vec::new(),
        );
        let function_id = FunctionId::from_bytes([0xd5; 16]);
        let function_revision_id = FunctionRevisionId::from_bytes([0xd6; 16]);
        let function = FunctionDefinition::new(
            function_id,
            semantic_name(["std", "first", "function"]),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            function_revision_id,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let function_payload = b"namespace-function".to_vec();
        let function_artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "orna.server-plan",
            1,
            function_payload.clone(),
            artifact_payload_digest(&function_payload).unwrap(),
        )
        .unwrap();
        let function_revision = FunctionRevisionRecord::new(
            function_id,
            function_revision_id,
            1,
            SourceOrigin::new(SourceUnitId::from_bytes([0xd8; 16]), 3, 4).unwrap(),
            Sha256Digest::from_bytes([0xd9; 32]),
            function_semantic_digest(&function, "orna.language/1", &function_artifact, &[], &[])
                .unwrap(),
            "orna.language/1",
            function_artifact,
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new_with_functions_and_types(
            CatalogueRevisionId::from_bytes([0xd7; 16]),
            vec![first_schema.clone(), second_schema],
            vec![object],
            Vec::new(),
            Vec::new(),
            vec![function],
        )
        .unwrap();
        assert_eq!(catalogue.schemas()[0].name(), first_schema.name());
        assert_eq!(catalogue.object_types().len(), 1);
        assert_eq!(catalogue.value_types().len(), 0);
        assert_eq!(catalogue.type_bindings().len(), 0);
        assert_eq!(catalogue.functions().len(), 1);

        let source_unit = SourceUnitId::from_bytes([0xd8; 16]);
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(first_schema.id()),
                SourceOrigin::new(source_unit, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes([0xd2; 16])),
                SourceOrigin::new(source_unit, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(TypeId::from_bytes([0xd3; 16])),
                SourceOrigin::new(source_unit, 2, 3).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Function(function_id),
                SourceOrigin::new(source_unit, 3, 4).unwrap(),
            ),
        ];
        let source = stored_source_with_ids(
            "0123456789",
            source_unit,
            SourceBundleId::from_bytes([0xd9; 16]),
            SourceRevisionId::from_bytes([0xda; 16]),
        );
        let catalogue_hash = catalogue_digest(
            &catalogue,
            std::slice::from_ref(&function_revision),
            &[],
            &origins,
            &[],
        )
        .unwrap();
        let active = ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            vec![function_revision],
            origins,
            Vec::new(),
        )
        .unwrap();
        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            PrepareStandardUpgradeError::NamespaceOccupied { name }
                if name == first_schema.name()
        ));
        assert_eq!(
            error.to_string(),
            "the application catalogue already uses the reserved std namespace"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn standard_upgrade_reaches_schema_name_conflict_for_a_non_std_checked_standard() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_non_std_schema_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let source_text = "CREATE SCHEMA library;";
        let source_unit = SourceUnitId::from_bytes([0xa0; 16]);
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0xa3; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0xa4; 16]),
                semantic_name(["library"]),
            )],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let active = version_one_active_with_source(
            stored_source_with_ids(
                source_text,
                source_unit,
                SourceBundleId::from_bytes([0xa1; 16]),
                SourceRevisionId::from_bytes([0xa2; 16]),
            ),
            catalogue,
            vec![DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes([0xa4; 16])),
                SourceOrigin::new(source_unit, 0, source_text.len() as u32).unwrap(),
            )],
        );

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        let expected = StandardApplicationContextError::SchemaNameConflict {
            name: semantic_name(["library"]),
        };
        assert!(matches!(
            &error,
            PrepareStandardUpgradeError::Context { source } if source == &expected
        ));
        assert_eq!(
            error.to_string(),
            "the checked standard library cannot form an application context: the application catalogue conflicts with standard schema name library"
        );
        assert!(std::error::Error::source(&error).is_some());
        let nested = std::error::Error::source(&error).unwrap();
        assert_eq!(nested.to_string(), expected.to_string());
        assert!(std::error::Error::source(nested).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn standard_upgrade_reserved_schema_identity_precedes_non_std_schema_name_conflict() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_non_std_schema_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let source_text = "CREATE SCHEMA library;";
        let source_unit = SourceUnitId::from_bytes([0xa5; 16]);
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0xa6; 16]),
            vec![SchemaDefinition::new(
                verified.catalogue().schemas()[0].id(),
                semantic_name(["library"]),
            )],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let active = version_one_active_with_source(
            stored_source_with_ids(
                source_text,
                source_unit,
                SourceBundleId::from_bytes([0xa7; 16]),
                SourceRevisionId::from_bytes([0xa8; 16]),
            ),
            catalogue,
            vec![DefinitionOrigin::new(
                DefinitionIdentity::Schema(verified.catalogue().schemas()[0].id()),
                SourceOrigin::new(source_unit, 0, source_text.len() as u32).unwrap(),
            )],
        );

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PrepareStandardUpgradeError::ReservedIdentity {
                identity: super::StandardUpgradeIdentity::Schema(id),
            } if id == verified.catalogue().schemas()[0].id()
        ));
        assert_eq!(
            error.to_string(),
            "the application state conflicts with a reserved standard library identity"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn standard_upgrade_rejects_reserved_catalogue_identity_before_context_and_source() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let catalogue =
            CatalogueSnapshot::new(verified.catalogue().revision(), Vec::new(), Vec::new())
                .unwrap();
        let active = version_one_active_with("CREATE SCHEMA ;", catalogue);

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PrepareStandardUpgradeError::ReservedIdentity {
                identity: super::StandardUpgradeIdentity::CatalogueRevision(id),
            } if id == verified.catalogue().revision()
        ));
        assert_eq!(
            error.to_string(),
            "the application state conflicts with a reserved standard library identity"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    #[test]
    fn standard_upgrade_reserved_identity_gate_checks_every_visible_class_in_order() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let safe_source = |unit_id, bundle_id, revision_id| {
            stored_source_with_ids("", unit_id, bundle_id, revision_id)
        };
        let empty_catalogue = |revision| {
            CatalogueSnapshot::new_with_types(
                revision,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap()
        };
        let source_unit = SourceUnitId::from_bytes([0x91; 16]);
        let source_bundle = SourceBundleId::from_bytes([0x92; 16]);
        let source_revision = SourceRevisionId::from_bytes([0x93; 16]);
        let application_catalogue = CatalogueRevisionId::from_bytes([0x94; 16]);
        let cases = [
            (
                version_one_active_with_source(
                    safe_source(
                        verified.source().units()[0].id(),
                        verified.source().bundle(),
                        verified.source().id(),
                    ),
                    empty_catalogue(verified.catalogue().revision()),
                    Vec::new(),
                ),
                super::StandardUpgradeIdentity::CatalogueRevision(verified.catalogue().revision()),
            ),
            (
                version_one_active_with_source(
                    safe_source(
                        verified.source().units()[0].id(),
                        verified.source().bundle(),
                        verified.source().id(),
                    ),
                    empty_catalogue(application_catalogue),
                    Vec::new(),
                ),
                super::StandardUpgradeIdentity::SourceBundle(verified.source().bundle()),
            ),
            (
                version_one_active_with_source(
                    safe_source(
                        verified.source().units()[0].id(),
                        source_bundle,
                        verified.source().id(),
                    ),
                    empty_catalogue(application_catalogue),
                    Vec::new(),
                ),
                super::StandardUpgradeIdentity::SourceRevision(verified.source().id()),
            ),
            (
                version_one_active_with_source(
                    safe_source(
                        verified.source().units()[0].id(),
                        source_bundle,
                        source_revision,
                    ),
                    empty_catalogue(application_catalogue),
                    Vec::new(),
                ),
                super::StandardUpgradeIdentity::SourceUnit(verified.source().units()[0].id()),
            ),
        ];

        for (active, expected) in cases {
            let error = prepare_checked_standard_upgrade_with_allocator(
                &standard,
                &active,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                PrepareStandardUpgradeError::ReservedIdentity { identity } if identity == expected
            ));
            assert_no_standard_upgrade_allocations();
        }

        let schema_source = "CREATE SCHEMA app;CREATE TYPE app.schema_first AS OBJECT ();";
        let schema_id = verified.catalogue().schemas()[0].id();
        let schema_type = verified.catalogue().value_types()[0].id();
        let schema_type_start = "CREATE SCHEMA app;".len() as u32;
        let schema_catalogue = CatalogueSnapshot::new(
            application_catalogue,
            vec![SchemaDefinition::new(schema_id, semantic_name(["app"]))],
            vec![ObjectTypeDefinition::new(
                schema_type,
                semantic_name(["app", "schema_first"]),
                Vec::new(),
            )],
        )
        .unwrap();
        let schema_active = version_one_active_with_source(
            stored_source_with_ids(schema_source, source_unit, source_bundle, source_revision),
            schema_catalogue,
            vec![
                DefinitionOrigin::new(
                    DefinitionIdentity::Schema(schema_id),
                    SourceOrigin::new(source_unit, 0, schema_type_start).unwrap(),
                ),
                DefinitionOrigin::new(
                    DefinitionIdentity::ObjectType(schema_type),
                    SourceOrigin::new(source_unit, schema_type_start, schema_source.len() as u32)
                        .unwrap(),
                ),
            ],
        );
        let schema_error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &schema_active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(matches!(
            schema_error,
            PrepareStandardUpgradeError::ReservedIdentity {
                identity: super::StandardUpgradeIdentity::Schema(id),
            } if id == schema_id
        ));
        assert_no_standard_upgrade_allocations();

        let type_source = "CREATE SCHEMA app;CREATE TYPE app.item AS OBJECT ();";
        let application_schema = SchemaId::from_bytes([0x95; 16]);
        let type_id = verified.catalogue().value_types()[0].id();
        let type_start = "CREATE SCHEMA app;".len() as u32;
        let type_catalogue = CatalogueSnapshot::new(
            application_catalogue,
            vec![SchemaDefinition::new(
                application_schema,
                semantic_name(["app"]),
            )],
            vec![ObjectTypeDefinition::new(
                type_id,
                semantic_name(["app", "item"]),
                Vec::new(),
            )],
        )
        .unwrap();
        let type_active = version_one_active_with_source(
            stored_source_with_ids(type_source, source_unit, source_bundle, source_revision),
            type_catalogue,
            vec![
                DefinitionOrigin::new(
                    DefinitionIdentity::Schema(application_schema),
                    SourceOrigin::new(source_unit, 0, type_start).unwrap(),
                ),
                DefinitionOrigin::new(
                    DefinitionIdentity::ObjectType(type_id),
                    SourceOrigin::new(source_unit, type_start, type_source.len() as u32).unwrap(),
                ),
            ],
        );
        let type_error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &type_active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(matches!(
            type_error,
            PrepareStandardUpgradeError::ReservedIdentity {
                identity: super::StandardUpgradeIdentity::Type(id),
            } if id == type_id
        ));
        assert_no_standard_upgrade_allocations();

        let binding_source = "CREATE SCHEMA app;CREATE TYPE app.flag AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE app.flag TO PRELUDE AS BOOLEAN;";
        let binding_schema = SchemaId::from_bytes([0x96; 16]);
        let binding_type = TypeId::from_bytes([0x97; 16]);
        let binding_value = ValueTypeDefinition::primitive(
            binding_type,
            semantic_name(["app", "flag"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let binding = TypeBinding::prelude(
            PreludeTypeName::new(["boolean"]).unwrap(),
            binding_value.id(),
        )
        .unwrap();
        let binding_start = binding_source.find("EXPORT TYPE").unwrap() as u32;
        let binding_catalogue = CatalogueSnapshot::new_with_types(
            application_catalogue,
            vec![SchemaDefinition::new(
                binding_schema,
                semantic_name(["app"]),
            )],
            Vec::new(),
            vec![binding_value],
            vec![binding.clone()],
        )
        .unwrap();
        let binding_origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(binding_schema),
                SourceOrigin::new(source_unit, 0, type_start).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(binding_type),
                SourceOrigin::new(source_unit, type_start, binding_start).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::TypeBinding(binding.id()),
                SourceOrigin::new(source_unit, binding_start, binding_source.len() as u32).unwrap(),
            ),
        ];
        let binding_source_revision =
            stored_source_with_ids(binding_source, source_unit, source_bundle, source_revision);
        let binding_context = CatalogueHashContext::version_two(verified.clone());
        let binding_hash = catalogue_digest_with_context(
            &binding_context,
            &binding_catalogue,
            &[],
            &[],
            &binding_origins,
            &[],
        )
        .unwrap();
        let binding_active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(binding_source_revision.id(), binding_catalogue.revision()),
                binding_source_revision.clone(),
                binding_catalogue,
                binding_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), binding_origins, Vec::new()),
            ),
            binding_context.clone(),
        )
        .unwrap();
        assert_eq!(
            crate::prepare::active_reserved_standard_identity(&standard, &binding_active),
            Some(super::StandardUpgradeIdentity::TypeBinding(binding.id()))
        );

        let binding_type_collision = ValueTypeDefinition::primitive(
            type_id,
            semantic_name(["app", "type_first"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let binding_after_type = TypeBinding::prelude(
            PreludeTypeName::new(["boolean"]).unwrap(),
            binding_type_collision.id(),
        )
        .unwrap();
        let type_before_binding_catalogue = CatalogueSnapshot::new_with_types(
            application_catalogue,
            vec![SchemaDefinition::new(
                binding_schema,
                semantic_name(["app"]),
            )],
            Vec::new(),
            vec![binding_type_collision],
            vec![binding_after_type.clone()],
        )
        .unwrap();
        let type_before_binding_origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(binding_schema),
                SourceOrigin::new(source_unit, 0, type_start).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(type_id),
                SourceOrigin::new(source_unit, type_start, binding_start).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::TypeBinding(binding_after_type.id()),
                SourceOrigin::new(source_unit, binding_start, binding_source.len() as u32).unwrap(),
            ),
        ];
        let type_before_binding_hash = catalogue_digest_with_context(
            &binding_context,
            &type_before_binding_catalogue,
            &[],
            &[],
            &type_before_binding_origins,
            &[],
        )
        .unwrap();
        let type_before_binding_active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(
                    binding_source_revision.id(),
                    type_before_binding_catalogue.revision(),
                ),
                binding_source_revision,
                type_before_binding_catalogue,
                type_before_binding_hash,
                ActiveRevisionContent::new(
                    Vec::new(),
                    Vec::new(),
                    type_before_binding_origins,
                    Vec::new(),
                ),
            ),
            binding_context,
        )
        .unwrap();
        assert_eq!(
            crate::prepare::active_reserved_standard_identity(
                &standard,
                &type_before_binding_active
            ),
            Some(super::StandardUpgradeIdentity::Type(type_id))
        );
    }

    #[test]
    fn standard_upgrade_maps_reachable_context_contract_failures_before_source_work() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let active = version_one_active_with(
            "CREATE SCHEMA ;",
            CatalogueSnapshot::new(
                CatalogueRevisionId::from_bytes([0xb3; 16]),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
        );
        let unsupported =
            super::resolver::checked_standard_library_with_contract_overrides_for_test(
                &verified,
                &[(0, "unsupported@1")],
            )
            .unwrap();
        let duplicate = super::resolver::checked_standard_library_with_contract_overrides_for_test(
            &verified_canonical_standard_source_fixture(),
            &[(1, "orna.kernel.value.boolean@1")],
        )
        .unwrap();
        let cases = [
            (
                &unsupported,
                StandardApplicationContextError::UnsupportedCompatibilityContract {
                    type_id: TypeId::from_bytes([3; 16]),
                    contract: "unsupported@1".to_owned(),
                },
            ),
            (
                &duplicate,
                StandardApplicationContextError::CompatibilityContractConflict {
                    contract: "orna.kernel.value.boolean@1".to_owned(),
                },
            ),
        ];

        for (standard, expected) in cases {
            let error = prepare_checked_standard_upgrade_with_allocator(
                standard,
                &active,
                retrying_standard_allocator(standard.verified_snapshot()),
            )
            .unwrap_err();

            assert!(matches!(
                &error,
                PrepareStandardUpgradeError::Context { source } if source == &expected
            ));
            assert_eq!(
                error.to_string(),
                format!(
                    "the checked standard library cannot form an application context: {expected}"
                )
            );
            assert!(std::error::Error::source(&error).is_some());
            assert_no_standard_upgrade_allocations();
        }
    }

    #[test]
    fn standard_upgrade_returns_parser_diagnostics_before_active_source_matching() {
        let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
        let verified = verified_standard_source_fixture();
        let standard = check_standard_library_source(&verified).unwrap();
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0xb2; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let active = version_one_active_with("CREATE SCHEMA ;", catalogue);
        let expected = parse_bundle(
            &SourceBundle::new([SourceUnit::new("active.orna", "CREATE SCHEMA ;")]).unwrap(),
        )
        .diagnostics()
        .to_vec();

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            PrepareStandardUpgradeError::ActiveSourceDiagnostics { .. }
        ));
        if let PrepareStandardUpgradeError::ActiveSourceDiagnostics { diagnostics } = &error {
            assert_eq!(diagnostics.as_slice(), expected.as_slice());
        }
        assert_no_standard_upgrade_allocations();
    }
}
