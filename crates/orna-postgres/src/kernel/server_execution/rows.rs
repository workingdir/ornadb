use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResultCardinality {
    BoundedMany,
    AtMostOne,
    ExactlyOne,
}

impl ResultCardinality {
    pub(super) fn validate(self, row_count: usize) -> Result<(), PostgresKernelError> {
        match self {
            Self::BoundedMany => Ok(()),
            Self::AtMostOne => validate_identity_selected_cardinality(row_count),
            Self::ExactlyOne if row_count > 1 => {
                Err(server_error(ServerSelectError::Cardinality {
                    rule: "a scalar SERVER SELECT returned more than one row",
                }))
            }
            Self::ExactlyOne => Ok(()),
        }
    }

    pub(super) fn finish(self, row_count: usize) -> Result<(), PostgresKernelError> {
        if matches!(self, Self::ExactlyOne) && row_count == 0 {
            return Err(server_error(ServerSelectError::Cardinality {
                rule: "a scalar SERVER SELECT returned zero rows",
            }));
        }
        Ok(())
    }
}

pub(super) struct ResultReadShape<'a> {
    pub(super) active: &'a ActiveDatabaseRevision,
    pub(super) columns: &'a [ResultColumn],
    pub(super) guards: &'a [VariableGuard],
    pub(super) variable_payload_limit: usize,
    pub(super) cardinality: ResultCardinality,
}

pub(super) async fn stream_rows(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: &[SelectBindValue],
    shape: ResultReadShape<'_>,
) -> Result<ResultRows, PostgresKernelError> {
    let parameters = binds
        .iter()
        .map(SelectBindValue::as_to_sql)
        .collect::<Vec<_>>();
    let stream = transaction
        .query_raw(statement, parameters)
        .await
        .map_err(PostgresKernelError::Database)?;
    futures_util::pin_mut!(stream);
    let mut rows = Vec::new();
    let mut cells = 0usize;
    let mut payload = initial_payload_len(shape.columns)?;
    while let Some(row) = stream
        .try_next()
        .await
        .map_err(PostgresKernelError::Database)?
    {
        shape.cardinality.validate(rows.len().saturating_add(1))?;
        if rows.len() == ROW_LIMIT {
            return Err(server_error(ServerSelectError::RowLimit {
                maximum: ROW_LIMIT,
            }));
        }
        cells = cells.checked_add(shape.columns.len()).ok_or_else(|| {
            server_error(ServerSelectError::CellLimit {
                maximum: CELL_LIMIT,
            })
        })?;
        if cells > CELL_LIMIT {
            return Err(server_error(ServerSelectError::CellLimit {
                maximum: CELL_LIMIT,
            }));
        }
        let row_index = rows.len();
        for (guard_index, guard) in shape.guards.iter().enumerate() {
            let accepted = row
                .try_get::<usize, bool>(shape.columns.len() + guard_index)
                .map_err(|source| {
                    server_error(ServerSelectError::RowDecode {
                        row: row_index,
                        column: shape.columns.len() + guard_index,
                        source,
                    })
                })?;
            if !accepted {
                return Err(server_error(ServerSelectError::VariablePayload {
                    row: row_index,
                    column: guard.column,
                    maximum: shape.variable_payload_limit,
                }));
            }
        }
        let mut values = Vec::with_capacity(shape.columns.len());
        for (column_index, column) in shape.columns.iter().enumerate() {
            let value = decode_value(shape.active, &row, row_index, column_index, column)?;
            let value_payload = match &value {
                RuntimeValue::Record(_) => {
                    canonical_record_payload_len(shape.active, &value, row_index, column_index)?
                }
                _ => logical_payload_len(&value)?,
            };
            payload = add_payload(payload, value_payload)?;
            values.push(value);
        }
        rows.push(ResultRow::new(values));
    }
    shape.cardinality.finish(rows.len())?;
    ResultRows::new(shape.columns.to_vec(), rows)
        .map_err(ServerSelectError::ReturnedRows)
        .map_err(server_error)
}

pub(super) fn validate_identity_selected_cardinality(
    row_count: usize,
) -> Result<(), PostgresKernelError> {
    if row_count > 1 {
        return Err(server_error(ServerSelectError::Cardinality {
            rule: "more than one row was returned for the requested object",
        }));
    }
    Ok(())
}

pub(super) fn initial_payload_len(columns: &[ResultColumn]) -> Result<usize, PostgresKernelError> {
    columns.iter().try_fold(0usize, |payload, column| {
        add_payload(payload, column.name().len())
    })
}

pub(super) fn add_payload(payload: usize, additional: usize) -> Result<usize, PostgresKernelError> {
    let payload = payload.checked_add(additional).ok_or_else(|| {
        server_error(ServerSelectError::PayloadLimit {
            maximum: PAYLOAD_LIMIT,
        })
    })?;
    if payload > PAYLOAD_LIMIT {
        return Err(server_error(ServerSelectError::PayloadLimit {
            maximum: PAYLOAD_LIMIT,
        }));
    }
    Ok(payload)
}

