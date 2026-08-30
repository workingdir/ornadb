use super::*;

#[derive(Default)]
pub(crate) struct ReservedStandardIds {
    pub(super) catalogues: HashSet<CatalogueRevisionId>,
    pub(super) source_bundles: HashSet<SourceBundleId>,
    pub(super) source_revisions: HashSet<SourceRevisionId>,
    pub(super) source_units: HashSet<SourceUnitId>,
    pub(super) schemas: HashSet<SchemaId>,
    pub(super) types: HashSet<TypeId>,
}

impl ReservedStandardIds {
    pub(crate) fn from_snapshot(snapshot: &VerifiedStandardLibrarySnapshot) -> Self {
        let mut result = Self::default();
        result.catalogues.insert(snapshot.catalogue().revision());
        result.source_bundles.insert(snapshot.source().bundle());
        result.source_revisions.insert(snapshot.source().id());
        result
            .source_units
            .extend(snapshot.source().units().iter().map(StoredSourceUnit::id));
        result.schemas.extend(
            snapshot
                .catalogue()
                .schemas()
                .iter()
                .map(SchemaDefinition::id),
        );
        result.types.extend(
            snapshot
                .catalogue()
                .object_types()
                .iter()
                .map(ObjectTypeDefinition::id)
                .chain(
                    snapshot
                        .catalogue()
                        .value_types()
                        .iter()
                        .map(|value| value.id()),
                ),
        );
        result
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CandidateIdSource {
    pub(crate) catalogue_revision: fn() -> CatalogueRevisionId,
    pub(crate) source_bundle: fn() -> SourceBundleId,
    pub(crate) source_revision: fn() -> SourceRevisionId,
    pub(crate) source_unit: fn() -> SourceUnitId,
    pub(crate) schema: fn() -> SchemaId,
    pub(crate) type_id: fn() -> TypeId,
    pub(crate) function_revision: fn() -> FunctionRevisionId,
}

impl CandidateIdSource {
    const RANDOM: Self = Self {
        catalogue_revision: CatalogueRevisionId::new,
        source_bundle: SourceBundleId::new,
        source_revision: SourceRevisionId::new,
        source_unit: SourceUnitId::new,
        schema: SchemaId::new,
        type_id: TypeId::new,
        function_revision: FunctionRevisionId::new,
    };
}

pub(crate) struct CandidateAllocator {
    reserved: Option<ReservedStandardIds>,
    source: CandidateIdSource,
    standard_source_seed: Option<StandardSourceBoundary>,
}

#[derive(Clone, Copy)]
struct StandardSourceBoundary {
    catalogue_revision: CatalogueRevisionId,
    source_bundle: SourceBundleId,
    source_revision: SourceRevisionId,
    source_unit: SourceUnitId,
}

impl CandidateAllocator {
    pub(super) const fn legacy() -> Self {
        Self {
            reserved: None,
            source: CandidateIdSource::RANDOM,
            standard_source_seed: None,
        }
    }

    #[cfg(test)]
    pub(super) fn legacy_with_source(source: CandidateIdSource) -> Self {
        Self {
            reserved: None,
            source,
            standard_source_seed: None,
        }
    }

    pub(super) fn standard(snapshot: &VerifiedStandardLibrarySnapshot) -> Self {
        Self::with_source(
            ReservedStandardIds::from_snapshot(snapshot),
            CandidateIdSource::RANDOM,
        )
    }

    pub(super) fn standard_source(
        snapshot: &VerifiedStandardLibrarySnapshot,
        seed: &StandardSourceIdentitySeed,
    ) -> Self {
        Self {
            reserved: Some(ReservedStandardIds::from_snapshot(snapshot)),
            source: CandidateIdSource::RANDOM,
            standard_source_seed: Some(StandardSourceBoundary {
                catalogue_revision: seed.catalogue_revision,
                source_bundle: seed.source_bundle,
                source_revision: seed.source_revision,
                source_unit: seed
                    .source_units
                    .first()
                    .copied()
                    .expect("standard source seed requires one source unit"),
            }),
        }
    }

    pub(crate) fn with_source(reserved: ReservedStandardIds, source: CandidateIdSource) -> Self {
        Self {
            reserved: Some(reserved),
            source,
            standard_source_seed: None,
        }
    }
    pub(super) fn catalogue_revision(&mut self) -> CatalogueRevisionId {
        if let Some(seed) = self.standard_source_seed {
            return seed.catalogue_revision;
        }
        loop {
            let id = (self.source.catalogue_revision)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.catalogues.contains(&id))
            {
                return id;
            }
        }
    }

    pub(super) fn source_bundle(&mut self) -> SourceBundleId {
        if let Some(seed) = self.standard_source_seed {
            return seed.source_bundle;
        }
        loop {
            let id = (self.source.source_bundle)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.source_bundles.contains(&id))
            {
                return id;
            }
        }
    }

    pub(super) fn source_revision(&mut self) -> SourceRevisionId {
        if let Some(seed) = self.standard_source_seed {
            return seed.source_revision;
        }
        loop {
            let id = (self.source.source_revision)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.source_revisions.contains(&id))
            {
                return id;
            }
        }
    }

    pub(super) fn source_unit(&mut self) -> SourceUnitId {
        if let Some(seed) = self.standard_source_seed {
            return seed.source_unit;
        }
        loop {
            let id = (self.source.source_unit)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.source_units.contains(&id))
            {
                return id;
            }
        }
    }
    pub(super) fn schema(&mut self) -> SchemaId {
        loop {
            let id = (self.source.schema)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.schemas.contains(&id))
            {
                return id;
            }
        }
    }

    pub(super) fn type_id(&mut self) -> TypeId {
        loop {
            let id = (self.source.type_id)();
            if self
                .reserved
                .as_ref()
                .is_none_or(|reserved| !reserved.types.contains(&id))
                && INVOCATION_CARRIERS.iter().all(|carrier| carrier.id() != id)
            {
                return id;
            }
        }
    }

    pub(super) fn function_revision(&mut self) -> FunctionRevisionId {
        (self.source.function_revision)()
    }
}

