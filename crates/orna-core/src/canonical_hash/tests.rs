use super::*;
use crate::{
    CatalogueRevisionId,
    catalogue::{
        EnumTypeDefinition, FieldDefinition, FunctionDefinition, FunctionReturn,
        FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        PreludeTypeName, RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
        TypeBinding, ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    },
    revision::{CatalogueHashContext, SourceOrigin, StandardExecutable},
    types::{ResolvedType, StandardScalar, TypeDescriptor},
};

const fn id<const BYTE: u8>() -> [u8; 16] {
    [BYTE; 16]
}

fn digest_bytes(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

fn source_unit(id: SourceUnitId, ordinal: u32, path: &str, content: &str) -> StoredSourceUnit {
    StoredSourceUnit::new(
        id,
        ordinal,
        path,
        content,
        source_unit_content_digest(content).unwrap(),
    )
    .unwrap()
}

fn expression() -> ExpressionArtifact {
    let payload = b"expression-v1".to_vec();
    ExpressionArtifact::new(
        ExpressionId::from_bytes(id::<8>()),
        "orna.constant-expression",
        1,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap()
}

fn artifact() -> ExecutableArtifact {
    let payload = b"server-plan-v1".to_vec();
    ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap()
}

fn standard_boolean_id() -> TypeId {
    TypeId::from_bytes(id::<21>())
}

fn standard_snapshot(
    reverse: bool,
    retained_digest: Option<Sha256Digest>,
) -> StandardLibrarySnapshot {
    let unit = source_unit(
        SourceUnitId::from_bytes(id::<22>()),
        0,
        "std/types.orna",
        "CREATE SCHEMA std; CREATE SCHEMA std.types; CREATE TYPE boolean;",
    );
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes(id::<23>()),
        SourceRevisionId::from_bytes(id::<24>()),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes(id::<23>()), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();

    let mut schemas = vec![
        SchemaDefinition::new(
            SchemaId::from_bytes(id::<25>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        ),
        SchemaDefinition::new(
            SchemaId::from_bytes(id::<26>()),
            QualifiedSemanticName::new(["std", "types"]).unwrap(),
        ),
    ];
    let boolean = ValueTypeDefinition::primitive(
        standard_boolean_id(),
        QualifiedSemanticName::new(["std", "types", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let mut bindings = vec![
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
            standard_boolean_id(),
        )
        .unwrap(),
        TypeBinding::prelude(
            PreludeTypeName::new(["BOOLEAN"]).unwrap(),
            standard_boolean_id(),
        )
        .unwrap(),
    ];
    let qualified_binding = bindings[0].id();
    let prelude_binding = bindings[1].id();
    if reverse {
        schemas.reverse();
        bindings.reverse();
    }
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<27>()),
        schemas,
        vec![],
        vec![boolean],
        bindings.clone(),
    )
    .unwrap();
    let source_unit = SourceUnitId::from_bytes(id::<22>());
    let mut origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<25>())),
            SourceOrigin::new(source_unit, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<26>())),
            SourceOrigin::new(source_unit, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(standard_boolean_id()),
            SourceOrigin::new(source_unit, 2, 3).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(qualified_binding),
            SourceOrigin::new(source_unit, 3, 4).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(prelude_binding),
            SourceOrigin::new(source_unit, 4, 5).unwrap(),
        ),
    ];
    if reverse {
        origins.reverse();
    }
    let provisional = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes(id::<28>()),
        StandardLibraryDigestVersion::Version1,
        source.clone(),
        "orna.language/1",
        catalogue.clone(),
        origins.clone(),
        digest_bytes(b"provisional"),
    )
    .unwrap();
    let exact = calculate_standard_library_digest(&provisional).unwrap();
    StandardLibrarySnapshot::new(
        provisional.revision(),
        provisional.digest_version(),
        source,
        provisional.language_version(),
        catalogue,
        origins,
        retained_digest.unwrap_or(exact),
    )
    .unwrap()
}

fn verified_standard_snapshot(reverse: bool) -> VerifiedStandardLibrarySnapshot {
    verify_standard_library_snapshot(standard_snapshot(reverse, None)).unwrap()
}

fn standard_v2_snapshot(retained_digest: Option<Sha256Digest>) -> StandardLibrarySnapshot {
    let types = source_unit(
        SourceUnitId::from_bytes(id::<90>()),
        0,
        "std/types.orna",
        "CREATE SCHEMA std;\n",
    );
    let invoke_source = "CREATE SERVER FUNCTION std.invoke.echo() RETURNS BOOLEAN SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT TRUE;\n";
    let invoke = source_unit(
        SourceUnitId::from_bytes(id::<91>()),
        1,
        "std/invoke.orna",
        invoke_source,
    );
    let bundle_hash = source_bundle_digest(&[types.clone(), invoke.clone()]).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes(id::<92>()),
        SourceRevisionId::from_bytes(id::<93>()),
        Some(SourceRevisionId::from_bytes(id::<94>())),
        vec![types, invoke],
        bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes(id::<92>()),
            Some(SourceRevisionId::from_bytes(id::<94>())),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let function = FunctionDefinition::new(
        FunctionId::from_bytes(id::<95>()),
        QualifiedSemanticName::new(["std", "invoke", "echo"]).unwrap(),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionRevisionId::from_bytes(id::<96>()),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<97>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<98>()),
            QualifiedSemanticName::new(["std", "invoke"]).unwrap(),
        )],
        vec![],
        vec![function.clone()],
    )
    .unwrap();
    let declaration = SourceOrigin::new(
        SourceUnitId::from_bytes(id::<91>()),
        0,
        u32::try_from(invoke_source.len()).unwrap(),
    )
    .unwrap();
    let artifact = artifact();
    let semantic = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &function,
        "orna.language/1",
        &artifact,
        &[],
        &[],
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function.id(),
        function.current_revision(),
        1,
        declaration,
        digest_bytes(invoke_source.as_bytes()),
        semantic,
        "orna.language/1",
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<98>())),
            SourceOrigin::new(SourceUnitId::from_bytes(id::<90>()), 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(DefinitionIdentity::Function(function.id()), declaration),
    ];
    let provisional = StandardLibrarySnapshot::new_with_executables(
        StandardLibraryRevisionId::from_bytes(id::<99>()),
        StandardLibraryDigestVersion::Version2,
        source.clone(),
        "orna.language/1",
        catalogue.clone(),
        vec![StandardExecutable::new(function.id(), revision, vec![]).unwrap()],
        origins.clone(),
        digest_bytes(b"provisional-v2"),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&provisional).unwrap();
    StandardLibrarySnapshot::new_with_executables(
        provisional.revision(),
        provisional.digest_version(),
        source,
        provisional.language_version(),
        catalogue,
        provisional.executables().to_vec(),
        origins,
        retained_digest.unwrap_or(digest),
    )
    .unwrap()
}

fn standard_v2_function_with_revision(
    base: &FunctionDefinition,
    id: FunctionId,
    revision: FunctionRevisionId,
) -> FunctionDefinition {
    FunctionDefinition::new(
        id,
        base.name().clone(),
        base.domain(),
        base.parameters().to_vec(),
        base.return_type().clone(),
        revision,
        base.security(),
        base.transaction(),
        base.volatility(),
    )
}

fn standard_v2_revision(
    function: &FunctionDefinition,
    id: FunctionRevisionId,
    base: &FunctionRevisionRecord,
    artifact: ExecutableArtifact,
    references: &[DefinitionReference],
) -> FunctionRevisionRecord {
    let semantic = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        base.language_version(),
        &artifact,
        &[],
        references,
    )
    .unwrap();
    FunctionRevisionRecord::new(
        function.id(),
        id,
        base.revision_number(),
        base.declaration_origin(),
        base.declaration_content_hash(),
        semantic,
        base.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2)
}

fn rebuilt_standard_v2_snapshot(
    base: &StandardLibrarySnapshot,
    source: StoredSourceRevision,
    catalogue: CatalogueSnapshot,
    executable: StandardExecutable,
    origins: Vec<DefinitionOrigin>,
) -> StandardLibrarySnapshot {
    StandardLibrarySnapshot::new_with_executables(
        base.revision(),
        StandardLibraryDigestVersion::Version2,
        source,
        base.language_version(),
        catalogue,
        vec![executable],
        origins,
        base.digest(),
    )
    .unwrap()
}

fn catalogue() -> CatalogueSnapshot {
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes(id::<1>()),
        QualifiedSemanticName::new(["crm"]).unwrap(),
    );
    let object_type = ObjectTypeDefinition::new(
        TypeId::from_bytes(id::<2>()),
        QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
        vec![FieldDefinition::new(
            FieldId::from_bytes(id::<3>()),
            "active",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
            true,
            Some(ExpressionId::from_bytes(id::<8>())),
            None,
        )],
    );
    let function = FunctionDefinition::new(
        FunctionId::from_bytes(id::<4>()),
        QualifiedSemanticName::new(["crm", "lookup"]).unwrap(),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            ParameterId::from_bytes(id::<5>()),
            "enabled",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            Some(ExpressionId::from_bytes(id::<8>())),
        )],
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "found",
            0,
            ResolvedType::reference(TypeId::from_bytes(id::<2>())),
        )]),
        FunctionRevisionId::from_bytes(id::<6>()),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![schema],
        vec![object_type],
        vec![function],
    )
    .unwrap()
}

fn record_value_catalogue(records: Vec<RecordValueTypeDefinition>) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<160>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<161>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![],
        records,
        vec![],
    )
    .unwrap()
}

fn record_value_type(
    record_byte: u8,
    name: &str,
    fields: Vec<(u8, u32, TypeId)>,
) -> RecordValueTypeDefinition {
    let fields = fields
        .into_iter()
        .map(|(field_byte, ordinal, target)| {
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([field_byte; 16]),
                format!("edge_{ordinal}"),
                ordinal,
                TypeDescriptor::named(target),
            )
            .unwrap()
        })
        .collect();
    RecordValueTypeDefinition::new(
        TypeId::from_bytes([record_byte; 16]),
        QualifiedSemanticName::new(["crm", name]).unwrap(),
        fields,
    )
}

