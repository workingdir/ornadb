use orna_artifact::server_json_encode::{self, JsonEncodePlan, JsonEncodePlanError};
use orna_artifact::server_parameter_echo::{self, ServerParameterEcho, ServerParameterEchoError};
use orna_artifact::server_plan::{IdentitySelector, Scan, ValueType};
use orna_artifact::server_terminal_table::{self, TerminalTablePlan, TerminalTablePlanError};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, InvocationId, ParameterId, PrincipalId, SchemaId,
    SourceBundleId, SourceRevisionId, SourceUnitId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, FunctionReturnColumnDefinition,
        ObjectTypeDefinition, ParameterDefinition, QualifiedSemanticName,
        RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
    },
    invocation::{
        InvocationClientOffer, InvocationEventBody, InvocationOutputTypeSelector,
        InvocationSinkOffer, InvocationStreamingRequirement, InvokeValue,
    },
    revision::{
        ActiveDatabaseRevisionInput, ActiveRevisionContent, CatalogueHashContext,
        DefinitionIdentity, DefinitionOrigin, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, Sha256Digest, SourceOrigin, StoredSourceRevision, StoredSourceUnit,
        VerifiedStandardLibrarySnapshot,
    },
    types::TypeDescriptor,
};

use super::*;
#[path = "tests/presenters.rs"]
mod presenters;

/// The fixed ADR 0055 `std.invoke.echo` function identity: `...10`.
const STD_INVOKE_ECHO_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);
/// The fixed ADR 0055 `std.invoke.echo.p_value` parameter identity: `...10`.
const STD_INVOKE_ECHO_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);
/// The fixed ADR 0055 `std.invoke.echo` function-revision identity: `...10`.
const STD_INVOKE_ECHO_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);

fn echo_parameter(parameter: ParameterId) -> ParameterDefinition {
    ParameterDefinition::new(
        parameter,
        "p_value",
        0,
        ResolvedType::value(orna_standard::INTEGER_TYPE_ID),
        None,
    )
}

fn echo_function(function: FunctionId, parameter: ParameterId) -> FunctionDefinition {
    FunctionDefinition::new(
        function,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        vec![echo_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

fn echo_payload(parameter: ParameterId) -> Vec<u8> {
    ServerParameterEcho::new(parameter, orna_standard::INTEGER_TYPE_ID)
        .expect("any identities form a valid echo model")
        .encode()
        .expect("the canonical echo model encodes")
}

fn artifact(
    kind: ExecutableArtifactKind,
    format: &str,
    version: u32,
    payload: Vec<u8>,
) -> ExecutableArtifact {
    let content_hash = artifact_payload_digest(&payload).expect("the payload digests");
    ExecutableArtifact::new(kind, format, version, payload, content_hash)
        .expect("the artifact is valid")
}

fn echo_artifact(parameter: ParameterId) -> ExecutableArtifact {
    artifact(
        ExecutableArtifactKind::Server,
        server_parameter_echo::FORMAT_IDENTITY,
        server_parameter_echo::FORMAT_VERSION,
        echo_payload(parameter),
    )
}

fn revision_with_artifact(
    function: FunctionId,
    artifact: ExecutableArtifact,
) -> FunctionRevisionRecord {
    FunctionRevisionRecord::new(
        function,
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        1,
        SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), 0, 1)
            .expect("a test source origin is valid"),
        Sha256Digest::from_bytes([0x42; 32]),
        Sha256Digest::from_bytes([0x43; 32]),
        server_parameter_echo::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .expect("the test revision is valid")
}

fn echo_revision(function: FunctionId, parameter: ParameterId) -> FunctionRevisionRecord {
    revision_with_artifact(function, echo_artifact(parameter))
}

/// The active-catalogue object type targeted by presenter reference tests.
const PRESENTER_OBJECT_TYPE: TypeId = TypeId::from_bytes([0x91; 16]);
/// The active-catalogue enum type rendered by table cells.
const PRESENTER_ENUM_TYPE: TypeId = TypeId::from_bytes([0x92; 16]);
/// The active-catalogue record type rendered by table cells.
const PRESENTER_RECORD_TYPE: TypeId = TypeId::from_bytes([0x93; 16]);
const PRESENTER_RECORD_X_FIELD: FieldId = FieldId::from_bytes([0x94; 16]);
const PRESENTER_RECORD_Y_FIELD: FieldId = FieldId::from_bytes([0x95; 16]);

/// Verifies the retained `orna.std/3` standard snapshot.
fn presenter_standard() -> VerifiedStandardLibrarySnapshot {
    orna_standard::verify_standard_library_v3_snapshot(
        orna_standard::retained_standard_library_v3_snapshot()
            .expect("the retained V3 standard source is valid"),
    )
    .expect("the retained V3 standard source verifies")
}

/// Verifies the retained `orna.std/5` standard snapshot.
fn presenter_v5_standard() -> VerifiedStandardLibrarySnapshot {
    orna_standard::verify_standard_library_v5_snapshot(
        orna_standard::retained_standard_library_v5_snapshot()
            .expect("the retained V5 standard source is valid"),
    )
    .expect("the retained V5 standard source verifies")
}

/// Verifies the append-only `orna.std/8` Rows snapshot.
fn presenter_v8_standard() -> VerifiedStandardLibrarySnapshot {
    orna_standard::verify_standard_library_v8_snapshot(
        orna_standard::retained_standard_library_v8_snapshot()
            .expect("the retained V8 standard source is valid"),
    )
    .expect("the retained V8 standard source verifies")
}

/// Verifies the append-only `orna.std/9` standard snapshot.
fn presenter_v9_standard() -> VerifiedStandardLibrarySnapshot {
    orna_standard::verify_standard_library_v9_snapshot(
        orna_standard::retained_standard_library_v9_snapshot()
            .expect("the retained V9 standard source is valid"),
    )
    .expect("the retained V9 standard source verifies")
}

fn presenter_client_offer() -> InvocationClientOffer {
    let document = InvocationSinkOffer::new(
        TypeDescriptor::named(orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID),
        ["text/plain"],
        false,
        0,
        None,
    )
    .expect("a valid document sink offer");
    let byte_stream = InvocationSinkOffer::new(
        TypeDescriptor::named(orna_standard::STD_IO_BYTE_STREAM_TYPE_ID),
        ["application/octet-stream", "application/json", "text/csv"],
        false,
        0,
        None,
    )
    .expect("a valid byte-stream sink offer");
    InvocationClientOffer::new(
        5,
        "en-GB",
        "Europe/London",
        [document, byte_stream],
        [],
        1_024,
        0,
        None,
        None,
    )
    .expect("a valid presenter client offer")
}

/// Builds the active revision the presenter tests execute against: an
/// application catalogue holding one object type, one enum type, and one
/// record type, pinned to the verified V3 standard snapshot.
fn presenter_active(standard: &VerifiedStandardLibrarySnapshot) -> ActiveDatabaseRevision {
    let schema = SchemaId::from_bytes([0x81; 16]);
    let catalogue_revision = CatalogueRevisionId::from_bytes([0x82; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        catalogue_revision,
        vec![SchemaDefinition::new(schema, name(&["app"]))],
        vec![ObjectTypeDefinition::new(
            PRESENTER_OBJECT_TYPE,
            name(&["app", "item"]),
            vec![],
        )],
        vec![],
        vec![EnumTypeDefinition::new(
            PRESENTER_ENUM_TYPE,
            name(&["app", "stage"]),
            ["lead", "qualified"],
        )],
        vec![RecordValueTypeDefinition::new(
            PRESENTER_RECORD_TYPE,
            name(&["app", "status"]),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    PRESENTER_RECORD_X_FIELD,
                    "x",
                    0,
                    TypeDescriptor::named(orna_standard::INTEGER_TYPE_ID),
                )
                .expect("the record field descriptor is valid"),
                RecordValueFieldDefinition::try_new_descriptor(
                    PRESENTER_RECORD_Y_FIELD,
                    "y",
                    1,
                    TypeDescriptor::named(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
                )
                .expect("the record field descriptor is valid"),
            ],
        )],
        vec![],
    )
    .expect("the presenter test catalogue is valid");
    let context = CatalogueHashContext::version_two(standard.clone());
    let source_content = "abcdef";
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x83; 16]),
        0,
        "app/types.orna",
        source_content,
        source_unit_content_digest(source_content).expect("the source unit digests"),
    )
    .expect("the source unit is valid");
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit))
        .expect("the source bundle digests");
    let source_revision = SourceRevisionId::from_bytes([0x84; 16]);
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x85; 16]),
        source_revision,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x85; 16]), None, bundle_hash)
            .expect("the source revision record digests"),
    )
    .expect("the stored source revision is valid");
    let source_unit = SourceUnitId::from_bytes([0x83; 16]);
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema),
            SourceOrigin::new(source_unit, 0, 1).expect("the test origin is valid"),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(PRESENTER_OBJECT_TYPE),
            SourceOrigin::new(source_unit, 1, 2).expect("the test origin is valid"),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(PRESENTER_ENUM_TYPE),
            SourceOrigin::new(source_unit, 2, 3).expect("the test origin is valid"),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(PRESENTER_RECORD_TYPE),
            SourceOrigin::new(source_unit, 3, 4).expect("the test origin is valid"),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: PRESENTER_RECORD_TYPE,
                field: PRESENTER_RECORD_X_FIELD,
            },
            SourceOrigin::new(source_unit, 4, 5).expect("the test origin is valid"),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: PRESENTER_RECORD_TYPE,
                field: PRESENTER_RECORD_Y_FIELD,
            },
            SourceOrigin::new(source_unit, 5, 6).expect("the test origin is valid"),
        ),
    ];
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[])
            .expect("the active catalogue digests");
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source_revision, catalogue_revision),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
        ),
        context,
    )
    .expect("the presenter active revision is valid")
}

fn presenter_active_with_application_json_value_type(
    standard: &VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let base = presenter_active(standard);
    let application_type = TypeId::from_bytes([0x96; 16]);
    let schema = SchemaDefinition::new(SchemaId::from_bytes([0x97; 16]), name(&["std", "json"]));
    let json_name = name(&["std", "json", "value"]);
    let mut schemas = base.catalogue().schemas().to_vec();
    schemas.push(schema);
    let mut object_types = base.catalogue().object_types().to_vec();
    object_types.push(ObjectTypeDefinition::new(
        application_type,
        json_name.clone(),
        vec![],
    ));
    let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
        base.catalogue().revision(),
        schemas,
        object_types,
        base.catalogue().value_types().to_vec(),
        base.catalogue().enum_types().to_vec(),
        base.catalogue().record_value_types().to_vec(),
        base.catalogue().type_bindings().to_vec(),
        base.catalogue().functions().to_vec(),
    )
    .expect("the collision catalogue is valid");
    let source_unit = SourceUnitId::from_bytes([0x83; 16]);
    let mut origins = base.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([0x97; 16])),
        SourceOrigin::new(source_unit, 1, 2).expect("the collision schema origin is valid"),
    ));
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::ObjectType(application_type),
        SourceOrigin::new(source_unit, 2, 3).expect("the collision type origin is valid"),
    ));
    let context = base.catalogue_hash_context().clone();
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        base.function_revisions(),
        base.expressions(),
        &origins,
        base.references(),
    )
    .expect("the collision catalogue digests");
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            base.pair(),
            base.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                base.expressions().to_vec(),
                base.function_revisions().to_vec(),
                origins,
                base.references().to_vec(),
            ),
        ),
        context,
    )
    .expect("the collision active revision is valid")
}