pub(super) fn decode_value(
    active: &ActiveDatabaseRevision,
    row: &Row,
    row_index: usize,
    column_index: usize,
    column: &ResultColumn,
) -> Result<RuntimeValue, PostgresKernelError> {
    let catalogue = active.catalogue();
    let context = active.catalogue_hash_context();
    macro_rules! decode {
        ($type:ty, $value:expr) => {
            row.try_get::<usize, Option<$type>>(column_index)
                .map_err(|source| {
                    server_error(ServerSelectError::RowDecode {
                        row: row_index,
                        column: column_index,
                        source,
                    })
                })?
                .map($value)
                .transpose()?
        };
    }
    let resolved_type = column.resolved_type();
    let value = match resolve_catalogue_runtime_type(catalogue, context, resolved_type) {
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Boolean) => {
            decode!(bool, |value| Ok(RuntimeValue::Boolean(value)))
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Integer) => {
            decode!(i32, |value| Ok(RuntimeValue::Integer(value)))
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::BigInt) => {
            decode!(i64, |value| Ok(RuntimeValue::BigInt(value)))
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::Float) => {
            decode!(f64, |value| {
                RuntimeFloat::new(value)
                    .map(RuntimeValue::Float)
                    .map_err(ServerSelectError::ReturnedRows)
                    .map_err(server_error)
            })
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::CharacterLargeObject) => {
            decode!(String, |value| Ok(RuntimeValue::Text(value)))
        }
        runtime if runtime.compatibility_scalar() == Some(StandardScalar::BinaryLargeObject) => {
            decode!(Vec<u8>, |value| Ok(RuntimeValue::Bytes(value)))
        }
        ResolvedRuntimeType::Reference(target) => decode!(Vec<u8>, |value| {
            let object = value.try_into().map(ObjectId::from_bytes).map_err(|_| {
                server_error(ServerSelectError::ValueInvariant {
                    row: row_index,
                    column: column_index,
                    rule: "reference result values must contain exactly 16 bytes",
                })
            })?;
            Ok(RuntimeValue::Reference { target, object })
        }),
        ResolvedRuntimeType::CatalogueEnum(enum_type) => decode!(String, |value| {
            EnumValue::new(catalogue, enum_type, value)
            .map(RuntimeValue::Enum)
            .map_err(|_| {
                server_error(ServerSelectError::ValueInvariant {
                    row: row_index,
                    column: column_index,
                    rule: "enum result must contain one exact label declared by the active enum type",
                })
            })
        }),
        ResolvedRuntimeType::Record(record_type) => decode!(Vec<u8>, |encoded| {
            match decode_active_value(active, &encoded) {
                Ok(value) => match &value {
                    RuntimeValue::Record(record) if record.record_type() == record_type => {
                        Ok(value)
                    }
                    _ => Err(server_error(ServerSelectError::ValueInvariant {
                        row: row_index,
                        column: column_index,
                        rule: "canonical record result type must equal its declared active type",
                    })),
                },
                Err(source) => Err(server_error(ServerSelectError::ValueCodec {
                    row: row_index,
                    column: column_index,
                    source,
                })),
            }
        }),
        ResolvedRuntimeType::LegacyScalar(_)
        | ResolvedRuntimeType::VerifiedValue { .. }
        | ResolvedRuntimeType::Unsupported => {
            return Err(server_error(ServerSelectError::PreparedResult {
                rule: "result value type is outside the initial runtime subset",
            }));
        }
    };
    match value {
        Some(value) => Ok(value),
        None => RuntimeValue::null(resolved_type)
            .map_err(ServerSelectError::ReturnedRows)
            .map_err(server_error),
    }
}

pub(super) fn canonical_record_payload_len(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    row: usize,
    column: usize,
) -> Result<usize, PostgresKernelError> {
    encode_active_value(active, value)
        .map_err(|source| {
            server_error(ServerSelectError::ValueCodec {
                row,
                column,
                source,
            })
        })?
        .len()
        .checked_sub(ACTIVE_VALUE_ENVELOPE_LENGTH)
        .ok_or_else(|| {
            server_error(ServerSelectError::ValueInvariant {
                row,
                column,
                rule: "canonical record result must contain one complete ORV3 envelope",
            })
        })
}

pub(super) fn logical_payload_len(value: &RuntimeValue) -> Result<usize, PostgresKernelError> {
    Ok(match value {
        RuntimeValue::Null(_) => 0,
        RuntimeValue::Boolean(_) => 1,
        RuntimeValue::Integer(_) => 4,
        RuntimeValue::BigInt(_) | RuntimeValue::Float(_) => 8,
        RuntimeValue::Text(value) => value.len(),
        RuntimeValue::Bytes(value) => value.len(),
        RuntimeValue::Reference { .. } => 16,
        RuntimeValue::Enum(value) => value.label().len(),
        RuntimeValue::Record(_) => {
            return Err(server_error(ServerSelectError::ValueInvariant {
                row: 0,
                column: 0,
                rule: "record payload accounting requires an active revision",
            }));
        }
        _ => {
            return Err(server_error(ServerSelectError::ValueInvariant {
                row: 0,
                column: 0,
                rule: "unknown future RuntimeValue variants cannot contribute zero payload",
            }));
        }
    })
}
