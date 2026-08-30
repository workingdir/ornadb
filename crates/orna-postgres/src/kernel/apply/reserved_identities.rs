use super::*;

/// One durable application identity that would collide with the executable
/// standard snapshot. The compiler identity vocabulary ends at the catalogue
/// type families; the kernel scan extends the disjointness check to the V2
/// executable function, parameter, and function-revision identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StandardExecutableIdentity {
    /// A standard catalogue function identity.
    Function(FunctionId),
    /// A pinned standard function-revision identity.
    FunctionRevision(FunctionRevisionId),
}

/// One durable application parameter identity, scoped by its owning function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StandardExecutableParameter {
    /// The owning standard catalogue function identity.
    pub(super) function: FunctionId,
    /// The parameter identity owned by that function.
    pub(super) parameter: ParameterId,
}

#[derive(Default)]
struct ReservedIdentityLists {
    standard_library_revisions: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    catalogue_revisions: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    source_bundles: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    source_revisions: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    source_units: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    schemas: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    types: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    type_bindings: Vec<(StandardUpgradeIdentity, Vec<u8>)>,
    functions: Vec<(StandardExecutableIdentity, Vec<u8>)>,
    function_revisions: Vec<(StandardExecutableIdentity, Vec<u8>)>,
    parameters: Vec<StandardExecutableParameter>,
}

impl ReservedIdentityLists {
    const fn classes(&self) -> [&Vec<(StandardUpgradeIdentity, Vec<u8>)>; 8] {
        [
            &self.standard_library_revisions,
            &self.catalogue_revisions,
            &self.source_bundles,
            &self.source_revisions,
            &self.source_units,
            &self.schemas,
            &self.types,
            &self.type_bindings,
        ]
    }
}

fn upgrade_reserved_identities(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> ReservedIdentityLists {
    let catalogue = snapshot.catalogue();
    let source = snapshot.source();
    let mut identities = ReservedIdentityLists::default();
    identities.standard_library_revisions.push((
        StandardUpgradeIdentity::StandardLibraryRevision(snapshot.revision()),
        bytes(snapshot.revision()),
    ));
    identities.catalogue_revisions.push((
        StandardUpgradeIdentity::CatalogueRevision(catalogue.revision()),
        bytes(catalogue.revision()),
    ));
    identities.source_bundles.push((
        StandardUpgradeIdentity::SourceBundle(source.bundle()),
        bytes(source.bundle()),
    ));
    identities.source_revisions.push((
        StandardUpgradeIdentity::SourceRevision(source.id()),
        bytes(source.id()),
    ));
    for unit in source.units() {
        identities.source_units.push((
            StandardUpgradeIdentity::SourceUnit(unit.id()),
            bytes(unit.id()),
        ));
    }
    for schema in catalogue.schemas() {
        identities.schemas.push((
            StandardUpgradeIdentity::Schema(schema.id()),
            bytes(schema.id()),
        ));
    }
    for value_type in catalogue.value_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(value_type.id()),
            bytes(value_type.id()),
        ));
    }
    for enum_type in catalogue.enum_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(enum_type.id()),
            bytes(enum_type.id()),
        ));
    }
    for binding in catalogue.type_bindings() {
        identities.type_bindings.push((
            StandardUpgradeIdentity::TypeBinding(binding.id()),
            bytes(binding.id()),
        ));
    }
    for function in catalogue.functions() {
        identities.functions.push((
            StandardExecutableIdentity::Function(function.id()),
            bytes(function.id()),
        ));
        for parameter in function.parameters() {
            identities.parameters.push(StandardExecutableParameter {
                function: function.id(),
                parameter: parameter.id(),
            });
        }
    }
    for executable in snapshot.executables() {
        identities.function_revisions.push((
            StandardExecutableIdentity::FunctionRevision(executable.revision().id()),
            bytes(executable.revision().id()),
        ));
    }
    identities
}