#[derive(Clone, Default)]
pub(super) struct IdentityMap {
    pub(super) schemas: HashMap<CheckedSchemaId, SchemaId>,
    pub(super) types: HashMap<CheckedTypeId, TypeId>,
    pub(super) fields: HashMap<CheckedFieldId, FieldId>,
    pub(super) expressions: HashMap<CheckedExpressionId, ExpressionId>,
    pub(super) functions: HashMap<CheckedFunctionId, FunctionId>,
    pub(super) parameters: HashMap<CheckedParameterId, ParameterId>,
}

impl IdentityMap {
    pub(super) fn build_generic(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        allocations: &mut CandidateAllocator,
    ) -> Result<Self, PrepareError> {
        Self::build(checked, active, allocations, None, true, None)
    }

    pub(super) fn build_legacy(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        allocations: &mut CandidateAllocator,
    ) -> Result<Self, PrepareError> {
        Self::build(checked, active, allocations, None, true, None)
    }

    pub(super) fn build_standard(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        allocations: &mut CandidateAllocator,
        function_identities: &ValidatedFunctionIdentities,
        source_seed: Option<&StandardSourceIdentitySeed>,
    ) -> Result<Self, PrepareError> {
        Self::build(
            checked,
            active,
            allocations,
            Some(function_identities),
            true,
            source_seed,
        )
    }

    pub(super) fn build_matching_active(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        function_identities: &ValidatedFunctionIdentities,
    ) -> Result<Self, PrepareError> {
        let mut no_allocations = CandidateAllocator::legacy();
        Self::build(
            checked,
            active,
            &mut no_allocations,
            Some(function_identities),
            false,
            None,
        )
    }