fn record_value_origins_for(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let source = SourceUnitId::from_bytes(id::<162>());
    let mut identities = Vec::new();
    for schema in catalogue.schemas() {
        identities.push(DefinitionIdentity::Schema(schema.id()));
    }
    for value_type in catalogue.value_types() {
        identities.push(DefinitionIdentity::ValueType(value_type.id()));
    }
    for enum_type in catalogue.enum_types() {
        identities.push(DefinitionIdentity::ValueType(enum_type.id()));
    }
    let mut records = catalogue.record_value_types().to_vec();
    records.sort_by_key(|record| record.id().to_bytes());
    for record in records {
        identities.push(DefinitionIdentity::ValueType(record.id()));
        let mut fields = record.fields().to_vec();
        fields.sort_by_key(|field| field.ordinal());
        for field in fields {
            identities.push(DefinitionIdentity::Field {
                owner: record.id(),
                field: field.id(),
            });
        }
    }
    identities
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            DefinitionOrigin::new(
                identity,
                SourceOrigin::new(source, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect()
}

#[test]
fn version_two_record_value_field_tags_distinguish_application_record_from_primitive() {
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let standard = context.standard().unwrap().catalogue();
    let boolean_id = standard_boolean_id();
    let inner_id = TypeId::from_bytes(id::<202>());
    let encode = |records: Vec<RecordValueTypeDefinition>| {
        let catalogue = record_value_catalogue(records);
        let mut encoder = Encoder::new(b"probe");
        encode_record_value_types(&mut encoder, &catalogue, standard).unwrap();
        encoder.bytes
    };
    let application_bytes = encode(vec![
        record_value_type(200, "outer", vec![(210, 0, inner_id)]),
        record_value_type(202, "inner", vec![(212, 0, boolean_id)]),
    ]);
    let primitive_bytes = encode(vec![
        record_value_type(200, "outer", vec![(210, 0, boolean_id)]),
        record_value_type(202, "inner", vec![(212, 0, boolean_id)]),
    ]);

    let first_difference = application_bytes
        .iter()
        .zip(&primitive_bytes)
        .position(|(left, right)| left != right)
        .expect("the field tag must differ");
    assert_eq!(application_bytes[first_difference], 2);
    assert_eq!(primitive_bytes[first_difference], 4);
    assert_eq!(
        &application_bytes[first_difference + 1..first_difference + 17],
        &inner_id.to_bytes(),
    );
    assert_eq!(
        &primitive_bytes[first_difference + 1..first_difference + 17],
        &boolean_id.to_bytes(),
    );
    for bytes in [&application_bytes, &primitive_bytes] {
        assert!(
            bytes
                .windows(17)
                .any(|window| window[0] == 4 && window[1..17] == boolean_id.to_bytes()),
            "the boolean primitive field must encode with tag 4"
        );
    }
    assert!(
        application_bytes
            .windows(17)
            .any(|window| { window[0] == 2 && window[1..17] == inner_id.to_bytes() }),
        "the application record field must encode with tag 2"
    );
}

#[test]
fn version_two_nested_record_value_digest_is_order_independent() {
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let build = |reverse: bool| {
        let outer = record_value_type(
            200,
            "outer",
            vec![(210, 0, TypeId::from_bytes(id::<201>()))],
        );
        let inner = record_value_type(201, "inner", vec![(211, 0, standard_boolean_id())]);
        let mut records = vec![outer, inner];
        if reverse {
            records.reverse();
        }
        let catalogue = record_value_catalogue(records);
        let origins = record_value_origins_for(&catalogue);
        let digest =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        (digest, hex(digest))
    };
    let (forward, forward_hex) = build(false);
    let (reversed, _) = build(true);
    assert_eq!(forward, reversed);
    assert_eq!(
        forward_hex, "83caded9eb4cdea395f021f983dbf03f4135cc394dbd3102d17dd8625066061c",
        "the nested record digest must stay pinned"
    );
}

#[test]
fn version_two_record_value_field_errors_are_exact_and_precede_any_digest() {
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let digest_error = |records: Vec<RecordValueTypeDefinition>| {
        let catalogue = record_value_catalogue(records);
        let origins = record_value_origins_for(&catalogue);
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap_err()
    };

    assert_eq!(
        digest_error(vec![
            record_value_type(
                200,
                "record_a",
                vec![(210, 0, TypeId::from_bytes(id::<201>()))],
            ),
            record_value_type(
                201,
                "record_b",
                vec![(211, 0, TypeId::from_bytes(id::<200>()))],
            ),
        ]),
        CanonicalHashError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<201>()),
            field: FieldId::from_bytes(id::<211>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );

    let too_deep = (0..34)
        .map(|index| {
            let next_byte = if index == 33 {
                21
            } else {
                200 + index as u8 + 1
            };
            record_value_type(
                200 + index as u8,
                &format!("chain_{index}"),
                vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        digest_error(too_deep),
        CanonicalHashError::RecordValueNestingTooDeep {
            record_value_type: TypeId::from_bytes(id::<232>()),
            field: FieldId::from_bytes(id::<242>()),
            nested_record_value_type: TypeId::from_bytes(id::<233>()),
            maximum: 32,
            actual: 33,
        }
    );

    assert_eq!(
        digest_error(vec![record_value_type(
            21,
            "collision",
            vec![(210, 0, TypeId::from_bytes(id::<21>()))],
        )]),
        CanonicalHashError::AmbiguousRecordValueFieldType {
            record_value_type: TypeId::from_bytes(id::<21>()),
            field: FieldId::from_bytes(id::<210>()),
            type_id: TypeId::from_bytes(id::<21>()),
        }
    );
}

#[test]
fn version_two_record_value_inner_change_keeps_outer_field_bytes_and_changes_digest() {
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let standard = context.standard().unwrap().catalogue();
    let enum_type = TypeId::from_bytes(id::<47>());
    let build = |inner_target: TypeId, with_enum: bool| {
        let records = vec![
            record_value_type(
                200,
                "outer",
                vec![(210, 0, TypeId::from_bytes(id::<201>()))],
            ),
            record_value_type(201, "inner", vec![(211, 0, inner_target)]),
        ];
        let mut enums = Vec::new();
        if with_enum {
            enums.push(EnumTypeDefinition::new(
                enum_type,
                QualifiedSemanticName::new(["crm", "phase"]).unwrap(),
                ["new"],
            ));
        }
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes(id::<160>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<161>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            enums,
            records,
            vec![],
        )
        .unwrap();
        let mut encoder = Encoder::new(b"probe");
        encode_record_value_types(&mut encoder, &catalogue, standard).unwrap();
        let record_bytes = encoder.bytes;
        let origins = record_value_origins_for(&catalogue);
        let digest =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        (record_bytes, digest)
    };

    let (boolean_bytes, boolean_digest) = build(standard_boolean_id(), false);
    let (enum_bytes, enum_digest) = build(enum_type, true);
    let first_difference = boolean_bytes
        .iter()
        .zip(&enum_bytes)
        .position(|(left, right)| left != right)
        .expect("the inner field target must differ");
    assert_eq!(
        &boolean_bytes[..first_difference],
        &enum_bytes[..first_difference],
        "the outer field encoding must stay nominal"
    );
    assert!(
        first_difference > 40,
        "the outer field target bytes must be unchanged"
    );
    for bytes in [&boolean_bytes, &enum_bytes] {
        assert!(
            bytes
                .windows(17)
                .any(|window| window[0] == 2 && window[1..17] == [201; 16]),
            "the outer field must keep its application record target"
        );
    }
    assert_ne!(
        boolean_digest, enum_digest,
        "an inner definition change must change the whole digest"
    );
}

fn catalogue_with_record_value_type() -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<40>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<41>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![],
        vec![RecordValueTypeDefinition::new(
            TypeId::from_bytes(id::<42>()),
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<43>()),
                    "active",
                    0,
                    TypeDescriptor::named(standard_boolean_id()),
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap()
}

fn catalogue_with_record_value_slot() -> CatalogueSnapshot {
    let record_value_type = TypeId::from_bytes(id::<42>());
    CatalogueSnapshot::new_with_functions_and_record_value_types(
        CatalogueRevisionId::from_bytes(id::<40>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<41>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<45>()),
            QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
            vec![FieldDefinition::new(
                FieldId::from_bytes(id::<46>()),
                "status",
                0,
                ResolvedType::named(record_value_type),
                false,
                false,
                None,
                None,
            )],
        )],
        vec![],
        vec![],
        vec![RecordValueTypeDefinition::new(
            record_value_type,
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<43>()),
                    "active",
                    0,
                    TypeDescriptor::named(standard_boolean_id()),
                )
                .unwrap(),
            ],
        )],
        vec![],
        vec![FunctionDefinition::new(
            FunctionId::from_bytes(id::<47>()),
            QualifiedSemanticName::new(["crm", "read_status"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "status",
                0,
                ResolvedType::named(record_value_type),
            )]),
            FunctionRevisionId::from_bytes(id::<48>()),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap()
}

fn record_value_origins() -> Vec<DefinitionOrigin> {
    let source = SourceUnitId::from_bytes(id::<44>());
    vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<41>())),
            SourceOrigin::new(source, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<42>())),
            SourceOrigin::new(source, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<42>()),
                field: FieldId::from_bytes(id::<43>()),
            },
            SourceOrigin::new(source, 2, 3).unwrap(),
        ),
    ]
}

