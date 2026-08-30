use super::*;

/// Preloads the object graph reachable from authenticated CLIENT arguments.
///
/// Every generated relation and field identifier comes from the active
/// catalogue. The supplied transaction remains the sole storage session, so
/// all roots and recursively followed references observe one snapshot.
pub(crate) async fn load_client_reference_loader(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    principal: PrincipalId,
    security_context_digest: Sha256Digest,
    roots: &[FunctionArgument],
) -> Result<ClientReferenceLoader, PostgresKernelError> {
    let mut pending = VecDeque::new();
    let mut visited = BTreeSet::new();
    for argument in roots {
        if let RuntimeValue::Reference { target, object } = argument.value() {
            enqueue_client_reference_object(active, &mut visited, &mut pending, *target, *object)?;
        }
    }

    let mut objects = Vec::with_capacity(visited.len());
    while let Some((target, object)) = pending.pop_front() {
        let object_definition = active
            .catalogue()
            .object_type_by_id(target)
            .ok_or_else(|| {
                server_error(ServerSelectError::PlanInvariant {
                    rule: "client reference loader target must be an active object type",
                })
            })?;
        let fields = object_definition.fields();
        let supported_fields = fields
            .iter()
            .filter(|field| client_reference_loader_field_supported(active, field.resolved_type()))
            .collect::<Vec<_>>();
        let columns = supported_fields
            .iter()
            .map(|field| {
                ResultColumn::new(
                    field_name(field.id()),
                    field.resolved_type(),
                    field.nullable(),
                )
                .map_err(ServerSelectError::ResultRows)
                .map_err(server_error)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut projections = String::from(OBJECT_ID_COLUMN);
        for field in &supported_fields {
            projections.push_str(", ");
            projections.push_str(&field_name(field.id()));
        }
        let sql = format!(
            "SELECT {projections} FROM {DATA_SCHEMA}.{} \
             WHERE {OBJECT_ID_COLUMN} = $1 LIMIT 2",
            relation_name(target),
        );
        if sql.len() > SQL_LIMIT {
            return Err(server_error(ServerSelectError::ComplexityLimit {
                category: "generated client reference loader SQL bytes",
                maximum: SQL_LIMIT,
            }));
        }
        let object_bytes = object.to_bytes().to_vec();
        let rows = transaction
            .query(&sql, &[&object_bytes])
            .await
            .map_err(PostgresKernelError::Database)?;
        if rows.len() > 1 {
            return Err(server_error(ServerSelectError::Cardinality {
                rule: "more than one row was returned for the requested client reference object",
            }));
        }
        let Some(row) = rows.into_iter().next() else {
            // Missing objects remain absent. The evaluator maps that absence
            // to its redacted FieldPath error.
            continue;
        };

        let returned_bytes = row.try_get::<usize, Vec<u8>>(0).map_err(|source| {
            server_error(ServerSelectError::RowDecode {
                row: 0,
                column: 0,
                source,
            })
        })?;
        let returned_bytes: [u8; 16] = returned_bytes.try_into().map_err(|_| {
            server_error(ServerSelectError::ValueInvariant {
                row: 0,
                column: 0,
                rule: "client reference object rows must contain exactly 16 object-id bytes",
            })
        })?;
        if ObjectId::from_bytes(returned_bytes) != object {
            return Err(server_error(ServerSelectError::ValueInvariant {
                row: 0,
                column: 0,
                rule: "client reference object row identity must equal the requested object",
            }));
        }

        let mut values = Vec::with_capacity(columns.len());
        for (field_index, column) in columns.iter().enumerate() {
            values.push(decode_value(
                active,
                &row,
                0,
                field_index.saturating_add(1),
                column,
            )?);
        }

        let mut field_values = Vec::with_capacity(supported_fields.len());
        for (field, value) in supported_fields.iter().zip(values) {
            if let RuntimeValue::Reference {
                target: child_target,
                object: child_object,
            } = &value
            {
                enqueue_client_reference_object(
                    active,
                    &mut visited,
                    &mut pending,
                    *child_target,
                    *child_object,
                )?;
            }
            field_values.push((field.id(), value));
        }
        objects.push(ClientReferenceObject::new(target, object, field_values));
    }

    ClientReferenceLoader::new(active.pair(), principal, security_context_digest, objects).map_err(
        |_| {
            PostgresKernelError::CatalogueInvariant(
                "client reference loader contains duplicate object identities",
            )
        },
    )
}

fn client_reference_loader_field_supported(
    active: &ActiveDatabaseRevision,
    resolved_type: ResolvedType,
) -> bool {
    let runtime = resolve_catalogue_runtime_type(
        active.catalogue(),
        active.catalogue_hash_context(),
        resolved_type,
    );
    matches!(
        runtime,
        ResolvedRuntimeType::CatalogueEnum(_)
            | ResolvedRuntimeType::Record(_)
            | ResolvedRuntimeType::Reference(_)
    ) || matches!(
        runtime.compatibility_scalar(),
        Some(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        )
    )
}

fn enqueue_client_reference_object(
    active: &ActiveDatabaseRevision,
    visited: &mut BTreeSet<(TypeId, ObjectId)>,
    pending: &mut VecDeque<(TypeId, ObjectId)>,
    target: TypeId,
    object: ObjectId,
) -> Result<(), PostgresKernelError> {
    if active.catalogue().object_type_by_id(target).is_none() {
        return Ok(());
    }
    if !visited.insert((target, object)) {
        return Ok(());
    }
    if visited.len() > TARGET_ENTRY_LIMIT {
        return Err(server_error(ServerSelectError::ComplexityLimit {
            category: "client reference loader objects",
            maximum: TARGET_ENTRY_LIMIT,
        }));
    }
    pending.push_back((target, object));
    Ok(())
}