    fn build(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        allocations: &mut CandidateAllocator,
        function_identities: Option<&ValidatedFunctionIdentities>,
        allow_provisional: bool,
        source_seed: Option<&StandardSourceIdentitySeed>,
    ) -> Result<Self, PrepareError> {
        Self::validate_existing(checked, active, function_identities.is_none())?;
        let mut result = Self::default();
        for schema in checked.schemas() {
            let id = match schema.id() {
                CheckedSchemaId::Existing(id) => id,
                CheckedSchemaId::Provisional(_) if let Some(seed) = source_seed => seed.schema,
                CheckedSchemaId::Provisional(_) if allow_provisional => allocations.schema(),
                CheckedSchemaId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional schema",
                    });
                }
            };
            insert_unique(
                &mut result.schemas,
                schema.id(),
                id,
                "duplicate checked schema",
            )?;
        }

        for object_type in checked.object_types() {
            let type_id = match object_type.id() {
                CheckedTypeId::Existing(id) => id,
                CheckedTypeId::Provisional(_) if allow_provisional => allocations.type_id(),
                CheckedTypeId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional object type",
                    });
                }
            };
            insert_unique(
                &mut result.types,
                object_type.id(),
                type_id,
                "duplicate checked object type",
            )?;

            for field in object_type.fields() {
                let field_id = match field.id() {
                    CheckedFieldId::Existing(id) => id,
                    CheckedFieldId::Provisional(_) if allow_provisional => FieldId::new(),
                    CheckedFieldId::Provisional(_) => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "matched active source contains a provisional field",
                        });
                    }
                };
                insert_consistent(
                    &mut result.fields,
                    field.id(),
                    field_id,
                    "checked field identity maps inconsistently",
                )?;

                if let Some(default) = field.default() {
                    let expression_id = match default.id() {
                        CheckedExpressionId::Existing(id) => id,
                        CheckedExpressionId::Provisional(_) if allow_provisional => {
                            ExpressionId::new()
                        }
                        CheckedExpressionId::Provisional(_) => {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "matched active source contains a provisional expression",
                            });
                        }
                    };
                    insert_consistent(
                        &mut result.expressions,
                        default.id(),
                        expression_id,
                        "checked expression identity maps inconsistently",
                    )?;
                }
            }
        }

        for (checked_id, _, _, _) in checked.enum_types() {
            let type_id = match checked_id {
                CheckedTypeId::Existing(id) => id,
                CheckedTypeId::Provisional(_) if allow_provisional => allocations.type_id(),
                CheckedTypeId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional enum type",
                    });
                }
            };
            insert_unique(
                &mut result.types,
                checked_id,
                type_id,
                "duplicate checked enum type",
            )?;
        }

        for record_value_type in checked.record_value_types() {
            let type_id = match record_value_type.id() {
                CheckedTypeId::Existing(id) => id,
                CheckedTypeId::Provisional(_) if allow_provisional => allocations.type_id(),
                CheckedTypeId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional record value type",
                    });
                }
            };
            insert_unique(
                &mut result.types,
                record_value_type.id(),
                type_id,
                "duplicate checked record value type",
            )?;
            for field in record_value_type.fields() {
                let field_id = match field.id() {
                    CheckedFieldId::Existing(id) => id,
                    CheckedFieldId::Provisional(_) if allow_provisional => FieldId::new(),
                    CheckedFieldId::Provisional(_) => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "matched active source contains a provisional record value field",
                        });
                    }
                };
                insert_consistent(
                    &mut result.fields,
                    field.id(),
                    field_id,
                    "checked record value field identity maps inconsistently",
                )?;
            }
        }

        match function_identities {
            None => {
                for function in checked.server_functions() {
                    Self::map_server_function(&mut result, function, true, allow_provisional)?;
                }
                for function in checked.client_functions() {
                    let function_id = match function.id() {
                        CheckedFunctionId::Existing(id) => id,
                        CheckedFunctionId::Provisional(_) if let Some(seed) = source_seed => seed
                            .functions
                            .get(result.functions.len())
                            .copied()
                            .ok_or(PrepareError::InvalidCheckedBundle {
                                reason: "standard source function seed count does not match checked source",
                            })?,
                        CheckedFunctionId::Provisional(_) if allow_provisional => FunctionId::new(),
                        CheckedFunctionId::Provisional(_) => {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "matched active source contains a provisional function",
                            });
                        }
                    };
                    insert_unique(
                        &mut result.functions,
                        function.id(),
                        function_id,
                        "duplicate checked function",
                    )?;
                    for (parameter_index, parameter) in function.parameters().iter().enumerate() {
                        let parameter_id = match parameter.id() {
                            CheckedParameterId::Existing(id) => id,
                            CheckedParameterId::Provisional(_) if let Some(seed) = source_seed => seed
                                .parameters
                                .get(result.functions.len() - 1)
                                .and_then(|parameters| parameters.get(parameter_index))
                                .copied()
                                .ok_or(PrepareError::InvalidCheckedBundle {
                                    reason: "standard source parameter seed shape does not match checked source",
                                })?,
                            CheckedParameterId::Provisional(_) if allow_provisional => {
                                ParameterId::new()
                            }
                            CheckedParameterId::Provisional(_) => {
                                return Err(PrepareError::InvalidCheckedBundle {
                                    reason: "matched active source contains a provisional parameter",
                                });
                            }
                        };
                        insert_consistent(
                            &mut result.parameters,
                            parameter.id(),
                            parameter_id,
                            "checked parameter identity maps inconsistently",
                        )?;
                    }
                }
            }
            Some(function_identities) => {
                for owner in function_identities.order() {
                    match function_identities.domain(*owner)? {
                        FunctionDomain::Server => {
                            let function = checked
                                .server_functions()
                                .iter()
                                .find(|function| function.id() == *owner)
                                .ok_or(PrepareError::InvalidCheckedBundle {
                                    reason: "checked standard function owners do not match declaration evidence",
                                })?;
                            Self::map_server_function(
                                &mut result,
                                function,
                                false,
                                allow_provisional,
                            )?;
                        }
                        FunctionDomain::Client => {
                            let function = checked
                                .client_functions()
                                .iter()
                                .find(|function| function.id() == *owner)
                                .ok_or(PrepareError::InvalidCheckedBundle {
                                    reason: "checked standard function owners do not match declaration evidence",
                                })?;
                            let function_index = function_identities
                                .order()
                                .iter()
                                .position(|candidate| candidate == owner)
                                .ok_or(PrepareError::InvalidCheckedBundle {
                                    reason: "standard source function order is incomplete",
                                })?;
                            let function_id = match function.id() {
                                CheckedFunctionId::Existing(id) => id,
                                CheckedFunctionId::Provisional(_) if let Some(seed) = source_seed => seed
                                    .functions
                                    .get(function_index)
                                    .copied()
                                    .ok_or(PrepareError::InvalidCheckedBundle {
                                        reason: "standard source function seed does not match checked source",
                                    })?,
                                CheckedFunctionId::Provisional(_) if allow_provisional => {
                                    FunctionId::new()
                                }
                                CheckedFunctionId::Provisional(_) => {
                                    return Err(PrepareError::InvalidCheckedBundle {
                                        reason: "matched active source contains a provisional function",
                                    });
                                }
                            };
                            result.functions.insert(function.id(), function_id);
                            for (parameter_index, parameter) in function.parameters().iter().enumerate() {
                                let parameter_id = match parameter.id() {
                                    CheckedParameterId::Existing(id) => id,
                                    CheckedParameterId::Provisional(_) if let Some(seed) = source_seed => seed
                                        .parameters
                                        .get(function_index)
                                        .and_then(|parameters| parameters.get(parameter_index))
                                        .copied()
                                        .ok_or(PrepareError::InvalidCheckedBundle {
                                            reason: "standard source parameter seed shape does not match checked source",
                                        })?,
                                    CheckedParameterId::Provisional(_) if allow_provisional => {
                                        ParameterId::new()
                                    }
                                    CheckedParameterId::Provisional(_) => {
                                        return Err(PrepareError::InvalidCheckedBundle {
                                            reason: "matched active source contains a provisional parameter",
                                        });
                                    }
                                };
                                insert_consistent(
                                    &mut result.parameters,
                                    parameter.id(),
                                    parameter_id,
                                    "checked parameter identity maps inconsistently",
                                )?;
                            }
                        }
                    }
                }
            }
        }
        // CLIENT expressions may target functions that are already installed
        // and therefore are not part of this checked bundle.  Their stable
        // function and parameter identities still have to be present in the
        // map before resource (or ordinary CLIENT-call) lowering can emit a
        // durable artifact.
        for function in checked.client_functions() {
            for reference in function.references() {
                let CheckedDefinitionReferenceTarget::Function(CheckedFunctionId::Existing(
                    function_id,
                )) = reference.target()
                else {
                    continue;
                };
                let active_function = active
                    .catalogue()
                    .function_by_id(function_id)
                    .or_else(|| {
                        active
                            .catalogue_hash_context()
                            .standard()
                            .and_then(|standard| standard.catalogue().function_by_id(function_id))
                    })
                    .ok_or_else(|| existing_mismatch(DefinitionIdentity::Function(function_id)))?;
                result
                    .functions
                    .entry(CheckedFunctionId::Existing(function_id))
                    .or_insert(function_id);
                for parameter in active_function.parameters() {
                    result
                        .parameters
                        .entry(CheckedParameterId::Existing(parameter.id()))
                        .or_insert(parameter.id());
                }
            }
        }

        Ok(result)
    }

    fn map_server_function(
        result: &mut Self,
        function: &crate::CheckedServerFunction,
        reject_duplicate: bool,
        allow_provisional: bool,
    ) -> Result<(), PrepareError> {
        let function_id = match function.id() {
            CheckedFunctionId::Existing(id) => id,
            CheckedFunctionId::Provisional(_) if allow_provisional => FunctionId::new(),
            CheckedFunctionId::Provisional(_) => {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "matched active source contains a provisional function",
                });
            }
        };
        if reject_duplicate {
            insert_unique(
                &mut result.functions,
                function.id(),
                function_id,
                "duplicate checked function",
            )?;
        } else {
            result.functions.insert(function.id(), function_id);
        }

        for parameter in function.parameters() {
            let parameter_id = match parameter.id() {
                CheckedParameterId::Existing(id) => id,
                CheckedParameterId::Provisional(_) if allow_provisional => ParameterId::new(),
                CheckedParameterId::Provisional(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "matched active source contains a provisional parameter",
                    });
                }
            };
            insert_consistent(
                &mut result.parameters,
                parameter.id(),
                parameter_id,
                "checked parameter identity maps inconsistently",
            )?;
        }
        Ok(())
    }

    fn validate_existing(
        checked: &CheckedBundle,
        active: &ActiveDatabaseRevision,
        validate_legacy_server_functions: bool,
    ) -> Result<(), PrepareError> {
        for schema in checked.schemas() {
            let CheckedSchemaId::Existing(id) = schema.id() else {
                continue;
            };
            let matches = active
                .catalogue()
                .schema_by_id(id)
                .is_some_and(|base| base.name() == schema.name());
            if !matches {
                return Err(existing_mismatch(DefinitionIdentity::Schema(id)));
            }
        }

        for object_type in checked.object_types() {
            let owner = match object_type.id() {
                CheckedTypeId::Existing(id) => {
                    let matches = active
                        .catalogue()
                        .object_type_by_id(id)
                        .is_some_and(|base| base.name() == object_type.name());
                    if !matches {
                        return Err(existing_mismatch(DefinitionIdentity::ObjectType(id)));
                    }
                    Some(id)
                }
                CheckedTypeId::Provisional(_) => None,
            };

            for field in object_type.fields() {
                let field_id = match field.id() {
                    CheckedFieldId::Existing(id) => {
                        let owner = owner.ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "existing checked field belongs to a provisional object type",
                        })?;
                        let matches = active
                            .catalogue()
                            .object_type_by_id(owner)
                            .and_then(|base| base.field_by_id(id))
                            .is_some_and(|base| base.name() == field.name());
                        let renamed = active
                            .catalogue()
                            .object_type_by_id(owner)
                            .and_then(|base| base.field_by_id(id))
                            .is_some_and(|base| {
                                checked.field_renames().iter().any(|rename| {
                                    rename.owner == object_type.id()
                                        && rename.field == field.id()
                                        && rename.old_name == base.name()
                                        && rename.new_name == field.name()
                                })
                            });
                        if !matches && !renamed {
                            return Err(existing_mismatch(DefinitionIdentity::Field {
                                owner,
                                field: id,
                            }));
                        }
                        Some(id)
                    }
                    CheckedFieldId::Provisional(_) => None,
                };

                if let Some(default) = field.default()
                    && let CheckedExpressionId::Existing(id) = default.id()
                {
                    let owner = owner.ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "existing checked expression belongs to a provisional object type",
                    })?;
                    let field_id = field_id.ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "existing checked expression belongs to a provisional field",
                    })?;
                    let field_matches = active
                        .catalogue()
                        .object_type_by_id(owner)
                        .and_then(|base| base.field_by_id(field_id))
                        .is_some_and(|base| base.default_expression() == Some(id));
                    let artifact_exists = active.expressions().iter().any(|value| value.id() == id);
                    if !field_matches || !artifact_exists {
                        return Err(existing_mismatch(DefinitionIdentity::Expression(id)));
                    }
                }
            }
        }

        for (checked_id, name, _, _) in checked.enum_types() {
            let CheckedTypeId::Existing(id) = checked_id else {
                continue;
            };
            let matches = active
                .catalogue()
                .enum_type_by_id(id)
                .is_some_and(|base| base.name() == name);
            if !matches {
                return Err(existing_mismatch(DefinitionIdentity::ValueType(id)));
            }
        }

        for record_value_type in checked.record_value_types() {
            let owner = match record_value_type.id() {
                CheckedTypeId::Existing(id) => {
                    let matches = active
                        .catalogue()
                        .record_value_type_by_id(id)
                        .is_some_and(|base| base.name() == record_value_type.name());
                    if !matches {
                        return Err(existing_mismatch(DefinitionIdentity::ValueType(id)));
                    }
                    Some(id)
                }
                CheckedTypeId::Provisional(_) => None,
            };
            for field in record_value_type.fields() {
                let CheckedFieldId::Existing(id) = field.id() else {
                    continue;
                };
                let owner = owner.ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "existing checked record value field belongs to a provisional type",
                })?;
                let matches = active
                    .catalogue()
                    .record_value_type_by_id(owner)
                    .and_then(|base| base.field_by_id(id))
                    .is_some_and(|base| base.name() == field.name());
                if !matches {
                    return Err(existing_mismatch(DefinitionIdentity::Field {
                        owner,
                        field: id,
                    }));
                }
            }
        }

        if validate_legacy_server_functions {
            for function in checked.server_functions() {
                if let CheckedFunctionId::Existing(id) = function.id() {
                    let matches = active
                        .catalogue()
                        .function_by_id(id)
                        .is_some_and(|base| base.name() == function.name());
                    if !matches {
                        return Err(existing_mismatch(DefinitionIdentity::Function(id)));
                    }
                }
                validate_existing_server_parameters(function, active)?;
            }
        }
        Ok(())
    }

    pub(super) fn schema(&self, id: CheckedSchemaId) -> Result<SchemaId, PrepareError> {
        copied(&self.schemas, id, "checked schema has no durable identity")
    }

    pub(super) fn type_id(&self, id: CheckedTypeId) -> Result<TypeId, PrepareError> {
        if let CheckedTypeId::Existing(type_id) = id
            && (is_sealed_inspect_type_id(type_id)
                || type_id == orna_core::system::SYS_SOURCE_FUNCTION_TYPE_ID)
        {
            // Sealed system carriers use fixed identities and do not belong to
            // the application catalogue.
            return Ok(type_id);
        }
        copied(&self.types, id, "checked type has no durable identity")
    }

    pub(super) fn field(&self, id: CheckedFieldId) -> Result<FieldId, PrepareError> {
        copied(&self.fields, id, "checked field has no durable identity")
    }

    pub(super) fn expression(&self, id: CheckedExpressionId) -> Result<ExpressionId, PrepareError> {
        copied(
            &self.expressions,
            id,
            "checked expression has no durable identity",
        )
    }

    pub(super) fn function(&self, id: CheckedFunctionId) -> Result<FunctionId, PrepareError> {
        copied(
            &self.functions,
            id,
            "checked function has no durable identity",
        )
    }

    pub(super) fn parameter(&self, id: CheckedParameterId) -> Result<ParameterId, PrepareError> {
        copied(
            &self.parameters,
            id,
            "checked parameter has no durable identity",
        )
    }

    pub(super) fn resolved_type(
        &self,
        semantic_type: SemanticType<CheckedTypeId>,
    ) -> Result<ResolvedType, PrepareError> {
        Ok(match semantic_type {
            SemanticType::Scalar(scalar) => ResolvedType::Scalar(scalar),
            SemanticType::Named(id) => ResolvedType::Named(self.type_id(id)?),
            SemanticType::Reference { target } => ResolvedType::Reference {
                target: self.type_id(target)?,
            },
        })
    }

    pub(super) fn reference_target(
        &self,
        target: CheckedDefinitionReferenceTarget,
    ) -> Result<DefinitionReferenceTarget, PrepareError> {
        Ok(match target {
            CheckedDefinitionReferenceTarget::ObjectType(id) => {
                DefinitionReferenceTarget::ObjectType(self.type_id(id)?)
            }
            CheckedDefinitionReferenceTarget::ValueType(id) => {
                DefinitionReferenceTarget::ValueType(self.type_id(id)?)
            }
            CheckedDefinitionReferenceTarget::Field { owner, field } => {
                DefinitionReferenceTarget::Field {
                    owner: self.type_id(owner)?,
                    field: self.field(field)?,
                }
            }
            CheckedDefinitionReferenceTarget::Function(id) => {
                DefinitionReferenceTarget::Function(self.function(id)?)
            }
            CheckedDefinitionReferenceTarget::Parameter { owner, parameter } => {
                DefinitionReferenceTarget::Parameter {
                    owner: self.function(owner)?,
                    parameter: self.parameter(parameter)?,
                }
            }
            CheckedDefinitionReferenceTarget::Expression(id) => {
                DefinitionReferenceTarget::Expression(self.expression(id)?)
            }
        })
    }
}