fn catalogue_with_ordered_record_values(
    reverse: bool,
) -> (CatalogueSnapshot, Vec<DefinitionOrigin>) {
    let enum_type = TypeId::from_bytes(id::<47>());
    let first_record = TypeId::from_bytes(id::<42>());
    let second_record = TypeId::from_bytes(id::<48>());
    let binding = TypeBinding::qualified(
        QualifiedSemanticName::new(["crm", "status_alias"]).unwrap(),
        first_record,
    )
    .unwrap();
    let binding_id = binding.id();
    let mut records = vec![
        RecordValueTypeDefinition::new(
            first_record,
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<43>()),
                    "active",
                    0,
                    TypeDescriptor::named(standard_boolean_id()),
                )
                .unwrap(),
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<44>()),
                    "phase",
                    1,
                    TypeDescriptor::named(enum_type),
                )
                .unwrap(),
            ],
        ),
        RecordValueTypeDefinition::new(
            second_record,
            QualifiedSemanticName::new(["crm", "marker"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<49>()),
                    "value",
                    0,
                    TypeDescriptor::named(standard_boolean_id()),
                )
                .unwrap(),
            ],
        ),
    ];
    if reverse {
        records.reverse();
    }
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<40>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<41>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            enum_type,
            QualifiedSemanticName::new(["crm", "phase"]).unwrap(),
            ["new", "done"],
        )],
        records,
        vec![binding],
    )
    .unwrap();
    let source = SourceUnitId::from_bytes(id::<50>());
    let identities = [
        DefinitionIdentity::Schema(SchemaId::from_bytes(id::<41>())),
        DefinitionIdentity::ValueType(enum_type),
        DefinitionIdentity::ValueType(first_record),
        DefinitionIdentity::Field {
            owner: first_record,
            field: FieldId::from_bytes(id::<43>()),
        },
        DefinitionIdentity::Field {
            owner: first_record,
            field: FieldId::from_bytes(id::<44>()),
        },
        DefinitionIdentity::ValueType(second_record),
        DefinitionIdentity::Field {
            owner: second_record,
            field: FieldId::from_bytes(id::<49>()),
        },
        DefinitionIdentity::TypeBinding(binding_id),
    ];
    let origins = identities
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            DefinitionOrigin::new(
                identity,
                SourceOrigin::new(source, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect();
    (catalogue, origins)
}

fn catalogue_with_resolved_slot_types(
    field_type: ResolvedType,
    parameter_type: ResolvedType,
    return_type: FunctionReturn,
    domain: FunctionDomain,
) -> CatalogueSnapshot {
    let base = catalogue();
    let object = &base.object_types()[0];
    let field = &object.fields()[0];
    let object_type = ObjectTypeDefinition::new(
        object.id(),
        object.name().clone(),
        vec![FieldDefinition::new(
            field.id(),
            field.name(),
            field.ordinal(),
            field_type,
            field.nullable(),
            field.unique(),
            field.default_expression(),
            field.on_delete(),
        )],
    );
    let prior_function = &base.functions()[0];
    let function = FunctionDefinition::new(
        prior_function.id(),
        prior_function.name().clone(),
        domain,
        vec![ParameterDefinition::new(
            ParameterId::from_bytes(id::<5>()),
            "enabled",
            0,
            parameter_type,
            Some(ExpressionId::from_bytes(id::<8>())),
        )],
        return_type,
        prior_function.current_revision(),
        FunctionSecurity::Invoker,
        (domain == FunctionDomain::Server).then_some(FunctionTransaction::ReadOnly),
        if domain == FunctionDomain::Client {
            FunctionVolatility::Immutable
        } else {
            FunctionVolatility::Stable
        },
    );
    CatalogueSnapshot::new_with_functions(
        base.revision(),
        base.schemas().to_vec(),
        vec![object_type],
        vec![function],
    )
    .unwrap()
}

fn catalogue_with_opaque_client_return(
    opaque: TypeId,
    domain: FunctionDomain,
    parameters: Vec<ParameterDefinition>,
    security: FunctionSecurity,
    volatility: FunctionVolatility,
) -> CatalogueSnapshot {
    let base = catalogue();
    let prior_function = &base.functions()[0];
    let function = FunctionDefinition::new(
        prior_function.id(),
        prior_function.name().clone(),
        domain,
        parameters,
        FunctionReturn::Single(ResolvedType::value(opaque)),
        prior_function.current_revision(),
        security,
        None,
        volatility,
    );
    CatalogueSnapshot::new_with_functions(
        base.revision(),
        base.schemas().to_vec(),
        vec![],
        vec![function],
    )
    .unwrap()
}

fn verified_standard_snapshot_with_extra_value(
    value_type: ValueTypeDefinition,
) -> VerifiedStandardLibrarySnapshot {
    let base = standard_snapshot(false, None);
    let mut value_types = base.catalogue().value_types().to_vec();
    let value_type_id = value_type.id();
    value_types.push(value_type);
    let mut origins = base.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::ValueType(value_type_id),
        SourceOrigin::new(SourceUnitId::from_bytes(id::<22>()), 5, 6).unwrap(),
    ));
    let catalogue = CatalogueSnapshot::new_with_types(
        base.catalogue().revision(),
        base.catalogue().schemas().to_vec(),
        base.catalogue().object_types().to_vec(),
        value_types,
        base.catalogue().type_bindings().to_vec(),
    )
    .unwrap();
    let provisional = StandardLibrarySnapshot::new(
        base.revision(),
        base.digest_version(),
        base.source().clone(),
        base.language_version(),
        catalogue,
        origins,
        digest_bytes(b"provisional extra standard"),
    )
    .unwrap();
    let digest = calculate_standard_library_digest_for_test(&provisional).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            provisional.revision(),
            provisional.digest_version(),
            provisional.source().clone(),
            provisional.language_version(),
            provisional.catalogue().clone(),
            provisional.origins().to_vec(),
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

fn catalogue_with_application_types(reverse: bool) -> (CatalogueSnapshot, Vec<DefinitionOrigin>) {
    let base = catalogue();
    let mut value_types = vec![
        ValueTypeDefinition::primitive(
            TypeId::from_bytes(id::<30>()),
            QualifiedSemanticName::new(["crm", "flag"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.flag@1",
        ),
        ValueTypeDefinition::primitive(
            TypeId::from_bytes(id::<31>()),
            QualifiedSemanticName::new(["crm", "token"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Transient,
            "orna.kernel.value.token@1",
        ),
    ];
    let mut bindings = vec![
        TypeBinding::qualified(
            QualifiedSemanticName::new(["crm", "flag_alias"]).unwrap(),
            TypeId::from_bytes(id::<30>()),
        )
        .unwrap(),
        TypeBinding::prelude(
            PreludeTypeName::new(["APP", "TOKEN"]).unwrap(),
            TypeId::from_bytes(id::<31>()),
        )
        .unwrap(),
    ];
    let binding_ids = [bindings[0].id(), bindings[1].id()];
    if reverse {
        value_types.reverse();
        bindings.reverse();
    }
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        base.revision(),
        base.schemas().to_vec(),
        base.object_types().to_vec(),
        value_types,
        bindings,
        base.functions().to_vec(),
    )
    .unwrap();
    let source = SourceUnitId::from_bytes(id::<10>());
    let mut origins = origins();
    origins.extend([
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<30>())),
            SourceOrigin::new(source, 37, 38).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<31>())),
            SourceOrigin::new(source, 38, 39).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(binding_ids[0]),
            SourceOrigin::new(source, 39, 40).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(binding_ids[1]),
            SourceOrigin::new(source, 40, 41).unwrap(),
        ),
    ]);
    if reverse {
        origins.reverse();
    }
    (catalogue, origins)
}

fn catalogue_with_standard_boolean_slots(base: &CatalogueSnapshot) -> CatalogueSnapshot {
    let object = &base.object_types()[0];
    let field = &object.fields()[0];
    let object_type = ObjectTypeDefinition::new(
        object.id(),
        object.name().clone(),
        vec![FieldDefinition::new(
            field.id(),
            field.name(),
            field.ordinal(),
            ResolvedType::value(standard_boolean_id()),
            field.nullable(),
            field.unique(),
            field.default_expression(),
            field.on_delete(),
        )],
    );
    let prior_function = &base.functions()[0];
    let parameter = &prior_function.parameters()[0];
    let function = FunctionDefinition::new(
        prior_function.id(),
        prior_function.name().clone(),
        prior_function.domain(),
        vec![ParameterDefinition::new(
            parameter.id(),
            parameter.name(),
            parameter.ordinal(),
            ResolvedType::value(standard_boolean_id()),
            parameter.default_expression(),
        )],
        prior_function.return_type().clone(),
        prior_function.current_revision(),
        prior_function.security(),
        prior_function.transaction(),
        prior_function.volatility(),
    );
    CatalogueSnapshot::new_with_functions_and_types(
        base.revision(),
        base.schemas().to_vec(),
        vec![object_type],
        base.value_types().to_vec(),
        base.type_bindings().to_vec(),
        vec![function],
    )
    .unwrap()
}

fn catalogue_with_application_types_version_two(
    reverse: bool,
) -> (CatalogueSnapshot, Vec<DefinitionOrigin>) {
    let (catalogue, origins) = catalogue_with_application_types(reverse);
    (catalogue_with_standard_boolean_slots(&catalogue), origins)
}

fn catalogue_with_unaffected_function() -> (CatalogueSnapshot, Vec<DefinitionOrigin>) {
    let (base, mut origins) = catalogue_with_application_types_version_two(false);
    let unaffected = FunctionDefinition::new(
        FunctionId::from_bytes(id::<40>()),
        QualifiedSemanticName::new(["crm", "health"]).unwrap(),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Single(ResolvedType::value(standard_boolean_id())),
        FunctionRevisionId::from_bytes(id::<41>()),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    );
    let mut functions = base.functions().to_vec();
    functions.push(unaffected);
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        base.revision(),
        base.schemas().to_vec(),
        base.object_types().to_vec(),
        base.value_types().to_vec(),
        base.type_bindings().to_vec(),
        functions,
    )
    .unwrap();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(FunctionId::from_bytes(id::<40>())),
        SourceOrigin::new(SourceUnitId::from_bytes(id::<10>()), 41, 42).unwrap(),
    ));
    (catalogue, origins)
}

fn references() -> Vec<DefinitionReference> {
    vec![reference(
        0,
        DefinitionReferenceTarget::Field {
            owner: TypeId::from_bytes(id::<2>()),
            field: FieldId::from_bytes(id::<3>()),
        },
        DefinitionReferenceKind::QueryField,
        10,
        18,
    )]
}

fn reference(
    ordinal: u32,
    target: DefinitionReferenceTarget,
    kind: DefinitionReferenceKind,
    byte_start: u32,
    byte_end: u32,
) -> DefinitionReference {
    DefinitionReference::new(
        FunctionId::from_bytes(id::<4>()),
        FunctionRevisionId::from_bytes(id::<6>()),
        ordinal,
        target,
        kind,
        SourceOrigin::new(SourceUnitId::from_bytes(id::<10>()), byte_start, byte_end).unwrap(),
    )
}

fn write_references() -> Vec<DefinitionReference> {
    vec![
        reference(
            0,
            DefinitionReferenceTarget::ObjectType(TypeId::from_bytes(id::<2>())),
            DefinitionReferenceKind::WriteObject,
            10,
            18,
        ),
        reference(
            1,
            DefinitionReferenceTarget::Field {
                owner: TypeId::from_bytes(id::<2>()),
                field: FieldId::from_bytes(id::<3>()),
            },
            DefinitionReferenceKind::WriteField,
            18,
            26,
        ),
    ]
}

fn function_revision(catalogue: &CatalogueSnapshot) -> FunctionRevisionRecord {
    function_revision_with_references(catalogue, &references())
}

fn function_revision_with_references(
    catalogue: &CatalogueSnapshot,
    references: &[DefinitionReference],
) -> FunctionRevisionRecord {
    let function = catalogue
        .function_by_id(FunctionId::from_bytes(id::<4>()))
        .unwrap();
    let artifact = artifact();
    FunctionRevisionRecord::new(
        function.id(),
        function.current_revision(),
        1,
        SourceOrigin::new(SourceUnitId::from_bytes(id::<10>()), 0, 24).unwrap(),
        function_declaration_digest(b"FUNCTION crm.lookup").unwrap(),
        function_semantic_digest(function, "orna-1", &artifact, &[expression()], references)
            .unwrap(),
        "orna-1",
        artifact,
    )
    .unwrap()
}

fn value_type_references(target: TypeId) -> Vec<DefinitionReference> {
    vec![reference(
        0,
        DefinitionReferenceTarget::ValueType(target),
        DefinitionReferenceKind::NamedType,
        10,
        18,
    )]
}

fn function_revision_v2(
    catalogue: &CatalogueSnapshot,
    references: &[DefinitionReference],
) -> FunctionRevisionRecord {
    let function = catalogue
        .function_by_id(FunctionId::from_bytes(id::<4>()))
        .unwrap();
    let artifact = artifact();
    FunctionRevisionRecord::new(
        function.id(),
        function.current_revision(),
        1,
        SourceOrigin::new(SourceUnitId::from_bytes(id::<10>()), 0, 24).unwrap(),
        function_declaration_digest(b"FUNCTION crm.lookup").unwrap(),
        function_semantic_digest_with_version(
            FunctionSemanticHashVersion::Version2,
            function,
            "orna-1",
            &artifact,
            &[expression()],
            references,
        )
        .unwrap(),
        "orna-1",
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2)
}