fn json_encode_parameter(parameter: ParameterId) -> ParameterDefinition {
    ParameterDefinition::new(
        parameter,
        "p_value",
        0,
        ResolvedType::named(STD_JSON_VALUE_TYPE_ID),
        None,
    )
}

fn json_encode_function(
    function: FunctionId,
    parameter: ParameterId,
    revision: FunctionRevisionId,
) -> FunctionDefinition {
    FunctionDefinition::new(
        function,
        name(&["std", "json", "encode"]),
        FunctionDomain::Server,
        vec![json_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

fn terminal_table_parameter(parameter: ParameterId) -> ParameterDefinition {
    ParameterDefinition::new(
        parameter,
        "p_rows",
        0,
        ResolvedType::named(STD_DATA_ROWS_TYPE_ID),
        None,
    )
}

fn terminal_table_function(
    function: FunctionId,
    parameter: ParameterId,
    revision: FunctionRevisionId,
) -> FunctionDefinition {
    FunctionDefinition::new(
        function,
        name(&["std", "terminal", "present_table"]),
        FunctionDomain::Server,
        vec![terminal_table_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_TERMINAL_DOCUMENT_TYPE_ID,
        )),
        revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

fn json_encode_payload(parameter: ParameterId) -> Vec<u8> {
    JsonEncodePlan::new(parameter, STD_JSON_VALUE_TYPE_ID)
        .expect("any identities form a valid json-encode model")
        .encode()
        .expect("the canonical json-encode model encodes")
}

fn terminal_table_payload(parameter: ParameterId) -> Vec<u8> {
    TerminalTablePlan::new(parameter, STD_DATA_ROWS_TYPE_ID)
        .expect("any identities form a valid terminal-table model")
        .encode()
        .expect("the canonical terminal-table model encodes")
}

fn json_encode_artifact(parameter: ParameterId) -> ExecutableArtifact {
    artifact(
        ExecutableArtifactKind::Server,
        server_json_encode::FORMAT_IDENTITY,
        server_json_encode::FORMAT_VERSION,
        json_encode_payload(parameter),
    )
}

fn terminal_table_artifact(parameter: ParameterId) -> ExecutableArtifact {
    artifact(
        ExecutableArtifactKind::Server,
        server_terminal_table::FORMAT_IDENTITY,
        server_terminal_table::FORMAT_VERSION,
        terminal_table_payload(parameter),
    )
}

fn presenter_revision(
    function: FunctionId,
    revision_id: FunctionRevisionId,
    language_version: &str,
    artifact: ExecutableArtifact,
) -> FunctionRevisionRecord {
    FunctionRevisionRecord::new(
        function,
        revision_id,
        1,
        SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), 0, 1)
            .expect("a test source origin is valid"),
        Sha256Digest::from_bytes([0x42; 32]),
        Sha256Digest::from_bytes([0x43; 32]),
        language_version,
        artifact,
    )
    .expect("the test revision is valid")
}

fn json_encode_revision(function: FunctionId, parameter: ParameterId) -> FunctionRevisionRecord {
    presenter_revision(
        function,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        json_encode_artifact(parameter),
    )
}

fn terminal_table_revision(function: FunctionId, parameter: ParameterId) -> FunctionRevisionRecord {
    presenter_revision(
        function,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        terminal_table_artifact(parameter),
    )
}

fn csv_encode_parameter(parameter: ParameterId) -> ParameterDefinition {
    ParameterDefinition::new(
        parameter,
        "p_rows",
        0,
        ResolvedType::named(STD_DATA_ROWS_TYPE_ID),
        None,
    )
}

fn csv_encode_function(
    function: FunctionId,
    parameter: ParameterId,
    revision: FunctionRevisionId,
) -> FunctionDefinition {
    FunctionDefinition::new(
        function,
        name(&["std", "csv", "encode"]),
        FunctionDomain::Server,
        vec![csv_encode_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::named(
            orna_standard::STD_IO_BYTE_STREAM_TYPE_ID,
        )),
        revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}

fn csv_encode_payload(parameter: ParameterId) -> Vec<u8> {
    CsvEncodePlan::new(parameter, STD_DATA_ROWS_TYPE_ID)
        .expect("any identities form a valid csv-encode model")
        .encode()
        .expect("the canonical csv-encode model encodes")
}

fn csv_encode_artifact(parameter: ParameterId) -> ExecutableArtifact {
    artifact(
        ExecutableArtifactKind::Server,
        server_csv_encode::FORMAT_IDENTITY,
        server_csv_encode::FORMAT_VERSION,
        csv_encode_payload(parameter),
    )
}

fn csv_encode_revision(function: FunctionId, parameter: ParameterId) -> FunctionRevisionRecord {
    presenter_revision(
        function,
        STD_CSV_ENCODE_FUNCTION_REVISION_ID,
        server_csv_encode::LANGUAGE_VERSION_IDENTITY,
        csv_encode_artifact(parameter),
    )
}

fn assert_csv_encode_decode_rule(
    result: Result<RuntimeValue, PostgresKernelError>,
    expected: CsvEncodePlanError,
) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::CsvEncodeDecode(actual))) = result
    else {
        panic!("expected a csv-encode decode rejection");
    };
    assert_eq!(actual, expected);
}

fn json_encode_argument(parameter: ParameterId, value: RuntimeValue) -> FunctionArgument {
    FunctionArgument::new(parameter, value).expect("the bound json argument is valid")
}

fn assert_presenter_artifact_rule(
    result: Result<RuntimeValue, PostgresKernelError>,
    function: FunctionId,
    expected: &'static str,
) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact {
        function: actual_function,
        rule,
    })) = result
    else {
        panic!("expected an artifact rejection");
    };
    assert_eq!(actual_function, function);
    assert_eq!(rule, expected);
}

fn assert_json_encode_decode_rule(
    result: Result<RuntimeValue, PostgresKernelError>,
    expected: JsonEncodePlanError,
) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::JsonEncodeDecode(actual))) =
        result
    else {
        panic!("expected a json-encode decode rejection");
    };
    assert_eq!(actual, expected);
}

fn assert_terminal_table_decode_rule(
    result: Result<RuntimeValue, PostgresKernelError>,
    expected: TerminalTablePlanError,
) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::TerminalTableDecode(actual))) =
        result
    else {
        panic!("expected a terminal-table decode rejection");
    };
    assert_eq!(actual, expected);
}

fn assert_presenter_rule<T>(result: Result<T, PostgresKernelError>, expected: &'static str) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::Presenter { rule })) = result
    else {
        panic!("expected a presenter conversion rejection");
    };
    assert_eq!(rule, expected);
}

fn assert_presenter_domain_rule(result: Result<RuntimeValue, PostgresKernelError>) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::FunctionDomain { .. })) = result
    else {
        panic!("expected a function-domain rejection");
    };
}

fn assert_presenter_opaque_rule(result: Result<RuntimeValue, PostgresKernelError>) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::PresenterOpaque(_))) = result
    else {
        panic!("expected an opaque-value rejection");
    };
}

fn assert_echo_artifact_rule(
    result: Result<RuntimeValue, PostgresKernelError>,
    function: FunctionId,
    expected: &'static str,
) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact {
        function: actual_function,
        rule,
    })) = result
    else {
        panic!("expected an artifact rejection");
    };
    assert_eq!(actual_function, function);
    assert_eq!(rule, expected);
}

fn assert_echo_decode_rule(
    result: Result<RuntimeValue, PostgresKernelError>,
    expected: ServerParameterEchoError,
) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::ParameterEchoDecode(actual))) =
        result
    else {
        panic!("expected a parameter-echo decode rejection");
    };
    assert_eq!(actual, expected);
}

fn assert_echo_domain_rule(result: Result<RuntimeValue, PostgresKernelError>) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::FunctionDomain { .. })) = result
    else {
        panic!("expected a function-domain rejection");
    };
}

fn name(parts: &[&str]) -> QualifiedSemanticName {
    QualifiedSemanticName::new(parts.iter().copied()).unwrap()
}

fn catalogue() -> (CatalogueSnapshot, TypeId, FieldId, FieldId) {
    let source = TypeId::from_bytes([0x10; 16]);
    let target = TypeId::from_bytes([0x20; 16]);
    let reference = FieldId::from_bytes([0x11; 16]);
    let value = FieldId::from_bytes([0x21; 16]);
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x01; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x02; 16]),
            name(&["test"]),
        )],
        vec![
            ObjectTypeDefinition::new(
                source,
                name(&["test", "semantic_source"]),
                vec![FieldDefinition::new(
                    reference,
                    "semantic_reference",
                    0,
                    ResolvedType::reference(target),
                    true,
                    false,
                    None,
                    None,
                )],
            ),
            ObjectTypeDefinition::new(
                target,
                name(&["test", "semantic_target"]),
                vec![FieldDefinition::new(
                    value,
                    "semantic_value",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                    false,
                    None,
                    None,
                )],
            ),
        ],
    )
    .unwrap();
    (catalogue, source, reference, value)
}

fn nullable_text_path(
    source: TypeId,
    reference: FieldId,
    target: TypeId,
    value: FieldId,
) -> Expression {
    Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: vec![
                FieldStep {
                    owner: source,
                    field: reference,
                },
                FieldStep {
                    owner: target,
                    field: value,
                },
            ],
        },
        value_type: ValueType {
            resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            nullable: true,
        },
    }
}

fn retained_value_context(contract: &str) -> (CatalogueHashContext, TypeId) {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()
            .expect("retained standard-library snapshot"),
    )
    .expect("verified standard-library snapshot");
    let value_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|definition| definition.representation_contract() == contract)
        .expect("retained value type")
        .id();
    (CatalogueHashContext::version_two(standard), value_type)
}

fn catalogue_with_value_field(value_type: TypeId) -> (CatalogueSnapshot, TypeId, FieldId) {
    let source = TypeId::from_bytes([0x70; 16]);
    let field = FieldId::from_bytes([0x71; 16]);
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x72; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x73; 16]),
            name(&["value_test"]),
        )],
        vec![ObjectTypeDefinition::new(
            source,
            name(&["value_test", "source"]),
            vec![FieldDefinition::new(
                field,
                "value",
                0,
                ResolvedType::value(value_type),
                false,
                false,
                None,
                None,
            )],
        )],
    )
    .expect("value catalogue");
    (catalogue, source, field)
}

fn catalogue_with_record_field() -> (CatalogueSnapshot, TypeId, FieldId, TypeId) {
    let object = TypeId::from_bytes([0x74; 16]);
    let field = FieldId::from_bytes([0x75; 16]);
    let record = TypeId::from_bytes([0x76; 16]);
    let enum_type = TypeId::from_bytes([0x77; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes([0x78; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x79; 16]),
            name(&["record_test"]),
        )],
        vec![ObjectTypeDefinition::new(
            object,
            name(&["record_test", "object"]),
            vec![FieldDefinition::new(
                field,
                "status",
                0,
                ResolvedType::named(record),
                false,
                false,
                None,
                None,
            )],
        )],
        vec![],
        vec![EnumTypeDefinition::new(
            enum_type,
            name(&["record_test", "stage"]),
            ["lead"],
        )],
        vec![RecordValueTypeDefinition::new(
            record,
            name(&["record_test", "status"]),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes([0x7a; 16]),
                    "stage",
                    0,
                    TypeDescriptor::named(enum_type),
                )
                .expect("record field"),
            ],
        )],
        vec![],
    )
    .expect("record catalogue");
    (catalogue, object, field, record)
}