pub(super) fn existing_mismatch(definition: DefinitionIdentity) -> PrepareError {
    PrepareError::ExistingDefinitionMismatch { definition }
}

fn insert_unique<K: Eq + std::hash::Hash, V>(
    values: &mut HashMap<K, V>,
    key: K,
    value: V,
    reason: &'static str,
) -> Result<(), PrepareError> {
    if values.insert(key, value).is_some() {
        Err(PrepareError::InvalidCheckedBundle { reason })
    } else {
        Ok(())
    }
}

fn insert_consistent<K: Eq + std::hash::Hash, V: Copy + Eq>(
    values: &mut HashMap<K, V>,
    key: K,
    value: V,
    reason: &'static str,
) -> Result<(), PrepareError> {
    if values.get(&key).is_some_and(|existing| *existing != value) {
        return Err(PrepareError::InvalidCheckedBundle { reason });
    }
    values.insert(key, value);
    Ok(())
}

pub(super) fn copied<K: Eq + std::hash::Hash + Copy, V: Copy>(
    values: &HashMap<K, V>,
    key: K,
    reason: &'static str,
) -> Result<V, PrepareError> {
    values
        .get(&key)
        .copied()
        .ok_or(PrepareError::InvalidCheckedBundle { reason })
}

pub(super) struct PreparedSource {
    pub(super) revision: StoredSourceRevision,
    unit_ids: HashMap<String, SourceUnitId>,
}

