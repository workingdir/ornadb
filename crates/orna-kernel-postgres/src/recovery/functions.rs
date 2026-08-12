//! Recovery of executable function catalogue state.

use std::collections::{BTreeMap, BTreeSet};

use orna_core::{
    CatalogueRevisionId, ExpressionId, FunctionId, FunctionRevisionId, ParameterId, SchemaId,
    SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeId,
    canonical_hash::{
        artifact_payload_digest, function_declaration_digest, source_bundle_digest,
        source_revision_digest,
    },
    catalogue::{
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionReturnColumnDefinition,
        FunctionSecurity, FunctionTransaction, FunctionVolatility, ParameterDefinition,
        QualifiedSemanticName,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionPair, Sha256Digest, SourceOrigin,
        StoredSourceRevision,
    },
    types::ResolvedType,
};
use tokio_postgres::{Row, Transaction};

#[cfg(test)]
use orna_core::types::StandardScalar;

use crate::{
    PostgresKernelError,
    decode::{
        DurableRecord, digest_bytes, exact_enum, identity_bytes, optional_identity_bytes,
        u32_from_i64, u64_from_i64,
    },
};

use super::{
    LegacyResolvedTypeTupleMember, ResolvedTypeTuple, catalogue_hash_context_for,
    decode_catalogue_hash_version, decode_durable_version, decode_legacy_resolved_type_tuple,
    decode_legacy_resolved_type_tuple_kind, decode_origin, decode_resolved_type_tuple,
    load_catalogue_semantics, load_source_units, require_hash_contract,
};

const FUNCTION_RELATION: &str = "_orna_kernel.catalogue_functions";
const PARAMETER_RELATION: &str = "_orna_kernel.catalogue_function_parameters";
const RETURN_RELATION: &str = "_orna_kernel.catalogue_function_return_columns";
const REVISION_RELATION: &str = "_orna_kernel.function_revisions";
const ARTIFACT_RELATION: &str = "_orna_kernel.function_artifacts";
const REFERENCE_RELATION: &str = "_orna_kernel.definition_references";

pub(super) struct RecoveredFunctionState {
    pub(super) functions: Vec<RecoveredFunction>,
    pub(super) active_revisions: Vec<FunctionRevisionRecord>,
    pub(super) historical_revisions: Vec<FunctionRevisionRecord>,
    pub(super) origins: Vec<DefinitionOrigin>,
    pub(super) references: Vec<DefinitionReference>,
    pub(super) introductions: BTreeMap<CatalogueRevisionId, RecoveredIntroduction>,
}

impl RecoveredFunctionState {
    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self {
            functions: Vec::new(),
            active_revisions: Vec::new(),
            historical_revisions: Vec::new(),
            origins: Vec::new(),
            references: Vec::new(),
            introductions: BTreeMap::new(),
        }
    }
}

pub(super) struct RecoveredFunction {
    pub(super) schema: SchemaId,
    pub(super) definition: FunctionDefinition,
}

pub(super) struct RecoveredIntroduction {
    pub(super) catalogue_hash: Sha256Digest,
    pub(super) source: StoredSourceRevision,
    catalogue_hash_version: CatalogueHashVersion,
    standard_library_revision: Option<StandardLibraryRevisionId>,
}

struct RecoveredParameter {
    function: FunctionId,
    definition: ParameterDefinition,
    origin: DefinitionOrigin,
}

struct RecoveredReturnColumn {
    function: FunctionId,
    definition: FunctionReturnColumnDefinition,
    origin: DefinitionOrigin,
}