fn field_projection(source: TypeId, field: FieldId, scalar: StandardScalar) -> Expression {
    Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: vec![FieldStep {
                owner: source,
                field,
            }],
        },
        value_type: ValueType {
            resolved_type: ResolvedType::scalar(scalar),
            nullable: false,
        },
    }
}

fn function(
    domain: FunctionDomain,
    parameters: Vec<orna_core::catalogue::ParameterDefinition>,
    return_type: FunctionReturn,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
) -> FunctionDefinition {
    function_with_volatility(
        domain,
        parameters,
        return_type,
        security,
        transaction,
        FunctionVolatility::Stable,
    )
}

fn function_with_volatility(
    domain: FunctionDomain,
    parameters: Vec<orna_core::catalogue::ParameterDefinition>,
    return_type: FunctionReturn,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
    volatility: FunctionVolatility,
) -> FunctionDefinition {
    FunctionDefinition::new(
        FunctionId::from_bytes([0x31; 16]),
        name(&["test", "function"]),
        domain,
        parameters,
        return_type,
        FunctionRevisionId::from_bytes([0x32; 16]),
        security,
        transaction,
        volatility,
    )
}

fn rows_return() -> FunctionReturn {
    FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
        "value",
        0,
        ResolvedType::scalar(StandardScalar::Integer),
    )])
}

fn boolean_rows_return() -> FunctionReturn {
    FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
        "selected",
        0,
        ResolvedType::scalar(StandardScalar::Boolean),
    )])
}

fn assert_signature_rule<T>(
    result: Result<T, PostgresKernelError>,
    function: FunctionId,
    expected: &'static str,
) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::FunctionSignature {
        function: actual_function,
        rule,
    })) = result
    else {
        panic!("expected a function-signature rejection");
    };
    assert_eq!(actual_function, function);
    assert_eq!(rule, expected);
}

fn assert_plan_rule(result: Result<(), PostgresKernelError>, expected: &'static str) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::PlanInvariant { rule })) = result
    else {
        panic!("expected a saved-query rejection");
    };
    assert_eq!(rule, expected);
}

fn assert_distinct_rule<T>(result: Result<T, PostgresKernelError>, expected: &'static str) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::Distinct { rule })) = result
    else {
        panic!("expected a SELECT DISTINCT rejection");
    };
    assert_eq!(rule, expected);
}

fn assert_argument_rule<T>(
    result: Result<T, PostgresKernelError>,
    parameter: Option<ParameterId>,
    expected: &'static str,
) {
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::Argument {
        parameter: actual_parameter,
        rule,
    })) = result
    else {
        panic!("expected a function-argument rejection");
    };
    assert_eq!(actual_parameter, parameter);
    assert_eq!(rule, expected);
}

#[test]
fn row_execution_adapts_single_result_and_preserves_canonical_value() {
    let function = function(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    let adapted = row_execution_function(&function).expect("single adapts");
    let FunctionReturn::Rows(columns) = adapted.return_type() else {
        panic!("single execution view must be row-shaped");
    };
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].name(), "value");
    assert_eq!(
        columns[0].resolved_type(),
        ResolvedType::scalar(StandardScalar::Integer)
    );

    let (catalogue, _, _, _) = catalogue();
    let context = CatalogueHashContext::version_one();
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .expect("single result column is valid")],
        [ResultRow::new([RuntimeValue::Integer(42)])],
    )
    .expect("single result rows are valid");
    let result = ServerSelectResult::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x51; 16]),
            CatalogueRevisionId::from_bytes([0x52; 16]),
        ),
        function.id(),
        function.current_revision(),
        rows,
    );
    assert_eq!(
        into_raw_server_values_for_context(&catalogue, &context, function.id(), result)
            .expect("canonical single value conversion succeeds"),
        vec![RuntimeValue::Integer(42)]
    );
}

#[test]
fn row_execution_adapts_stream_result_and_preserves_item_type() {
    let function = function(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Stream(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    let adapted = row_execution_function(&function).expect("stream adapts");
    let FunctionReturn::Rows(columns) = adapted.return_type() else {
        panic!("stream execution view must be row-shaped");
    };
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].name(), "value");
    assert_eq!(
        columns[0].resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
}

#[test]
fn row_execution_leaves_rows_result_unchanged() {
    let function = function(
        FunctionDomain::Server,
        Vec::new(),
        rows_return(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    assert!(row_execution_function(&function).is_none());
}

#[test]
fn scalar_execution_rejects_wrong_projection_type_and_cardinality() {
    let standard = presenter_standard();
    let active = presenter_active(&standard);
    let function = function(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    let adapted = row_execution_function(&function).expect("single adapts");
    let plan = ServerPlan {
        scan: Scan {
            input: 0,
            object_type: PRESENTER_OBJECT_TYPE,
        },
        projections: vec![Expression {
            kind: ExpressionKind::ObjectReference { input: 0 },
            value_type: ValueType {
                resolved_type: ResolvedType::reference(PRESENTER_OBJECT_TYPE),
                nullable: false,
            },
        }],
        selection: None,
        ordering: Vec::new(),
    };
    assert_plan_rule(
        validate_plan(&active, &adapted, &plan),
        "projection type must equal its ROWS column",
    );
    assert!(matches!(
        ResultCardinality::ExactlyOne.validate(2),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::Cardinality { .. }
        ))
    ));
    assert!(matches!(
        ResultCardinality::ExactlyOne.finish(0),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::Cardinality { .. }
        ))
    ));
}

#[test]
fn lowerer_uses_identity_names_cached_nullable_joins_and_boolean_binds() {
    let (catalogue, source, reference, value) = catalogue();
    let context = CatalogueHashContext::version_one();
    let target = TypeId::from_bytes([0x20; 16]);
    let path = nullable_text_path(source, reference, target, value);
    let plan = ServerPlan {
        scan: Scan {
            input: 0,
            object_type: source,
        },
        projections: vec![path.clone(), path.clone()],
        selection: Some(Expression {
            kind: ExpressionKind::BooleanLiteral { value: true },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        }),
        ordering: vec![server_plan::Ordering {
            expression: path,
            direction: SortDirection::Descending,
            null_order: server_plan::NullOrder::Unspecified,
        }],
    };

    let columns = [
        ResultColumn::new(
            "first",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        )
        .unwrap(),
        ResultColumn::new(
            "second",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        )
        .unwrap(),
    ];
    let lowered = lower_plan(&catalogue, &context, &plan, &columns).unwrap();

    assert_eq!(lowered.binds, vec![SelectBindValue::Boolean(true)]);
    assert_eq!(lowered.sql.matches("LEFT JOIN").count(), 1);
    assert!(
        lowered
            .sql
            .contains("CASE WHEN octet_length(j0.f_21212121212121212121212121212121) <=")
    );
    assert!(lowered.sql.contains("AS c0, CASE WHEN octet_length"));
    assert!(lowered.sql.contains("AS c1, CASE WHEN"));
    assert!(lowered.sql.contains("AS g0, CASE WHEN"));
    assert!(lowered.sql.contains("AS g1"));
    assert_eq!(lowered.guards.len(), 2);
    assert!(lowered.sql.contains("WHERE $1"));
    assert!(
        lowered
            .sql
            .contains("ORDER BY j0.f_21212121212121212121212121212121 DESC NULLS FIRST")
    );
    assert!(lowered.sql.ends_with("LIMIT 10001"));
    assert!(!lowered.sql.contains("semantic_source"));
    assert!(!lowered.sql.contains("semantic_reference"));
    assert!(!lowered.sql.contains("semantic_target"));
    assert!(!lowered.sql.contains("semantic_value"));
}

#[test]
fn identity_selected_lowering_keeps_projection_bind_order_and_appends_selector() {
    let (catalogue, source, _, _) = catalogue();
    let context = CatalogueHashContext::version_one();
    let function = FunctionId::from_bytes([0x31; 16]);
    let parameter = ParameterId::from_bytes([0x33; 16]);
    let plan = IdentitySelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [Expression {
            kind: ExpressionKind::BooleanLiteral { value: true },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        }],
        IdentitySelector::new(function, parameter),
    )
    .unwrap();
    let columns = [ResultColumn::new(
        "selected",
        ResolvedType::scalar(StandardScalar::Boolean),
        false,
    )
    .unwrap()];
    let object = ObjectId::from_bytes([0x41; 16]);
    let lowered =
        lower_identity_selected_plan(&catalogue, &context, &plan, &columns, object).unwrap();

    assert_eq!(
        lowered.binds,
        vec![
            SelectBindValue::Boolean(true),
            SelectBindValue::Bytes(object.to_bytes().to_vec()),
        ]
    );
    assert_eq!(lowered.bind_types, vec![Type::BOOL, Type::BYTEA]);
    assert!(lowered.sql.contains("SELECT $1 AS c0"));
    assert!(lowered.sql.contains("WHERE i0._orna_object_id = $2"));
    assert!(lowered.sql.ends_with("LIMIT 2"));
    assert!(!lowered.sql.contains("semantic_source"));
}

#[test]
fn distinct_lowering_changes_only_the_select_policy_and_adds_no_bind() {
    let (catalogue, source, _, _) = catalogue();
    let context = CatalogueHashContext::version_one();
    let projection = Expression {
        kind: ExpressionKind::BooleanLiteral { value: true },
        value_type: ValueType {
            resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
            nullable: false,
        },
    };
    let selection = Expression {
        kind: ExpressionKind::BooleanLiteral { value: false },
        value_type: ValueType {
            resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
            nullable: false,
        },
    };
    let plan = DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [projection.clone()],
        Some(selection.clone()),
    )
    .unwrap();
    let version_one = ServerPlan {
        scan: plan.scan(),
        projections: vec![projection],
        selection: Some(selection),
        ordering: Vec::new(),
    };
    let columns = [ResultColumn::new(
        "selected",
        ResolvedType::scalar(StandardScalar::Boolean),
        false,
    )
    .unwrap()];

    let distinct = lower_distinct_plan(&catalogue, &context, &plan, &columns).unwrap();
    let preserving = lower_plan(&catalogue, &context, &version_one, &columns).unwrap();
    assert_eq!(
        distinct.sql,
        format!(
            "SELECT DISTINCT $1 AS c0\nFROM {}.{} AS i0\nWHERE $2\nLIMIT 10001",
            DATA_SCHEMA,
            relation_name(source),
        )
    );
    assert_eq!(
        distinct.sql,
        preserving.sql.replacen("SELECT ", "SELECT DISTINCT ", 1)
    );
    assert_eq!(
        distinct.binds,
        vec![
            SelectBindValue::Boolean(true),
            SelectBindValue::Boolean(false),
        ]
    );
    assert_eq!(distinct.bind_types, vec![Type::BOOL, Type::BOOL]);
}