#[derive(Debug)]
pub(super) struct PreparedSourceIds {
    pub(super) bundle: SourceBundleId,
    pub(super) revision: SourceRevisionId,
    pub(super) units: Vec<SourceUnitId>,
}

pub(super) struct CandidateMaterial {
    pub(super) source: StoredSourceRevision,
    pub(super) catalogue: CatalogueSnapshot,
    pub(super) origins: Vec<DefinitionOrigin>,
    pub(super) expressions: Vec<ExpressionArtifact>,
    pub(super) current_function_revisions: Vec<FunctionRevisionRecord>,
    pub(super) new_function_revisions: Vec<FunctionRevisionRecord>,
    pub(super) references: Vec<DefinitionReference>,
}

impl AllocatedStandardUpgradeFunctionPlan {
    fn revision_id(&self) -> FunctionRevisionId {
        match &self.revision {
            AllocatedStandardUpgradeFunctionRevision::Reused(revision) => revision.id(),
            AllocatedStandardUpgradeFunctionRevision::New { id, .. } => *id,
        }
    }
}

impl AllocatedStandardUpgradePlan {
    /// Gate 8 constructs and validates only the candidate catalogue.
    pub(super) fn into_catalogue(
        self,
    ) -> Result<StandardUpgradeCatalogueCandidate, CatalogueSnapshotError> {
        let functions = self
            .functions
            .iter()
            .map(|function| {
                rebind_function_definition_revision(&function.definition, function.revision_id())
            })
            .collect();
        let catalogue = CatalogueSnapshot::new_with_functions(
            self.catalogue_revision,
            self.schemas.clone(),
            self.object_types.clone(),
            functions,
        )?;
        Ok(StandardUpgradeCatalogueCandidate {
            plan: self,
            catalogue,
        })
    }
}