struct PendingRevision {
    function: FunctionId,
    id: FunctionRevisionId,
    revision_number: u64,
    declaration_origin: SourceOrigin,
    declaration_hash: Sha256Digest,
    semantic_hash: Sha256Digest,
    semantic_hash_version: FunctionSemanticHashVersion,
    language_version: String,
    artifact: ExecutableArtifact,
    status: RevisionStatus,
    introduction: IntroductionHeader,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RevisionStatus {
    Active,
    Retired,
}

#[derive(Clone)]
struct IntroductionHeader {
    catalogue: CatalogueRevisionId,
    catalogue_hash: Sha256Digest,
    source: SourceRevisionId,
    source_parent: Option<SourceRevisionId>,
    source_hash: Sha256Digest,
    bundle: SourceBundleId,
    bundle_hash: Sha256Digest,
    catalogue_hash_version: CatalogueHashVersion,
    standard_library_revision: Option<StandardLibraryRevisionId>,
}

pub(super) async fn load_function_state(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    active_ancestry: &BTreeSet<(CatalogueRevisionId, SourceRevisionId)>,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredFunctionState, PostgresKernelError> {
    let (functions, origins) =
        load_catalogue_functions(transaction, catalogue, catalogue_hash_context).await?;

    let mut artifacts = load_artifacts(transaction).await?;
    let pending = load_revisions(transaction, &mut artifacts).await?;
    if let Some((revision, _)) = artifacts.first_key_value() {
        return Err(DurableRecord::new(ARTIFACT_RELATION, revision.canonical())
            .invariant("every function artifact must belong to one recovered function revision"));
    }
    let (active_revisions, historical_revisions, introductions) =
        finish_revisions(transaction, &functions, pending, active_ancestry).await?;

    verify_historical_introductions(
        transaction,
        catalogue,
        &active_revisions,
        &historical_revisions,
        &introductions,
        catalogue_hash_context,
    )
    .await?;

    let references = load_references(
        transaction,
        catalogue,
        catalogue_hash_context
            .standard()
            .map(|standard| standard.revision()),
    )
    .await?;
    validate_reference_sources(&functions, &references)?;

    Ok(RecoveredFunctionState {
        functions,
        active_revisions,
        historical_revisions,
        origins,
        references,
        introductions,
    })
}

async fn load_catalogue_functions(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<(Vec<RecoveredFunction>, Vec<DefinitionOrigin>), PostgresKernelError> {
    let mut parameters = load_parameters(transaction, catalogue, catalogue_hash_context).await?;
    let mut returns = load_return_columns(transaction, catalogue, catalogue_hash_context).await?;
    let result = load_functions(
        transaction,
        catalogue,
        &mut parameters,
        &mut returns,
        catalogue_hash_context,
    )
    .await?;
    reject_leftover_members(&parameters, PARAMETER_RELATION, "parameter")?;
    reject_leftover_members(&returns, RETURN_RELATION, "return column")?;
    Ok(result)
}

async fn load_parameters(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<BTreeMap<FunctionId, Vec<RecoveredParameter>>, PostgresKernelError> {
    let rows = if catalogue_hash_context.standard().is_some() {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, parameter_id, name, ordinal,
                        type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id,
                        enum_type_id,
                        default_expression_id, source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_function_parameters
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id, ordinal, parameter_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    } else {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, parameter_id, name, ordinal,
                        type_kind, scalar_type, target_type_id, default_expression_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_function_parameters
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id, ordinal, parameter_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    };
    let mut parameters = BTreeMap::<FunctionId, Vec<RecoveredParameter>>::new();
    for (index, row) in rows.iter().enumerate() {
        let parameter = decode_parameter(row, index, catalogue, catalogue_hash_context)?;
        parameters
            .entry(parameter.function)
            .or_default()
            .push(parameter);
    }
    Ok(parameters)
}

fn decode_parameter(
    row: &Row,
    index: usize,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredParameter, PostgresKernelError> {
    let row_record = DurableRecord::new(PARAMETER_RELATION, format!("row={index}"));
    require_catalogue(row, &row_record, catalogue, "parameter")?;
    let function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "parameter owner identity must be 16 bytes",
        )?,
        &row_record,
        "parameter owner identity must be 16 bytes",
    )?);
    let id = ParameterId::from_bytes(identity_bytes(
        row_record.column(row, "parameter_id", "parameter identity must be 16 bytes")?,
        &row_record,
        "parameter identity must be 16 bytes",
    )?);
    let record = parameter_record(function, id);
    let name: String = record.column(row, "name", "parameter name must be PostgreSQL text")?;
    if name.is_empty() {
        return Err(record.invariant("parameter name must not be empty"));
    }
    let ordinal = u32_from_i64(
        record.column(row, "ordinal", "parameter ordinal must fit u32")?,
        &record,
        "parameter ordinal must fit u32",
    )?;
    let resolved_type = decode_type_columns(
        row,
        &record,
        LegacyResolvedTypeTupleMember::Parameter,
        catalogue_hash_context,
    )?;
    let default_expression = optional_identity_bytes(
        record.column(
            row,
            "default_expression_id",
            "parameter default expression identity must be null or 16 bytes",
        )?,
        &record,
        "parameter default expression identity must be null or 16 bytes",
    )?
    .map(ExpressionId::from_bytes);
    let origin = decode_origin(
        row,
        &record,
        DefinitionIdentity::Parameter {
            owner: function,
            parameter: id,
        },
    )?;
    Ok(RecoveredParameter {
        function,
        definition: ParameterDefinition::new(id, name, ordinal, resolved_type, default_expression),
        origin,
    })
}

async fn load_return_columns(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<BTreeMap<FunctionId, Vec<RecoveredReturnColumn>>, PostgresKernelError> {
    let rows = if catalogue_hash_context.standard().is_some() {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, name, ordinal,
                        type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id,
                        enum_type_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_function_return_columns
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id, ordinal",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    } else {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, name, ordinal,
                        type_kind, scalar_type, target_type_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_function_return_columns
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id, ordinal",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    };
    let mut columns = BTreeMap::<FunctionId, Vec<RecoveredReturnColumn>>::new();
    for (index, row) in rows.iter().enumerate() {
        let column = decode_return_column(row, index, catalogue, catalogue_hash_context)?;
        columns.entry(column.function).or_default().push(column);
    }
    Ok(columns)
}

fn decode_return_column(
    row: &Row,
    index: usize,
    catalogue: CatalogueRevisionId,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<RecoveredReturnColumn, PostgresKernelError> {
    let row_record = DurableRecord::new(RETURN_RELATION, format!("row={index}"));
    require_catalogue(row, &row_record, catalogue, "return column")?;
    let function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_id",
            "return column owner identity must be 16 bytes",
        )?,
        &row_record,
        "return column owner identity must be 16 bytes",
    )?);
    let ordinal = u32_from_i64(
        row_record.column(row, "ordinal", "return column ordinal must fit u32")?,
        &row_record,
        "return column ordinal must fit u32",
    )?;
    let record = DurableRecord::new(
        RETURN_RELATION,
        format!("function={} ordinal={ordinal}", function.canonical()),
    );
    let name: String = record.column(row, "name", "return column name must be PostgreSQL text")?;
    if name.is_empty() {
        return Err(record.invariant("return column name must not be empty"));
    }
    let resolved_type = decode_type_columns(
        row,
        &record,
        LegacyResolvedTypeTupleMember::ReturnColumn,
        catalogue_hash_context,
    )?;
    let origin = decode_origin(
        row,
        &record,
        DefinitionIdentity::FunctionReturnColumn {
            owner: function,
            ordinal,
        },
    )?;
    Ok(RecoveredReturnColumn {
        function,
        definition: FunctionReturnColumnDefinition::new(name, ordinal, resolved_type),
        origin,
    })
}

async fn load_functions(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    parameters: &mut BTreeMap<FunctionId, Vec<RecoveredParameter>>,
    returns: &mut BTreeMap<FunctionId, Vec<RecoveredReturnColumn>>,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<(Vec<RecoveredFunction>, Vec<DefinitionOrigin>), PostgresKernelError> {
    let rows = if catalogue_hash_context.standard().is_some() {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, schema_id, name_parts,
                        domain, security_mode, transaction_mode, volatility,
                        return_shape, return_type_kind AS type_kind,
                        return_scalar_type AS scalar_type,
                        return_target_type_id AS target_type_id,
                        return_value_type_id AS value_type_id,
                        return_standard_library_revision_id AS value_standard_library_revision_id,
                        return_enum_type_id AS enum_type_id,
                        current_function_revision_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    } else {
        transaction
            .query(
                "SELECT catalogue_revision_id, function_id, schema_id, name_parts,
                        domain, security_mode, transaction_mode, volatility,
                        return_shape, return_type_kind AS type_kind,
                        return_scalar_type AS scalar_type,
                        return_target_type_id AS target_type_id,
                        current_function_revision_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1
                 ORDER BY function_id",
                &[&catalogue.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
    };
    let mut functions = Vec::with_capacity(rows.len());
    let mut origins = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (function, mut function_origins) = decode_function(
            row,
            index,
            catalogue,
            parameters,
            returns,
            catalogue_hash_context,
        )?;
        origins.append(&mut function_origins);
        functions.push(function);
    }
    Ok((functions, origins))
}

fn decode_function(
    row: &Row,
    index: usize,
    catalogue: CatalogueRevisionId,
    parameters: &mut BTreeMap<FunctionId, Vec<RecoveredParameter>>,
    returns: &mut BTreeMap<FunctionId, Vec<RecoveredReturnColumn>>,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<(RecoveredFunction, Vec<DefinitionOrigin>), PostgresKernelError> {
    let row_record = DurableRecord::new(FUNCTION_RELATION, format!("row={index}"));
    require_catalogue(row, &row_record, catalogue, "function")?;
    let id = FunctionId::from_bytes(identity_bytes(
        row_record.column(row, "function_id", "function identity must be 16 bytes")?,
        &row_record,
        "function identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(FUNCTION_RELATION, id.canonical());
    let schema = SchemaId::from_bytes(identity_bytes(
        record.column(
            row,
            "schema_id",
            "function schema identity must be 16 bytes",
        )?,
        &record,
        "function schema identity must be 16 bytes",
    )?);
    let name_parts: Vec<String> = record.column(
        row,
        "name_parts",
        "function name parts must be an exact PostgreSQL text array",
    )?;
    let name = QualifiedSemanticName::new(name_parts)
        .map_err(|_| record.invariant("function name parts must form one exact semantic name"))?;
    let domain_name: String = record.column(row, "domain", "function domain must decode")?;
    let domain = exact_enum(
        &domain_name,
        &[
            ("server", FunctionDomain::Server),
            ("client", FunctionDomain::Client),
        ],
        &record,
        "function domain must be server or client",
    )?;
    let security_name: String =
        record.column(row, "security_mode", "function security mode must decode")?;
    let security = exact_enum(
        &security_name,
        &[
            ("invoker", FunctionSecurity::Invoker),
            ("definer", FunctionSecurity::Definer),
        ],
        &record,
        "function security mode must be invoker or definer",
    )?;
    let transaction_name: Option<String> = record.column(
        row,
        "transaction_mode",
        "function transaction mode must be null, atomic, or read_only",
    )?;
    let transaction = match transaction_name.as_deref() {
        None => None,
        Some("atomic") => Some(FunctionTransaction::Atomic),
        Some("read_only") => Some(FunctionTransaction::ReadOnly),
        Some(_) => {
            return Err(record.invariant(
                "function transaction mode must be null, atomic, or read_only; manual is unsupported",
            ));
        }
    };
    if domain == FunctionDomain::Client && transaction.is_some() {
        return Err(record.invariant("client functions must not declare transaction behaviour"));
    }
    let volatility_name: String =
        record.column(row, "volatility", "function volatility must decode")?;
    let volatility = exact_enum(
        &volatility_name,
        &[
            ("immutable", FunctionVolatility::Immutable),
            ("stable", FunctionVolatility::Stable),
            ("volatile", FunctionVolatility::Volatile),
        ],
        &record,
        "function volatility must be immutable, stable, or volatile",
    )?;
    let current_revision = FunctionRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "current_function_revision_id",
            "current function revision identity must be 16 bytes",
        )?,
        &record,
        "current function revision identity must be 16 bytes",
    )?);
    let recovered_parameters = parameters.remove(&id).unwrap_or_default();
    let mut member_origins = recovered_parameters
        .iter()
        .map(|parameter| parameter.origin.clone())
        .collect::<Vec<_>>();
    let parameter_definitions = recovered_parameters
        .into_iter()
        .map(|parameter| parameter.definition)
        .collect();
    let recovered_returns = returns.remove(&id).unwrap_or_default();
    let return_shape: String =
        record.column(row, "return_shape", "function return shape must decode")?;
    let return_type = match return_shape.as_str() {
        "single" if recovered_returns.is_empty() => FunctionReturn::Single(decode_type_columns(
            row,
            &record,
            LegacyResolvedTypeTupleMember::SingleReturn,
            catalogue_hash_context,
        )?),
        "rows" => {
            require_null_type_columns(row, &record, catalogue_hash_context)?;
            member_origins.extend(recovered_returns.iter().map(|column| column.origin.clone()));
            FunctionReturn::Rows(
                recovered_returns
                    .into_iter()
                    .map(|column| column.definition)
                    .collect(),
            )
        }
        "single" => {
            return Err(record.invariant("SINGLE functions must not have ROWS return columns"));
        }
        _ => return Err(record.invariant("function return shape must be single or rows")),
    };
    let origin = decode_origin(row, &record, DefinitionIdentity::Function(id))?;
    member_origins.push(origin);
    Ok((
        RecoveredFunction {
            schema,
            definition: FunctionDefinition::new(
                id,
                name,
                domain,
                parameter_definitions,
                return_type,
                current_revision,
                security,
                transaction,
                volatility,
            ),
        },
        member_origins,
    ))
}

fn reject_leftover_members<T>(
    members: &BTreeMap<FunctionId, Vec<T>>,
    relation: &'static str,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    if let Some((function, _)) = members.first_key_value() {
        return Err(
            DurableRecord::new(relation, format!("function={}", function.canonical())).invariant(
                match member {
                    "parameter" => "every parameter owner must be an active function",
                    _ => "every return column owner must be an active function",
                },
            ),
        );
    }
    Ok(())
}

fn decode_type_columns(
    row: &Row,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<ResolvedType, PostgresKernelError> {
    if catalogue_hash_context.standard().is_some() {
        return decode_version_two_type_columns(row, record, member, catalogue_hash_context);
    }
    let kind: Option<String> = record.column(
        row,
        "type_kind",
        "resolved type kind must be scalar, named, or reference",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "resolved scalar type must be null or an exact standard scalar name",
    )?;
    let target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "resolved target identity must be null or 16 bytes",
        )?,
        record,
        "resolved target identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let kind = decode_legacy_resolved_type_tuple_kind(kind.as_deref(), record, member)?;
    decode_legacy_resolved_type_tuple(kind, scalar.as_deref(), target, record, member)
}

fn decode_version_two_type_columns(
    row: &Row,
    record: &DurableRecord,
    member: LegacyResolvedTypeTupleMember,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<ResolvedType, PostgresKernelError> {
    let kind: Option<String> = record.column(
        row,
        "type_kind",
        "resolved type kind must be scalar, named, reference, value, or enum",
    )?;
    let scalar: Option<String> = record.column(
        row,
        "scalar_type",
        "resolved scalar type must be null or an exact standard scalar name",
    )?;
    let target = optional_identity_bytes(
        record.column(
            row,
            "target_type_id",
            "resolved target identity must be null or 16 bytes",
        )?,
        record,
        "resolved target identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let value_type = optional_identity_bytes(
        record.column(
            row,
            "value_type_id",
            "resolved value type identity must be null or 16 bytes",
        )?,
        record,
        "resolved value type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "value_standard_library_revision_id",
            "resolved value type standard library revision identity must be null or 16 bytes",
        )?,
        record,
        "resolved value type standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let enum_type = optional_identity_bytes(
        record.column(
            row,
            "enum_type_id",
            "resolved enum type identity must be null or 16 bytes",
        )?,
        record,
        "resolved enum type identity must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    decode_resolved_type_tuple(
        ResolvedTypeTuple {
            kind,
            scalar,
            target,
            value_type,
            standard_library_revision,
            enum_type,
        },
        catalogue_hash_context,
        record,
        member,
    )
}

fn require_null_type_columns(
    row: &Row,
    record: &DurableRecord,
    catalogue_hash_context: &CatalogueHashContext,
) -> Result<(), PostgresKernelError> {
    let kind: Option<String> = record.column(row, "type_kind", "ROWS type kind must be null")?;
    let scalar: Option<String> =
        record.column(row, "scalar_type", "ROWS scalar type must be null")?;
    let target: Option<Vec<u8>> =
        record.column(row, "target_type_id", "ROWS target type must be null")?;
    let value_type: Option<Vec<u8>> = if catalogue_hash_context.standard().is_some() {
        record.column(
            row,
            "value_type_id",
            "ROWS value type identity must be null",
        )?
    } else {
        None
    };
    let standard_library_revision: Option<Vec<u8>> = if catalogue_hash_context.standard().is_some()
    {
        record.column(
            row,
            "value_standard_library_revision_id",
            "ROWS value type standard library revision identity must be null",
        )?
    } else {
        None
    };
    let enum_type: Option<Vec<u8>> = if catalogue_hash_context.standard().is_some() {
        record.column(row, "enum_type_id", "ROWS enum type identity must be null")?
    } else {
        None
    };
    if kind.is_some()
        || scalar.is_some()
        || target.is_some()
        || value_type.is_some()
        || standard_library_revision.is_some()
        || enum_type.is_some()
    {
        return Err(record.invariant("ROWS functions must not store one SINGLE return type tuple"));
    }
    Ok(())
}

async fn load_artifacts(
    transaction: &Transaction<'_>,
) -> Result<BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT function_revision_id, artifact_kind, format,
                    format_version::bigint AS format_version, payload, content_hash,
                    hash_algorithm, hash_contract_version
             FROM _orna_kernel.function_artifacts
             ORDER BY function_revision_id, artifact_kind",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut artifacts = BTreeMap::<FunctionRevisionId, Vec<ExecutableArtifact>>::new();
    for (index, row) in rows.iter().enumerate() {
        let (revision, artifact) = decode_artifact(row, index)?;
        artifacts.entry(revision).or_default().push(artifact);
    }
    Ok(artifacts)
}

fn decode_artifact(
    row: &Row,
    index: usize,
) -> Result<(FunctionRevisionId, ExecutableArtifact), PostgresKernelError> {
    let row_record = DurableRecord::new(ARTIFACT_RELATION, format!("row={index}"));
    let revision = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "function_revision_id",
            "artifact function revision identity must be 16 bytes",
        )?,
        &row_record,
        "artifact function revision identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(ARTIFACT_RELATION, revision.canonical());
    require_hash_contract(
        row,
        &record,
        "hash_algorithm",
        "hash_contract_version",
        "function artifact hash algorithm must be sha256",
        "function artifact hash contract version must be 1",
    )?;
    let kind_name: String = record.column(row, "artifact_kind", "artifact kind must decode")?;
    let kind = exact_enum(
        &kind_name,
        &[
            ("server_plan", ExecutableArtifactKind::Server),
            ("client_bytecode", ExecutableArtifactKind::Client),
        ],
        &record,
        "artifact kind must be server_plan or client_bytecode",
    )?;
    let format: String = record.column(row, "format", "artifact format must be text")?;
    let version = u32_from_i64(
        record.column(
            row,
            "format_version",
            "artifact format version must fit u32",
        )?,
        &record,
        "artifact format version must fit u32",
    )?;
    let payload: Vec<u8> = record.column(row, "payload", "artifact payload must be exact bytes")?;
    let content_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "content_hash",
            "artifact content hash must be 32 bytes",
        )?,
        &record,
        "artifact content hash must be 32 bytes",
    )?);
    let computed = artifact_payload_digest(&payload).map_err(PostgresKernelError::CanonicalHash)?;
    if computed != content_hash {
        return Err(record.invariant("artifact digest must match its exact payload"));
    }
    let artifact = ExecutableArtifact::new(kind, format, version, payload, content_hash)
        .map_err(PostgresKernelError::RevisionInvariant)?;
    Ok((revision, artifact))
}