fn unaffected_function_revision(catalogue: &CatalogueSnapshot) -> FunctionRevisionRecord {
    let function = catalogue
        .function_by_id(FunctionId::from_bytes(id::<40>()))
        .unwrap();
    let artifact = artifact();
    FunctionRevisionRecord::new(
        function.id(),
        function.current_revision(),
        1,
        SourceOrigin::new(SourceUnitId::from_bytes(id::<10>()), 41, 42).unwrap(),
        function_declaration_digest(b"FUNCTION crm.health").unwrap(),
        function_semantic_digest(function, "orna-1", &artifact, &[expression()], &[]).unwrap(),
        "orna-1",
        artifact,
    )
    .unwrap()
}

fn origins() -> Vec<DefinitionOrigin> {
    let source = SourceUnitId::from_bytes(id::<10>());
    vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<1>())),
            SourceOrigin::new(source, 0, 10).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(TypeId::from_bytes(id::<2>())),
            SourceOrigin::new(source, 11, 28).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<2>()),
                field: FieldId::from_bytes(id::<3>()),
            },
            SourceOrigin::new(source, 20, 27).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(FunctionId::from_bytes(id::<4>())),
            SourceOrigin::new(source, 0, 24).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: FunctionId::from_bytes(id::<4>()),
                parameter: ParameterId::from_bytes(id::<5>()),
            },
            SourceOrigin::new(source, 12, 19).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::FunctionReturnColumn {
                owner: FunctionId::from_bytes(id::<4>()),
                ordinal: 0,
            },
            SourceOrigin::new(source, 20, 24).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Expression(ExpressionId::from_bytes(id::<8>())),
            SourceOrigin::new(source, 30, 36).unwrap(),
        ),
    ]
}

fn hex(digest: Sha256Digest) -> String {
    digest
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn direct_hashes_have_stable_version_one_goldens() {
    assert_eq!(
        hex(source_unit_content_digest("hello").unwrap()),
        "d9f0c06003693e2e6be338bba922af77ef8f73d11192efd9d3cdd3eaccf17f9b"
    );
    assert_eq!(
        hex(artifact_payload_digest(b"a\0b").unwrap()),
        "c8b32ae8adc4ca209b47c569aa6c66a98de66b6d9cecad8f5e58ad098c942a0d"
    );
    assert_eq!(
        hex(function_declaration_digest(b"FUNCTION crm.lookup").unwrap()),
        "9492455bf7b62ddbe4ee91ceef6ca5ea7114d47636ec7224724aa72219fd90f7"
    );
    let empty_bundle = source_bundle_digest(&[]).unwrap();
    assert_eq!(
        hex(empty_bundle),
        "965513f9c104e3c3fca13b46dcd382a64041a063d35ff0a316149bf5a4bfd641"
    );
    assert_eq!(
        hex(source_revision_record_digest(
            SourceBundleId::from_bytes(id::<42>()),
            None,
            empty_bundle,
        )
        .unwrap()),
        "af56d821e5fa3ce3cccd67b0a46050435bb4da70c0f516297f6cc3cecb934729"
    );
    let empty_catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes(id::<43>()), vec![], vec![])
            .unwrap();
    assert_eq!(
        hex(catalogue_digest(&empty_catalogue, &[], &[], &[], &[]).unwrap()),
        "02dc700934a603ff73b56e1f63e8051a103922aa267cbf1e984ed3cf7964160b"
    );
}

#[test]
fn resolved_type_encoding_has_exact_tags_and_payloads() {
    for (scalar, tag) in StandardScalar::ALL.into_iter().zip(1_u8..=13) {
        let mut encoder = Encoder::new(&[]);
        encode_resolved_type(&mut encoder, ResolvedType::scalar(scalar));
        assert_eq!(encoder.bytes, vec![1, tag]);
    }

    let type_id = TypeId::from_bytes(id::<44>());
    let mut named = Encoder::new(&[]);
    encode_resolved_type(&mut named, ResolvedType::named(type_id));
    assert_eq!(named.bytes, [vec![2], id::<44>().to_vec()].concat());

    let mut reference = Encoder::new(&[]);
    encode_resolved_type(&mut reference, ResolvedType::reference(type_id));
    assert_eq!(reference.bytes, [vec![3], id::<44>().to_vec()].concat());

    let mut value = Encoder::new(&[]);
    encode_resolved_type(&mut value, ResolvedType::value(type_id));
    assert_eq!(value.bytes, [vec![4], id::<44>().to_vec()].concat());
}

#[test]
fn function_return_encoding_assigns_distinct_shape_tags() {
    let type_id = TypeId::from_bytes(id::<45>());

    let mut single = Encoder::new(&[]);
    encode_function_return(
        &mut single,
        &FunctionReturn::Single(ResolvedType::named(type_id)),
        ReturnColumnNames::Include,
    )
    .unwrap();

    let mut rows = Encoder::new(&[]);
    encode_function_return(
        &mut rows,
        &FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "item",
            0,
            ResolvedType::named(type_id),
        )]),
        ReturnColumnNames::Include,
    )
    .unwrap();

    let mut stream = Encoder::new(&[]);
    encode_function_return(
        &mut stream,
        &FunctionReturn::Stream(ResolvedType::named(type_id)),
        ReturnColumnNames::Include,
    )
    .unwrap();

    assert_eq!(single.bytes[0], 1);
    assert_eq!(rows.bytes[0], 2);
    assert_eq!(stream.bytes, [vec![3, 2], id::<45>().to_vec()].concat());
    assert_ne!(stream.bytes, single.bytes);
    assert_ne!(stream.bytes, rows.bytes);
}

#[test]
fn opaque_value_type_uses_the_exact_version_two_kind_byte() {
    let value_type = ValueTypeDefinition::opaque(
        TypeId::from_bytes(id::<96>()),
        QualifiedSemanticName::new(["std", "example", "token"]).unwrap(),
        "std.example.token@1",
    );
    let mut encoder = Encoder::new(&[]);

    encode_value_types(&mut encoder, &[value_type], None).unwrap();

    let mut expected = 1_u32.to_be_bytes().to_vec();
    expected.extend_from_slice(&id::<96>());
    expected.extend_from_slice(&3_u32.to_be_bytes());
    for part in ["std", "example", "token"] {
        expected.extend_from_slice(&(part.len() as u32).to_be_bytes());
        expected.extend_from_slice(part.as_bytes());
    }
    expected.extend_from_slice(&[2, 1, 2]);
    expected.extend_from_slice(&19_u32.to_be_bytes());
    expected.extend_from_slice(b"std.example.token@1");
    assert_eq!(encoder.bytes, expected);
}

#[test]
fn resolved_value_hash_errors_have_exact_source_free_contracts() {
    let identity = DefinitionIdentity::Parameter {
        owner: FunctionId::from_bytes(id::<4>()),
        parameter: ParameterId::from_bytes(id::<5>()),
    };
    let value_type = TypeId::from_bytes(id::<99>());
    let errors = [
        (
            CanonicalHashError::ResolvedValueRequiresCatalogueHashVersionTwo {
                identity,
                value_type,
            },
            "resolved value type requires catalogue hash version 2",
        ),
        (
            CanonicalHashError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity,
                scalar: StandardScalar::Boolean,
            },
            "legacy scalar resolved type requires catalogue hash version 1",
        ),
        (
            CanonicalHashError::ResolvedValueTypeNotInPinnedStandard {
                identity,
                value_type,
            },
            "resolved value type is absent from the pinned standard library",
        ),
        (
            CanonicalHashError::OpaqueValueTypeNotAcceptedInSlot {
                identity,
                value_type,
            },
            "opaque value type is not accepted in a catalogue slot",
        ),
    ];
    for (error, display) in errors {
        assert_eq!(error.to_string(), display);
        assert!(error.source().is_none());
    }
}

#[test]
fn resolved_value_slot_scan_has_the_closed_order_and_version_gates() {
    let field_value = ResolvedType::value(TypeId::from_bytes(id::<90>()));
    let parameter_value = ResolvedType::value(TypeId::from_bytes(id::<91>()));
    let return_value = ResolvedType::value(TypeId::from_bytes(id::<92>()));
    let standard = verified_standard_snapshot(false);
    let version_two = CatalogueHashContext::version_two(standard);

    let field_first = catalogue_with_resolved_slot_types(
        field_value,
        parameter_value,
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "found",
            0,
            return_value,
        )]),
        FunctionDomain::Client,
    );
    assert!(matches!(
        catalogue_digest(&field_first, &[], &[], &[], &[]),
        Err(CanonicalHashError::ResolvedValueRequiresCatalogueHashVersionTwo {
            identity: DefinitionIdentity::Field { owner, field },
            value_type,
        }) if owner == TypeId::from_bytes(id::<2>())
            && field == FieldId::from_bytes(id::<3>())
            && value_type == TypeId::from_bytes(id::<90>())
    ));
    assert!(matches!(
        catalogue_digest_with_context(
            &version_two,
            &field_first,
            &[],
            &[],
            &[],
            &[],
        ),
        Err(CanonicalHashError::ResolvedValueTypeNotInPinnedStandard {
            identity: DefinitionIdentity::Field { owner, field },
            value_type,
        }) if owner == TypeId::from_bytes(id::<2>())
            && field == FieldId::from_bytes(id::<3>())
            && value_type == TypeId::from_bytes(id::<90>())
    ));

    let parameter_first = catalogue_with_resolved_slot_types(
        ResolvedType::named(TypeId::from_bytes(id::<2>())),
        parameter_value,
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionDomain::Client,
    );
    assert!(matches!(
        catalogue_digest_with_context(
            &version_two,
            &parameter_first,
            &[],
            &[],
            &[],
            &[],
        ),
        Err(CanonicalHashError::ResolvedValueTypeNotInPinnedStandard {
            identity: DefinitionIdentity::Parameter { owner, parameter },
            value_type,
        }) if owner == FunctionId::from_bytes(id::<4>())
            && parameter == ParameterId::from_bytes(id::<5>())
            && value_type == TypeId::from_bytes(id::<91>())
    ));

    let rows_return = catalogue_with_resolved_slot_types(
        ResolvedType::named(TypeId::from_bytes(id::<2>())),
        ResolvedType::named(TypeId::from_bytes(id::<2>())),
        FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new(
                "first",
                0,
                ResolvedType::value(TypeId::from_bytes(id::<93>())),
            ),
            FunctionReturnColumnDefinition::new(
                "second",
                1,
                ResolvedType::value(TypeId::from_bytes(id::<94>())),
            ),
        ]),
        FunctionDomain::Server,
    );
    assert!(matches!(
        catalogue_digest_with_context(
            &version_two,
            &rows_return,
            &[],
            &[],
            &[],
            &[],
        ),
        Err(CanonicalHashError::ResolvedValueTypeNotInPinnedStandard {
            identity: DefinitionIdentity::FunctionReturnColumn { owner, ordinal },
            value_type,
        }) if owner == FunctionId::from_bytes(id::<4>())
            && ordinal == 0
            && value_type == TypeId::from_bytes(id::<93>())
    ));

    let field_scalar_before_missing_parameter = catalogue_with_resolved_slot_types(
        ResolvedType::scalar(StandardScalar::Boolean),
        parameter_value,
        FunctionReturn::Single(return_value),
        FunctionDomain::Client,
    );
    assert_eq!(
        catalogue_digest_with_context(
            &version_two,
            &field_scalar_before_missing_parameter,
            &[],
            &[],
            &[],
            &[],
        ),
        Err(
            CanonicalHashError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity: DefinitionIdentity::Field {
                    owner: TypeId::from_bytes(id::<2>()),
                    field: FieldId::from_bytes(id::<3>()),
                },
                scalar: StandardScalar::Boolean,
            },
        )
    );

    let hostile_client_parameter = catalogue_with_resolved_slot_types(
        ResolvedType::named(TypeId::from_bytes(id::<2>())),
        ResolvedType::scalar(StandardScalar::Integer),
        FunctionReturn::Single(ResolvedType::value(standard_boolean_id())),
        FunctionDomain::Client,
    );
    assert_eq!(
        catalogue_digest_with_context(&version_two, &hostile_client_parameter, &[], &[], &[], &[],),
        Err(
            CanonicalHashError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity: DefinitionIdentity::Parameter {
                    owner: FunctionId::from_bytes(id::<4>()),
                    parameter: ParameterId::from_bytes(id::<5>()),
                },
                scalar: StandardScalar::Integer,
            },
        )
    );

    let rows_scalar_before_missing_column = catalogue_with_resolved_slot_types(
        ResolvedType::named(TypeId::from_bytes(id::<2>())),
        ResolvedType::value(standard_boolean_id()),
        FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new(
                "first",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            FunctionReturnColumnDefinition::new(
                "second",
                1,
                ResolvedType::value(TypeId::from_bytes(id::<94>())),
            ),
        ]),
        FunctionDomain::Server,
    );
    assert_eq!(
        catalogue_digest_with_context(
            &version_two,
            &rows_scalar_before_missing_column,
            &[],
            &[],
            &[],
            &[],
        ),
        Err(
            CanonicalHashError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity: DefinitionIdentity::FunctionReturnColumn {
                    owner: FunctionId::from_bytes(id::<4>()),
                    ordinal: 0,
                },
                scalar: StandardScalar::Boolean,
            },
        )
    );

    let single_scalar = catalogue_with_resolved_slot_types(
        ResolvedType::named(TypeId::from_bytes(id::<2>())),
        ResolvedType::value(standard_boolean_id()),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionDomain::Client,
    );
    assert_eq!(
        catalogue_digest_with_context(&version_two, &single_scalar, &[], &[], &[], &[]),
        Err(
            CanonicalHashError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity: DefinitionIdentity::Function(FunctionId::from_bytes(id::<4>())),
                scalar: StandardScalar::Boolean,
            },
        )
    );
}

