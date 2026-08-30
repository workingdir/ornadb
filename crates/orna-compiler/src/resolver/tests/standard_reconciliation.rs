use super::*;

#[test]
fn standard_reconciliation_accepts_reordered_declarations_and_catalogue_facts() {
    let (stored_unit, parsed_unit, catalogue, origins) =
        two_type_reconciliation_inputs(TWO_TYPE_STANDARD_SOURCE);

    let families =
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins).unwrap();
    let schemas = families.schemas;
    let value_types = families.value_types;
    let bindings = families.type_bindings;

    assert_eq!(schemas[0].name().to_string(), "std.types");
    assert_eq!(schemas[1].name().to_string(), "std");
    assert_eq!(value_types[0].name().to_string(), "std.types.integer");
    assert_eq!(value_types[1].name().to_string(), "std.types.boolean");
    assert_eq!(bindings[0].name().to_string(), "integer");
    assert_eq!(bindings[1].name().to_string(), "std.integer");
    assert_eq!(bindings[2].name().to_string(), "boolean");
    assert_eq!(bindings[3].name().to_string(), "std.boolean");
}

#[test]
fn standard_reconciliation_binds_one_exact_opaque_definition_and_origin() {
    let source = "CREATE SCHEMA std;CREATE TYPE std.TOKEN AS VALUE OPAQUE KERNEL CONTRACT 'std.token@1' IMMUTABLE TRANSIENT;";
    let (stored_unit, parsed_unit, catalogue, origins) = opaque_standard_reconciliation_inputs(
        source,
        QualifiedSemanticName::new(["std", "token"]).unwrap(),
        "std.token@1",
    );

    let families =
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins).unwrap();
    assert_eq!(families.value_types.len(), 1);
    let opaque = &families.value_types[0];
    assert_eq!(opaque.id(), TypeId::from_bytes([3; 16]));
    assert_eq!(opaque.name().to_string(), "std.token");
    assert_eq!(opaque.kind(), ValueTypeKind::Opaque);
    assert_eq!(opaque.mutability(), ValueTypeMutability::Immutable);
    assert_eq!(opaque.persistence(), ValueTypePersistence::Transient);
    assert_eq!(opaque.representation_contract(), "std.token@1");
    assert_eq!(opaque.origin(), origins[1].source());

    for contract in ["", &"a".repeat(129), "line\nbreak", "\u{7f}"] {
        assert!(!super::super::opaque_contract_is_valid(contract));
    }
    for contract in ["a", "std.token@1", "!~"] {
        assert!(super::super::opaque_contract_is_valid(contract));
    }
}

#[test]
fn standard_reconciliation_keeps_primitive_and_opaque_kinds_distinct() {
    let opaque_source = "CREATE SCHEMA std;CREATE TYPE std.TOKEN AS VALUE OPAQUE KERNEL CONTRACT 'std.token@1' IMMUTABLE TRANSIENT;";
    let (stored_unit, parsed_unit, mut catalogue, origins) = opaque_standard_reconciliation_inputs(
        opaque_source,
        QualifiedSemanticName::new(["std", "token"]).unwrap(),
        "std.token@1",
    );
    catalogue = CatalogueSnapshot::new_with_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![],
        vec![ValueTypeDefinition::primitive(
            TypeId::from_bytes([3; 16]),
            QualifiedSemanticName::new(["std", "token"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Transient,
            "std.token@1",
        )],
        vec![],
    )
    .unwrap();

    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );
}