async fn load_revisions(
    transaction: &Transaction<'_>,
    artifacts: &mut BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>,
) -> Result<Vec<PendingRevision>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT revision.id, revision.function_id, revision.revision_number,
                    revision.content_hash, revision.semantic_ir_hash,
                    revision.semantic_hash_version,
                    revision.hash_algorithm, revision.hash_contract_version,
                    revision.language_version, revision.status,
                    revision.introduced_catalogue_revision_id,
                    introduced_function.current_function_revision_id AS introduced_current_revision_id,
                    introduced_function.domain AS introduced_domain,
                    introduced_function.source_unit_id,
                    introduced_function.source_start,
                    introduced_function.source_end,
                    catalogue.source_revision_id,
                    catalogue.content_hash AS catalogue_hash,
                    catalogue.hash_algorithm AS catalogue_algorithm,
                    catalogue.hash_contract_version AS catalogue_contract_version,
                    catalogue.canonical_hash_version AS catalogue_canonical_hash_version,
                    catalogue.standard_library_revision_id AS catalogue_standard_library_revision_id,
                    source.parent_source_revision_id,
                    source.bundle_id,
                    source.content_hash AS source_hash,
                    source.hash_algorithm AS source_algorithm,
                    source.hash_contract_version AS source_contract_version,
                    bundle.content_hash AS bundle_hash,
                    bundle.hash_algorithm AS bundle_algorithm,
                    bundle.hash_contract_version AS bundle_contract_version
             FROM _orna_kernel.function_revisions AS revision
             LEFT JOIN _orna_kernel.catalogue_revisions AS catalogue
               ON catalogue.id = revision.introduced_catalogue_revision_id
             LEFT JOIN _orna_kernel.catalogue_functions AS introduced_function
               ON introduced_function.catalogue_revision_id = revision.introduced_catalogue_revision_id
              AND introduced_function.function_id = revision.function_id
             LEFT JOIN _orna_kernel.source_revisions AS source
               ON source.id = catalogue.source_revision_id
             LEFT JOIN _orna_kernel.source_bundles AS bundle ON bundle.id = source.bundle_id
             ORDER BY revision.function_id, revision.revision_number, revision.id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut revisions = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        revisions.push(decode_revision(row, index, artifacts)?);
    }
    Ok(revisions)
}

