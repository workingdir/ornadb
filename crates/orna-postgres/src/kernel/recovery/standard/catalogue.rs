//! Standard-library catalogue member recovery.
#[path = "catalogue/functions.rs"]
mod functions;
#[path = "catalogue/types.rs"]
mod types;
use functions::{load_standard_functions, load_standard_parameters};
#[cfg(test)]
pub(in super::super) use types::{
    decode_standard_binding_target, recovered_standard_value_definition,
};
use types::{
    load_standard_enum_types, load_standard_schemas, load_standard_type_bindings,
    load_standard_value_types,
};

use super::*;

pub(super) async fn load_standard_catalogue(
    transaction: &Transaction<'_>,
    header: &RecoveredStandardHeader,
) -> Result<(CatalogueSnapshot, Vec<DefinitionOrigin>), PostgresKernelError> {
    let schemas = load_standard_schemas(transaction, header.revision).await?;
    let value_types = load_standard_value_types(transaction, header.revision).await?;
    let value_type_ids = value_types
        .iter()
        .map(|value_type| value_type.definition.id())
        .collect::<HashSet<_>>();
    let enum_types = load_standard_enum_types(transaction, header.revision).await?;
    let bindings = load_standard_type_bindings(transaction, header.revision).await?;
    let functions = load_standard_functions(transaction, header.revision, &value_type_ids).await?;
    let parameters =
        load_standard_parameters(transaction, header.revision, &value_type_ids).await?;

    let schema_names = schemas
        .iter()
        .map(|schema| (schema.definition.id(), schema.definition.name().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut origins = Vec::with_capacity(
        schemas.len()
            + value_types.len()
            + enum_types.len()
            + bindings.len()
            + functions.len()
            + parameters.len(),
    );
    let schemas = schemas
        .into_iter()
        .map(|schema| {
            origins.push(schema.origin);
            schema.definition
        })
        .collect::<Vec<_>>();
    let mut definitions = Vec::with_capacity(value_types.len());
    for value_type in value_types {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_value_types",
            value_type.definition.id().canonical(),
        );
        require_standard_definition_schema(
            &record,
            &schema_names,
            value_type.schema,
            value_type.definition.name(),
            "standard value type schema identity must identify a recovered schema",
            "standard value type qualified name must contain a schema namespace",
            "standard value type schema identity must equal the schema named by its namespace",
        )?;
        origins.push(value_type.origin);
        definitions.push(value_type.definition);
    }
    let mut enum_definitions = Vec::with_capacity(enum_types.len());
    for enum_type in enum_types {
        let record = DurableRecord::new(
            "_orna_kernel.standard_catalogue_enum_types",
            enum_type.definition.id().canonical(),
        );
        require_standard_definition_schema(
            &record,
            &schema_names,
            enum_type.schema,
            enum_type.definition.name(),
            "standard enum schema identity must identify a recovered schema",
            "standard enum qualified name must contain a schema namespace",
            "standard enum schema identity must equal the schema named by its namespace",
        )?;
        origins.push(enum_type.origin);
        enum_definitions.push(enum_type.definition);
    }
    let bindings = bindings
        .into_iter()
        .map(|binding| {
            origins.push(binding.origin);
            binding.binding
        })
        .collect::<Vec<_>>();
    let function_definitions = functions
        .into_iter()
        .map(|function| {
            let record = DurableRecord::new(
                "_orna_kernel.standard_catalogue_functions",
                function.id.canonical(),
            );
            require_standard_definition_schema(
                &record,
                &schema_names,
                function.schema,
                &function.name,
                "standard function schema identity must identify a recovered schema",
                "standard function qualified name must contain a schema namespace",
                "standard function schema identity must equal the schema named by its namespace",
            )?;
            let recovered_parameters = parameters.get(&function.id).cloned().unwrap_or_default();
            let definition = FunctionDefinition::new(
                function.id,
                function.name,
                function.domain,
                recovered_parameters
                    .iter()
                    .map(|parameter| parameter.definition.clone())
                    .collect(),
                function.return_type,
                function.current_revision,
                function.security,
                function.transaction,
                function.volatility,
            );
            origins.push(function.origin);
            origins.extend(
                recovered_parameters
                    .into_iter()
                    .map(|parameter| parameter.origin),
            );
            Ok(definition)
        })
        .collect::<Result<Vec<_>, PostgresKernelError>>()?;
    let catalogue = CatalogueSnapshot::new_with_functions_and_enum_types(
        header.catalogue,
        schemas,
        Vec::new(),
        definitions,
        enum_definitions,
        bindings,
        function_definitions,
    )
    .map_err(PostgresKernelError::CatalogueSnapshot)?;
    Ok((catalogue, origins))
}

#[allow(clippy::too_many_arguments)]
fn require_standard_definition_schema(
    record: &DurableRecord,
    schema_names: &BTreeMap<SchemaId, QualifiedSemanticName>,
    schema: SchemaId,
    name: &QualifiedSemanticName,
    missing_schema_rule: &'static str,
    missing_namespace_rule: &'static str,
    mismatch_rule: &'static str,
) -> Result<(), PostgresKernelError> {
    let schema_name = schema_names
        .get(&schema)
        .ok_or_else(|| record.invariant(missing_schema_rule))?;
    let name_parts = name.parts();
    let namespace = name_parts
        .get(..name_parts.len().saturating_sub(1))
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| record.invariant(missing_namespace_rule))?;
    if namespace != schema_name.parts() {
        return Err(record.invariant(mismatch_rule));
    }
    Ok(())
}