fn active_visible_reserved_identities(
    active: &ActiveDatabaseRevision,
    include_standard: bool,
) -> ReservedIdentityLists {
    let mut identities = ReservedIdentityLists::default();
    let source = active.source();
    let catalogue = active.catalogue();
    identities.catalogue_revisions.push((
        StandardUpgradeIdentity::CatalogueRevision(catalogue.revision()),
        bytes(catalogue.revision()),
    ));
    identities.source_bundles.push((
        StandardUpgradeIdentity::SourceBundle(source.bundle()),
        bytes(source.bundle()),
    ));
    identities.source_revisions.push((
        StandardUpgradeIdentity::SourceRevision(source.id()),
        bytes(source.id()),
    ));
    for unit in source.units() {
        identities.source_units.push((
            StandardUpgradeIdentity::SourceUnit(unit.id()),
            bytes(unit.id()),
        ));
    }
    append_catalogue_reserved_identities(catalogue, &mut identities);
    append_application_executable_reserved_identities(active, &mut identities);
    if include_standard && let Some(standard) = active.catalogue_hash_context().standard() {
        let source = standard.source();
        let catalogue = standard.catalogue();
        identities.standard_library_revisions.push((
            StandardUpgradeIdentity::StandardLibraryRevision(standard.revision()),
            bytes(standard.revision()),
        ));
        identities.catalogue_revisions.push((
            StandardUpgradeIdentity::CatalogueRevision(catalogue.revision()),
            bytes(catalogue.revision()),
        ));
        identities.source_bundles.push((
            StandardUpgradeIdentity::SourceBundle(source.bundle()),
            bytes(source.bundle()),
        ));
        identities.source_revisions.push((
            StandardUpgradeIdentity::SourceRevision(source.id()),
            bytes(source.id()),
        ));
        for unit in source.units() {
            identities.source_units.push((
                StandardUpgradeIdentity::SourceUnit(unit.id()),
                bytes(unit.id()),
            ));
        }
        append_catalogue_reserved_identities(catalogue, &mut identities);
        append_standard_executable_reserved_identities(standard, &mut identities);
    }
    identities
}

fn append_application_executable_reserved_identities(
    active: &ActiveDatabaseRevision,
    identities: &mut ReservedIdentityLists,
) {
    for function in active.catalogue().functions() {
        identities.functions.push((
            StandardExecutableIdentity::Function(function.id()),
            bytes(function.id()),
        ));
        for parameter in function.parameters() {
            identities.parameters.push(StandardExecutableParameter {
                function: function.id(),
                parameter: parameter.id(),
            });
        }
    }
    for revision in active.function_revisions() {
        identities.function_revisions.push((
            StandardExecutableIdentity::FunctionRevision(revision.id()),
            bytes(revision.id()),
        ));
    }
    for revision in active.historical_function_revisions() {
        identities.function_revisions.push((
            StandardExecutableIdentity::FunctionRevision(revision.id()),
            bytes(revision.id()),
        ));
    }
}

fn append_standard_executable_reserved_identities(
    standard: &VerifiedStandardLibrarySnapshot,
    identities: &mut ReservedIdentityLists,
) {
    for function in standard.catalogue().functions() {
        identities.functions.push((
            StandardExecutableIdentity::Function(function.id()),
            bytes(function.id()),
        ));
        for parameter in function.parameters() {
            identities.parameters.push(StandardExecutableParameter {
                function: function.id(),
                parameter: parameter.id(),
            });
        }
    }
    for executable in standard.executables() {
        identities.function_revisions.push((
            StandardExecutableIdentity::FunctionRevision(executable.revision().id()),
            bytes(executable.revision().id()),
        ));
    }
}