fn decode_revision(
    row: &Row,
    index: usize,
    artifacts: &mut BTreeMap<FunctionRevisionId, Vec<ExecutableArtifact>>,
) -> Result<PendingRevision, PostgresKernelError> {
    let row_record = DurableRecord::new(REVISION_RELATION, format!("row={index}"));
    let id = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(row, "id", "function revision identity must be 16 bytes")?,
        &row_record,
        "function revision identity must be 16 bytes",
    )?);
    let record = DurableRecord::new(REVISION_RELATION, id.canonical());
    let function = FunctionId::from_bytes(identity_bytes(
        record.column(
            row,
            "function_id",
            "function revision owner identity must be 16 bytes",
        )?,
        &record,
        "function revision owner identity must be 16 bytes",
    )?);
    let revision_number = u64_from_i64(
        record.column(
            row,
            "revision_number",
            "function revision number must be a positive bigint",
        )?,
        &record,
        "function revision number must be a positive u64",
    )?;
    if revision_number == 0 {
        return Err(record.invariant("function revision number must be positive"));
    }
    let status_name: String =
        record.column(row, "status", "function revision status must decode")?;
    let status = exact_enum(
        &status_name,
        &[
            ("active", RevisionStatus::Active),
            ("retired", RevisionStatus::Retired),
        ],
        &record,
        "recoverable function revision status must be active or retired, never candidate or invalid",
    )?;
    require_hash_contract(
        row,
        &record,
        "hash_algorithm",
        "hash_contract_version",
        "function revision hash algorithm must be sha256",
        "function revision hash contract version must be 1",
    )?;
    let declaration_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "content_hash",
            "function declaration hash must be 32 bytes",
        )?,
        &record,
        "function declaration hash must be 32 bytes",
    )?);
    let semantic_hash = Sha256Digest::from_bytes(digest_bytes(
        record.column(
            row,
            "semantic_ir_hash",
            "function semantic hash must be 32 bytes",
        )?,
        &record,
        "function semantic hash must be 32 bytes",
    )?);
    let semantic_hash_version = decode_function_semantic_hash_version(
        record.column(
            row,
            "semantic_hash_version",
            "function semantic hash version must be a supported smallint",
        )?,
        &record,
    )?;
    let language_version: String = record.column(
        row,
        "language_version",
        "function language version must be text",
    )?;
    if language_version.is_empty() {
        return Err(record.invariant("function language version must not be empty"));
    }
    let mut revision_artifacts = artifacts.remove(&id).unwrap_or_default();
    if revision_artifacts.len() != 1 {
        return Err(record.invariant(
            "each function revision must have exactly one versioned executable artifact",
        ));
    }
    let artifact = revision_artifacts
        .pop()
        .ok_or_else(|| record.invariant("function revision artifact must exist"))?;
    let introduced_domain_name: String = record.column(
        row,
        "introduced_domain",
        "introducing function domain must decode",
    )?;
    let introduced_domain = exact_enum(
        &introduced_domain_name,
        &[
            ("server", FunctionDomain::Server),
            ("client", FunctionDomain::Client),
        ],
        &record,
        "introducing function domain must be server or client",
    )?;
    let expected_kind = match introduced_domain {
        FunctionDomain::Server => ExecutableArtifactKind::Server,
        FunctionDomain::Client => ExecutableArtifactKind::Client,
    };
    if artifact.kind() != expected_kind {
        return Err(record.invariant(
            "function artifact kind must exactly match the introducing function domain",
        ));
    }
    let introduced_current = FunctionRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "introduced_current_revision_id",
            "introducing function current revision identity must be 16 bytes",
        )?,
        &record,
        "introducing function current revision identity must be 16 bytes",
    )?);
    if introduced_current != id {
        return Err(record.invariant(
            "the introducing catalogue function must identify the immutable revision it introduced",
        ));
    }
    let declaration_origin = decode_required_source_origin(row, &record)?;
    let introduction = decode_introduction_header(row, &record)?;
    Ok(PendingRevision {
        function,
        id,
        revision_number,
        declaration_origin,
        declaration_hash,
        semantic_hash,
        semantic_hash_version,
        language_version,
        artifact,
        status,
        introduction,
    })
}