#[test]
fn version_two_resolved_value_accepts_a_present_non_golden_standard_id() {
    let value_type = TypeId::from_bytes(id::<95>());
    let standard = verified_standard_snapshot_with_extra_value(ValueTypeDefinition::primitive(
        value_type,
        QualifiedSemanticName::new(["std", "types", "extra"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.extra@1",
    ));
    let context = CatalogueHashContext::version_two(standard);
    let (catalogue, origins) = catalogue_with_application_types_version_two(false);
    let object = &catalogue.object_types()[0];
    let field = &object.fields()[0];
    let value_object = ObjectTypeDefinition::new(
        object.id(),
        object.name().clone(),
        vec![FieldDefinition::new(
            field.id(),
            field.name(),
            field.ordinal(),
            ResolvedType::value(value_type),
            field.nullable(),
            field.unique(),
            field.default_expression(),
            field.on_delete(),
        )],
    );
    let value_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![value_object],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        catalogue.functions().to_vec(),
    )
    .unwrap();
    let references = value_type_references(standard_boolean_id());
    let revision = function_revision_v2(&value_catalogue, &references);
    assert!(
        catalogue_digest_with_context(
            &context,
            &value_catalogue,
            &[revision],
            &[expression()],
            &origins,
            &references,
        )
        .is_ok()
    );
}

#[test]
fn version_two_rejects_a_pinned_opaque_value_in_non_function_catalogue_slots() {
    let opaque = TypeId::from_bytes(id::<96>());
    let standard = verified_standard_snapshot_with_extra_value(ValueTypeDefinition::opaque(
        opaque,
        QualifiedSemanticName::new(["std", "types", "token"]).unwrap(),
        "std.types.token@1",
    ));
    let context = CatalogueHashContext::version_two(standard);
    let named = ResolvedType::named(TypeId::from_bytes(id::<2>()));
    let cases = [
        (
            catalogue_with_resolved_slot_types(
                ResolvedType::value(opaque),
                named,
                FunctionReturn::Single(named),
                FunctionDomain::Client,
            ),
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<2>()),
                field: FieldId::from_bytes(id::<3>()),
            },
        ),
        (
            catalogue_with_resolved_slot_types(
                named,
                ResolvedType::value(opaque),
                FunctionReturn::Single(named),
                FunctionDomain::Client,
            ),
            DefinitionIdentity::Parameter {
                owner: FunctionId::from_bytes(id::<4>()),
                parameter: ParameterId::from_bytes(id::<5>()),
            },
        ),
        (
            catalogue_with_resolved_slot_types(
                named,
                named,
                FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                    "token",
                    0,
                    ResolvedType::value(opaque),
                )]),
                FunctionDomain::Server,
            ),
            DefinitionIdentity::FunctionReturnColumn {
                owner: FunctionId::from_bytes(id::<4>()),
                ordinal: 0,
            },
        ),
    ];

    for (catalogue, identity) in cases {
        assert_eq!(
            validate_resolved_type_slots(&context, &catalogue),
            Err(CanonicalHashError::OpaqueValueTypeNotAcceptedInSlot {
                identity,
                value_type: opaque,
            })
        );
    }
}

#[test]
fn version_two_accepts_only_the_exact_pinned_opaque_client_return() {
    let opaque = TypeId::from_bytes(id::<96>());
    let standard = verified_standard_snapshot_with_extra_value(ValueTypeDefinition::opaque(
        opaque,
        QualifiedSemanticName::new(["std", "types", "token"]).unwrap(),
        "std.types.token@1",
    ));
    let context = CatalogueHashContext::version_two(standard);
    let parameter = ParameterDefinition::new(
        ParameterId::from_bytes(id::<5>()),
        "enabled",
        0,
        ResolvedType::value(standard_boolean_id()),
        None,
    );
    let accepted = catalogue_with_opaque_client_return(
        opaque,
        FunctionDomain::Client,
        vec![parameter],
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    assert_eq!(validate_resolved_type_slots(&context, &accepted), Ok(()));

    for catalogue in [
        catalogue_with_opaque_client_return(
            opaque,
            FunctionDomain::Server,
            vec![],
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        ),
        catalogue_with_opaque_client_return(
            opaque,
            FunctionDomain::Client,
            vec![],
            FunctionSecurity::Definer,
            FunctionVolatility::Immutable,
        ),
        catalogue_with_opaque_client_return(
            opaque,
            FunctionDomain::Client,
            vec![],
            FunctionSecurity::Invoker,
            FunctionVolatility::Stable,
        ),
    ] {
        assert_eq!(
            validate_resolved_type_slots(&context, &catalogue),
            Err(CanonicalHashError::OpaqueValueTypeNotAcceptedInSlot {
                identity: DefinitionIdentity::Function(FunctionId::from_bytes(id::<4>())),
                value_type: opaque,
            })
        );
    }
}

#[test]
fn version_one_rejects_an_opaque_definition_before_slot_validation() {
    let opaque = TypeId::from_bytes(id::<96>());
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<97>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<98>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![ValueTypeDefinition::opaque(
            opaque,
            QualifiedSemanticName::new(["std", "token"]).unwrap(),
            "std.token@1",
        )],
        vec![],
    )
    .unwrap();

    assert_eq!(
        catalogue_digest(&catalogue, &[], &[], &[], &[]),
        Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
            version: CatalogueHashVersion::Version1,
            fact: CatalogueHashFact::ValueTypeDefinition(opaque),
        })
    );
}

#[test]
fn standard_library_digest_has_a_stable_order_independent_version_one_golden() {
    let standard = standard_snapshot(false, None);
    let reversed = standard_snapshot(true, None);
    let digest = standard_library_digest(&standard).unwrap();

    assert_eq!(
        hex(digest),
        "2f5c79f487506075cf189852aa4197eb8aa7ed4470e391a6a8ca119175c02261"
    );
    assert_eq!(standard_library_digest(&reversed).unwrap(), digest);
    let verified = verify_standard_library_snapshot(standard).unwrap();
    assert_eq!(verified.digest(), digest);
    assert_eq!(
        verified.revision(),
        StandardLibraryRevisionId::from_bytes(id::<28>())
    );
}

#[test]
fn executable_standard_library_digest_uses_the_v2_contract() {
    let standard = standard_v2_snapshot(None);
    let digest = standard_library_digest(&standard).unwrap();

    assert_eq!(
        hex(digest),
        "e7525fa634825d8981b423829d18ce84c59c52783bc6f919dd008979929b35a2"
    );
    assert_eq!(
        verify_standard_library_v2_snapshot(standard.clone())
            .unwrap()
            .digest(),
        digest
    );
    assert!(matches!(
        verify_standard_library_v2_snapshot(standard_snapshot(false, None)),
        Err(CanonicalHashError::StandardLibraryDigestVersionMismatch {
            expected: StandardLibraryDigestVersion::Version2,
            actual: StandardLibraryDigestVersion::Version1,
            ..
        })
    ));
    assert!(matches!(
        standard_library_digest(&standard_v2_snapshot(Some(digest_bytes(b"tampered")))),
        Err(CanonicalHashError::StandardLibraryDigestMismatch { .. })
    ));
}

