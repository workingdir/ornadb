//! Exact compiler-to-runtime source catalogue projection.
//!
//! This module deliberately consumes only facts retained by
//! `ResolvedSourceCatalogue`; it never derives identities from source text or
//! allocates runtime object identifiers.

use std::collections::HashMap;

use orna_compiler::ResolvedSourceCatalogue;
use orna_core::{
    catalogue::{FunctionReturn, TypeDeclarationWitness},
    catalogue_diff::SemanticChange,
    types::ResolvedType,
};
use orna_foundation_v1::CwdCapture;
use orna_runtime_v1::{
    CatalogueAdmission, CatalogueDeclaration, CatalogueFunctionDeclaration, CatalogueObjectKind,
    CatalogueParameterDeclaration, CatalogueTypeDeclaration, CatalogueTypeSpec,
    RequestActivationCommit, RuntimeState, TableActivationError,
    ValidatedTableRequestActivationCommit,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueProjectionError {
    TypeWitness(String),
    MissingType(String),
    UnsupportedType { function: String, type_name: String },
    UnsupportedReturn { function: String },
    ParameterDefault { function: String, parameter: String },
    MissingFunctionRevision(String),
}

#[derive(Debug)]
pub enum SourceCatalogueActivationError {
    Projection(CatalogueProjectionError),
    Runtime(TableActivationError),
}

impl From<CatalogueProjectionError> for SourceCatalogueActivationError {
    fn from(error: CatalogueProjectionError) -> Self {
        Self::Projection(error)
    }
}

impl From<TableActivationError> for SourceCatalogueActivationError {
    fn from(error: TableActivationError) -> Self {
        Self::Runtime(error)
    }
}

fn rename_for_type(catalogue: &ResolvedSourceCatalogue, id: orna_core::TypeId) -> Option<String> {
    catalogue
        .diff()
        .changes()
        .iter()
        .find_map(|change| match change {
            SemanticChange::ObjectTypeRenamed {
                id: changed, from, ..
            } if *changed == id => Some(from.clone()),
            _ => None,
        })
}

fn rename_for_function(
    catalogue: &ResolvedSourceCatalogue,
    id: orna_core::FunctionId,
) -> Option<String> {
    catalogue
        .diff()
        .changes()
        .iter()
        .find_map(|change| match change {
            SemanticChange::FunctionRenamed {
                id: changed, from, ..
            } if *changed == id => Some(from.clone()),
            _ => None,
        })
}

fn witness_name(
    witnesses: &HashMap<orna_core::TypeId, &TypeDeclarationWitness>,
    ty: ResolvedType,
) -> Result<(String, CatalogueTypeSpec), CatalogueProjectionError> {
    let (id, form) = match ty {
        ResolvedType::Named(id) => (id, CatalogueTypeSpec::Named),
        ResolvedType::Reference { target } => (
            target,
            CatalogueTypeSpec::Reference {
                target: String::new(),
            },
        ),
        other => {
            return Err(CatalogueProjectionError::UnsupportedType {
                function: String::new(),
                type_name: format!("{other:?}"),
            });
        }
    };
    let witness = witnesses
        .get(&id)
        .ok_or_else(|| CatalogueProjectionError::MissingType(format!("{id}")))?;
    let name = witness.qualified_name().to_string();
    let form = match form {
        CatalogueTypeSpec::Reference { .. } => CatalogueTypeSpec::Reference {
            target: name.clone(),
        },
        other => other,
    };
    Ok((name, form))
}

/// Projects a compiler candidate into the runtime's source-only admission.
///
/// `predecessor_capture` is caller-supplied and passed through unchanged,
/// including `None` for the first catalogue admission. The
/// projection is closed over witnessed object types and newly compiled
/// functions; scalar, value, stream, row, default, missing-standard, and
/// unwitnessed forms fail closed.
pub fn project_source_catalogue(
    catalogue: &ResolvedSourceCatalogue,
    predecessor_capture: Option<CwdCapture>,
) -> Result<CatalogueAdmission, CatalogueProjectionError> {
    if let Some(error) = catalogue.type_witness_errors().first() {
        return Err(CatalogueProjectionError::TypeWitness(error.to_string()));
    }
    let witnesses: HashMap<_, _> = catalogue
        .type_witnesses()
        .iter()
        .map(|w| (w.type_id(), w))
        .collect();

    let types = catalogue
        .type_witnesses()
        .iter()
        .map(|w| CatalogueTypeDeclaration {
            declaration: CatalogueDeclaration {
                qualified_name: w.qualified_name().to_string(),
                kind: CatalogueObjectKind::Type,
                revision_id: w.revision_id(),
                semantic_hash: w.semantic_hash(),
                rename_from: rename_for_type(catalogue, w.type_id()),
            },
            form: CatalogueTypeSpec::Named,
        })
        .collect();

    let mut functions = Vec::new();
    for revision in catalogue.new_function_revisions() {
        let definition = catalogue
            .candidate()
            .function_by_id(revision.function())
            .ok_or_else(|| {
                CatalogueProjectionError::MissingFunctionRevision(format!(
                    "{}",
                    revision.function()
                ))
            })?;
        let function_name = definition.name().to_string();
        let parameters = definition
            .parameters()
            .iter()
            .map(|parameter| {
                if parameter.default_expression().is_some() {
                    return Err(CatalogueProjectionError::ParameterDefault {
                        function: function_name.clone(),
                        parameter: parameter.name().to_owned(),
                    });
                }
                let (type_name, _) = witness_name(&witnesses, parameter.resolved_type()).map_err(
                    |error| match error {
                        CatalogueProjectionError::UnsupportedType { type_name, .. } => {
                            CatalogueProjectionError::UnsupportedType {
                                function: function_name.clone(),
                                type_name,
                            }
                        }
                        other => other,
                    },
                )?;
                Ok(CatalogueParameterDeclaration {
                    name: parameter.name().to_owned(),
                    position: u64::from(parameter.ordinal()),
                    type_name,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result_type = match definition.return_type() {
            FunctionReturn::Single(ty) => {
                witness_name(&witnesses, *ty)
                    .map_err(|error| match error {
                        CatalogueProjectionError::UnsupportedType { type_name, .. } => {
                            CatalogueProjectionError::UnsupportedType {
                                function: function_name.clone(),
                                type_name,
                            }
                        }
                        other => other,
                    })?
                    .0
            }
            FunctionReturn::Stream(_) | FunctionReturn::Rows(_) => {
                return Err(CatalogueProjectionError::UnsupportedReturn {
                    function: function_name,
                });
            }
        };
        functions.push(CatalogueFunctionDeclaration {
            declaration: CatalogueDeclaration {
                qualified_name: function_name,
                kind: CatalogueObjectKind::Function,
                // The runtime uses a 32-byte immutable declaration-content
                // witness; this is retained directly by the compiler revision.
                revision_id: revision.declaration_content_hash().to_bytes(),
                semantic_hash: revision.semantic_hash().to_bytes(),
                rename_from: rename_for_function(catalogue, revision.function()),
            },
            parameters,
            result_type_name: result_type,
        });
    }

    Ok(CatalogueAdmission {
        predecessor_capture,
        types,
        functions,
    })
}

/// Projects and atomically commits one resolved source catalogue as part of
/// an already-admitted request activation.
///
/// The request carries the caller's writer, request, activation, staged table
/// mutations, validator, terminal outcome, and fault policy. This bridge only
/// supplies the source catalogue admission and binds its predecessor to the
/// exact activation capture; it does not derive request, object, reference,
/// or type evidence. Unsupported source facts remain projection errors.
pub async fn commit_resolved_source_catalogue_activation(
    runtime: &RuntimeState,
    catalogue: &ResolvedSourceCatalogue,
    request: ValidatedTableRequestActivationCommit<'_>,
) -> Result<RequestActivationCommit, SourceCatalogueActivationError> {
    let admission = project_source_catalogue(catalogue, Some(request.context.capture().clone()))?;
    runtime
        .commit_validated_catalogue_table_request_activation(request, &admission)
        .await
        .map_err(SourceCatalogueActivationError::Runtime)
}