fn decode_function_semantic_hash_version(
    value: i16,
    record: &DurableRecord,
) -> Result<FunctionSemanticHashVersion, PostgresKernelError> {
    let value = decode_durable_version(
        value,
        record,
        "function semantic hash version must be a supported smallint",
    )?;
    FunctionSemanticHashVersion::try_from(value)
        .map_err(|_| record.invariant("function semantic hash version must be 1 or 2"))
}

fn decode_required_source_origin(
    row: &Row,
    record: &DurableRecord,
) -> Result<SourceOrigin, PostgresKernelError> {
    let unit = SourceUnitId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_unit_id",
            "historical declaration source unit identity must be 16 bytes",
        )?,
        record,
        "historical declaration source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(
        record.column(
            row,
            "source_start",
            "historical declaration origin start must fit u32",
        )?,
        record,
        "historical declaration origin start must fit u32",
    )?;
    let end = u32_from_i64(
        record.column(
            row,
            "source_end",
            "historical declaration origin end must fit u32",
        )?,
        record,
        "historical declaration origin end must fit u32",
    )?;
    SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)
}

fn decode_introduction_header(
    row: &Row,
    record: &DurableRecord,
) -> Result<IntroductionHeader, PostgresKernelError> {
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "introduced_catalogue_revision_id",
            "introducing catalogue identity must be 16 bytes",
        )?,
        record,
        "introducing catalogue identity must be 16 bytes",
    )?);
    let source = SourceRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_revision_id",
            "introducing source revision identity must be 16 bytes",
        )?,
        record,
        "introducing source revision identity must be 16 bytes",
    )?);
    let source_parent = optional_identity_bytes(
        record.column(
            row,
            "parent_source_revision_id",
            "introducing source parent identity must be null or 16 bytes",
        )?,
        record,
        "introducing source parent identity must be null or 16 bytes",
    )?
    .map(SourceRevisionId::from_bytes);
    let bundle = SourceBundleId::from_bytes(identity_bytes(
        record.column(
            row,
            "bundle_id",
            "introducing source bundle identity must be 16 bytes",
        )?,
        record,
        "introducing source bundle identity must be 16 bytes",
    )?);
    let catalogue_hash_version = decode_catalogue_hash_version(
        record.column(
            row,
            "catalogue_canonical_hash_version",
            "introducing catalogue hash version must be a supported smallint",
        )?,
        record,
    )?;
    let standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "catalogue_standard_library_revision_id",
            "introducing catalogue standard library revision identity must be null or 16 bytes",
        )?,
        record,
        "introducing catalogue standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    match (catalogue_hash_version, standard_library_revision) {
        (CatalogueHashVersion::Version1, None) | (CatalogueHashVersion::Version2, Some(_)) => {}
        _ => {
            return Err(record.invariant(
                "introducing catalogue hash version and standard library revision must form one exact context",
            ));
        }
    }
    for (algorithm, version, algorithm_rule, version_rule) in [
        (
            "catalogue_algorithm",
            "catalogue_contract_version",
            "introducing catalogue hash algorithm must be sha256",
            "introducing catalogue hash contract version must be 1",
        ),
        (
            "source_algorithm",
            "source_contract_version",
            "introducing source hash algorithm must be sha256",
            "introducing source hash contract version must be 1",
        ),
        (
            "bundle_algorithm",
            "bundle_contract_version",
            "introducing bundle hash algorithm must be sha256",
            "introducing bundle hash contract version must be 1",
        ),
    ] {
        require_hash_contract(
            row,
            record,
            algorithm,
            version,
            algorithm_rule,
            version_rule,
        )?;
    }
    Ok(IntroductionHeader {
        catalogue,
        catalogue_hash: Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "catalogue_hash",
                "introducing catalogue hash must be 32 bytes",
            )?,
            record,
            "introducing catalogue hash must be 32 bytes",
        )?),
        source,
        source_parent,
        source_hash: Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "source_hash",
                "introducing source hash must be 32 bytes",
            )?,
            record,
            "introducing source hash must be 32 bytes",
        )?),
        bundle,
        bundle_hash: Sha256Digest::from_bytes(digest_bytes(
            record.column(
                row,
                "bundle_hash",
                "introducing bundle hash must be 32 bytes",
            )?,
            record,
            "introducing bundle hash must be 32 bytes",
        )?),
        catalogue_hash_version,
        standard_library_revision,
    })
}

async fn finish_revisions(
    transaction: &Transaction<'_>,
    functions: &[RecoveredFunction],
    pending: Vec<PendingRevision>,
    active_ancestry: &BTreeSet<(CatalogueRevisionId, SourceRevisionId)>,
) -> Result<
    (
        Vec<FunctionRevisionRecord>,
        Vec<FunctionRevisionRecord>,
        BTreeMap<CatalogueRevisionId, RecoveredIntroduction>,
    ),
    PostgresKernelError,