fn append_catalogue_reserved_identities(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    identities: &mut ReservedIdentityLists,
) {
    for schema in catalogue.schemas() {
        identities.schemas.push((
            StandardUpgradeIdentity::Schema(schema.id()),
            bytes(schema.id()),
        ));
    }
    for object_type in catalogue.object_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(object_type.id()),
            bytes(object_type.id()),
        ));
    }
    for value_type in catalogue.value_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(value_type.id()),
            bytes(value_type.id()),
        ));
    }
    for enum_type in catalogue.enum_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(enum_type.id()),
            bytes(enum_type.id()),
        ));
    }
    for record_type in catalogue.record_value_types() {
        identities.types.push((
            StandardUpgradeIdentity::Type(record_type.id()),
            bytes(record_type.id()),
        ));
    }
    for binding in catalogue.type_bindings() {
        identities.type_bindings.push((
            StandardUpgradeIdentity::TypeBinding(binding.id()),
            bytes(binding.id()),
        ));
    }
}

pub(super) async fn scan_reserved_standard_identities(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> Result<(), PostgresKernelError> {
    let upgrade = upgrade_reserved_identities(standard);
    // The in-memory collision check considers only the active revision's own
    // application identities: the pinned standard is the append-only parent
    // edge (work ADR 0059), so its reserved identities legitimately overlap
    // the upgrade's retained parent units. The database scan below still
    // excludes those already-installed parent rows from its collision check.
    let active_own = active_visible_reserved_identities(active, false);
    let active = active_visible_reserved_identities(active, true);
    let queries = [
        "SELECT id AS identity FROM _orna_kernel.standard_library_revisions
         WHERE id = ANY($1) AND NOT (id = ANY($2)) ORDER BY id LIMIT 1",
        "SELECT identity FROM (
             SELECT id AS identity FROM _orna_kernel.catalogue_revisions
             UNION
             SELECT catalogue_revision_id AS identity FROM _orna_kernel.standard_library_revisions
         ) AS identities
         WHERE identity = ANY($1) AND NOT (identity = ANY($2)) ORDER BY identity LIMIT 1",
        "SELECT id AS identity FROM _orna_kernel.source_bundles
         WHERE id = ANY($1) AND NOT (id = ANY($2)) ORDER BY id LIMIT 1",
        "SELECT id AS identity FROM _orna_kernel.source_revisions
         WHERE id = ANY($1) AND NOT (id = ANY($2)) ORDER BY id LIMIT 1",
        "SELECT id AS identity FROM _orna_kernel.source_units
         WHERE id = ANY($1) AND NOT (id = ANY($2)) ORDER BY id LIMIT 1",
        "SELECT identity FROM (
             SELECT schema_id AS identity FROM _orna_kernel.catalogue_schemas
             UNION
             SELECT schema_id AS identity FROM _orna_kernel.standard_catalogue_schemas
         ) AS identities
         WHERE identity = ANY($1) AND NOT (identity = ANY($2)) ORDER BY identity LIMIT 1",
        "SELECT identity FROM (
             SELECT type_id AS identity FROM _orna_kernel.catalogue_object_types
             UNION
             SELECT type_id AS identity FROM _orna_kernel.catalogue_enum_types
             UNION
             SELECT type_id AS identity FROM _orna_kernel.catalogue_record_value_types
             UNION
             SELECT type_id AS identity FROM _orna_kernel.standard_catalogue_value_types
             UNION
             SELECT type_id AS identity FROM _orna_kernel.standard_catalogue_enum_types
         ) AS identities
         WHERE identity = ANY($1) AND NOT (identity = ANY($2)) ORDER BY identity LIMIT 1",
        "SELECT type_binding_id AS identity FROM _orna_kernel.standard_catalogue_type_bindings
         WHERE type_binding_id = ANY($1) AND NOT (type_binding_id = ANY($2))
         ORDER BY type_binding_id LIMIT 1",
    ];
    for (((upgrade_class, active_own_class), active_class), query) in upgrade
        .classes()
        .into_iter()
        .zip(active_own.classes())
        .zip(active.classes())
        .zip(queries)
    {
        if let Some(identity) = first_active_reserved_identity(active_own_class, upgrade_class) {
            return Err(PostgresKernelError::ReservedStandardIdentity { identity });
        }
        let requested = upgrade_class
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        if requested.is_empty() {
            continue;
        }
        let excluded = active_class
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        let rows = transaction
            .query(query, &[&requested, &excluded])
            .await
            .map_err(PostgresKernelError::Database)?;
        if let Some(row) = rows.first() {
            let identity: Vec<u8> = row
                .try_get("identity")
                .map_err(PostgresKernelError::Database)?;
            let Some(reserved) = first_inactive_reserved_identity(upgrade_class, &[identity])
            else {
                return Err(invariant(
                    "reserved standard identity query must return one requested identity",
                ));
            };
            return Err(PostgresKernelError::ReservedStandardIdentity { identity: reserved });
        }
    }

    if let Some(identity) =
        first_active_standard_executable_identity(&active_own.functions, &upgrade.functions)
    {
        return Err(standard_executable_reserved(identity));
    }
    let requested = upgrade
        .functions
        .iter()
        .map(|(_, bytes)| bytes.clone())
        .collect::<Vec<_>>();
    if !requested.is_empty() {
        let excluded = active
            .functions
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        let rows = transaction
            .query(
                "SELECT function_id AS identity FROM _orna_kernel.catalogue_functions
                 WHERE function_id = ANY($1) AND NOT (function_id = ANY($2))
                 ORDER BY function_id LIMIT 1",
                &[&requested, &excluded],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if let Some(row) = rows.first() {
            let identity: Vec<u8> = row
                .try_get("identity")
                .map_err(PostgresKernelError::Database)?;
            let Some(reserved) =
                first_inactive_standard_executable_identity(&upgrade.functions, &[identity])
            else {
                return Err(invariant(
                    "reserved standard function identity query must return one requested identity",
                ));
            };
            return Err(standard_executable_reserved(reserved));
        }
    }
    for function in standard.catalogue().functions() {
        let name = function.name().parts().to_vec();
        let rows = transaction
            .query(
                "SELECT function_id AS identity FROM _orna_kernel.catalogue_functions
                 WHERE name_parts = $1 ORDER BY function_id LIMIT 1",
                &[&name],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if !rows.is_empty() {
            return Err(standard_executable_name_reserved(function.name()));
        }
    }

    if let Some(identity) = first_active_standard_executable_identity(
        &active_own.function_revisions,
        &upgrade.function_revisions,
    ) {
        return Err(standard_executable_reserved(identity));
    }
    let requested = upgrade
        .function_revisions
        .iter()
        .map(|(_, bytes)| bytes.clone())
        .collect::<Vec<_>>();
    if !requested.is_empty() {
        let excluded = active
            .function_revisions
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        let rows = transaction
            .query(
                "SELECT id AS identity FROM _orna_kernel.function_revisions
                 WHERE id = ANY($1) AND NOT (id = ANY($2))
                 ORDER BY id LIMIT 1",
                &[&requested, &excluded],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if let Some(row) = rows.first() {
            let identity: Vec<u8> = row
                .try_get("identity")
                .map_err(PostgresKernelError::Database)?;
            let Some(reserved) = first_inactive_standard_executable_identity(
                &upgrade.function_revisions,
                &[identity],
            ) else {
                return Err(invariant(
                    "reserved standard function revision query must return one requested identity",
                ));
            };
            return Err(standard_executable_reserved(reserved));
        }
    }

    if let Some(parameter) =
        first_active_standard_parameter(&active_own.parameters, &upgrade.parameters)
    {
        return Err(standard_executable_parameter_reserved(parameter));
    }
    let parameter_functions = upgrade
        .parameters
        .iter()
        .map(|parameter| bytes(parameter.function))
        .collect::<Vec<_>>();
    let parameter_ids = upgrade
        .parameters
        .iter()
        .map(|parameter| bytes(parameter.parameter))
        .collect::<Vec<_>>();
    if !parameter_functions.is_empty() {
        let rows = transaction
            .query(
                "SELECT 1 FROM _orna_kernel.catalogue_function_parameters AS parameter
                 JOIN unnest($1::bytea[], $2::bytea[])
                   AS wanted(function_id, parameter_id)
                   ON parameter.function_id = wanted.function_id
                  AND parameter.parameter_id = wanted.parameter_id
                 LIMIT 1",
                &[&parameter_functions, &parameter_ids],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if !rows.is_empty() {
            return Err(standard_executable_parameter_reserved(
                upgrade.parameters[0],
            ));
        }
    }
    Ok(())
}

fn standard_executable_reserved(identity: StandardExecutableIdentity) -> PostgresKernelError {
    let (relation, rule) = match identity {
        StandardExecutableIdentity::Function(_) => (
            "_orna_kernel.catalogue_functions",
            "application catalogue functions must not reuse a standard executable function identity",
        ),
        StandardExecutableIdentity::FunctionRevision(_) => (
            "_orna_kernel.function_revisions",
            "application function revisions must not reuse a standard executable revision identity",
        ),
    };
    PostgresKernelError::DurableInvariant {
        relation,
        record: format!("{identity:?}"),
        rule,
    }
}

fn standard_executable_name_reserved(name: &QualifiedSemanticName) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.catalogue_functions",
        record: name.parts().join("."),
        rule: "application catalogue functions must not reuse a standard executable function name",
    }
}

fn standard_executable_parameter_reserved(
    parameter: StandardExecutableParameter,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.catalogue_function_parameters",
        record: format!("{:?}", parameter.parameter),
        rule: "application catalogue parameters must not reuse a standard executable parameter identity within its owning function",
    }
}