impl StandardUpgradeCatalogueCandidate {
    /// Gate 9 constructs every typed candidate record. It does not calculate
    /// a canonical hash.
    pub(super) fn into_candidate_records(
        self,
    ) -> Result<StandardUpgradeCandidateRecords, RevisionInvariantError> {
        let StandardUpgradeCatalogueCandidate { plan, catalogue } = self;
        let PreparedSourceIds {
            bundle,
            revision,
            units: source_unit_ids,
        } = plan.source_ids;
        let units = plan
            .source_template
            .units()
            .iter()
            .zip(source_unit_ids)
            .map(|(template, id)| {
                StoredSourceUnit::new(
                    id,
                    template.ordinal(),
                    template.logical_path(),
                    template.content(),
                    template.content_hash(),
                )
            })
            .collect::<Result<Vec<_>, RevisionInvariantError>>()?;
        let zero_hash = Sha256Digest::from_bytes([0; 32]);
        let source = StoredSourceRevision::new(
            bundle,
            revision,
            Some(plan.source_template.id()),
            units,
            zero_hash,
            zero_hash,
        )?;

        let origins = plan
            .origin_templates
            .iter()
            .map(|origin| {
                Ok(DefinitionOrigin::new(
                    origin.identity(),
                    rebase_standard_upgrade_origin(
                        &plan.source_template,
                        &source,
                        origin.source(),
                    )?,
                ))
            })
            .collect::<Result<Vec<_>, RevisionInvariantError>>()?;
        let mut current_function_revisions = Vec::with_capacity(plan.functions.len());
        let mut new_function_revisions = Vec::new();
        let mut references = Vec::new();
        for function in plan.functions {
            let function_id = function.definition.id();
            let revision_id = function.revision_id();
            let current = match function.revision {
                AllocatedStandardUpgradeFunctionRevision::Reused(revision) => *revision,
                AllocatedStandardUpgradeFunctionRevision::New {
                    id,
                    revision_number,
                } => {
                    let declaration_origin = rebase_standard_upgrade_origin(
                        &plan.source_template,
                        &source,
                        function.declaration_origin,
                    )?;
                    let revision = FunctionRevisionRecord::new(
                        function_id,
                        id,
                        revision_number,
                        declaration_origin,
                        function.declaration_content_hash,
                        function.semantic_hash,
                        function.language_version,
                        function.artifact,
                    )?
                    .with_semantic_hash_version(function.semantic_hash_version);
                    new_function_revisions.push(revision.clone());
                    revision
                }
            };
            for reference in function.references {
                references.push(DefinitionReference::new(
                    function_id,
                    revision_id,
                    reference.ordinal(),
                    reference.target(),
                    reference.kind(),
                    rebase_standard_upgrade_origin(
                        &plan.source_template,
                        &source,
                        reference.source_origin(),
                    )?,
                ));
            }
            current_function_revisions.push(current);
        }
        Ok(StandardUpgradeCandidateRecords {
            source,
            catalogue,
            origins,
            expressions: plan.expressions,
            current_function_revisions,
            new_function_revisions,
            references,
        })
    }
}