> {
    let mut headers = BTreeMap::<CatalogueRevisionId, IntroductionHeader>::new();
    for revision in &pending {
        match headers.get(&revision.introduction.catalogue) {
            Some(existing) if !same_introduction(existing, &revision.introduction) => {
                return Err(DurableRecord::new(
                    REVISION_RELATION,
                    revision.introduction.catalogue.canonical(),
                )
                .invariant(
                    "all revisions introduced by one catalogue must join one exact source and hash chain",
                ));
            }
            Some(_) => {}
            None => {
                headers.insert(
                    revision.introduction.catalogue,
                    revision.introduction.clone(),
                );
            }
        }
    }
    let mut introductions = BTreeMap::new();
    for (catalogue, header) in headers {
        if !active_ancestry.contains(&(catalogue, header.source)) {
            return Err(DurableRecord::new(
                "_orna_kernel.catalogue_revisions",
                catalogue.canonical(),
            )
            .invariant(
                "every function revision introduction catalogue/source pair must lie on the active paired ancestry",
            ));
        }
        let units = load_source_units(transaction, header.bundle).await?;
        let bundle_record =
            DurableRecord::new("_orna_kernel.source_bundles", header.bundle.canonical());
        let computed_bundle =
            source_bundle_digest(&units).map_err(PostgresKernelError::CanonicalHash)?;
        if computed_bundle != header.bundle_hash {
            return Err(bundle_record.invariant(
                "introducing source bundle digest must match its complete ordered source units",
            ));
        }
        let source = StoredSourceRevision::new(
            header.bundle,
            header.source,
            header.source_parent,
            units,
            header.bundle_hash,
            header.source_hash,
        )
        .map_err(PostgresKernelError::RevisionInvariant)?;
        let computed_source =
            source_revision_digest(&source).map_err(PostgresKernelError::CanonicalHash)?;
        if computed_source != header.source_hash {
            return Err(DurableRecord::new(
                "_orna_kernel.source_revisions",
                header.source.canonical(),
            )
            .invariant(
                "introducing source revision digest must match its bundle, parent, and bundle digest",
            ));
        }
        introductions.insert(
            catalogue,
            RecoveredIntroduction {
                catalogue_hash: header.catalogue_hash,
                source,
                catalogue_hash_version: header.catalogue_hash_version,
                standard_library_revision: header.standard_library_revision,
            },
        );
    }

    let current_ids = functions
        .iter()
        .map(|function| {
            (
                function.definition.current_revision(),
                function.definition.id(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen_current = BTreeSet::new();
    let mut active = Vec::new();
    let mut historical = Vec::new();
    for revision in pending {
        let introduction = introductions
            .get(&revision.introduction.catalogue)
            .ok_or_else(|| {
                DurableRecord::new(REVISION_RELATION, revision.id.canonical())
                    .invariant("function revision introduction must be recovered")
            })?;
        validate_declaration(&revision, &introduction.source)?;
        let record = FunctionRevisionRecord::new(
            revision.function,
            revision.id,
            revision.revision_number,
            revision.declaration_origin,
            revision.declaration_hash,
            revision.semantic_hash,
            revision.language_version,
            revision.artifact,
        )
        .map_err(PostgresKernelError::RevisionInvariant)?
        .with_semantic_hash_version(revision.semantic_hash_version);
        if let Some(expected_function) = current_ids.get(&record.id()) {
            if revision.status != RevisionStatus::Active {
                return Err(
                    DurableRecord::new(REVISION_RELATION, record.id().canonical())
                        .invariant("every current function revision must have active status"),
                );
            }
            if *expected_function != record.function() {
                return Err(
                    DurableRecord::new(REVISION_RELATION, record.id().canonical())
                        .invariant("current function revision must belong to its active function"),
                );
            }
            seen_current.insert(record.id());
            active.push(record);
        } else {
            if revision.status != RevisionStatus::Retired {
                return Err(
                    DurableRecord::new(REVISION_RELATION, record.id().canonical()).invariant(
                        "every non-current immutable function revision must have retired status",
                    ),
                );
            }
            historical.push(record);
        }
    }
    if let Some((missing, _)) = current_ids
        .iter()
        .find(|(revision, _)| !seen_current.contains(revision))
    {
        return Err(DurableRecord::new(REVISION_RELATION, missing.canonical())
            .invariant("every active function must identify one recovered current revision"));
    }
    Ok((active, historical, introductions))
}

async fn verify_historical_introductions(
    transaction: &Transaction<'_>,
    active_catalogue: CatalogueRevisionId,
    active_revisions: &[FunctionRevisionRecord],
    historical_revisions: &[FunctionRevisionRecord],
    introductions: &BTreeMap<CatalogueRevisionId, RecoveredIntroduction>,
    active_catalogue_hash_context: &CatalogueHashContext,
) -> Result<(), PostgresKernelError> {
    let revisions = active_revisions
        .iter()
        .chain(historical_revisions)
        .map(|revision| (revision.id(), revision))
        .collect::<BTreeMap<_, _>>();

    for (catalogue_id, introduction) in introductions {
        if *catalogue_id == active_catalogue {
            continue;
        }

        let catalogue_record =
            DurableRecord::new("_orna_kernel.catalogue_revisions", catalogue_id.canonical());
        let catalogue_hash_context = catalogue_hash_context_for(
            introduction.catalogue_hash_version,
            introduction.standard_library_revision,
            active_catalogue_hash_context.standard(),
            &catalogue_record,
        )?;

        let (functions, function_origins) =
            load_catalogue_functions(transaction, *catalogue_id, &catalogue_hash_context).await?;
        let references = load_references(
            transaction,
            *catalogue_id,
            catalogue_hash_context
                .standard()
                .map(|standard| standard.revision()),
        )
        .await?;
        validate_reference_sources(&functions, &references)?;
        let semantics = load_catalogue_semantics(
            transaction,
            *catalogue_id,
            functions,
            function_origins,
            &catalogue_hash_context,
        )
        .await?;
        let mut current_revisions = Vec::with_capacity(semantics.catalogue.functions().len());
        for function in semantics.catalogue.functions() {
            let revision = revisions
                .get(&function.current_revision())
                .ok_or_else(|| {
                    DurableRecord::new(REVISION_RELATION, function.current_revision().canonical())
                        .invariant(
                            "every introducing catalogue function must resolve its immutable current revision",
                        )
                })?;
            if revision.function() != function.id() {
                return Err(
                    DurableRecord::new(REVISION_RELATION, revision.id().canonical()).invariant(
                        "introducing catalogue current revision must belong to its exact function",
                    ),
                );
            }
            current_revisions.push((*revision).clone());
        }

        let recovered = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(introduction.source.id(), *catalogue_id),
                introduction.source.clone(),
                semantics.catalogue,
                introduction.catalogue_hash,
                ActiveRevisionContent::new(
                    semantics.expressions,
                    current_revisions,
                    semantics.origins,
                    references,
                ),
            ),
            catalogue_hash_context,
        )
        .map_err(PostgresKernelError::RevisionInvariant)?;
        let computed = orna_core::canonical_hash::catalogue_digest_with_context(
            recovered.catalogue_hash_context(),
            recovered.catalogue(),
            recovered.function_revisions(),
            recovered.expressions(),
            recovered.origins(),
            recovered.references(),
        )
        .map_err(PostgresKernelError::CanonicalHash)?;
        if computed != introduction.catalogue_hash {
            return Err(DurableRecord::new(
                "_orna_kernel.catalogue_revisions",
                catalogue_id.canonical(),
            )
            .invariant(
                "introducing catalogue digest must match its complete recovered semantic catalogue",
            ));
        }
    }
    Ok(())
}

fn same_introduction(left: &IntroductionHeader, right: &IntroductionHeader) -> bool {
    left.catalogue == right.catalogue
        && left.catalogue_hash == right.catalogue_hash
        && left.source == right.source
        && left.source_parent == right.source_parent
        && left.source_hash == right.source_hash
        && left.bundle == right.bundle
        && left.bundle_hash == right.bundle_hash
        && left.catalogue_hash_version == right.catalogue_hash_version
        && left.standard_library_revision == right.standard_library_revision
}

fn validate_declaration(
    revision: &PendingRevision,
    source: &StoredSourceRevision,
) -> Result<(), PostgresKernelError> {
    let record = DurableRecord::new(REVISION_RELATION, revision.id.canonical());
    let origin = revision.declaration_origin;
    let unit = source
        .units()
        .iter()
        .find(|unit| unit.id() == origin.source_unit())
        .ok_or_else(|| {
            record.invariant(
                "historical declaration origin source unit must belong to its introducing source revision",
            )
        })?;
    let start = usize::try_from(origin.byte_start()).map_err(|_| {
        record.invariant("historical declaration origin start must fit the platform index")
    })?;
    let end = usize::try_from(origin.byte_end()).map_err(|_| {
        record.invariant("historical declaration origin end must fit the platform index")
    })?;
    if end > unit.content().len()
        || !unit.content().is_char_boundary(start)
        || !unit.content().is_char_boundary(end)
    {
        return Err(record.invariant(
            "historical declaration origin must be in bounds on exact UTF-8 character boundaries",
        ));
    }
    let declaration = unit
        .content()
        .as_bytes()
        .get(start..end)
        .ok_or_else(|| record.invariant("historical declaration byte range must exist"))?;
    let computed =
        function_declaration_digest(declaration).map_err(PostgresKernelError::CanonicalHash)?;
    if computed != revision.declaration_hash {
        return Err(record
            .invariant("function declaration hash must match the exact introducing source bytes"));
    }
    Ok(())
}

async fn load_references(
    transaction: &Transaction<'_>,
    catalogue: CatalogueRevisionId,
    expected_standard_library_revision: Option<StandardLibraryRevisionId>,
) -> Result<Vec<DefinitionReference>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT catalogue_revision_id, source_function_id,
                    source_function_revision_id, ordinal,
                    target_definition_id, target_kind, reference_kind,
                    source_subobject_id, target_owner_type_id,
                    target_owner_function_id, target_standard_library_revision_id,
                    target_enum_catalogue_revision_id,
                    source_unit_id, source_start, source_end
             FROM _orna_kernel.definition_references
             WHERE catalogue_revision_id = $1
             ORDER BY source_function_revision_id, ordinal",
            &[&catalogue.to_bytes().to_vec()],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut references = Vec::with_capacity(rows.len());
    let mut expected_ordinals = BTreeMap::<FunctionRevisionId, u32>::new();
    for (index, row) in rows.iter().enumerate() {
        let reference =
            decode_reference(row, index, catalogue, expected_standard_library_revision)?;
        let expected = expected_ordinals
            .entry(reference.source_revision())
            .or_default();
        if reference.ordinal() != *expected {
            return Err(DurableRecord::new(
                REFERENCE_RELATION,
                format!(
                    "revision={} ordinal={}",
                    reference.source_revision().canonical(),
                    reference.ordinal()
                ),
            )
            .invariant("definition reference ordinals must be contiguous from zero"));
        }
        *expected = expected.checked_add(1).ok_or_else(|| {
            DurableRecord::new(REFERENCE_RELATION, reference.source_revision().canonical())
                .invariant("definition reference ordinal count must fit u32")
        })?;
        references.push(reference);
    }
    Ok(references)
}

fn decode_reference(
    row: &Row,
    index: usize,
    catalogue: CatalogueRevisionId,
    expected_standard_library_revision: Option<StandardLibraryRevisionId>,
) -> Result<DefinitionReference, PostgresKernelError> {
    let row_record = DurableRecord::new(REFERENCE_RELATION, format!("row={index}"));
    require_catalogue(row, &row_record, catalogue, "reference")?;
    let source_function = FunctionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "source_function_id",
            "reference source function identity must be 16 bytes",
        )?,
        &row_record,
        "reference source function identity must be 16 bytes",
    )?);
    let source_revision = FunctionRevisionId::from_bytes(identity_bytes(
        row_record.column(
            row,
            "source_function_revision_id",
            "reference source revision identity must be 16 bytes",
        )?,
        &row_record,
        "reference source revision identity must be 16 bytes",
    )?);
    let ordinal = u32_from_i64(
        row_record.column(row, "ordinal", "reference ordinal must fit u32")?,
        &row_record,
        "reference ordinal must fit u32",
    )?;
    let record = DurableRecord::new(
        REFERENCE_RELATION,
        format!("revision={} ordinal={ordinal}", source_revision.canonical()),
    );
    let source_subobject: Option<Vec<u8>> = record.column(
        row,
        "source_subobject_id",
        "reference source subobject identity must be null",
    )?;
    if source_subobject.is_some() {
        return Err(record.invariant(
            "compiler-deployable definition references must not contain a stored source subobject",
        ));
    }
    let target_bytes = identity_bytes(
        record.column(
            row,
            "target_definition_id",
            "reference target identity must be 16 bytes",
        )?,
        &record,
        "reference target identity must be 16 bytes",
    )?;
    let owner_type = optional_identity_bytes(
        record.column(
            row,
            "target_owner_type_id",
            "reference target type owner must be null or 16 bytes",
        )?,
        &record,
        "reference target type owner must be null or 16 bytes",
    )?
    .map(TypeId::from_bytes);
    let owner_function = optional_identity_bytes(
        record.column(
            row,
            "target_owner_function_id",
            "reference target function owner must be null or 16 bytes",
        )?,
        &record,
        "reference target function owner must be null or 16 bytes",
    )?
    .map(FunctionId::from_bytes);
    let target_standard_library_revision = optional_identity_bytes(
        record.column(
            row,
            "target_standard_library_revision_id",
            "reference target standard library revision identity must be null or 16 bytes",
        )?,
        &record,
        "reference target standard library revision identity must be null or 16 bytes",
    )?
    .map(StandardLibraryRevisionId::from_bytes);
    let target_enum_catalogue_revision = optional_identity_bytes(
        record.column(
            row,
            "target_enum_catalogue_revision_id",
            "reference target enum catalogue revision identity must be null or 16 bytes",
        )?,
        &record,
        "reference target enum catalogue revision identity must be null or 16 bytes",
    )?
    .map(CatalogueRevisionId::from_bytes);
    let target_kind: String =
        record.column(row, "target_kind", "reference target kind must decode")?;
    let target = match (
        target_kind.as_str(),
        owner_type,
        owner_function,
        target_standard_library_revision,
        target_enum_catalogue_revision,
    ) {
        ("object_type", None, None, None, None) => {
            DefinitionReferenceTarget::ObjectType(TypeId::from_bytes(target_bytes))
        }
        ("field", Some(owner), None, None, None) => DefinitionReferenceTarget::Field {
            owner,
            field: orna_core::FieldId::from_bytes(target_bytes),
        },
        ("function", None, None, None, None) => {
            DefinitionReferenceTarget::Function(FunctionId::from_bytes(target_bytes))
        }
        ("parameter", None, Some(owner), None, None) => DefinitionReferenceTarget::Parameter {
            owner,
            parameter: ParameterId::from_bytes(target_bytes),
        },
        ("expression", None, None, None, None) => {
            DefinitionReferenceTarget::Expression(ExpressionId::from_bytes(target_bytes))
        }
        ("value_type", None, None, Some(revision), None)
            if Some(revision) == expected_standard_library_revision =>
        {
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(target_bytes))
        }
        ("enum_type", None, None, None, Some(revision)) if revision == catalogue => {
            DefinitionReferenceTarget::ValueType(TypeId::from_bytes(target_bytes))
        }
        _ => {
            return Err(record.invariant(
                "reference target kind and owner columns must form one exact owner-qualified target",
            ));
        }
    };
    let kind_name: String = record.column(row, "reference_kind", "reference kind must decode")?;
    let kind = decode_reference_kind(&kind_name, &record)?;
    if !reference_kind_matches_target(kind, target) {
        return Err(
            record.invariant("reference kind must be compatible with its exact target kind")
        );
    }
    let source_origin = decode_reference_origin(row, &record)?;
    Ok(DefinitionReference::new(
        source_function,
        source_revision,
        ordinal,
        target,
        kind,
        source_origin,
    ))
}