#[test]
fn executable_standard_digest_binds_each_retained_v2_fact() {
    let base = standard_v2_snapshot(None);
    let function = base.catalogue().functions()[0].clone();
    let base_executable = base.executables()[0].clone();
    let base_revision = base_executable.revision().clone();
    let schemas = base.catalogue().schemas().to_vec();
    let catalogue_for = |function| {
        CatalogueSnapshot::new_with_functions(
            base.catalogue().revision(),
            schemas.clone(),
            vec![],
            vec![function],
        )
        .unwrap()
    };

    let changed_function = standard_v2_function_with_revision(
        &function,
        FunctionId::from_bytes(id::<100>()),
        FunctionRevisionId::from_bytes(id::<101>()),
    );
    let changed_function_revision = standard_v2_revision(
        &changed_function,
        changed_function.current_revision(),
        &base_revision,
        base_revision.artifact().clone(),
        &[],
    );
    let changed_function_origins = base
        .origins()
        .iter()
        .map(|origin| match origin.identity() {
            DefinitionIdentity::Function(_) => DefinitionOrigin::new(
                DefinitionIdentity::Function(changed_function.id()),
                origin.source(),
            ),
            _ => origin.clone(),
        })
        .collect::<Vec<_>>();
    let changed_function = rebuilt_standard_v2_snapshot(
        &base,
        base.source().clone(),
        catalogue_for(changed_function.clone()),
        StandardExecutable::new(changed_function.id(), changed_function_revision, vec![]).unwrap(),
        changed_function_origins,
    );

    let changed_current = standard_v2_function_with_revision(
        &function,
        function.id(),
        FunctionRevisionId::from_bytes(id::<102>()),
    );
    let changed_current_revision = standard_v2_revision(
        &changed_current,
        changed_current.current_revision(),
        &base_revision,
        base_revision.artifact().clone(),
        &[],
    );
    let changed_current = rebuilt_standard_v2_snapshot(
        &base,
        base.source().clone(),
        catalogue_for(changed_current.clone()),
        StandardExecutable::new(changed_current.id(), changed_current_revision, vec![]).unwrap(),
        base.origins().to_vec(),
    );

    let payload = b"changed-standard-artifact".to_vec();
    let changed_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let changed_artifact_revision = standard_v2_revision(
        &function,
        base_revision.id(),
        &base_revision,
        changed_artifact,
        &[],
    );
    let changed_artifact = rebuilt_standard_v2_snapshot(
        &base,
        base.source().clone(),
        base.catalogue().clone(),
        StandardExecutable::new(function.id(), changed_artifact_revision, vec![]).unwrap(),
        base.origins().to_vec(),
    );

    let changed_reference = DefinitionReference::new(
        function.id(),
        base_revision.id(),
        0,
        DefinitionReferenceTarget::Function(function.id()),
        DefinitionReferenceKind::FunctionCall,
        base_revision.declaration_origin(),
    );
    let changed_reference_revision = standard_v2_revision(
        &function,
        base_revision.id(),
        &base_revision,
        base_revision.artifact().clone(),
        std::slice::from_ref(&changed_reference),
    );
    let changed_reference = rebuilt_standard_v2_snapshot(
        &base,
        base.source().clone(),
        base.catalogue().clone(),
        StandardExecutable::new(
            function.id(),
            changed_reference_revision,
            vec![changed_reference],
        )
        .unwrap(),
        base.origins().to_vec(),
    );

    let changed_origin = rebuilt_standard_v2_snapshot(
        &base,
        base.source().clone(),
        base.catalogue().clone(),
        base_executable,
        base.origins()
            .iter()
            .map(|origin| match origin.identity() {
                DefinitionIdentity::Schema(schema) => DefinitionOrigin::new(
                    DefinitionIdentity::Schema(schema),
                    SourceOrigin::new(origin.source().source_unit(), 1, 2).unwrap(),
                ),
                _ => origin.clone(),
            })
            .collect(),
    );

    let source = base.source();
    let changed_parent = SourceRevisionId::from_bytes(id::<103>());
    let changed_source = StoredSourceRevision::new(
        source.bundle(),
        source.id(),
        Some(changed_parent),
        source.units().to_vec(),
        source.bundle_hash(),
        source_revision_record_digest(source.bundle(), Some(changed_parent), source.bundle_hash())
            .unwrap(),
    )
    .unwrap();
    let changed_parent = rebuilt_standard_v2_snapshot(
        &base,
        changed_source,
        base.catalogue().clone(),
        base.executables()[0].clone(),
        base.origins().to_vec(),
    );

    for tampered in [
        changed_function,
        changed_current,
        changed_artifact,
        changed_reference,
        changed_origin,
        changed_parent,
    ] {
        assert_ne!(
            calculate_standard_library_digest(&tampered).unwrap(),
            base.digest()
        );
        assert!(matches!(
            verify_standard_library_v2_snapshot(tampered),
            Err(CanonicalHashError::StandardLibraryDigestMismatch { .. })
        ));
    }
}

#[test]
fn version_two_function_and_catalogue_hashes_have_stable_goldens() {
    let (catalogue, application_origins) = catalogue_with_application_types_version_two(false);
    let references = value_type_references(standard_boolean_id());
    let revision = function_revision_v2(&catalogue, &references);
    let function = catalogue.functions().first().unwrap();
    let semantic = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        revision.language_version(),
        revision.artifact(),
        &[expression()],
        &references,
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        std::slice::from_ref(&revision),
        &[expression()],
        &application_origins,
        &references,
    )
    .unwrap();

    assert_eq!(
        hex(semantic),
        "8ce90cad1ae586523eb512e59e3f651b13ff641d19fb5a39b02be96d684fbc8e"
    );
    assert_eq!(
        hex(catalogue_hash),
        "26afbfeff3ef374e040859c061c43220f8d87255a7fdaba51c209b28859a4d23"
    );

    let reversed_context = CatalogueHashContext::version_two(verified_standard_snapshot(true));
    let (reversed_catalogue, reversed_origins) = catalogue_with_application_types_version_two(true);
    let reversed_revision = function_revision_v2(&reversed_catalogue, &references);
    assert_eq!(
        catalogue_digest_with_context(
            &reversed_context,
            &reversed_catalogue,
            &[reversed_revision],
            &[expression()],
            &reversed_origins,
            &references,
        )
        .unwrap(),
        catalogue_hash
    );
}

#[test]
fn version_two_catalogue_accepts_mixed_current_semantic_hash_versions() {
    let (catalogue, origins) = catalogue_with_unaffected_function();
    let references = value_type_references(standard_boolean_id());
    let affected = function_revision_v2(&catalogue, &references);
    let unaffected = unaffected_function_revision(&catalogue);

    assert_eq!(
        affected.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(
        unaffected.semantic_hash_version(),
        FunctionSemanticHashVersion::Version1
    );
    assert!(
        catalogue_digest_with_context(
            &CatalogueHashContext::version_two(verified_standard_snapshot(false)),
            &catalogue,
            &[unaffected, affected],
            &[expression()],
            &origins,
            &references,
        )
        .is_ok()
    );
}

#[test]
fn catalogue_hash_binds_every_enum_label_and_its_declaration_order() {
    let digest = |labels: &[&str]| {
        let schema = SchemaDefinition::new(
            SchemaId::from_bytes(id::<1>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        );
        let enum_types = if labels.is_empty() {
            Vec::new()
        } else {
            vec![EnumTypeDefinition::new(
                TypeId::from_bytes(id::<2>()),
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                labels.iter().copied(),
            )]
        };
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes(id::<7>()),
            vec![schema],
            vec![],
            vec![],
            enum_types,
            vec![],
        )
        .unwrap();
        let mut origins = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<1>())),
            SourceOrigin::new(SourceUnitId::from_bytes(id::<10>()), 0, 1).unwrap(),
        )];
        if !labels.is_empty() {
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ValueType(TypeId::from_bytes(id::<2>())),
                SourceOrigin::new(SourceUnitId::from_bytes(id::<10>()), 1, 2).unwrap(),
            ));
        }
        catalogue_digest_with_context(
            &CatalogueHashContext::version_two(verified_standard_snapshot(false)),
            &catalogue,
            &[],
            &[],
            &origins,
            &[],
        )
        .unwrap()
    };

    let original = digest(&["lead", "qualified", "customer"]);
    assert_ne!(digest(&[]), original);
    assert_ne!(digest(&["lead", "qualified"]), original);
    assert_ne!(digest(&["lead", "accepted", "customer"]), original);
    assert_ne!(digest(&["qualified", "lead", "customer"]), original);
    assert_eq!(digest(&["lead", "qualified", "customer"]), original);
}

#[test]
fn versioned_hashes_reject_mismatches_and_version_two_only_facts_in_version_one() {
    let valid_standard = standard_snapshot(false, None);
    let mismatched_source = StoredSourceRevision::new(
        valid_standard.source().bundle(),
        valid_standard.source().id(),
        None,
        valid_standard.source().units().to_vec(),
        valid_standard.source().bundle_hash(),
        digest_bytes(b"incorrect standard source revision hash"),
    )
    .unwrap();
    let mismatched_source_standard = StandardLibrarySnapshot::new(
        valid_standard.revision(),
        valid_standard.digest_version(),
        mismatched_source,
        valid_standard.language_version(),
        valid_standard.catalogue().clone(),
        valid_standard.origins().to_vec(),
        valid_standard.digest(),
    )
    .unwrap();
    assert!(matches!(
        standard_library_digest(&mismatched_source_standard),
        Err(CanonicalHashError::StandardSourceRevisionHashMismatch { .. })
    ));
    assert!(matches!(
        verify_standard_library_snapshot(mismatched_source_standard),
        Err(CanonicalHashError::StandardSourceRevisionHashMismatch { .. })
    ));

    let mismatched_standard =
        standard_snapshot(false, Some(digest_bytes(b"incorrect standard digest")));
    assert!(matches!(
        standard_library_digest(&mismatched_standard),
        Err(CanonicalHashError::StandardLibraryDigestMismatch { .. })
    ));
    assert!(matches!(
        verify_standard_library_snapshot(mismatched_standard),
        Err(CanonicalHashError::StandardLibraryDigestMismatch { .. })
    ));

    let catalogue = catalogue();
    let version_two_catalogue = catalogue_with_standard_boolean_slots(&catalogue);
    let references = value_type_references(standard_boolean_id());
    let revision = function_revision_v2(&catalogue, &references);
    let function = catalogue.functions().first().unwrap();
    assert!(matches!(
        function_semantic_digest(
            function,
            revision.language_version(),
            revision.artifact(),
            &[expression()],
            &references,
        ),
        Err(CanonicalHashError::FunctionFactUnsupportedBySemanticHashVersion {
            version: FunctionSemanticHashVersion::Version1,
            function: rejected_function,
            fact: FunctionSemanticHashFact::ValueTypeReference(target),
        }) if rejected_function == function.id() && target == standard_boolean_id()
    ));
    let version_two_revision = function_revision_v2(&version_two_catalogue, &references);
    let mismatched_revision = FunctionRevisionRecord::new(
        version_two_revision.function(),
        version_two_revision.id(),
        version_two_revision.revision_number(),
        version_two_revision.declaration_origin(),
        version_two_revision.declaration_content_hash(),
        digest_bytes(b"incorrect function semantic hash"),
        version_two_revision.language_version(),
        version_two_revision.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(version_two_revision.semantic_hash_version());
    assert!(matches!(
        catalogue_digest_with_context(
            &CatalogueHashContext::version_two(verified_standard_snapshot(false)),
            &version_two_catalogue,
            &[mismatched_revision],
            &[expression()],
            &origins(),
            &references,
        ),
        Err(CanonicalHashError::FunctionSemanticHashMismatch { .. })
    ));
    assert!(matches!(
        catalogue_digest(
            &catalogue,
            &[revision],
            &[expression()],
            &origins(),
            &references,
        ),
        Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
            version: CatalogueHashVersion::Version1,
            fact: CatalogueHashFact::DefinitionReferenceTarget(
                DefinitionReferenceTarget::ValueType(target),
            ),
        }) if target == standard_boolean_id()
    ));
}