#[test]
fn artifact_versions_decode_only_their_matching_plan_model() {
    let (_, source, _, _) = catalogue();
    let projection = Expression {
        kind: ExpressionKind::BooleanLiteral { value: true },
        value_type: ValueType {
            resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
            nullable: false,
        },
    };
    let v1 = ServerPlan {
        scan: Scan {
            input: 0,
            object_type: source,
        },
        projections: vec![projection.clone()],
        selection: None,
        ordering: Vec::new(),
    }
    .encode()
    .unwrap();
    let v2 = IdentitySelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [projection.clone()],
        IdentitySelector::new(
            FunctionId::from_bytes([0x31; 16]),
            ParameterId::from_bytes([0x33; 16]),
        ),
    )
    .unwrap()
    .encode()
    .unwrap();
    let v3 = DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [projection.clone()],
        None,
    )
    .unwrap()
    .encode()
    .unwrap();
    let v4 = UniqueTextSelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [projection],
        UniqueTextSelectBindValue::Text {
            scan_object_type: source,
            field_owner: source,
            field: FieldId::from_bytes([0x34; 16]),
            parameter_owner: FunctionId::from_bytes([0x31; 16]),
            parameter: ParameterId::from_bytes([0x33; 16]),
            resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            field_nullable: true,
            parameter_required_non_null: true,
        },
    )
    .unwrap()
    .encode()
    .unwrap();
    let function = FunctionId::from_bytes([0x31; 16]);

    assert!(matches!(
        decode_plan(function, SERVER_PLAN_FORMAT, SERVER_PLAN_VERSION, &v1),
        Ok(DecodedServerPlan::V1(_))
    ));
    assert!(matches!(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            IDENTITY_SELECTED_SERVER_PLAN_VERSION,
            &v2,
        ),
        Ok(DecodedServerPlan::V2(_))
    ));
    assert!(matches!(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            DISTINCT_SERVER_PLAN_VERSION,
            &v3,
        ),
        Ok(DecodedServerPlan::V3(_))
    ));
    assert!(matches!(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            UNIQUE_TEXT_SELECTED_SERVER_PLAN_VERSION,
            &v4,
        ),
        Ok(DecodedServerPlan::V4(_))
    ));
    assert!(matches!(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            IDENTITY_SELECTED_SERVER_PLAN_VERSION,
            &v1,
        ),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                SERVER_PLAN_VERSION
            ))
        ))
    ));
    assert!(matches!(
        decode_plan(function, SERVER_PLAN_FORMAT, SERVER_PLAN_VERSION, &v2),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                IDENTITY_SELECTED_SERVER_PLAN_VERSION
            ))
        ))
    ));
    assert!(matches!(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            DISTINCT_SERVER_PLAN_VERSION,
            &v1,
        ),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                SERVER_PLAN_VERSION
            ))
        ))
    ));
    assert!(matches!(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            DISTINCT_SERVER_PLAN_VERSION,
            &v2,
        ),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                IDENTITY_SELECTED_SERVER_PLAN_VERSION
            ))
        ))
    ));
    assert!(matches!(
        decode_plan(function, SERVER_PLAN_FORMAT, SERVER_PLAN_VERSION, &v3),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                DISTINCT_SERVER_PLAN_VERSION
            ))
        ))
    ));
    assert!(matches!(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            IDENTITY_SELECTED_SERVER_PLAN_VERSION,
            &v3,
        ),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::PlanDecode(server_plan::ServerPlanError::UnsupportedVersion(
                DISTINCT_SERVER_PLAN_VERSION
            ))
        ))
    ));
    assert!(matches!(
        decode_plan(function, "unknown", SERVER_PLAN_VERSION, &v1),
        Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact {
            function: actual,
            rule: "current SERVER artifact must use orna.server-plan",
        })) if actual == function
    ));
    assert!(matches!(
        decode_plan(function, SERVER_PLAN_FORMAT, 99, &v1),
        Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact {
            function: actual,
            rule: "current SERVER artifact must use supported orna.server-plan version 1, version 2, version 3, or version 4",
        })) if actual == function
    ));
}

#[test]
fn distinct_decode_maps_only_human_actionable_v3_failures() {
    let (_, source, reference, value) = catalogue();
    let target = TypeId::from_bytes([0x20; 16]);
    let function = FunctionId::from_bytes([0x31; 16]);
    let mut unsupported_projection = ServerPlan {
        scan: Scan {
            input: 0,
            object_type: source,
        },
        projections: vec![nullable_text_path(source, reference, target, value)],
        selection: None,
        ordering: Vec::new(),
    }
    .encode()
    .unwrap();
    unsupported_projection[8..12].copy_from_slice(&DISTINCT_SERVER_PLAN_VERSION.to_be_bytes());
    assert_distinct_rule(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            DISTINCT_SERVER_PLAN_VERSION,
            &unsupported_projection,
        ),
        DISTINCT_PROJECTION_RULE,
    );

    let projection = Expression {
        kind: ExpressionKind::BooleanLiteral { value: true },
        value_type: ValueType {
            resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
            nullable: false,
        },
    };
    let mut ordering = ServerPlan {
        scan: Scan {
            input: 0,
            object_type: source,
        },
        projections: vec![projection.clone()],
        selection: None,
        ordering: vec![Ordering {
            expression: projection,
            direction: SortDirection::Unspecified,
            null_order: server_plan::NullOrder::Unspecified,
        }],
    }
    .encode()
    .unwrap();
    ordering[8..12].copy_from_slice(&DISTINCT_SERVER_PLAN_VERSION.to_be_bytes());
    assert_distinct_rule(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            DISTINCT_SERVER_PLAN_VERSION,
            &ordering,
        ),
        "ORDER BY is not allowed",
    );

    assert!(matches!(
        decode_plan(
            function,
            SERVER_PLAN_FORMAT,
            DISTINCT_SERVER_PLAN_VERSION,
            b"not a server plan",
        ),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::PlanDecode(server_plan::ServerPlanError::InvalidMagic)
        ))
    ));
}

#[test]
fn distinct_error_display_is_human_facing_without_changing_existing_copy() {
    let function = FunctionId::from_bytes([0x31; 16]);
    assert_eq!(
        ServerSelectError::Distinct {
            rule: DISTINCT_PROJECTION_RULE,
        }
        .to_string(),
        "saved SELECT DISTINCT function cannot run: projections support only BOOLEAN, INTEGER, BIGINT, BYTES, and REF values"
    );
    assert_eq!(
        ServerSelectError::PlanInvariant { rule: "test" }.to_string(),
        "server plan invariant failed: test"
    );
    assert_eq!(
        ServerSelectError::ReferenceEvidence {
            function,
            rule: "test",
        }
        .to_string(),
        "function function:64rk2c9h64rk2c9h64rk2c9h64 has invalid definition-reference evidence: test"
    );
}

#[test]
fn distinct_signature_rejects_each_unsupported_shape_exactly() {
    let function_id = FunctionId::from_bytes([0x31; 16]);
    let valid = function(
        FunctionDomain::Server,
        Vec::new(),
        rows_return(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    assert!(validate_distinct_function_signature(&valid).is_ok());

    let wrong_domain = function(
        FunctionDomain::Client,
        Vec::new(),
        rows_return(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    assert!(matches!(
        validate_distinct_function_signature(&wrong_domain),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::FunctionDomain { function }
        )) if function == function_id
    ));

    assert_signature_rule(
        validate_distinct_function_signature(&function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                ParameterId::from_bytes([0x33; 16]),
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            )],
            rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        )),
        function_id,
        "SELECT DISTINCT SERVER functions must have zero parameters",
    );
    for return_type in [
        FunctionReturn::Rows(Vec::new()),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
    ] {
        assert_signature_rule(
            validate_distinct_function_signature(&function(
                FunctionDomain::Server,
                Vec::new(),
                return_type,
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
            )),
            function_id,
            "SELECT DISTINCT SERVER functions must return nonempty ROWS",
        );
    }
    assert_signature_rule(
        validate_distinct_function_signature(&function(
            FunctionDomain::Server,
            Vec::new(),
            rows_return(),
            FunctionSecurity::Definer,
            Some(FunctionTransaction::ReadOnly),
        )),
        function_id,
        "SELECT DISTINCT SERVER functions must use INVOKER security",
    );
    for transaction in [
        None,
        Some(FunctionTransaction::Atomic),
        Some(FunctionTransaction::Manual),
    ] {
        assert_signature_rule(
            validate_distinct_function_signature(&function(
                FunctionDomain::Server,
                Vec::new(),
                rows_return(),
                FunctionSecurity::Invoker,
                transaction,
            )),
            function_id,
            "SELECT DISTINCT SERVER functions must use READ ONLY transactions",
        );
    }
    for volatility in [FunctionVolatility::Immutable, FunctionVolatility::Volatile] {
        assert_signature_rule(
            validate_distinct_function_signature(&function_with_volatility(
                FunctionDomain::Server,
                Vec::new(),
                rows_return(),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                volatility,
            )),
            function_id,
            "SELECT DISTINCT SERVER functions must use STABLE volatility",
        );
    }
}

#[test]
fn parameter_free_versions_accept_only_an_empty_argument_slice() {
    assert!(validate_no_arguments(&[]).is_ok());
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0x33; 16]),
        RuntimeValue::Integer(7),
    )
    .unwrap();
    assert_argument_rule(
        validate_no_arguments(&[argument]),
        None,
        "this function does not accept arguments",
    );
}