fn decode_reference_kind(
    name: &str,
    record: &DurableRecord,
) -> Result<DefinitionReferenceKind, PostgresKernelError> {
    exact_enum(
        name,
        SUPPORTED_REFERENCE_KINDS,
        record,
        "reference kind must be one exact supported semantic relation",
    )
}

const SUPPORTED_REFERENCE_KINDS: &[(&str, DefinitionReferenceKind)] = &[
    ("function_call", DefinitionReferenceKind::FunctionCall),
    ("named_type", DefinitionReferenceKind::NamedType),
    ("object_reference", DefinitionReferenceKind::ObjectReference),
    ("parameter_read", DefinitionReferenceKind::ParameterRead),
    ("query_object", DefinitionReferenceKind::QueryObject),
    ("query_field", DefinitionReferenceKind::QueryField),
    ("expression", DefinitionReferenceKind::Expression),
    ("write_object", DefinitionReferenceKind::WriteObject),
    ("write_field", DefinitionReferenceKind::WriteField),
];

fn decode_reference_origin(
    row: &Row,
    record: &DurableRecord,
) -> Result<SourceOrigin, PostgresKernelError> {
    let unit = SourceUnitId::from_bytes(identity_bytes(
        record.column(
            row,
            "source_unit_id",
            "reference source unit identity must be 16 bytes",
        )?,
        record,
        "reference source unit identity must be 16 bytes",
    )?);
    let start = u32_from_i64(
        record.column(row, "source_start", "reference source start must fit u32")?,
        record,
        "reference source start must fit u32",
    )?;
    let end = u32_from_i64(
        record.column(row, "source_end", "reference source end must fit u32")?,
        record,
        "reference source end must fit u32",
    )?;
    SourceOrigin::new(unit, start, end).map_err(PostgresKernelError::RevisionInvariant)
}

