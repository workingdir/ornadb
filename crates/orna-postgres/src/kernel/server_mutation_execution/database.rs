use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UniqueConstraint {
    Reference {
        owner: TypeId,
        field: FieldId,
        referenced_type: TypeId,
    },
    Text {
        owner: TypeId,
        field: FieldId,
    },
}

impl UniqueConstraint {
    const fn field(self) -> FieldId {
        match self {
            Self::Reference { field, .. } | Self::Text { field, .. } => field,
        }
    }

    pub(super) fn error(self, source: tokio_postgres::Error) -> ServerMutationError {
        match self {
            Self::Reference {
                owner,
                field,
                referenced_type,
            } => ServerMutationError::UniqueReferenceConflict {
                owner,
                field,
                referenced_type,
                source,
            },
            Self::Text { owner, field } => ServerMutationError::UniqueTextConflict {
                owner,
                field,
                source,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct UniqueConstraints {
    pub(super) fields: Vec<UniqueConstraint>,
}

impl UniqueConstraints {
    pub(super) fn from_target(
        context: &CatalogueHashContext,
        target: &ObjectTypeDefinition,
    ) -> Result<Self, PostgresKernelError> {
        let mut fields = Vec::new();
        for field in target.fields() {
            if !field.unique() {
                continue;
            }
            if field.is_required_unique_reference() {
                let Some(referenced_type) = field.resolved_type().reference_target() else {
                    return Err(plan_invariant(
                        "UNIQUE target fields must be exact Text or required typed references",
                    ));
                };
                fields.push(UniqueConstraint::Reference {
                    owner: target.id(),
                    field: field.id(),
                    referenced_type,
                });
                continue;
            }
            if !supports_unique_text(context, field.resolved_type()) {
                return Err(plan_invariant(
                    "UNIQUE target fields must be exact Text or required typed references",
                ));
            }
            fields.push(UniqueConstraint::Text {
                owner: target.id(),
                field: field.id(),
            });
        }
        Ok(Self { fields })
    }

    pub(super) fn conflict(&self, source: &tokio_postgres::Error) -> Option<UniqueConstraint> {
        let error = source.as_db_error()?;
        unique_constraint(self, Some(error.code()), error.constraint())
    }
}

fn supports_unique_text(context: &CatalogueHashContext, resolved_type: ResolvedType) -> bool {
    match (context.standard(), resolved_type) {
        (None, ResolvedType::Scalar(StandardScalar::CharacterLargeObject)) => true,
        (Some(standard), ResolvedType::Value(type_id)) => standard
            .catalogue()
            .value_type_by_id(type_id)
            .is_some_and(|value_type| {
                value_type.kind() == ValueTypeKind::Primitive
                    && value_type.mutability() == ValueTypeMutability::Immutable
                    && value_type.persistence() == ValueTypePersistence::Persistable
                    && value_type.representation_contract()
                        == "orna.kernel.value.character-large-object@1"
            }),
        _ => false,
    }
}

pub(super) fn unique_constraint(
    constraints: &UniqueConstraints,
    code: Option<&SqlState>,
    constraint: Option<&str>,
) -> Option<UniqueConstraint> {
    if code != Some(&SqlState::UNIQUE_VIOLATION) {
        return None;
    }
    let constraint = constraint?;
    constraints
        .fields
        .iter()
        .copied()
        .find(|expected| unique_constraint_name(expected.field()) == constraint)
}

fn mutation_database_error(
    source: tokio_postgres::Error,
    constraints: &UniqueConstraints,
) -> ServerMutationError {
    if let Some(constraint) = constraints.conflict(&source) {
        constraint.error(source)
    } else {
        ServerMutationError::Database { source }
    }
}

pub(super) async fn execute_insert(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    object: ObjectId,
    unique_constraints: &UniqueConstraints,
) -> Result<(), PostgresKernelError> {
    let object_bytes = object.to_bytes().to_vec();
    let mut parameters = Vec::<&(dyn ToSql + Sync)>::with_capacity(binds.len() + 1);
    parameters.push(&object_bytes);
    parameters.extend(binds.iter().map(BindValue::as_to_sql));
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| server_error(mutation_database_error(source, unique_constraints)))?;
    let [row] = rows.as_slice() else {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: "INSERT must return exactly one row",
        }));
    };
    let returned = row
        .try_get::<usize, Vec<u8>>(0)
        .map_err(|source| server_error(ServerInsertError::RowDecode { source }))?;
    let returned: [u8; 16] = returned.try_into().map_err(|_| {
        server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must contain exactly 16 bytes",
        })
    })?;
    if ObjectId::from_bytes(returned) != object {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must equal the allocated identity",
        }));
    }
    Ok(())
}

pub(super) async fn execute_update(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    selector: ObjectId,
    unique_constraints: &UniqueConstraints,
) -> Result<bool, PostgresKernelError> {
    let parameters = binds.iter().map(BindValue::as_to_sql).collect::<Vec<_>>();
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| server_error(mutation_database_error(source, unique_constraints)))?;
    decode_selected_result(&rows, selector, "UPDATE")
}

pub(super) async fn execute_delete(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    context: ServerDeleteContext,
    target: TypeId,
    selector: ObjectId,
) -> Result<bool, PostgresKernelError> {
    let parameters = binds.iter().map(BindValue::as_to_sql).collect::<Vec<_>>();
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| {
            if delete_commit_failure(
                source
                    .as_db_error()
                    .map(tokio_postgres::error::DbError::code),
            ) == DeleteCommitFailure::Restricted
            {
                delete_error(ServerDeleteError::DeleteRestricted {
                    context,
                    target,
                    selector,
                    source,
                })
            } else {
                server_error(ServerMutationError::Database { source })
            }
        })?;
    decode_selected_result(&rows, selector, "DELETE")
}

fn decode_selected_result(
    rows: &[Row],
    selector: ObjectId,
    operation: &'static str,
) -> Result<bool, PostgresKernelError> {
    let [row] = rows else {
        if rows.is_empty() {
            return Ok(false);
        }
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: match operation {
                "UPDATE" => "UPDATE must return at most one row",
                "DELETE" => "DELETE must return at most one row",
                _ => "identity-selected mutation must return at most one row",
            },
        }));
    };
    let returned = row
        .try_get::<usize, Vec<u8>>(0)
        .map_err(|source| server_error(ServerInsertError::RowDecode { source }))?;
    let returned: [u8; 16] = returned.try_into().map_err(|_| {
        server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must contain exactly 16 bytes",
        })
    })?;
    if ObjectId::from_bytes(returned) != selector {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: match operation {
                "UPDATE" => "updated object identity must equal the selected identity",
                "DELETE" => "deleted object identity must equal the selected identity",
                _ => "returned object identity must equal the selected identity",
            },
        }));
    }
    Ok(true)
}