#[test]
fn identity_selected_signature_rejects_each_unsupported_shape_exactly() {
    let (catalogue, source, _, _) = catalogue();
    let function_id = FunctionId::from_bytes([0x31; 16]);
    let parameter_id = ParameterId::from_bytes([0x33; 16]);
    let selector_parameter = |resolved_type, default_expression| {
        ParameterDefinition::new(
            parameter_id,
            "selected",
            0,
            resolved_type,
            default_expression,
        )
    };
    let valid_parameter = || selector_parameter(ResolvedType::reference(source), None);

    let valid = function(
        FunctionDomain::Server,
        vec![valid_parameter()],
        boolean_rows_return(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    assert!(validate_identity_selected_function_signature(&catalogue, &valid).is_ok());

    let wrong_domain = function(
        FunctionDomain::Client,
        vec![valid_parameter()],
        boolean_rows_return(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    assert!(matches!(
        validate_identity_selected_function_signature(&catalogue, &wrong_domain),
        Err(PostgresKernelError::ServerSelect(
            ServerSelectError::FunctionDomain { function }
        )) if function == function_id
    ));

    assert_signature_rule(
        validate_identity_selected_function_signature(
            &catalogue,
            &function(
                FunctionDomain::Server,
                vec![valid_parameter()],
                FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
            ),
        ),
        function_id,
        "SERVER SELECT functions must return nonempty ROWS",
    );
    assert_signature_rule(
        validate_identity_selected_function_signature(
            &catalogue,
            &function(
                FunctionDomain::Server,
                vec![valid_parameter()],
                boolean_rows_return(),
                FunctionSecurity::Definer,
                Some(FunctionTransaction::ReadOnly),
            ),
        ),
        function_id,
        "parameterised SERVER SELECT functions must use INVOKER security",
    );
    for transaction in [
        None,
        Some(FunctionTransaction::Atomic),
        Some(FunctionTransaction::Manual),
    ] {
        assert_signature_rule(
            validate_identity_selected_function_signature(
                &catalogue,
                &function(
                    FunctionDomain::Server,
                    vec![valid_parameter()],
                    boolean_rows_return(),
                    FunctionSecurity::Invoker,
                    transaction,
                ),
            ),
            function_id,
            "parameterised SERVER SELECT functions must use READ ONLY transactions",
        );
    }
    for volatility in [FunctionVolatility::Immutable, FunctionVolatility::Volatile] {
        assert_signature_rule(
            validate_identity_selected_function_signature(
                &catalogue,
                &function_with_volatility(
                    FunctionDomain::Server,
                    vec![valid_parameter()],
                    boolean_rows_return(),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                    volatility,
                ),
            ),
            function_id,
            "parameterised SERVER SELECT functions must use STABLE volatility",
        );
    }
    for parameters in [
        Vec::new(),
        vec![
            valid_parameter(),
            ParameterDefinition::new(
                ParameterId::from_bytes([0x34; 16]),
                "other",
                1,
                ResolvedType::reference(source),
                None,
            ),
        ],
    ] {
        assert_signature_rule(
            validate_identity_selected_function_signature(
                &catalogue,
                &function(
                    FunctionDomain::Server,
                    parameters,
                    boolean_rows_return(),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                ),
            ),
            function_id,
            "parameterised SERVER SELECT functions must declare exactly one parameter",
        );
    }
    assert_signature_rule(
        validate_identity_selected_function_signature(
            &catalogue,
            &function(
                FunctionDomain::Server,
                vec![selector_parameter(
                    ResolvedType::reference(source),
                    Some(orna_core::ExpressionId::from_bytes([0x35; 16])),
                )],
                boolean_rows_return(),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
            ),
        ),
        function_id,
        "the identity selector parameter cannot have a default expression",
    );
    for unsupported in [
        ResolvedType::scalar(StandardScalar::Integer),
        ResolvedType::reference(TypeId::from_bytes([0x99; 16])),
    ] {
        assert_signature_rule(
            validate_identity_selected_function_signature(
                &catalogue,
                &function(
                    FunctionDomain::Server,
                    vec![selector_parameter(unsupported, None)],
                    boolean_rows_return(),
                    FunctionSecurity::Invoker,
                    Some(FunctionTransaction::ReadOnly),
                ),
            ),
            function_id,
            "the selector parameter must use REF to an available object type",
        );
    }
}

#[test]
fn identity_selected_plan_requires_the_exact_selector_owner_parameter_and_target() {
    let (catalogue, source, _, _) = catalogue();
    let context = CatalogueHashContext::version_one();
    let other_active = TypeId::from_bytes([0x20; 16]);
    let function_id = FunctionId::from_bytes([0x31; 16]);
    let parameter_id = ParameterId::from_bytes([0x33; 16]);
    let projection = Expression {
        kind: ExpressionKind::BooleanLiteral { value: true },
        value_type: ValueType {
            resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
            nullable: false,
        },
    };
    let plan = |owner, parameter| {
        IdentitySelectedServerPlan::new(
            Scan {
                input: 0,
                object_type: source,
            },
            [projection.clone()],
            IdentitySelector::new(owner, parameter),
        )
        .unwrap()
    };
    let function_for_target = |target| {
        function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "selected",
                0,
                ResolvedType::reference(target),
                None,
            )],
            boolean_rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
        )
    };
    let valid = function_for_target(source);
    assert!(
        validate_identity_selected_plan(
            &catalogue,
            &context,
            &valid,
            &plan(function_id, parameter_id),
        )
        .is_ok()
    );

    for invalid in [
        plan(FunctionId::from_bytes([0x98; 16]), parameter_id),
        plan(function_id, ParameterId::from_bytes([0x97; 16])),
    ] {
        assert_plan_rule(
            validate_identity_selected_plan(&catalogue, &context, &valid, &invalid),
            "identity selector owner and parameter must equal the active function signature",
        );
    }
    assert_plan_rule(
        validate_identity_selected_plan(
            &catalogue,
            &context,
            &function_for_target(other_active),
            &plan(function_id, parameter_id),
        ),
        "the selector parameter must use REF to the object type selected in FROM",
    );
}

#[test]
fn version_two_value_rows_accept_compatibility_plan_scalars() {
    let (context, integer) = retained_value_context("orna.kernel.value.integer@1");
    let (catalogue, source, field) = catalogue_with_value_field(integer);
    let function_id = FunctionId::from_bytes([0x31; 16]);
    let parameter_id = ParameterId::from_bytes([0x75; 16]);
    let function = function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "selected",
            0,
            ResolvedType::reference(source),
            None,
        )],
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::value(integer),
        )]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    let plan = IdentitySelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [field_projection(source, field, StandardScalar::Integer)],
        IdentitySelector::new(function_id, parameter_id),
    )
    .expect("identity selected plan");

    assert!(validate_identity_selected_plan(&catalogue, &context, &function, &plan).is_ok());
    let columns = result_columns_for_projections(&function, plan.projections()).unwrap();
    assert_eq!(
        columns[0].resolved_type(),
        ResolvedType::scalar(StandardScalar::Integer)
    );
    let lowered = lower_identity_selected_plan(
        &catalogue,
        &context,
        &plan,
        &columns,
        ObjectId::from_bytes([0x76; 16]),
    )
    .unwrap();
    assert_eq!(lowered.bind_types, vec![Type::BYTEA]);
}

#[test]
fn version_two_value_contracts_keep_the_existing_runtime_allowlist_error() {
    let (context, decimal) = retained_value_context("orna.kernel.value.decimal@1");
    let (catalogue, source, field) = catalogue_with_value_field(decimal);
    let function_id = FunctionId::from_bytes([0x31; 16]);
    let parameter_id = ParameterId::from_bytes([0x78; 16]);
    let function = function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "selected",
            0,
            ResolvedType::reference(source),
            None,
        )],
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::value(decimal),
        )]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    let plan = IdentitySelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [field_projection(source, field, StandardScalar::Decimal)],
        IdentitySelector::new(function_id, parameter_id),
    )
    .expect("identity selected plan");

    assert_plan_rule(
        validate_identity_selected_plan(&catalogue, &context, &function, &plan),
        "projection type is outside the initial runtime result subset",
    );
}

#[test]
fn identity_selected_equality_rejection_names_the_parameterised_query() {
    let (catalogue, source, reference, value) = catalogue();
    let context = CatalogueHashContext::version_one();
    let target = TypeId::from_bytes([0x20; 16]);
    let function_id = FunctionId::from_bytes([0x31; 16]);
    let parameter_id = ParameterId::from_bytes([0x33; 16]);
    let text = nullable_text_path(source, reference, target, value);
    let plan = IdentitySelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [Expression {
            kind: ExpressionKind::Equality {
                left: Box::new(text.clone()),
                right: Box::new(text),
            },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: true,
            },
        }],
        IdentitySelector::new(function_id, parameter_id),
    )
    .unwrap();
    let function = function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "selected",
            0,
            ResolvedType::reference(source),
            None,
        )],
        boolean_rows_return(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );

    assert_plan_rule(
        validate_identity_selected_plan(&catalogue, &context, &function, &plan),
        PARAMETERISED_EQUALITY_RULE,
    );
}

#[test]
fn identity_selector_arguments_are_exact_complete_and_target_typed() {
    let (catalogue, source, _, _) = catalogue();
    let context = CatalogueHashContext::version_one();
    let function_id = FunctionId::from_bytes([0x31; 16]);
    let parameter_id = ParameterId::from_bytes([0x33; 16]);
    let function = function(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "selected",
            0,
            ResolvedType::reference(source),
            None,
        )],
        rows_return(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    let plan = IdentitySelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [Expression {
            kind: ExpressionKind::BooleanLiteral { value: true },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        }],
        IdentitySelector::new(function_id, parameter_id),
    )
    .unwrap();
    let object = ObjectId::from_bytes([0x42; 16]);
    let argument = FunctionArgument::new(
        parameter_id,
        RuntimeValue::Reference {
            target: source,
            object,
        },
    )
    .unwrap();

    assert!(validate_identity_selected_function_signature(&catalogue, &function).is_ok());
    assert_eq!(
        validate_identity_selected_arguments(
            &catalogue,
            &context,
            &function,
            &plan,
            std::slice::from_ref(&argument),
        )
        .unwrap(),
        object
    );
    assert_argument_rule(
        validate_identity_selected_arguments(&catalogue, &context, &function, &plan, &[]),
        Some(parameter_id),
        "a required argument is missing",
    );
    assert_argument_rule(
        validate_identity_selected_arguments(
            &catalogue,
            &context,
            &function,
            &plan,
            &[argument.clone(), argument],
        ),
        Some(parameter_id),
        "the same parameter was supplied twice",
    );
    let unknown_parameter = ParameterId::from_bytes([0x98; 16]);
    let unknown = FunctionArgument::new(
        unknown_parameter,
        RuntimeValue::Reference {
            target: source,
            object,
        },
    )
    .unwrap();
    assert_argument_rule(
        validate_identity_selected_arguments(&catalogue, &context, &function, &plan, &[unknown]),
        Some(unknown_parameter),
        "an argument was supplied for a parameter that this function does not declare",
    );
    let wrong_scalar = FunctionArgument::new(parameter_id, RuntimeValue::Integer(7)).unwrap();
    assert_argument_rule(
        validate_identity_selected_arguments(
            &catalogue,
            &context,
            &function,
            &plan,
            &[wrong_scalar],
        ),
        Some(parameter_id),
        "the argument type does not match the declared parameter type",
    );
    let wrong_active_target = FunctionArgument::new(
        parameter_id,
        RuntimeValue::Reference {
            target: TypeId::from_bytes([0x20; 16]),
            object,
        },
    )
    .unwrap();
    assert_argument_rule(
        validate_identity_selected_arguments(
            &catalogue,
            &context,
            &function,
            &plan,
            &[wrong_active_target],
        ),
        Some(parameter_id),
        "the argument type does not match the declared parameter type",
    );
    let wrong_inactive_target = FunctionArgument::new(
        parameter_id,
        RuntimeValue::Reference {
            target: TypeId::from_bytes([0x99; 16]),
            object,
        },
    )
    .unwrap();
    assert_argument_rule(
        validate_identity_selected_arguments(
            &catalogue,
            &context,
            &function,
            &plan,
            &[wrong_inactive_target],
        ),
        Some(parameter_id),
        "the argument uses an unsupported type or refers to an unavailable object type",
    );
}

#[test]
fn identity_selected_evidence_orders_query_projection_selector_and_parameter() {
    let (_, source, _, _) = catalogue();
    let owner = FunctionId::from_bytes([0x31; 16]);
    let parameter = ParameterId::from_bytes([0x33; 16]);
    let plan = IdentitySelectedServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [Expression {
            kind: ExpressionKind::ObjectReference { input: 0 },
            value_type: ValueType {
                resolved_type: ResolvedType::reference(source),
                nullable: false,
            },
        }],
        IdentitySelector::new(owner, parameter),
    )
    .unwrap();

    assert_eq!(
        expected_identity_selected_body_references(&plan),
        vec![
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(source),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(source),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(source),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter { owner, parameter },
            ),
        ]
    );
}

#[test]
fn distinct_evidence_orders_source_projections_then_optional_selection() {
    let (_, source, reference, _) = catalogue();
    let target = TypeId::from_bytes([0x20; 16]);
    let projection = Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: vec![FieldStep {
                owner: source,
                field: reference,
            }],
        },
        value_type: ValueType {
            resolved_type: ResolvedType::reference(target),
            nullable: true,
        },
    };
    let object_reference = || Expression {
        kind: ExpressionKind::ObjectReference { input: 0 },
        value_type: ValueType {
            resolved_type: ResolvedType::reference(source),
            nullable: false,
        },
    };
    let selection = Expression {
        kind: ExpressionKind::Equality {
            left: Box::new(object_reference()),
            right: Box::new(object_reference()),
        },
        value_type: ValueType {
            resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
            nullable: false,
        },
    };

    assert_eq!(
        expected_unordered_body_references(source, &[projection], Some(&selection)),
        vec![
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(source),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: source,
                    field: reference,
                },
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(source),
            ),
            ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(source),
            ),
        ]
    );
}