const fn reference_kind_matches_target(
    kind: DefinitionReferenceKind,
    target: DefinitionReferenceTarget,
) -> bool {
    matches!(
        (kind, target),
        (
            DefinitionReferenceKind::FunctionCall,
            DefinitionReferenceTarget::Function(_)
        ) | (
            DefinitionReferenceKind::NamedType
                | DefinitionReferenceKind::ObjectReference
                | DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceTarget::ValueType(_)
        ) | (
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter { .. }
        ) | (
            DefinitionReferenceKind::QueryField,
            DefinitionReferenceTarget::Field { .. }
        ) | (
            DefinitionReferenceKind::Expression,
            DefinitionReferenceTarget::Expression(_)
        ) | (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(_)
        ) | (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field { .. }
        )
    )
}

fn validate_reference_sources(
    functions: &[RecoveredFunction],
    references: &[DefinitionReference],
) -> Result<(), PostgresKernelError> {
    let current = functions
        .iter()
        .map(|function| {
            (
                function.definition.id(),
                function.definition.current_revision(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for reference in references {
        let record = DurableRecord::new(
            REFERENCE_RELATION,
            format!(
                "revision={} ordinal={}",
                reference.source_revision().canonical(),
                reference.ordinal()
            ),
        );
        if current.get(&reference.source_function()) != Some(&reference.source_revision()) {
            return Err(record.invariant(
                "reference source function and revision must be the active current pair",
            ));
        }
    }
    Ok(())
}

fn require_catalogue(
    row: &Row,
    record: &DurableRecord,
    expected: CatalogueRevisionId,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    let catalogue = CatalogueRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "catalogue_revision_id",
            "function catalogue member revision identity must be 16 bytes",
        )?,
        record,
        "function catalogue member revision identity must be 16 bytes",
    )?);
    if catalogue != expected {
        return Err(record.invariant(match member {
            "function" => "function must belong to the selected catalogue revision",
            "parameter" => "parameter must belong to the selected catalogue revision",
            "return column" => "return column must belong to the selected catalogue revision",
            _ => "reference must belong to the selected catalogue revision",
        }));
    }
    Ok(())
}

fn parameter_record(function: FunctionId, parameter: ParameterId) -> DurableRecord {
    DurableRecord::new(
        PARAMETER_RELATION,
        format!(
            "function={} parameter={}",
            function.canonical(),
            parameter.canonical()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_kind_decoder_maps_all_supported_spellings_exactly() {
        let record = DurableRecord::new(REFERENCE_RELATION, "test");
        let expected = [
            ("function_call", DefinitionReferenceKind::FunctionCall),
            ("named_type", DefinitionReferenceKind::NamedType),
            ("object_reference", DefinitionReferenceKind::ObjectReference),
            ("parameter_read", DefinitionReferenceKind::ParameterRead),
            ("query_object", DefinitionReferenceKind::QueryObject),
            ("query_field", DefinitionReferenceKind::QueryField),
            ("expression", DefinitionReferenceKind::Expression),
            ("write_object", DefinitionReferenceKind::WriteObject),
            ("write_field", DefinitionReferenceKind::WriteField),
        ];

        assert_eq!(SUPPORTED_REFERENCE_KINDS, expected.as_slice());
        for (name, kind) in expected {
            assert_eq!(decode_reference_kind(name, &record).unwrap(), kind);
        }
    }

    #[test]
    fn reference_kind_decoder_rejects_unknown_spellings() {
        let record = DurableRecord::new(REFERENCE_RELATION, "test");

        assert!(decode_reference_kind("write_Object", &record).is_err());
        assert!(decode_reference_kind("insert", &record).is_err());
    }

    #[test]
    fn write_reference_kinds_require_their_exact_targets() {
        let object = TypeId::from_bytes([1; 16]);
        let field = orna_core::FieldId::from_bytes([2; 16]);

        assert!(reference_kind_matches_target(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(object),
        ));
        assert!(reference_kind_matches_target(
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field {
                owner: object,
                field,
            },
        ));
        assert!(!reference_kind_matches_target(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::Field {
                owner: object,
                field,
            },
        ));
        assert!(!reference_kind_matches_target(
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::ObjectType(object),
        ));
    }

    #[test]
    fn named_type_references_accept_only_value_type_targets_in_the_new_family() {
        let value_type = TypeId::from_bytes([3; 16]);

        assert!(reference_kind_matches_target(
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceTarget::ValueType(value_type),
        ));
        assert!(!reference_kind_matches_target(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ValueType(value_type),
        ));
    }

    #[test]
    fn void_scalar_is_reserved_for_single_function_returns() {
        let record = DurableRecord::new(PARAMETER_RELATION, "function=test parameter=test");
        let single_kind = decode_legacy_resolved_type_tuple_kind(
            Some("scalar"),
            &record,
            LegacyResolvedTypeTupleMember::SingleReturn,
        )
        .expect("SINGLE scalar kind");
        let parameter_kind = decode_legacy_resolved_type_tuple_kind(
            Some("scalar"),
            &record,
            LegacyResolvedTypeTupleMember::Parameter,
        )
        .expect("parameter scalar kind");

        assert_eq!(
            decode_legacy_resolved_type_tuple(
                single_kind,
                Some("void"),
                None,
                &record,
                LegacyResolvedTypeTupleMember::SingleReturn,
            )
            .expect("SINGLE return void"),
            ResolvedType::scalar(StandardScalar::Void)
        );
        assert!(matches!(
            decode_legacy_resolved_type_tuple(
                parameter_kind,
                Some("void"),
                None,
                &record,
                LegacyResolvedTypeTupleMember::Parameter,
            ),
            Err(PostgresKernelError::DurableInvariant {
                relation: PARAMETER_RELATION,
                record,
                rule: "void is valid only as a SINGLE function return, never as a parameter or ROWS column",
            }) if record == "function=test parameter=test"
        ));
    }
}