pub(super) fn first_active_standard_executable_identity(
    active: &[(StandardExecutableIdentity, Vec<u8>)],
    upgrade: &[(StandardExecutableIdentity, Vec<u8>)],
) -> Option<StandardExecutableIdentity> {
    active
        .iter()
        .find(|(_, bytes)| upgrade.iter().any(|(_, wanted)| wanted == bytes))
        .map(|(identity, _)| *identity)
}

pub(super) fn first_inactive_standard_executable_identity(
    upgrade: &[(StandardExecutableIdentity, Vec<u8>)],
    inactive_raw_order: &[Vec<u8>],
) -> Option<StandardExecutableIdentity> {
    inactive_raw_order.iter().find_map(|identity| {
        upgrade
            .iter()
            .find(|(_, wanted)| wanted == identity)
            .map(|(reserved, _)| *reserved)
    })
}

pub(super) fn first_active_standard_parameter(
    active: &[StandardExecutableParameter],
    upgrade: &[StandardExecutableParameter],
) -> Option<StandardExecutableParameter> {
    active
        .iter()
        .find(|parameter| upgrade.contains(parameter))
        .copied()
}

pub(super) fn first_active_reserved_identity(
    active: &[(StandardUpgradeIdentity, Vec<u8>)],
    upgrade: &[(StandardUpgradeIdentity, Vec<u8>)],
) -> Option<StandardUpgradeIdentity> {
    active
        .iter()
        .find(|(_, bytes)| upgrade.iter().any(|(_, wanted)| wanted == bytes))
        .map(|(identity, _)| *identity)
}

pub(super) fn first_inactive_reserved_identity(
    upgrade: &[(StandardUpgradeIdentity, Vec<u8>)],
    inactive_raw_order: &[Vec<u8>],
) -> Option<StandardUpgradeIdentity> {
    inactive_raw_order.iter().find_map(|identity| {
        upgrade
            .iter()
            .find(|(_, wanted)| wanted == identity)
            .map(|(reserved, _)| *reserved)
    })
}