#[test]
fn distinct_evidence_mismatches_use_the_exact_human_rules() {
    assert_distinct_rule::<()>(
        Err(distinct_reference_error(ReferenceReplayMismatch::Count)),
        DISTINCT_REFERENCE_COUNT_RULE,
    );
    assert_distinct_rule::<()>(
        Err(distinct_reference_error(ReferenceReplayMismatch::Sequence)),
        DISTINCT_REFERENCE_SEQUENCE_RULE,
    );
}

#[test]
fn identity_selected_cardinality_accepts_zero_or_one_and_rejects_two() {
    assert!(ResultCardinality::BoundedMany.validate(2).is_ok());
    assert!(validate_identity_selected_cardinality(0).is_ok());
    assert!(validate_identity_selected_cardinality(1).is_ok());
    assert!(ResultCardinality::AtMostOne.validate(1).is_ok());
    let error = ResultCardinality::AtMostOne.validate(2).unwrap_err();
    assert_eq!(
        error.to_string(),
        "server SELECT failed: SERVER SELECT returned too many rows: more than one row was returned for the requested object"
    );
    assert!(matches!(
        error,
        PostgresKernelError::ServerSelect(ServerSelectError::Cardinality { .. })
    ));
}

#[test]
fn field_path_validation_rejects_a_wrong_owner_or_final_type() {
    let (catalogue, source, reference, value) = catalogue();
    let target = TypeId::from_bytes([0x20; 16]);
    let path = nullable_text_path(source, reference, target, value);
    assert_eq!(
        field_path_type(
            &catalogue,
            source,
            match &path.kind {
                ExpressionKind::FieldPath { steps, .. } => steps,
                _ => unreachable!(),
            }
        )
        .unwrap(),
        (
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true
        ),
    );
    let wrong = [FieldStep {
        owner: target,
        field: reference,
    }];
    assert!(field_path_type(&catalogue, source, &wrong).is_err());
}

#[test]
fn object_reference_emits_ordered_query_object_evidence() {
    let source = TypeId::from_bytes([0x10; 16]);
    let expression = Expression {
        kind: ExpressionKind::ObjectReference { input: 0 },
        value_type: ValueType {
            resolved_type: ResolvedType::reference(source),
            nullable: false,
        },
    };
    let mut references = Vec::new();
    add_expression_references(&mut references, source, &expression);
    assert_eq!(
        references,
        vec![ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(source),
        )]
    );
}

#[test]
fn signature_matrix_accepts_only_active_server_rows_invoker_modes() {
    for transaction in [
        None,
        Some(FunctionTransaction::Atomic),
        Some(FunctionTransaction::ReadOnly),
    ] {
        assert!(
            validate_function_signature(&function(
                FunctionDomain::Server,
                Vec::new(),
                rows_return(),
                FunctionSecurity::Invoker,
                transaction,
            ))
            .is_ok()
        );
    }
    assert!(
        validate_function_signature(&function(
            FunctionDomain::Client,
            Vec::new(),
            rows_return(),
            FunctionSecurity::Invoker,
            None,
        ))
        .is_err()
    );
    assert!(
        validate_function_signature(&function(
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            FunctionSecurity::Invoker,
            None,
        ))
        .is_err()
    );
    assert!(
        validate_function_signature(&function(
            FunctionDomain::Server,
            Vec::new(),
            rows_return(),
            FunctionSecurity::Definer,
            None,
        ))
        .is_err()
    );
    assert!(
        validate_function_signature(&function(
            FunctionDomain::Server,
            Vec::new(),
            rows_return(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Manual),
        ))
        .is_err()
    );
}

#[test]
fn operation_matrix_is_closed_for_equality_and_ordering() {
    let context = CatalogueHashContext::version_one();
    for resolved_type in [
        ResolvedType::scalar(StandardScalar::Boolean),
        ResolvedType::scalar(StandardScalar::Integer),
        ResolvedType::scalar(StandardScalar::BigInt),
        ResolvedType::scalar(StandardScalar::BinaryLargeObject),
        ResolvedType::reference(TypeId::from_bytes([0x55; 16])),
    ] {
        assert!(supports_equality_type(&context, resolved_type));
    }
    for scalar in [
        StandardScalar::Float,
        StandardScalar::CharacterLargeObject,
        StandardScalar::Decimal,
        StandardScalar::Uuid,
        StandardScalar::Date,
        StandardScalar::Time,
        StandardScalar::Timestamp,
        StandardScalar::Duration,
        StandardScalar::Void,
    ] {
        assert!(!supports_equality_type(
            &context,
            ResolvedType::scalar(scalar)
        ));
    }
    assert!(!supports_equality_type(
        &context,
        ResolvedType::named(TypeId::from_bytes([0x56; 16]))
    ));
    assert!(supports_ordering_type(
        &context,
        ResolvedType::scalar(StandardScalar::Integer)
    ));
    assert!(supports_ordering_type(
        &context,
        ResolvedType::scalar(StandardScalar::BigInt)
    ));
    assert!(!supports_ordering_type(
        &context,
        ResolvedType::scalar(StandardScalar::Boolean)
    ));
    assert!(!supports_ordering_type(
        &context,
        ResolvedType::reference(TypeId::from_bytes([0x57; 16]))
    ));
}

#[test]
fn distinct_projection_domain_is_exhaustive_and_independent() {
    let context = CatalogueHashContext::version_one();
    let mut accepted_scalars = 0usize;
    for scalar in StandardScalar::ALL {
        let expected = matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        );
        assert_eq!(
            supports_distinct_projection_type(&context, ResolvedType::scalar(scalar)),
            expected,
            "unexpected SELECT DISTINCT support for {scalar:?}",
        );
        accepted_scalars += usize::from(expected);
    }
    assert_eq!(accepted_scalars, 4);
    assert!(supports_distinct_projection_type(
        &context,
        ResolvedType::reference(TypeId::from_bytes([0x55; 16]))
    ));
    assert!(!supports_distinct_projection_type(
        &context,
        ResolvedType::named(TypeId::from_bytes([0x56; 16]))
    ));
}

#[test]
fn distinct_plan_revalidates_catalogue_shape_and_uses_its_own_equality_copy() {
    let (catalogue, source, reference, value) = catalogue();
    let context = CatalogueHashContext::version_one();
    let target = TypeId::from_bytes([0x20; 16]);
    let reference_projection = |scan| Expression {
        kind: ExpressionKind::ObjectReference { input: 0 },
        value_type: ValueType {
            resolved_type: ResolvedType::reference(scan),
            nullable: false,
        },
    };
    let plan = DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [reference_projection(source)],
        None,
    )
    .unwrap();
    let reference_rows = FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
        "value",
        0,
        ResolvedType::reference(source),
    )]);
    let reference_function = function(
        FunctionDomain::Server,
        Vec::new(),
        reference_rows,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
    );
    assert!(validate_distinct_plan(&catalogue, &context, &reference_function, &plan).is_ok());

    let inactive = TypeId::from_bytes([0x99; 16]);
    let inactive_plan = DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: inactive,
        },
        [reference_projection(inactive)],
        None,
    )
    .unwrap();
    assert_plan_rule(
        validate_distinct_plan(&catalogue, &context, &reference_function, &inactive_plan),
        "scan must use active input zero and an active object type",
    );
    assert_plan_rule(
        validate_distinct_plan(
            &catalogue,
            &context,
            &function(
                FunctionDomain::Server,
                Vec::new(),
                boolean_rows_return(),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
            ),
            &plan,
        ),
        "projection type must equal its ROWS column",
    );

    let unknown_field = DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [Expression {
            kind: ExpressionKind::FieldPath {
                input: 0,
                steps: vec![FieldStep {
                    owner: source,
                    field: FieldId::from_bytes([0x99; 16]),
                }],
            },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: false,
            },
        }],
        None,
    )
    .unwrap();
    assert_plan_rule(
        validate_distinct_plan(
            &catalogue,
            &context,
            &function(
                FunctionDomain::Server,
                Vec::new(),
                boolean_rows_return(),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
            ),
            &unknown_field,
        ),
        "field path field must exist on its active owner",
    );

    let text = nullable_text_path(source, reference, target, value);
    let unsupported_equality = DistinctServerPlan::new(
        Scan {
            input: 0,
            object_type: source,
        },
        [Expression {
            kind: ExpressionKind::Equality {
                left: Box::new(text.clone()),
                right: Box::new(text),
            },
            value_type: ValueType {
                resolved_type: ResolvedType::scalar(StandardScalar::Boolean),
                nullable: true,
            },
        }],
        None,
    )
    .unwrap();
    assert_plan_rule(
        validate_distinct_plan(
            &catalogue,
            &context,
            &function(
                FunctionDomain::Server,
                Vec::new(),
                boolean_rows_return(),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
            ),
            &unsupported_equality,
        ),
        DISTINCT_EQUALITY_RULE,
    );
}

#[test]
fn variable_payload_budget_reserves_names_and_fixed_values() {
    let context = CatalogueHashContext::version_one();
    let catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();
    let columns = [
        ResultColumn::new(
            "integer",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .unwrap(),
        ResultColumn::new(
            "left",
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
        )
        .unwrap(),
        ResultColumn::new(
            "right",
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            true,
        )
        .unwrap(),
    ];
    assert_eq!(
        variable_payload_limit(&catalogue, &context, &columns).unwrap(),
        (PAYLOAD_LIMIT - "integerleftright".len() - 4) / 2
    );
}

#[test]
fn contextualized_kernel_failures_keep_the_pinned_execution_context_and_source() {
    let context = ServerSelectContext::new(
        RevisionPair::new(
            orna_core::SourceRevisionId::from_bytes([0x61; 16]),
            CatalogueRevisionId::from_bytes([0x62; 16]),
        ),
        FunctionId::from_bytes([0x63; 16]),
        FunctionRevisionId::from_bytes([0x64; 16]),
    );
    let error = contextualize(
        context,
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.test",
            record: String::from("record"),
            rule: "test",
        },
    );
    let PostgresKernelError::ServerSelect(ServerSelectError::Execution {
        context: actual,
        source,
    }) = error
    else {
        panic!("active failures must retain context");
    };
    assert_eq!(actual, context);
    assert!(source.source().is_some());
}

#[test]
fn successful_result_reconstructs_the_shutdown_error_context() {
    let pair = RevisionPair::new(
        orna_core::SourceRevisionId::from_bytes([0x65; 16]),
        CatalogueRevisionId::from_bytes([0x66; 16]),
    );
    let result = ServerSelectResult::new(
        pair,
        FunctionId::from_bytes([0x67; 16]),
        FunctionRevisionId::from_bytes([0x68; 16]),
        ResultRows::new(
            [ResultColumn::new(
                "value",
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
            )
            .unwrap()],
            [ResultRow::new([RuntimeValue::Boolean(true)])],
        )
        .unwrap(),
    );
    assert_eq!(
        context_from_result(&result),
        ServerSelectContext::new(pair, result.function(), result.function_revision())
    );
}

#[test]
fn select_binds_are_prepared_with_exact_types() {
    assert!(Vec::<SelectBindValue>::new().is_empty());
    assert_eq!(
        [
            SelectBindValue::Boolean(true),
            SelectBindValue::Bytes(vec![0]),
            SelectBindValue::Text(String::from("selector")),
        ]
        .iter()
        .map(SelectBindValue::bind_type)
        .collect::<Vec<_>>(),
        vec![Type::BOOL, Type::BYTEA, Type::TEXT]
    );
}