#[test]
fn unsupported_version_errors_name_the_fact_and_required_upgrade() {
    let catalogue_cases = [
        (
            CatalogueHashFact::ValueTypeDefinition(TypeId::from_bytes(id::<30>())),
            "catalogue hash version 1 cannot include value types; use catalogue hash version 2",
        ),
        (
            CatalogueHashFact::TypeBinding(TypeBindingId::from_bytes(id::<31>())),
            "catalogue hash version 1 cannot include type-name bindings; use catalogue hash version 2",
        ),
        (
            CatalogueHashFact::DefinitionOrigin(DefinitionIdentity::ValueType(TypeId::from_bytes(
                id::<30>(),
            ))),
            "catalogue hash version 1 cannot include value-type or binding origins; use catalogue hash version 2",
        ),
        (
            CatalogueHashFact::DefinitionReferenceTarget(DefinitionReferenceTarget::ValueType(
                TypeId::from_bytes(id::<30>()),
            )),
            "catalogue hash version 1 cannot include value-type references; use catalogue hash version 2",
        ),
        (
            CatalogueHashFact::FunctionSemanticHashVersion {
                function: FunctionId::from_bytes(id::<4>()),
                revision: FunctionRevisionId::from_bytes(id::<6>()),
                version: FunctionSemanticHashVersion::Version2,
            },
            "catalogue hash version 1 cannot include function semantic hash version 2; use catalogue hash version 2",
        ),
    ];
    for (fact, message) in catalogue_cases {
        assert_eq!(
            CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                version: CatalogueHashVersion::Version1,
                fact,
            }
            .to_string(),
            message
        );
    }

    assert_eq!(
        CanonicalHashError::FunctionFactUnsupportedBySemanticHashVersion {
            version: FunctionSemanticHashVersion::Version1,
            function: FunctionId::from_bytes(id::<4>()),
            fact: FunctionSemanticHashFact::ValueTypeReference(TypeId::from_bytes(id::<30>())),
        }
        .to_string(),
        "function semantic hash version 1 cannot include value-type references; use function semantic hash version 2"
    );
}

#[test]
fn record_value_types_use_only_the_version_two_hash_contract() {
    let catalogue = catalogue_with_record_value_type();
    let standard = verified_standard_snapshot(false);
    let context = CatalogueHashContext::version_two(standard.clone());
    let expected_fact =
        CatalogueHashFact::RecordValueTypeDefinition(TypeId::from_bytes(id::<42>()));

    assert_eq!(
        catalogue_digest(&catalogue, &[], &[], &[], &[]),
        Err(CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
            version: CatalogueHashVersion::Version1,
            fact: expected_fact.clone(),
        })
    );
    assert!(
            catalogue_digest_with_context(
                &context,
                &catalogue,
                &[],
                &[],
                &record_value_origins(),
                &[],
            )
            .is_ok()
        );

    let mut encoder = Encoder::new(b"");
    encode_record_value_types(&mut encoder, &catalogue, standard.catalogue()).unwrap();
    let mut expected = vec![0, 0, 0, 1];
    expected.extend([42; 16]);
    expected.extend([0, 0, 0, 2, 0, 0, 0, 3]);
    expected.extend(b"crm");
    expected.extend([0, 0, 0, 6]);
    expected.extend(b"status");
    expected.extend([2, 1, 1, 0, 0, 0, 1]);
    expected.extend([43; 16]);
    expected.extend([0, 0, 0, 6]);
    expected.extend(b"active");
    expected.extend([0, 0, 0, 0, 4]);
    expected.extend([21; 16]);
    assert_eq!(encoder.bytes, expected);
}

#[test]
fn version_two_record_hash_has_a_stable_order_independent_golden() {
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let (catalogue, origins) = catalogue_with_ordered_record_values(false);
    let (reversed, reversed_origins) = catalogue_with_ordered_record_values(true);
    let digest =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    assert_eq!(
        digest,
        catalogue_digest_with_context(&context, &reversed, &[], &[], &reversed_origins, &[],)
            .unwrap()
    );
    assert_eq!(
        digest,
        Sha256Digest::from_bytes([
            0xb0, 0xb7, 0x3b, 0x5c, 0x41, 0xcd, 0x6d, 0x78, 0x0c, 0x38, 0x56, 0xd6, 0xd0, 0x6d,
            0xa2, 0x25, 0x1f, 0x0d, 0xa1, 0x9a, 0x3f, 0xf2, 0x05, 0x58, 0xa8, 0x13, 0xf1, 0xdd,
            0x3a, 0xb5, 0xa7, 0x40,
        ])
    );
}

#[test]
fn version_two_record_hash_binds_every_semantic_member_fact() {
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let digest = |name: &str, fields: Vec<RecordValueFieldDefinition>| {
        let enum_type = TypeId::from_bytes(id::<47>());
        let source = SourceUnitId::from_bytes(id::<3>());
        let mut origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(SchemaId::from_bytes(id::<41>())),
                SourceOrigin::new(source, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(enum_type),
                SourceOrigin::new(source, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(TypeId::from_bytes(id::<42>())),
                SourceOrigin::new(source, 2, 3).unwrap(),
            ),
        ];
        origins.extend(fields.iter().enumerate().map(|(index, field)| {
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: TypeId::from_bytes(id::<42>()),
                    field: field.id(),
                },
                SourceOrigin::new(source, index as u32 + 3, index as u32 + 4).unwrap(),
            )
        }));
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes(id::<40>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<41>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                enum_type,
                QualifiedSemanticName::new(["crm", "phase"]).unwrap(),
                ["new", "done"],
            )],
            vec![RecordValueTypeDefinition::new(
                TypeId::from_bytes(id::<42>()),
                QualifiedSemanticName::new(["crm", name]).unwrap(),
                fields,
            )],
            vec![],
        )
        .unwrap();
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap()
    };
    let field = |id: u8, name, ordinal, descriptor| {
        RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([id; 16]),
            name,
            ordinal,
            descriptor,
        )
        .expect("record field")
    };
    let base_fields = || {
        vec![
            field(
                43,
                "active",
                0,
                TypeDescriptor::named(standard_boolean_id()),
            ),
            field(
                44,
                "enabled",
                1,
                TypeDescriptor::named(standard_boolean_id()),
            ),
        ]
    };
    let base = digest("status", base_fields());
    let variants = [
        digest("state", base_fields()),
        digest(
            "status",
            vec![
                field(
                    45,
                    "active",
                    0,
                    TypeDescriptor::named(standard_boolean_id()),
                ),
                field(
                    44,
                    "enabled",
                    1,
                    TypeDescriptor::named(standard_boolean_id()),
                ),
            ],
        ),
        digest(
            "status",
            vec![
                field(
                    43,
                    "changed",
                    0,
                    TypeDescriptor::named(standard_boolean_id()),
                ),
                field(
                    44,
                    "enabled",
                    1,
                    TypeDescriptor::named(standard_boolean_id()),
                ),
            ],
        ),
        digest(
            "status",
            vec![
                field(
                    44,
                    "enabled",
                    0,
                    TypeDescriptor::named(standard_boolean_id()),
                ),
                field(
                    43,
                    "active",
                    1,
                    TypeDescriptor::named(standard_boolean_id()),
                ),
            ],
        ),
        digest(
            "status",
            vec![
                field(
                    43,
                    "active",
                    0,
                    TypeDescriptor::named(TypeId::from_bytes(id::<47>())),
                ),
                field(
                    44,
                    "enabled",
                    1,
                    TypeDescriptor::named(standard_boolean_id()),
                ),
            ],
        ),
    ];
    for variant in variants {
        assert_ne!(variant, base);
    }
}

#[test]
fn version_two_record_fields_accept_only_the_closed_primitive_and_enum_family() {
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let build = |descriptor: &TypeDescriptor| {
        CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes(id::<40>()),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes(id::<41>()),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![],
            vec![RecordValueTypeDefinition::new(
                TypeId::from_bytes(id::<42>()),
                QualifiedSemanticName::new(["crm", "status"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::try_new_descriptor(
                        FieldId::from_bytes(id::<43>()),
                        "value",
                        0,
                        descriptor.clone(),
                    )
                    .expect("record field"),
                ],
            )],
            vec![],
        )
        .unwrap()
    };

    for descriptor in [
        TypeDescriptor::named(TypeId::from_bytes(id::<99>())),
        TypeDescriptor::named(TypeId::from_bytes(id::<98>())),
    ] {
        assert_eq!(
            catalogue_digest_with_context(
                &context,
                &build(&descriptor),
                &[],
                &[],
                &record_value_origins(),
                &[],
            ),
            Err(CanonicalHashError::UnsupportedRecordValueFieldType {
                record_value_type: TypeId::from_bytes(id::<42>()),
                field: FieldId::from_bytes(id::<43>()),
                descriptor,
            })
        );
    }

    let enum_type = TypeId::from_bytes(id::<97>());
    let enum_catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<40>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<41>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            enum_type,
            QualifiedSemanticName::new(["crm", "phase"]).unwrap(),
            ["new", "done"],
        )],
        vec![RecordValueTypeDefinition::new(
            TypeId::from_bytes(id::<42>()),
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<43>()),
                    "phase",
                    0,
                    TypeDescriptor::named(enum_type),
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap();
    assert!(validate_record_value_field_types(&context, &enum_catalogue).is_ok());

    let mut enum_encoder = Encoder::new(b"");
    encode_record_value_types(
        &mut enum_encoder,
        &enum_catalogue,
        context.standard().unwrap().catalogue(),
    )
    .unwrap();
    let mut enum_tail = vec![0, 0, 0, 0, 2];
    enum_tail.extend(enum_type.to_bytes());
    assert!(enum_encoder.bytes.ends_with(&enum_tail));

    let collision = standard_boolean_id();
    let collision_catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<40>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<41>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            collision,
            QualifiedSemanticName::new(["crm", "collision"]).unwrap(),
            ["value"],
        )],
        vec![RecordValueTypeDefinition::new(
            TypeId::from_bytes(id::<42>()),
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<43>()),
                    "value",
                    0,
                    TypeDescriptor::named(collision),
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap();
    assert_eq!(
        catalogue_digest_with_context(&context, &collision_catalogue, &[], &[], &[], &[]),
        Err(CanonicalHashError::AmbiguousRecordValueFieldType {
            record_value_type: TypeId::from_bytes(id::<42>()),
            field: FieldId::from_bytes(id::<43>()),
            type_id: collision,
        })
    );
    assert_eq!(
        CanonicalHashError::AmbiguousRecordValueFieldType {
            record_value_type: TypeId::from_bytes(id::<42>()),
            field: FieldId::from_bytes(id::<43>()),
            type_id: collision,
        }
        .to_string(),
        "record field type is present in both application and standard catalogues"
    );
}

#[test]
fn version_two_hash_accepts_record_value_object_and_rows_slots() {
    assert!(
        validate_resolved_type_slots(
            &CatalogueHashContext::version_two(verified_standard_snapshot(false)),
            &catalogue_with_record_value_slot(),
        )
        .is_ok()
    );
}