#[test]
fn standard_reconciliation_rejects_crossed_and_duplicate_type_and_binding_facts() {
    let crossed_cases = [
            TWO_TYPE_STANDARD_SOURCE.replacen(
                "CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'int@1' IMMUTABLE TRANSIENT;",
                "CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'boolean@1' IMMUTABLE PERSISTABLE;",
                1,
            ),
            TWO_TYPE_STANDARD_SOURCE.replacen(
                "EXPORT TYPE std.types.INTEGER AS std.INTEGER;",
                "EXPORT TYPE std.types.BOOLEAN AS std.INTEGER;",
                1,
            ),
            TWO_TYPE_STANDARD_SOURCE.replacen(
                "EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;",
                "EXPORT TYPE std.BOOLEAN TO PRELUDE AS INTEGER;",
                1,
            ),
        ];

    for source in crossed_cases {
        let (stored_unit, parsed_unit, catalogue, origins) =
            two_type_reconciliation_inputs(&source);
        assert_eq!(
            reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
            Err(super::super::StandardLibraryCheckError::SourceMismatch)
        );
    }

    let duplicate_primitive = TWO_TYPE_STANDARD_SOURCE.replacen(
            "CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'int@1' IMMUTABLE TRANSIENT;",
            "CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'boolean@1' IMMUTABLE PERSISTABLE;",
            1,
        );
    let (stored_unit, parsed_unit, catalogue, mut origins) =
        two_type_reconciliation_inputs(&duplicate_primitive);
    replace_origin(
        &mut origins,
        DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
        &parsed_unit.parsed().primitive_value_types()[0].span,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let duplicate_qualified = TWO_TYPE_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.types.INTEGER AS std.INTEGER;",
        "EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;",
        1,
    );
    let (stored_unit, parsed_unit, catalogue, mut origins) =
        two_type_reconciliation_inputs(&duplicate_qualified);
    let qualified_boolean = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        ))
        .unwrap()
        .id();
    let first_qualified = parsed_unit
        .parsed()
        .type_exports()
        .iter()
        .find(|declaration| {
            matches!(
                &declaration.target,
                orna_syntax::TypeExportTarget::Qualified { .. }
            )
        })
        .unwrap();
    replace_origin(
        &mut origins,
        DefinitionIdentity::TypeBinding(qualified_boolean),
        &first_qualified.span,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let duplicate_prelude = TWO_TYPE_STANDARD_SOURCE.replacen(
        "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
        "EXPORT TYPE std.INTEGER TO PRELUDE AS INTEGER;",
        1,
    );
    let (stored_unit, parsed_unit, catalogue, mut origins) =
        two_type_reconciliation_inputs(&duplicate_prelude);
    let prelude_integer = catalogue
        .type_binding_by_name(&TypeLookupName::prelude(
            PreludeTypeName::new(["integer"]).unwrap(),
        ))
        .unwrap()
        .id();
    let first_prelude = parsed_unit
        .parsed()
        .type_exports()
        .iter()
        .find(|declaration| {
            matches!(
                &declaration.target,
                orna_syntax::TypeExportTarget::Prelude { .. }
            )
        })
        .unwrap();
    replace_origin(
        &mut origins,
        DefinitionIdentity::TypeBinding(prelude_integer),
        &first_prelude.span,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );
}

fn replace_origin(
    origins: &mut [DefinitionOrigin],
    identity: DefinitionIdentity,
    span: &SourceSpan,
) {
    let origin = origins
        .iter_mut()
        .find(|origin| origin.identity() == identity)
        .unwrap();
    *origin = parsed_origin(identity, span);
}

#[test]
fn standard_reconciliation_rejects_missing_and_unsupported_declarations() {
    assert_standard_source_mismatch(
        "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;",
    );
    assert_standard_source_mismatch(
        "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;CREATE TYPE std.extra AS OBJECT ();",
    );
}

#[test]
fn standard_reconciliation_rejects_duplicate_and_crossed_source_facts() {
    assert_standard_source_mismatch(
        "CREATE SCHEMA std;CREATE SCHEMA std;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;",
    );
    assert_standard_source_mismatch(
        "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.types.BOOLEAN TO PRELUDE AS BOOLEAN;",
    );
}

#[test]
fn standard_reconciliation_rejects_quoted_and_changed_primitive_facts() {
    let cases = [
        STANDARD_SOURCE.replacen("CREATE SCHEMA std;", "CREATE SCHEMA \"std\";", 1),
        STANDARD_SOURCE.replacen(
            "CREATE TYPE std.types.BOOLEAN",
            "CREATE TYPE \"std\".types.BOOLEAN",
            1,
        ),
        STANDARD_SOURCE.replacen("AS std.BOOLEAN", "AS \"std\".BOOLEAN", 1),
        STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.types.BOOLEAN",
            "EXPORT TYPE \"std\".types.BOOLEAN",
            1,
        ),
        STANDARD_SOURCE.replacen(
            "EXPORT TYPE std.BOOLEAN TO PRELUDE",
            "EXPORT TYPE \"std\".BOOLEAN TO PRELUDE",
            1,
        ),
        STANDARD_SOURCE.replacen("boolean@1", "boolean@2", 1),
        STANDARD_SOURCE.replacen("PERSISTABLE", "TRANSIENT", 1),
    ];

    for source in cases {
        let (stored_unit, parsed_unit, catalogue, mut origins) =
            standard_reconciliation_inputs(&source);
        rebase_standard_origins_to_source(&mut origins, &parsed_unit);
        assert_eq!(
            reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
            Err(super::super::StandardLibraryCheckError::SourceMismatch)
        );
    }
}