#[test]
fn payload_accounting_has_stable_fixed_width_values() {
    assert_eq!(
        logical_payload_len(&RuntimeValue::Boolean(true)).unwrap(),
        1
    );
    assert_eq!(logical_payload_len(&RuntimeValue::Integer(1)).unwrap(), 4);
    assert_eq!(logical_payload_len(&RuntimeValue::BigInt(1)).unwrap(), 8);
    assert_eq!(
        logical_payload_len(&RuntimeValue::Float(RuntimeFloat::new(1.0).unwrap())).unwrap(),
        8
    );
    assert_eq!(
        logical_payload_len(&RuntimeValue::Text(String::from("abc"))).unwrap(),
        3
    );
    assert_eq!(
        logical_payload_len(&RuntimeValue::Bytes(vec![1, 2])).unwrap(),
        2
    );
    assert_eq!(
        logical_payload_len(
            &RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap()
        )
        .unwrap(),
        0
    );
}

#[test]
fn record_results_require_non_null_bytea_and_guard_the_outer_envelope() {
    let (catalogue, object, field, record) = catalogue_with_record_field();
    let context = CatalogueHashContext::version_one();
    assert!(supports_result_type(
        &catalogue,
        &context,
        ResolvedType::named(record),
        false,
    ));
    assert!(!supports_result_type(
        &catalogue,
        &context,
        ResolvedType::named(record),
        true,
    ));
    assert_eq!(
        expected_postgres_type(&catalogue, &context, ResolvedType::named(record)).unwrap(),
        Type::BYTEA,
    );

    let columns = [ResultColumn::new("status", ResolvedType::named(record), false).unwrap()];
    let expression = Expression {
        kind: ExpressionKind::FieldPath {
            input: 0,
            steps: vec![FieldStep {
                owner: object,
                field,
            }],
        },
        value_type: ValueType {
            resolved_type: ResolvedType::named(record),
            nullable: false,
        },
    };
    let logical_limit = variable_payload_limit(&catalogue, &context, &columns).unwrap();
    let lowered = lower_select_projections(
        &catalogue,
        RuntimeResultColumns {
            context: &context,
            columns: &columns,
        },
        object,
        &[expression],
    )
    .unwrap();
    assert_eq!(lowered.variable_payload_limit, logical_limit);
    let guarded_limit = logical_limit + ACTIVE_VALUE_ENVELOPE_LENGTH;
    assert!(
        lowered
            .projections
            .iter()
            .all(|projection| projection.contains(&format!("<= {guarded_limit}")))
    );
}

#[test]
fn query_limit_uses_the_stricter_row_or_cell_bound() {
    assert_eq!(effective_query_limit(1).unwrap(), ROW_LIMIT + 1);
    assert_eq!(effective_query_limit(1_024).unwrap(), 977);
    assert!(effective_query_limit(0).is_err());
}

#[test]
fn target_entry_limit_reserves_postgres_headroom() {
    assert!(validate_target_entry_count(1_000, 400, 200).is_ok());
    assert!(validate_target_entry_count(1_000, 400, 201).is_err());
    assert!(validate_target_entry_count(usize::MAX, 1, 0).is_err());
}

#[test]
fn ordering_rules_are_explicit_and_independent_of_postgres_defaults() {
    assert_eq!(ordering_sql(SortDirection::Unspecified), "ASC NULLS LAST");
    assert_eq!(ordering_sql(SortDirection::Ascending), "ASC NULLS LAST");
    assert_eq!(ordering_sql(SortDirection::Descending), "DESC NULLS FIRST");
}

#[test]
fn payload_accounting_includes_column_names_and_fails_closed() {
    let columns = [
        ResultColumn::new("one", ResolvedType::scalar(StandardScalar::Boolean), false).unwrap(),
        ResultColumn::new("two", ResolvedType::scalar(StandardScalar::Integer), false).unwrap(),
    ];
    assert_eq!(initial_payload_len(&columns).unwrap(), 6);
    assert!(add_payload(PAYLOAD_LIMIT, 1).is_err());
}

#[test]
fn raw_result_boundary_accepts_only_protocol_one_types() {
    let (catalogue, active_object, _, _) = catalogue();
    let context = CatalogueHashContext::version_one();
    for scalar in StandardScalar::ALL {
        let expected = matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        );
        assert_eq!(
            raw_result_type_is_supported(&catalogue, &context, ResolvedType::scalar(scalar)),
            expected,
            "unexpected raw support for {scalar:?}",
        );
    }
    assert!(raw_result_type_is_supported(
        &catalogue,
        &context,
        ResolvedType::reference(active_object),
    ));
    assert!(!raw_result_type_is_supported(
        &catalogue,
        &context,
        ResolvedType::reference(TypeId::from_bytes([0xfe; 16])),
    ));
    assert!(!raw_result_type_is_supported(
        &catalogue,
        &context,
        ResolvedType::named(TypeId::from_bytes([0xfd; 16])),
    ));
}

#[test]
fn raw_result_transfer_preserves_rows_and_reference_nulls() {
    let (catalogue, active_object, _, _) = catalogue();
    let context = CatalogueHashContext::version_one();
    let pair = RevisionPair::new(
        orna_core::SourceRevisionId::from_bytes([0xa1; 16]),
        CatalogueRevisionId::from_bytes([0xa2; 16]),
    );
    let function = FunctionId::from_bytes([0xa3; 16]);
    let revision = FunctionRevisionId::from_bytes([0xa4; 16]);
    let rows = ResultRows::new(
        [ResultColumn::new(
            "value",
            ResolvedType::scalar(StandardScalar::Integer),
            false,
        )
        .unwrap()],
        [
            ResultRow::new([RuntimeValue::Integer(1)]),
            ResultRow::new([RuntimeValue::Integer(2)]),
        ],
    )
    .unwrap();
    assert_eq!(
        into_raw_server_values_for_context(
            &catalogue,
            &context,
            function,
            ServerSelectResult::new(pair, function, revision, rows),
        )
        .unwrap(),
        vec![RuntimeValue::Integer(1), RuntimeValue::Integer(2)],
    );

    let reference = ResolvedType::reference(active_object);
    let rows = ResultRows::new(
        [ResultColumn::new("value", reference, true).unwrap()],
        [ResultRow::new([RuntimeValue::null(reference).unwrap()])],
    )
    .unwrap();
    assert_eq!(
        into_raw_server_values_for_context(
            &catalogue,
            &context,
            function,
            ServerSelectResult::new(pair, function, revision, rows),
        )
        .unwrap(),
        vec![RuntimeValue::null(reference).unwrap()]
    );
}

#[test]
fn raw_result_transfer_normalises_standard_value_nulls_to_protocol_one_scalars() {
    let (context, boolean) = retained_value_context("orna.kernel.value.boolean@1");
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0xc1; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let pair = RevisionPair::new(
        orna_core::SourceRevisionId::from_bytes([0xc2; 16]),
        CatalogueRevisionId::from_bytes([0xc1; 16]),
    );
    let function = FunctionId::from_bytes([0xc3; 16]);
    let revision = FunctionRevisionId::from_bytes([0xc4; 16]);
    let value_type = ResolvedType::value(boolean);
    let rows = ResultRows::new(
        [ResultColumn::new("value", value_type, true).unwrap()],
        [ResultRow::new([RuntimeValue::null(value_type).unwrap()])],
    )
    .unwrap();

    assert_eq!(
        into_raw_server_values_for_context(
            &catalogue,
            &context,
            function,
            ServerSelectResult::new(pair, function, revision, rows),
        )
        .unwrap(),
        vec![RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap()]
    );
}

#[test]
fn raw_target_classification_separates_validation_from_operations() {
    let function = FunctionId::from_bytes([0xb1; 16]);
    assert!(raw_server_target_is_unavailable(
        &ServerSelectError::RawTarget {
            function,
            rule: "test",
        }
    ));
    assert!(raw_server_target_is_unavailable(
        &ServerSelectError::Execution {
            context: ServerSelectContext::new(
                RevisionPair::new(
                    orna_core::SourceRevisionId::from_bytes([0xb2; 16]),
                    CatalogueRevisionId::from_bytes([0xb3; 16]),
                ),
                function,
                FunctionRevisionId::from_bytes([0xb4; 16]),
            ),
            source: Box::new(ServerSelectError::PayloadLimit {
                maximum: PAYLOAD_LIMIT,
            }),
        }
    ));
    assert!(!raw_server_target_is_unavailable(
        &ServerSelectError::PreparedResult { rule: "test" }
    ));
    assert!(!raw_server_target_is_unavailable(
        &ServerSelectError::ReturnedRows(ResultRowsError::NonFiniteFloat)
    ));
    assert!(!raw_server_target_is_unavailable(
        &ServerSelectError::CurrentRevision {
            function,
            revision: FunctionRevisionId::from_bytes([0xb5; 16]),
        }
    ));
}

#[test]
fn server_error_sources_remain_typed() {
    let error = ServerSelectError::ResultRows(ResultRowsError::EmptyColumns);
    assert!(error.source().is_some());
    assert!(
        ServerSelectError::PlanInvariant { rule: "test" }
            .source()
            .is_none()
    );
    assert!(
        ServerSelectError::ParameterEchoDecode(ServerParameterEchoError::InvalidMagic)
            .source()
            .is_some()
    );
    assert_eq!(
        ServerSelectError::ParameterEchoDecode(ServerParameterEchoError::Truncated).to_string(),
        "cannot decode server parameter-echo artifact: truncated orna.server-parameter-echo artifact"
    );
}