impl StandardUpgradeCandidateRecords {
    /// Gate 10 is the only standard-upgrade canonical encoder authority.
    pub(super) fn canonicalise(
        self,
        context: &CatalogueHashContext,
    ) -> Result<CanonicalStandardUpgradeCandidate, CanonicalHashError> {
        let source_bundle_hash = source_bundle_digest(self.source.units())?;
        let source_revision_hash = source_revision_record_digest(
            self.source.bundle(),
            self.source.parent(),
            source_bundle_hash,
        )?;
        let catalogue_hash = catalogue_digest_with_context(
            context,
            &self.catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
        )?;
        Ok(CanonicalStandardUpgradeCandidate {
            records: self,
            source_bundle_hash,
            source_revision_hash,
            catalogue_hash,
        })
    }
}

impl CanonicalStandardUpgradeCandidate {
    /// Gate 11 rebuilds the hashed source and constructs the final revision.
    pub(super) fn into_deployable(
        self,
        active: &ActiveDatabaseRevision,
        context: CatalogueHashContext,
    ) -> Result<DeployableRevision, RevisionInvariantError> {
        let records = self.records;
        let source = StoredSourceRevision::new(
            records.source.bundle(),
            records.source.id(),
            records.source.parent(),
            records.source.units().to_vec(),
            self.source_bundle_hash,
            self.source_revision_hash,
        )?;
        DeployableRevision::new_with_catalogue_hash_context(
            DeployableRevisionInput::new(
                active.pair(),
                source,
                active.pair().catalogue(),
                records.catalogue,
                self.catalogue_hash,
                DeployableRevisionContent::new(
                    records.origins,
                    records.expressions,
                    records.new_function_revisions,
                    records.references,
                )
                .with_current_function_revisions(records.current_function_revisions),
            ),
            context,
        )
    }
}

fn rebase_standard_upgrade_origin(
    source_template: &StoredSourceRevision,
    source: &StoredSourceRevision,
    origin: SourceOrigin,
) -> Result<SourceOrigin, RevisionInvariantError> {
    let index = source_template
        .units()
        .iter()
        .position(|unit| unit.id() == origin.source_unit())
        .ok_or(RevisionInvariantError::SourceOriginUnitNotInRevision {
            source_unit: origin.source_unit(),
        })?;
    let source_unit =
        source
            .units()
            .get(index)
            .ok_or(RevisionInvariantError::SourceOriginUnitNotInRevision {
                source_unit: origin.source_unit(),
            })?;
    SourceOrigin::new(source_unit.id(), origin.byte_start(), origin.byte_end())
}

impl CandidateMaterial {
    pub(super) fn matches_active(
        &self,
        active: &ActiveDatabaseRevision,
    ) -> Result<bool, PrepareError> {
        if self.source != *active.source()
            || !catalogue_matches(&self.catalogue, active.catalogue())
            || !same_member_multiset(&self.origins, active.origins())
            || !same_member_multiset(&self.expressions, active.expressions())
            || !same_member_multiset(
                &self.current_function_revisions,
                active.function_revisions(),
            )
            || !self.new_function_revisions.is_empty()
            || !same_member_multiset(&self.references, active.references())
        {
            return Ok(false);
        }
        Ok(catalogue_digest_with_context_and_parent(
            active.catalogue_hash_context(),
            &self.catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
            Some(active.catalogue()),
        )? == active.catalogue_hash())
    }

    pub(super) fn into_deployable(
        self,
        active: &ActiveDatabaseRevision,
        context: CatalogueHashContext,
    ) -> Result<DeployableRevision, PrepareError> {
        let catalogue_hash = self.catalogue_hash_with_parent(&context, Some(active.catalogue()))?;
        self.into_deployable_with_catalogue_hash(active, context, catalogue_hash)
    }

    fn catalogue_hash_with_parent(
        &self,
        context: &CatalogueHashContext,
        parent: Option<&CatalogueSnapshot>,
    ) -> Result<Sha256Digest, PrepareError> {
        Ok(catalogue_digest_with_context_and_parent(
            context,
            &self.catalogue,
            &self.current_function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
            parent,
        )?)
    }

    fn into_deployable_with_catalogue_hash(
        self,
        active: &ActiveDatabaseRevision,
        context: CatalogueHashContext,
        catalogue_hash: Sha256Digest,
    ) -> Result<DeployableRevision, PrepareError> {
        if context.standard().is_none() {
            return Ok(
                DeployableRevision::new_with_catalogue_hash_context_and_parent(
                    DeployableRevisionInput::new(
                        active.pair(),
                        self.source,
                        active.pair().catalogue(),
                        self.catalogue,
                        catalogue_hash,
                        DeployableRevisionContent::new(
                            self.origins,
                            self.expressions,
                            self.new_function_revisions,
                            self.references,
                        ),
                    ),
                    context,
                    Some(active.catalogue()),
                )?,
            );
        }
        Ok(
            DeployableRevision::new_with_catalogue_hash_context_and_parent(
                DeployableRevisionInput::new(
                    active.pair(),
                    self.source,
                    active.pair().catalogue(),
                    self.catalogue,
                    catalogue_hash,
                    DeployableRevisionContent::new(
                        self.origins,
                        self.expressions,
                        self.new_function_revisions,
                        self.references,
                    )
                    .with_current_function_revisions(self.current_function_revisions),
                ),
                context,
                Some(active.catalogue()),
            )?,
        )
    }
}