#[test]
fn quoted_prelude_words_are_rejected_by_the_parse_gate() {
    let report = parse_bundle(
        &SourceBundle::new([SourceUnit::new(
            "std/types.orna",
            "EXPORT TYPE std.BOOLEAN TO PRELUDE AS \"BOOLEAN\";",
        )])
        .unwrap(),
    );

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnexpectedToken
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "expected an unquoted prelude type name after AS"
    );
}

#[test]
fn standard_reconciliation_rejects_every_missing_or_extra_supported_family() {
    let cases = [
            "CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;".to_owned(),
            format!("CREATE SCHEMA std.extra;{STANDARD_SOURCE}"),
            "CREATE SCHEMA std;CREATE SCHEMA std.types;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;".to_owned(),
            format!("{STANDARD_SOURCE}CREATE TYPE std.types.EXTRA AS VALUE PRIMITIVE KERNEL CONTRACT 'extra@1' IMMUTABLE PERSISTABLE;"),
            "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;".to_owned(),
            format!("{STANDARD_SOURCE}EXPORT TYPE std.types.BOOLEAN AS std.BOOL;"),
            "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;".to_owned(),
            format!("{STANDARD_SOURCE}EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;"),
        ];

    for source in cases {
        assert_standard_source_mismatch(&source);
    }
}

#[test]
fn standard_reconciliation_rejects_every_unsupported_source_category() {
    let cases = [
        format!("{STANDARD_SOURCE}CREATE TYPE std.extra AS OBJECT ();"),
        format!("{STANDARD_SOURCE}ALTER TYPE std.extra RENAME FIELD old TO new;"),
        format!(
            "{STANDARD_SOURCE}CREATE SERVER FUNCTION std.extra() RETURNS ROWS (value BOOLEAN) AS SELECT o.value FROM std.object o;"
        ),
        format!("{STANDARD_SOURCE}CREATE CLIENT FUNCTION std.extra() RETURNS BOOLEAN RETURN TRUE;"),
    ];

    for source in cases {
        assert_standard_source_mismatch(&source);
    }
}

#[test]
fn standard_reconciliation_requires_exact_stored_bytes_and_origins() {
    let (stored_unit, mut parsed_unit, catalogue, origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    assert_eq!(parsed_unit.parsed().schemas().len(), 2);
    assert_eq!(parsed_unit.parsed().primitive_value_types().len(), 1);
    assert_eq!(parsed_unit.parsed().type_exports().len(), 2);
    parsed_unit.replace_source_text_for_test(format!("{STANDARD_SOURCE} "));
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins.remove(0);
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins[2] = standard_origin(
        DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
        43,
        159,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins.push(origins[0].clone());
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins.push(standard_origin(
        DefinitionIdentity::Expression(ExpressionId::from_bytes([9; 16])),
        0,
        0,
    ));
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins[2] = standard_origin(
        DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
        42,
        158,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    let first_source = origins[0].source();
    let second_source = origins[1].source();
    origins[0] = DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
        second_source,
    );
    origins[1] = DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
        first_source,
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins[0] = DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
        SourceOrigin::new(SourceUnitId::from_bytes([9; 16]), 0, 18).unwrap(),
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );
}

#[test]
fn catalogue_reconciliation_precedes_hostile_origin_validation() {
    let source = STANDARD_SOURCE.replace("boolean@1", "integer@1");
    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(&source);
    origins.push(origins[0].clone());

    assert_eq!(
        super::super::match_standard_source_facts(&parsed_unit, &catalogue),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );
    assert_eq!(
        reconcile_standard_source(&stored_unit, &parsed_unit, &catalogue, &origins),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );

    let (stored_unit, parsed_unit, catalogue, mut origins) =
        standard_reconciliation_inputs(STANDARD_SOURCE);
    origins.push(origins[0].clone());
    let pending = super::super::match_standard_source_facts(&parsed_unit, &catalogue);
    assert!(pending.is_ok());
    let Ok(pending) = pending else {
        return;
    };
    assert_eq!(
        super::super::validate_standard_source_origins(&stored_unit, &origins, pending),
        Err(super::super::StandardLibraryCheckError::SourceMismatch)
    );
}

pub(super) fn assert_no_checked_bundle(report: &super::super::CheckReport) {
    assert!(!report.diagnostics().is_empty());
    assert!(report.checked_bundle().is_none());
}