#[test]
fn standard_parameter_echo_executes_and_returns_the_bound_integer() {
    let function = echo_function(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
    let revision = echo_revision(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
    let argument = FunctionArgument::new(STD_INVOKE_ECHO_PARAMETER_ID, RuntimeValue::Integer(7))
        .expect("the bound integer argument is valid");
    assert_eq!(
        execute_standard_parameter_echo(&function, &revision, &[argument])
            .expect("the exact standard artifact must execute"),
        RuntimeValue::Integer(7)
    );
    let negative = FunctionArgument::new(STD_INVOKE_ECHO_PARAMETER_ID, RuntimeValue::Integer(-41))
        .expect("a negative bound integer argument is valid");
    assert_eq!(
        execute_standard_parameter_echo(&function, &revision, &[negative])
            .expect("a negative bound integer must echo unchanged"),
        RuntimeValue::Integer(-41)
    );
}

#[test]
fn standard_parameter_echo_dispatches_without_function_name_or_id_matching() {
    // A different function identity, revision, parameter identity, and name
    // with the same closed echo shape executes identically: the engine
    // dispatches only on artifact kind, format, and version, then validates
    // against the pinned signature.
    let other_function = FunctionId::from_bytes([0x41; 16]);
    let other_parameter = ParameterId::from_bytes([0x42; 16]);
    let function = FunctionDefinition::new(
        other_function,
        name(&["other", "echo"]),
        FunctionDomain::Server,
        vec![echo_parameter(other_parameter)],
        FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
        FunctionRevisionId::from_bytes([0x44; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let revision = echo_revision(other_function, other_parameter);
    let argument = FunctionArgument::new(other_parameter, RuntimeValue::Integer(3))
        .expect("the bound integer argument is valid");
    assert_eq!(
        execute_standard_parameter_echo(&function, &revision, &[argument])
            .expect("the same artifact shape must execute identically"),
        RuntimeValue::Integer(3)
    );
}

#[test]
fn standard_parameter_echo_rejects_wrong_kind_format_and_version() {
    let function = echo_function(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
    let parameter = STD_INVOKE_ECHO_PARAMETER_ID;
    let argument = || {
        FunctionArgument::new(parameter, RuntimeValue::Integer(5))
            .expect("the bound integer argument is valid")
    };

    // Wrong artifact kind: a CLIENT artifact with the exact echo payload.
    let revision = revision_with_artifact(
        STD_INVOKE_ECHO_FUNCTION_ID,
        artifact(
            ExecutableArtifactKind::Client,
            server_parameter_echo::FORMAT_IDENTITY,
            server_parameter_echo::FORMAT_VERSION,
            echo_payload(parameter),
        ),
    );
    assert_echo_artifact_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "current revision must contain a SERVER artifact",
    );

    // Wrong format: a different SERVER artifact format with the exact payload.
    let revision = revision_with_artifact(
        STD_INVOKE_ECHO_FUNCTION_ID,
        artifact(
            ExecutableArtifactKind::Server,
            "orna.server-plan",
            server_parameter_echo::FORMAT_VERSION,
            echo_payload(parameter),
        ),
    );
    assert_echo_artifact_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "current SERVER artifact must use orna.server-parameter-echo",
    );

    // Wrong version.
    let revision = revision_with_artifact(
        STD_INVOKE_ECHO_FUNCTION_ID,
        artifact(
            ExecutableArtifactKind::Server,
            server_parameter_echo::FORMAT_IDENTITY,
            2,
            echo_payload(parameter),
        ),
    );
    assert_echo_artifact_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "current SERVER artifact must use orna.server-parameter-echo version 1",
    );

    // Wrong revision language version.
    let revision = FunctionRevisionRecord::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        1,
        SourceOrigin::new(SourceUnitId::from_bytes([0x91; 16]), 0, 1)
            .expect("a test source origin is valid"),
        Sha256Digest::from_bytes([0x42; 32]),
        Sha256Digest::from_bytes([0x43; 32]),
        "orna.language/2",
        echo_artifact(parameter),
    )
    .expect("the test revision is valid");
    assert_echo_artifact_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "current SERVER revision must use the parameter-echo language version",
    );
}

#[test]
fn standard_parameter_echo_rejects_each_artifact_payload_deviation() {
    let function = echo_function(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
    let parameter = STD_INVOKE_ECHO_PARAMETER_ID;
    let argument = || {
        FunctionArgument::new(parameter, RuntimeValue::Integer(5))
            .expect("the bound integer argument is valid")
    };
    let canonical = echo_payload(parameter);

    // Wrong magic.
    let mut bytes = canonical.clone();
    bytes[0] = b'X';
    let revision = revision_with_artifact(
        STD_INVOKE_ECHO_FUNCTION_ID,
        artifact(
            ExecutableArtifactKind::Server,
            server_parameter_echo::FORMAT_IDENTITY,
            server_parameter_echo::FORMAT_VERSION,
            bytes,
        ),
    );
    assert_echo_decode_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        ServerParameterEchoError::InvalidMagic,
    );

    // Wrong parameter identity: the artifact pins a parameter the pinned
    // function does not declare.
    let other_parameter = ParameterId::from_bytes([0x45; 16]);
    let revision = echo_revision(STD_INVOKE_ECHO_FUNCTION_ID, other_parameter);
    assert_echo_decode_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        ServerParameterEchoError::UnexpectedParameter {
            actual: other_parameter,
            expected: parameter,
        },
    );

    // Wrong type identity: the artifact pins a non-INTEGER value type.
    let mut bytes = canonical.clone();
    bytes[43] = 0x03;
    let other_type = TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);
    let revision = revision_with_artifact(
        STD_INVOKE_ECHO_FUNCTION_ID,
        artifact(
            ExecutableArtifactKind::Server,
            server_parameter_echo::FORMAT_IDENTITY,
            server_parameter_echo::FORMAT_VERSION,
            bytes,
        ),
    );
    assert_echo_decode_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        ServerParameterEchoError::UnexpectedType {
            actual: other_type,
            expected: orna_standard::INTEGER_TYPE_ID,
        },
    );

    // Truncated payload.
    let revision = revision_with_artifact(
        STD_INVOKE_ECHO_FUNCTION_ID,
        artifact(
            ExecutableArtifactKind::Server,
            server_parameter_echo::FORMAT_IDENTITY,
            server_parameter_echo::FORMAT_VERSION,
            canonical[..43].to_vec(),
        ),
    );
    assert_echo_decode_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        ServerParameterEchoError::Truncated,
    );

    // Excess bytes after the canonical payload.
    let mut excess = canonical;
    excess.push(0);
    let revision = revision_with_artifact(
        STD_INVOKE_ECHO_FUNCTION_ID,
        artifact(
            ExecutableArtifactKind::Server,
            server_parameter_echo::FORMAT_IDENTITY,
            server_parameter_echo::FORMAT_VERSION,
            excess,
        ),
    );
    assert_echo_decode_rule(
        execute_standard_parameter_echo(&function, &revision, &[argument()]),
        ServerParameterEchoError::TrailingBytes,
    );
}

#[test]
fn standard_parameter_echo_signature_rejects_each_shape_deviation() {
    let parameter = STD_INVOKE_ECHO_PARAMETER_ID;
    let valid = || {
        FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![echo_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )
    };
    let revision = || echo_revision(STD_INVOKE_ECHO_FUNCTION_ID, parameter);
    let argument = || {
        FunctionArgument::new(parameter, RuntimeValue::Integer(5))
            .expect("the bound integer argument is valid")
    };
    let run = |function: &FunctionDefinition| {
        execute_standard_parameter_echo(function, &revision(), &[argument()])
    };

    // Wrong domain.
    let client = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Client,
        vec![echo_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_echo_domain_rule(run(&client));

    // Parameter count and default deviations.
    let none = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&none),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
    );

    let extra = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        vec![
            echo_parameter(parameter),
            echo_parameter(ParameterId::from_bytes([0x47; 16])),
        ],
        FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&extra),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
    );

    let defaulted = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter,
            "p_value",
            0,
            ResolvedType::value(orna_standard::INTEGER_TYPE_ID),
            Some(ExpressionId::from_bytes([0x48; 16])),
        )],
        FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&defaulted),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "standard parameter echo functions must declare exactly one required non-null INTEGER parameter",
    );

    // Result shape deviations.
    let rows = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        vec![echo_parameter(parameter)],
        rows_return(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&rows),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "standard parameter echo functions must return a single INTEGER value",
    );

    let boolean = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter,
            "p_value",
            0,
            ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&boolean),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "standard parameter echo functions must declare one INTEGER parameter and one INTEGER result",
    );

    let mismatched = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        vec![echo_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&mismatched),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "standard parameter echo functions must declare one INTEGER parameter and one INTEGER result",
    );

    // Security, transaction, and volatility deviations.
    let owner = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        vec![echo_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Definer,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_signature_rule(
        run(&owner),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "standard parameter echo functions must use INVOKER security",
    );

    for transaction in [None, Some(FunctionTransaction::Atomic)] {
        let wrong_transaction = FunctionDefinition::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            name(&["std", "invoke", "echo"]),
            FunctionDomain::Server,
            vec![echo_parameter(parameter)],
            FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            FunctionSecurity::Invoker,
            transaction,
            FunctionVolatility::Stable,
        );
        assert_signature_rule(
            run(&wrong_transaction),
            STD_INVOKE_ECHO_FUNCTION_ID,
            "standard parameter echo functions must use READ ONLY transactions",
        );
    }

    let volatile = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        name(&["std", "invoke", "echo"]),
        FunctionDomain::Server,
        vec![echo_parameter(parameter)],
        FunctionReturn::Single(ResolvedType::value(orna_standard::INTEGER_TYPE_ID)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Immutable,
    );
    assert_signature_rule(
        run(&volatile),
        STD_INVOKE_ECHO_FUNCTION_ID,
        "standard parameter echo functions must use STABLE volatility",
    );

    // The exact pinned shape still executes after every rejection.
    assert_eq!(
        run(&valid()).expect("the pinned shape must execute"),
        RuntimeValue::Integer(5)
    );
}

#[test]
fn standard_parameter_echo_arguments_are_exact_complete_and_typed() {
    let function = echo_function(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
    let revision = echo_revision(STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_PARAMETER_ID);
    let parameter = STD_INVOKE_ECHO_PARAMETER_ID;

    // Missing argument.
    assert_argument_rule(
        execute_standard_parameter_echo(&function, &revision, &[]),
        None,
        "standard parameter echo calls require exactly one argument",
    );

    // Extra argument.
    let first = FunctionArgument::new(parameter, RuntimeValue::Integer(1))
        .expect("the bound integer argument is valid");
    let second = FunctionArgument::new(parameter, RuntimeValue::Integer(2))
        .expect("the bound integer argument is valid");
    assert_argument_rule(
        execute_standard_parameter_echo(&function, &revision, &[first, second]),
        None,
        "standard parameter echo calls require exactly one argument",
    );

    // Argument bound to a different parameter identity.
    let other = ParameterId::from_bytes([0x46; 16]);
    let wrong = FunctionArgument::new(other, RuntimeValue::Integer(1))
        .expect("the bound integer argument is valid");
    assert_argument_rule(
        execute_standard_parameter_echo(&function, &revision, &[wrong]),
        Some(other),
        "standard parameter echo arguments must bind the pinned parameter identity",
    );

    // Non-INTEGER runtime value.
    let boolean = FunctionArgument::new(parameter, RuntimeValue::Boolean(true))
        .expect("a Boolean argument binds");
    assert_argument_rule(
        execute_standard_parameter_echo(&function, &revision, &[boolean]),
        Some(parameter),
        "standard parameter echo arguments must be one non-null INTEGER value",
    );

    // A typed null cannot cross the bound-argument boundary, so the engine
    // can never receive one: FunctionArgument::new rejects it.
    let null = RuntimeValue::null(ResolvedType::value(orna_standard::INTEGER_TYPE_ID))
        .expect("a typed INTEGER null is valid");
    assert!(matches!(
        FunctionArgument::new(parameter, null),
        Err(orna_core::value::FunctionArgumentError::NullValue {
            parameter: actual,
            ..
        }) if actual == parameter
    ));
}

#[test]
fn raw_server_execution_never_reaches_the_parameter_echo_engine() {
    // A direct raw request for a standard target is denied at raw dispatch
    // because the target is not in the active application catalogue. Even
    // if an echo-formatted artifact sat in an active application revision,
    // the raw SERVER executor's format gate rejects it before any plan
    // decoding: decode_plan accepts only orna.server-plan formats, so the
    // raw path can never reach execute_standard_parameter_echo.
    let parameter = STD_INVOKE_ECHO_PARAMETER_ID;
    let payload = echo_payload(parameter);
    let Err(PostgresKernelError::ServerSelect(ServerSelectError::Artifact { function, rule })) =
        decode_plan(
            STD_INVOKE_ECHO_FUNCTION_ID,
            server_parameter_echo::FORMAT_IDENTITY,
            server_parameter_echo::FORMAT_VERSION,
            &payload,
        )
    else {
        panic!("raw SERVER plan decoding must reject the parameter-echo format");
    };
    assert_eq!(function, STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(rule, "current SERVER artifact must use orna.server-plan");
}