#[test]
fn version_two_record_origins_are_complete_and_exact_fields_are_reference_targets() {
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let catalogue = catalogue_with_record_value_type();
    let origins = record_value_origins();
    for (missing_index, identity) in [
        (
            1,
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<42>())),
        ),
        (
            2,
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<42>()),
                field: FieldId::from_bytes(id::<43>()),
            },
        ),
    ] {
        let mut incomplete = origins.clone();
        incomplete.remove(missing_index);
        assert_eq!(
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &incomplete, &[],),
            Err(CanonicalHashError::MissingDefinitionOrigin { identity })
        );
    }

    let expressions = HashMap::new();
    assert!(reference_target_exists(
        &catalogue,
        None,
        None,
        &expressions,
        DefinitionReferenceTarget::Field {
            owner: TypeId::from_bytes(id::<42>()),
            field: FieldId::from_bytes(id::<43>()),
        },
    ));
    assert!(!reference_target_exists(
        &catalogue,
        None,
        None,
        &expressions,
        DefinitionReferenceTarget::Field {
            owner: TypeId::from_bytes(id::<42>()),
            field: FieldId::from_bytes(id::<44>()),
        },
    ));
}

#[test]
fn version_two_target_validation_uses_application_and_standard_catalogues() {
    let catalogue = catalogue_with_standard_boolean_slots(&catalogue());
    let context = CatalogueHashContext::version_two(verified_standard_snapshot(false));
    let accepted = value_type_references(standard_boolean_id());
    let accepted_revision = function_revision_v2(&catalogue, &accepted);
    assert!(
        catalogue_digest_with_context(
            &context,
            &catalogue,
            &[accepted_revision],
            &[expression()],
            &origins(),
            &accepted,
        )
        .is_ok()
    );

    let (application_catalogue, application_origins) =
        catalogue_with_application_types_version_two(false);
    let accepted_application = value_type_references(TypeId::from_bytes(id::<30>()));
    let accepted_application_revision =
        function_revision_v2(&application_catalogue, &accepted_application);
    assert!(
        catalogue_digest_with_context(
            &context,
            &application_catalogue,
            &[accepted_application_revision],
            &[expression()],
            &application_origins,
            &accepted_application,
        )
        .is_ok()
    );

    let missing = value_type_references(TypeId::from_bytes(id::<99>()));
    let missing_revision = function_revision_v2(&catalogue, &missing);
    assert!(matches!(
        catalogue_digest_with_context(
            &context,
            &catalogue,
            &[missing_revision],
            &[expression()],
            &origins(),
            &missing,
        ),
        Err(CanonicalHashError::ReferenceTargetNotFound {
            target: DefinitionReferenceTarget::ValueType(_),
        })
    ));
}

#[test]
fn reference_kind_tags_are_append_only() {
    assert_eq!(
        [
            DefinitionReferenceKind::FunctionCall,
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceKind::QueryObject,
            DefinitionReferenceKind::QueryField,
            DefinitionReferenceKind::Expression,
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceKind::WriteField,
        ]
        .map(reference_kind_tag),
        [1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
}

#[test]
fn version_two_identity_and_target_tags_are_append_only() {
    let value_type = TypeId::from_bytes(id::<21>());
    let binding = TypeBindingId::from_bytes(id::<22>());

    let mut value_identity = Encoder::new(&[]);
    value_identity.definition_identity(DefinitionIdentity::ValueType(value_type));
    assert_eq!(
        value_identity.bytes,
        [vec![8], id::<21>().to_vec()].concat()
    );

    let mut binding_identity = Encoder::new(&[]);
    binding_identity.definition_identity(DefinitionIdentity::TypeBinding(binding));
    assert_eq!(
        binding_identity.bytes,
        [vec![9], id::<22>().to_vec()].concat()
    );

    let mut value_target = Encoder::new(&[]);
    value_target.reference_target(DefinitionReferenceTarget::ValueType(value_type));
    assert_eq!(value_target.bytes, [vec![6], id::<21>().to_vec()].concat());
}

#[test]
fn source_bundle_hash_is_order_independent_but_binds_unit_identity() {
    let first = source_unit(SourceUnitId::from_bytes(id::<10>()), 0, "a.orna", "one");
    let second = source_unit(SourceUnitId::from_bytes(id::<11>()), 1, "b.orna", "two");
    let forward = source_bundle_digest(&[first.clone(), second.clone()]).unwrap();
    let reversed = source_bundle_digest(&[second.clone(), first.clone()]).unwrap();
    assert_eq!(forward, reversed);
    assert_eq!(
        hex(forward),
        "93ea9176309da4a0f0cbba83ae488909da109a054a1a4d8a13528c64e7c935be"
    );

    let changed_identity = source_unit(SourceUnitId::from_bytes(id::<12>()), 0, "a.orna", "one");
    assert_ne!(
        forward,
        source_bundle_digest(&[changed_identity, second]).unwrap()
    );
}

#[test]
fn source_bundle_hash_rejects_invalid_raw_bundle_order_and_identity() {
    let gapped = source_unit(SourceUnitId::from_bytes(id::<10>()), 1, "a.orna", "one");
    assert!(matches!(
        source_bundle_digest(&[gapped]),
        Err(CanonicalHashError::SourceOrdinalOutOfSequence { .. })
    ));

    let first = source_unit(SourceUnitId::from_bytes(id::<10>()), 0, "a.orna", "one");
    let duplicate_id = source_unit(SourceUnitId::from_bytes(id::<10>()), 1, "b.orna", "two");
    assert!(matches!(
        source_bundle_digest(&[first, duplicate_id]),
        Err(CanonicalHashError::DuplicateSourceUnitId { .. })
    ));
}

#[test]
fn source_revision_hash_verifies_retained_aggregate_hash() {
    let unit = source_unit(SourceUnitId::from_bytes(id::<10>()), 0, "a.orna", "one");
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let revision = StoredSourceRevision::new(
        SourceBundleId::from_bytes(id::<11>()),
        SourceRevisionId::from_bytes(id::<12>()),
        None,
        vec![unit],
        bundle_hash,
        digest_bytes(b"unused"),
    )
    .unwrap();
    let revision_hash = source_revision_digest(&revision).unwrap();
    assert_eq!(
        hex(revision_hash),
        "9f5d7a0e8c949c255d5217371afc1d57caf23ac7e17ab411b8df0eef81bc7776"
    );

    let inconsistent = StoredSourceRevision::new(
        revision.bundle(),
        revision.id(),
        revision.parent(),
        revision.units().to_vec(),
        digest_bytes(b"incorrect"),
        revision.revision_hash(),
    )
    .unwrap();
    assert!(matches!(
        source_revision_digest(&inconsistent),
        Err(CanonicalHashError::SourceBundleHashMismatch { .. })
    ));
}

#[test]
fn semantic_and_catalogue_hashes_are_order_independent() {
    let catalogue = catalogue();
    let function = catalogue.functions().first().unwrap();
    let base_references = references();
    let semantic = function_semantic_digest(
        function,
        "orna-1",
        &artifact(),
        &[expression()],
        &base_references,
    )
    .unwrap();
    let revision = function_revision(&catalogue);
    let catalogue_hash = catalogue_digest(
        &catalogue,
        std::slice::from_ref(&revision),
        &[expression()],
        &origins(),
        &base_references,
    )
    .unwrap();
    assert_eq!(
        hex(semantic),
        "f9e9cd170e5e37de3399fc80e1164bf3d51697dd3ec6a1c242976c3a21aa9f49"
    );
    assert_eq!(
        hex(catalogue_hash),
        "83f35f7e18618aa8608ac8cc58e35256bde1e9eb9c9ea56b82c7a5c3a83181f3"
    );

    let mut reversed_origins = origins();
    reversed_origins.reverse();
    let mut reversed_references = base_references.clone();
    reversed_references.reverse();
    assert_eq!(
        semantic,
        function_semantic_digest(
            function,
            "orna-1",
            &artifact(),
            &[expression()],
            &reversed_references,
        )
        .unwrap()
    );
    assert_eq!(
        catalogue_hash,
        catalogue_digest(
            &catalogue,
            &[revision],
            &[expression()],
            &reversed_origins,
            &reversed_references,
        )
        .unwrap()
    );
}

#[test]
fn write_reference_hashes_have_stable_tags_and_owner_qualified_targets() {
    let catalogue = catalogue();
    let function = catalogue.functions().first().unwrap();
    let references = write_references();
    let semantic = function_semantic_digest(
        function,
        "orna-1",
        &artifact(),
        &[expression()],
        &references,
    )
    .unwrap();
    let revision = function_revision_with_references(&catalogue, &references);
    let catalogue_hash = catalogue_digest(
        &catalogue,
        std::slice::from_ref(&revision),
        &[expression()],
        &origins(),
        &references,
    )
    .unwrap();
    assert_eq!(
        hex(semantic),
        "e634a9f3e1b93ebb33cd4022c3521b1c241533924f3c628021138183b227cdae"
    );
    assert_eq!(
        hex(catalogue_hash),
        "249e26f6a1cf9f4ff6bfa0d253bb8c317aab3424ada0ddd52de23cbd061da0a9"
    );
}

#[test]
fn catalogue_hash_requires_exact_current_function_revision_coverage() {
    let catalogue = catalogue();
    assert!(matches!(
        catalogue_digest(&catalogue, &[], &[expression()], &origins(), &references()),
        Err(CanonicalHashError::MissingCurrentFunctionRevision { .. })
    ));
}

#[test]
fn catalogue_hash_rejects_incompatible_reference_kind_and_target() {
    let catalogue = catalogue();
    for (kind, target) in [
        (
            DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::Field {
                owner: TypeId::from_bytes(id::<2>()),
                field: FieldId::from_bytes(id::<3>()),
            },
        ),
        (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::Field {
                owner: TypeId::from_bytes(id::<2>()),
                field: FieldId::from_bytes(id::<3>()),
            },
        ),
        (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::ObjectType(TypeId::from_bytes(id::<2>())),
        ),
        (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::Function(FunctionId::from_bytes(id::<4>())),
        ),
        (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Expression(ExpressionId::from_bytes(id::<8>())),
        ),
    ] {
        let incompatible = DefinitionReference::new(
            FunctionId::from_bytes(id::<4>()),
            FunctionRevisionId::from_bytes(id::<6>()),
            0,
            target,
            kind,
            SourceOrigin::new(SourceUnitId::from_bytes(id::<10>()), 10, 18).unwrap(),
        );
        assert!(matches!(
            catalogue_digest(
                &catalogue,
                &[function_revision(&catalogue)],
                &[expression()],
                &origins(),
                &[incompatible],
            ),
            Err(CanonicalHashError::ReferenceKindTargetMismatch { .. })
        ));
    }
}

#[test]
fn catalogue_hash_accepts_the_historical_origin_of_a_reused_function_revision() {
    let catalogue = catalogue();
    let current = function_revision(&catalogue);
    let reused = FunctionRevisionRecord::new(
        current.function(),
        current.id(),
        current.revision_number(),
        SourceOrigin::new(SourceUnitId::from_bytes(id::<99>()), 4, 20).unwrap(),
        current.declaration_content_hash(),
        current.semantic_hash(),
        current.language_version(),
        current.artifact().clone(),
    )
    .unwrap();
    assert!(
        catalogue_digest(
            &catalogue,
            &[reused],
            &[expression()],
            &origins(),
            &references(),
        )
        .is_ok()
    );
}