fn catalogue_matches(left: &CatalogueSnapshot, right: &CatalogueSnapshot) -> bool {
    left.revision() == right.revision()
        && same_member_multiset(left.schemas(), right.schemas())
        && same_member_multiset(left.object_types(), right.object_types())
        && same_member_multiset(left.value_types(), right.value_types())
        && same_member_multiset(left.enum_types(), right.enum_types())
        && same_member_multiset(left.record_value_types(), right.record_value_types())
        && same_member_multiset(left.type_bindings(), right.type_bindings())
        && same_member_multiset(left.functions(), right.functions())
}

pub(super) fn same_member_multiset<T: Eq>(left: &[T], right: &[T]) -> bool {
    left.len() == right.len()
        && left.iter().all(|member| {
            left.iter().filter(|candidate| *candidate == member).count()
                == right
                    .iter()
                    .filter(|candidate| *candidate == member)
                    .count()
        })
}

impl PreparedSourceIds {
    pub(super) fn allocate(
        parse_report: &ParseReport,
        allocations: &mut CandidateAllocator,
    ) -> Result<Self, PrepareError> {
        let bundle = allocations.source_bundle();
        let revision = allocations.source_revision();
        let mut units = Vec::with_capacity(parse_report.units().len());
        for _ in parse_report.units() {
            units.push(allocations.source_unit());
        }
        Ok(Self {
            bundle,
            revision,
            units,
        })
    }
}

impl PreparedSource {
    pub(super) fn from_active(revision: &StoredSourceRevision) -> Result<Self, PrepareError> {
        let mut unit_ids = HashMap::with_capacity(revision.units().len());
        for unit in revision.units() {
            if unit_ids
                .insert(unit.logical_path().to_owned(), unit.id())
                .is_some()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "active source revision contains a duplicate logical path",
                });
            }
        }
        Ok(Self {
            revision: revision.clone(),
            unit_ids,
        })
    }

    pub(super) fn new(
        parse_report: &ParseReport,
        parent: SourceRevisionId,
        allocations: &mut CandidateAllocator,
    ) -> Result<Self, PrepareError> {
        let ids = PreparedSourceIds::allocate(parse_report, allocations)?;
        Self::from_ids(parse_report, parent, ids)
    }

    pub(super) fn from_ids(
        parse_report: &ParseReport,
        parent: SourceRevisionId,
        ids: PreparedSourceIds,
    ) -> Result<Self, PrepareError> {
        let PreparedSourceIds {
            bundle,
            revision: revision_id,
            units: allocated_units,
        } = ids;
        if allocated_units.len() != parse_report.units().len() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked source bundle has inconsistent preallocated unit identities",
            });
        }
        let mut unit_ids = HashMap::new();
        let mut units = Vec::with_capacity(parse_report.units().len());
        for (ordinal, (unit, id)) in parse_report.units().iter().zip(allocated_units).enumerate() {
            if unit_ids
                .insert(unit.logical_path().to_owned(), id)
                .is_some()
            {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked source bundle contains a duplicate logical path",
                });
            }
            units.push(StoredSourceUnit::new(
                id,
                u32::try_from(ordinal).map_err(|_| PrepareError::SourceUnitCountExceedsU32 {
                    count: parse_report.units().len(),
                })?,
                unit.logical_path(),
                unit.source_text(),
                source_unit_content_digest(unit.source_text())?,
            )?);
        }
        let bundle_hash = source_bundle_digest(&units)?;
        let revision_hash = source_revision_record_digest(bundle, Some(parent), bundle_hash)?;
        let revision = StoredSourceRevision::new(
            bundle,
            revision_id,
            Some(parent),
            units,
            bundle_hash,
            revision_hash,
        )?;
        Ok(Self { revision, unit_ids })
    }

    pub(super) fn origin(&self, location: &SourceLocation) -> Result<SourceOrigin, PrepareError> {
        let source_unit = self
            .unit_ids
            .get(location.logical_path())
            .copied()
            .ok_or_else(|| invalid_location(location))?;
        Ok(SourceOrigin::new(
            source_unit,
            u32::try_from(location.span().start()).map_err(|_| invalid_location(location))?,
            u32::try_from(location.span().end()).map_err(|_| invalid_location(location))?,
        )?)
    }

    pub(super) fn declaration<'a>(
        &self,
        parse_report: &'a ParseReport,
        location: &SourceLocation,
    ) -> Result<&'a [u8], PrepareError> {
        let unit = parse_report
            .units()
            .iter()
            .find(|unit| unit.logical_path() == location.logical_path())
            .ok_or_else(|| invalid_location(location))?;
        unit.source_text()
            .as_bytes()
            .get(location.span().start()..location.span().end())
            .ok_or_else(|| invalid_location(location))
    }
}

pub(super) struct CandidateBuilder<'a> {
    pub(super) checked: &'a CheckedBundle,
    pub(super) parse_report: &'a ParseReport,
    pub(super) active: &'a ActiveDatabaseRevision,
    pub(super) mode: PreparationMode<'a>,
    pub(super) identities: IdentityMap,
    pub(super) source: PreparedSource,
    pub(super) catalogue_revision: CatalogueRevisionId,
    pub(super) origins: Vec<DefinitionOrigin>,
    pub(super) expressions: Vec<ExpressionArtifact>,
    pub(super) functions: Vec<FunctionDefinition>,
    pub(super) current_function_revisions: Vec<FunctionRevisionRecord>,
    pub(super) new_function_revisions: Vec<FunctionRevisionRecord>,
    pub(super) references: Vec<DefinitionReference>,
    pub(super) declaration_evidence: Option<RefCell<DeclarationEvidence>>,
}
