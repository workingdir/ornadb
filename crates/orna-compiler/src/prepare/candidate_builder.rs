//! Assembly of durable catalogue candidates and executable artifacts.

use super::*;

/// Maps one checked CLIENT capability requirement to its artifact carrier.
///
/// The checked name is the closed vocabulary name and the argument source is
/// the declaration's literal scope text or parameter reference. The
/// version-5 envelope carries both as plain text so it stays independent of
/// the client grant model.
pub(super) fn client_capability_requirement(
    capability: &crate::CheckedClientCapability,
) -> CapabilityRequirement {
    let argument = match capability.argument() {
        crate::CheckedClientCapabilityArgument::Text(text) => {
            CapabilityArgumentSource::Text(text.clone())
        }
        crate::CheckedClientCapabilityArgument::Parameter(parameter) => {
            CapabilityArgumentSource::Parameter(parameter.clone())
        }
    };
    CapabilityRequirement::new(capability.name().to_owned(), argument)
}

fn client_expression_contains_action(expression: &CheckedClientExpression) -> bool {
    match expression {
        CheckedClientExpression::Action { .. } => true,
        CheckedClientExpression::Await { expression, .. } => {
            client_expression_contains_action(expression)
        }
        CheckedClientExpression::Resource { operation } => operation
            .arguments()
            .iter()
            .any(|(_, value)| client_expression_contains_action(value)),
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, value)| client_expression_contains_action(value)),
        CheckedClientExpression::Inspect { operation } => match operation {
            CheckedInspectOperation::Snapshot { target, .. } => {
                client_expression_contains_action(target)
            }
            CheckedInspectOperation::Projection { snapshot, .. } => {
                client_expression_contains_action(snapshot)
            }
        },
        CheckedClientExpression::Evaluate { expression, .. } => {
            client_expression_contains_action(expression)
        }
        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_action(left) || client_expression_contains_action(right)
        }
        CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_action(expression)
        }
        CheckedClientExpression::Input { .. }
        | CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::LocalRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}

fn client_expression_contains_inspect(expression: &CheckedClientExpression) -> bool {
    match expression {
        CheckedClientExpression::Inspect { .. } => true,
        CheckedClientExpression::Await { expression, .. }
        | CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. }
        | CheckedClientExpression::Evaluate { expression, .. } => {
            client_expression_contains_inspect(expression)
        }
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, value)| client_expression_contains_inspect(value)),
        CheckedClientExpression::Resource { operation } => operation
            .arguments()
            .iter()
            .any(|(_, value)| client_expression_contains_inspect(value)),
        CheckedClientExpression::Action { operation } => operation
            .arguments()
            .iter()
            .any(|(_, value)| client_expression_contains_inspect(value)),
        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_inspect(left) || client_expression_contains_inspect(right)
        }
        CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::Input { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::LocalRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}

fn client_expression_contains_resource(expression: &CheckedClientExpression) -> bool {
    match expression {
        CheckedClientExpression::Await { expression, .. }
        | CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_resource(expression)
        }
        CheckedClientExpression::Inspect { operation } => match operation {
            CheckedInspectOperation::Snapshot {
                target, options, ..
            } => {
                client_expression_contains_resource(target)
                    || options
                        .as_deref()
                        .is_some_and(client_expression_contains_resource)
            }
            CheckedInspectOperation::Projection { snapshot, .. } => {
                client_expression_contains_resource(snapshot)
            }
        },
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, argument)| client_expression_contains_resource(argument)),
        CheckedClientExpression::Evaluate { expression, .. } => {
            client_expression_contains_resource(expression)
        }
        CheckedClientExpression::Resource { .. } | CheckedClientExpression::Action { .. } => true,
        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_resource(left) || client_expression_contains_resource(right)
        }
        CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::Input { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::LocalRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}
fn client_control_flow_scalar_type_id(scalar: StandardScalar) -> Option<TypeId> {
    match scalar {
        StandardScalar::Boolean => Some(STD_BOOLEAN_TYPE_ID),
        StandardScalar::Integer => Some(STD_INTEGER_TYPE_ID),
        StandardScalar::CharacterLargeObject => Some(STD_CHARACTER_LARGE_OBJECT_TYPE_ID),
        StandardScalar::BigInt
        | StandardScalar::Float
        | StandardScalar::Decimal
        | StandardScalar::BinaryLargeObject
        | StandardScalar::Uuid
        | StandardScalar::Date
        | StandardScalar::Time
        | StandardScalar::Timestamp
        | StandardScalar::Duration
        | StandardScalar::Void => None,
    }
}
pub(super) fn durable_client_local_id(function: FunctionId, ordinal: u32) -> LocalId {
    let mut payload = function.to_bytes().to_vec();
    payload.extend_from_slice(&ordinal.to_be_bytes());
    let digest =
        artifact_payload_digest(&payload).expect("client local identity payload is bounded");
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.to_bytes()[..16]);
    LocalId::from_bytes(bytes)
}

impl<'a> CandidateBuilder<'a> {
    pub(super) fn new(
        parse_report: &'a ParseReport,
        checked: &'a CheckedBundle,
        active: &'a ActiveDatabaseRevision,
        identities: IdentityMap,
        source: PreparedSource,
        mode: PreparationMode<'a>,
        catalogue_revision: CatalogueRevisionId,
    ) -> Self {
        let declaration_evidence = match &mode {
            PreparationMode::Generic | PreparationMode::LegacyV1 => None,
            PreparationMode::StandardV1Match {
                declaration_evidence,
                ..
            }
            | PreparationMode::StandardV2Plan {
                declaration_evidence,
                ..
            }
            | PreparationMode::StandardV2 {
                declaration_evidence,
                ..
            } => Some(RefCell::new(declaration_evidence.clone())),
        };
        Self {
            checked,
            parse_report,
            active,
            mode,
            identities,
            source,
            catalogue_revision,
            origins: Vec::new(),
            expressions: Vec::new(),
            functions: Vec::new(),
            current_function_revisions: Vec::new(),
            new_function_revisions: Vec::new(),
            references: Vec::new(),
            declaration_evidence,
        }
    }

    pub(super) fn build(self) -> Result<DeployableRevision, PrepareError> {
        let active = self.active;
        let context = self.mode.catalogue_hash_context();
        self.materialise()?.into_deployable(active, context)
    }

    pub(super) fn materialise(mut self) -> Result<CandidateMaterial, PrepareError> {
        let schemas = self.build_schemas()?;
        let object_types = self.build_object_types()?;
        let enum_types = self.build_enum_types()?;
        let record_value_types = self.build_record_value_types()?;
        self.validate_record_value_evolution(&record_value_types)?;
        self.build_functions(
            &object_types.compatibility,
            &enum_types,
            &record_value_types,
        )?;
        if self
            .declaration_evidence
            .as_ref()
            .is_some_and(|evidence| !evidence.borrow().is_empty())
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked standard declaration type evidence was not consumed",
            });
        }

        let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
            self.catalogue_revision,
            schemas,
            object_types.durable,
            Vec::new(),
            enum_types,
            record_value_types,
            Vec::new(),
            self.functions,
        )?;
        Ok(CandidateMaterial {
            source: self.source.revision,
            catalogue,
            origins: self.origins,
            expressions: self.expressions,
            current_function_revisions: self.current_function_revisions,
            new_function_revisions: self.new_function_revisions,
            references: self.references,
        })
    }

    fn candidate_resolved_type(
        &self,
        semantic_type: SemanticType<CheckedTypeId>,
        kind: crate::CheckedTypeUseKind,
        consume_evidence: bool,
    ) -> Result<CandidateResolvedType, PrepareError> {
        let compatibility = self.identities.resolved_type(semantic_type)?;
        let evidence = self
            .declaration_evidence
            .as_ref()
            .map(|declaration_evidence| {
                if consume_evidence {
                    declaration_evidence.borrow_mut().consume(kind)
                } else {
                    declaration_evidence.borrow().lookup(kind)
                }
            })
            .transpose()?;
        let evidence = evidence
            .map(|evidence| self.mapped_evidence_target(evidence.target))
            .transpose()?;
        candidate_from_mapped_evidence(compatibility, evidence)
    }

    fn mapped_evidence_target(
        &self,
        evidence: EvidenceTarget,
    ) -> Result<MappedEvidenceTarget, PrepareError> {
        match evidence {
            EvidenceTarget::Value(type_id) => Ok(MappedEvidenceTarget::Value(type_id)),
            EvidenceTarget::Named(target) => self
                .identities
                .type_id(target)
                .map(MappedEvidenceTarget::Named),
            EvidenceTarget::ObjectReference(target) => self
                .identities
                .type_id(target)
                .map(MappedEvidenceTarget::ObjectReference),
            EvidenceTarget::Unknown => Ok(MappedEvidenceTarget::Unknown),
        }
    }

    fn declaration_type(
        &self,
        semantic_type: SemanticType<CheckedTypeId>,
        kind: crate::CheckedTypeUseKind,
        consume_evidence: bool,
        projection: CandidateTypeProjection,
    ) -> Result<ResolvedType, PrepareError> {
        Ok(self.mode.lower_candidate_type(
            self.candidate_resolved_type(semantic_type, kind, consume_evidence)?,
            projection,
        ))
    }

    fn build_schemas(&mut self) -> Result<Vec<SchemaDefinition>, PrepareError> {
        let schemas = self.catalogue_schemas()?;
        for checked in self.checked.schemas() {
            self.push_origin(
                DefinitionIdentity::Schema(self.identities.schema(checked.id())?),
                checked.location(),
            )?;
        }
        Ok(schemas)
    }

    fn catalogue_schemas(&self) -> Result<Vec<SchemaDefinition>, PrepareError> {
        let mut schemas = Vec::with_capacity(self.checked.schemas().len());
        for checked in self.checked.schemas() {
            let id = self.identities.schema(checked.id())?;
            schemas.push(SchemaDefinition::new(id, checked.name().clone()));
        }
        Ok(schemas)
    }

    fn build_object_types(&mut self) -> Result<ObjectTypeProjections, PrepareError> {
        let mut compatibility = Vec::with_capacity(self.checked.object_types().len());
        let mut durable = Vec::with_capacity(self.checked.object_types().len());
        for checked_type in self.checked.object_types() {
            let type_id = self.identities.type_id(checked_type.id())?;
            let mut compatibility_fields = Vec::with_capacity(checked_type.fields().len());
            let mut durable_fields = Vec::with_capacity(checked_type.fields().len());
            for checked_field in checked_type.fields() {
                let field_id = self.identities.field(checked_field.id())?;
                let default_expression = checked_field
                    .default()
                    .map(|default| self.identities.expression(default.id()))
                    .transpose()?;
                let kind = crate::CheckedTypeUseKind::Field {
                    owner: checked_type.id(),
                    field: checked_field.id(),
                };
                let compatibility_type = self.declaration_type(
                    checked_field.semantic_type(),
                    kind,
                    false,
                    CandidateTypeProjection::Compatibility,
                )?;
                let durable_type = self.declaration_type(
                    checked_field.semantic_type(),
                    kind,
                    true,
                    CandidateTypeProjection::Durable,
                )?;
                if checked_field.unique()
                    && !supports_durable_unique_field(
                        durable_type,
                        checked_field.nullable(),
                        self.mode.durable_standard_catalogue(),
                    )
                {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: UNIQUE_FIELD_MESSAGE,
                    });
                }
                compatibility_fields.push(FieldDefinition::new(
                    field_id,
                    checked_field.name(),
                    checked_field.ordinal(),
                    compatibility_type,
                    checked_field.nullable(),
                    checked_field.unique(),
                    default_expression,
                    checked_field.on_delete(),
                ));
                durable_fields.push(FieldDefinition::new(
                    field_id,
                    checked_field.name(),
                    checked_field.ordinal(),
                    durable_type,
                    checked_field.nullable(),
                    checked_field.unique(),
                    default_expression,
                    checked_field.on_delete(),
                ));
            }
            compatibility.push(ObjectTypeDefinition::new(
                type_id,
                checked_type.name().clone(),
                compatibility_fields,
            ));
            durable.push(ObjectTypeDefinition::new(
                type_id,
                checked_type.name().clone(),
                durable_fields,
            ));
        }
        self.record_object_type_metadata()?;
        Ok(ObjectTypeProjections {
            compatibility,
            durable,
        })
    }

    fn build_enum_types(&mut self) -> Result<Vec<EnumTypeDefinition>, PrepareError> {
        let checked = self
            .checked
            .enum_types()
            .map(|(id, name, labels, location)| {
                Ok((
                    EnumTypeDefinition::new(
                        self.identities.type_id(id)?,
                        name.clone(),
                        labels.iter().cloned(),
                    ),
                    location.clone(),
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        for (enum_type, location) in &checked {
            self.push_origin(DefinitionIdentity::ValueType(enum_type.id()), location)?;
        }
        Ok(checked
            .into_iter()
            .map(|(enum_type, _)| enum_type)
            .collect())
    }

    fn build_record_value_types(&mut self) -> Result<Vec<RecordValueTypeDefinition>, PrepareError> {
        let mut record_value_types = Vec::with_capacity(self.checked.record_value_types().len());
        for checked_type in self.checked.record_value_types() {
            let type_id = self.identities.type_id(checked_type.id())?;
            let mut fields = Vec::with_capacity(checked_type.fields().len());
            for checked_field in checked_type.fields() {
                let field_id = self.identities.field(checked_field.id())?;
                let resolved_type = self.declaration_type(
                    checked_field.semantic_type(),
                    crate::CheckedTypeUseKind::Field {
                        owner: checked_type.id(),
                        field: checked_field.id(),
                    },
                    true,
                    CandidateTypeProjection::Durable,
                )?;
                let descriptor = match resolved_type {
                    ResolvedType::Named(type_id) | ResolvedType::Value(type_id) => {
                        TypeDescriptor::named(type_id)
                    }
                    ResolvedType::Reference { target } => TypeDescriptor::reference(target),
                    ResolvedType::Scalar(_) => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked record field has no catalogue identity",
                        });
                    }
                };
                fields.push(
                    RecordValueFieldDefinition::try_new_descriptor(
                        field_id,
                        checked_field.name(),
                        checked_field.ordinal(),
                        descriptor,
                    )
                    .map_err(|_| PrepareError::InvalidCheckedBundle {
                        reason: "checked record field has no catalogue identity",
                    })?,
                );
                self.push_origin(
                    DefinitionIdentity::Field {
                        owner: type_id,
                        field: field_id,
                    },
                    checked_field.location(),
                )?;
            }
            record_value_types.push(RecordValueTypeDefinition::new(
                type_id,
                checked_type.name().clone(),
                fields,
            ));
            self.push_origin(
                DefinitionIdentity::ValueType(type_id),
                checked_type.location(),
            )?;
        }
        Ok(record_value_types)
    }

    fn validate_record_value_evolution(
        &self,
        candidate: &[RecordValueTypeDefinition],
    ) -> Result<(), PrepareError> {
        for active in self.active.catalogue().record_value_types() {
            let candidate = candidate
                .iter()
                .find(|record_value_type| record_value_type.id() == active.id())
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "existing record value type is absent from the candidate catalogue",
                })?;
            if candidate.name() != active.name() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record value type rename is not supported",
                });
            }
            if candidate.fields().len() != active.fields().len() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "record value field addition or removal is not supported",
                });
            }
            for active_field in active.fields() {
                let candidate_field = candidate.field_by_id(active_field.id()).ok_or(
                    PrepareError::InvalidCheckedBundle {
                        reason: "record value field replacement is not supported",
                    },
                )?;
                if candidate_field.name() != active_field.name() {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "record value field rename is not supported",
                    });
                }
                if candidate_field.ordinal() != active_field.ordinal() {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "record value field reordering is not supported",
                    });
                }
                if candidate_field.descriptor() != active_field.descriptor() {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "record value field type change is not supported",
                    });
                }
            }
        }
        Ok(())
    }

    fn record_object_type_metadata(&mut self) -> Result<(), PrepareError> {
        for checked_type in self.checked.object_types() {
            let type_id = self.identities.type_id(checked_type.id())?;
            for checked_field in checked_type.fields() {
                let field_id = self.identities.field(checked_field.id())?;
                if let Some(default) = checked_field.default() {
                    let expression_id = self.identities.expression(default.id())?;
                    let value = match default.value() {
                        ConstantValue::Null => ConstantExpression::Null,
                        ConstantValue::Boolean(value) => ConstantExpression::Boolean(*value),
                        ConstantValue::Integer(value) => ConstantExpression::Integer(*value),
                        ConstantValue::Text(value) => ConstantExpression::Text(value.clone()),
                    };
                    let payload = value.encode()?;
                    let hash = artifact_payload_digest(&payload)?;
                    if let Some(existing) = self
                        .expressions
                        .iter()
                        .find(|artifact| artifact.id() == expression_id)
                    {
                        if existing.payload() != payload || existing.content_hash() != hash {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "shared checked expression has inconsistent values",
                            });
                        }
                    } else {
                        self.expressions.push(ExpressionArtifact::new(
                            expression_id,
                            CONSTANT_FORMAT,
                            CONSTANT_VERSION,
                            payload,
                            hash,
                        )?);
                        self.push_origin(
                            DefinitionIdentity::Expression(expression_id),
                            default.location(),
                        )?;
                    }
                }
                self.push_origin(
                    DefinitionIdentity::Field {
                        owner: type_id,
                        field: field_id,
                    },
                    checked_field.location(),
                )?;
            }
            self.push_origin(
                DefinitionIdentity::ObjectType(type_id),
                checked_type.location(),
            )?;
        }
        Ok(())
    }

    fn build_functions(
        &mut self,
        object_types: &[ObjectTypeDefinition],
        enum_types: &[EnumTypeDefinition],
        record_value_types: &[RecordValueTypeDefinition],
    ) -> Result<(), PrepareError> {
        let standard_owners = self
            .mode
            .standard_preflight()
            .map(|standard_preflight| {
                standard_preflight
                    .function_identities
                    .order()
                    .iter()
                    .map(|owner| {
                        Ok((
                            *owner,
                            standard_preflight.function_identities.domain(*owner)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()
            })
            .transpose()?;

        let Some(standard_owners) = standard_owners else {
            for checked in self.checked.server_functions() {
                self.build_server_function(checked, object_types, enum_types, record_value_types)?;
            }
            for checked in self.checked.client_functions() {
                let validated = validate_generic_client_function(checked, self.active)?;
                self.build_client_function(&validated)?;
            }
            return Ok(());
        };

        for (owner, domain) in standard_owners {
            match domain {
                FunctionDomain::Server => {
                    let checked = self
                        .checked
                        .server_functions()
                        .iter()
                        .find(|function| function.id() == owner)
                        .cloned()
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "checked standard function owners do not match declaration evidence",
                        })?;
                    self.build_server_function(
                        &checked,
                        object_types,
                        enum_types,
                        record_value_types,
                    )?;
                }
                FunctionDomain::Client => {
                    let validated = self.validated_client(owner)?;
                    self.build_client_function(&validated)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn plan_standard_upgrade_lowering(
        mut self,
    ) -> Result<StandardUpgradeLoweringPlan, PrepareError> {
        let schemas = self.build_schemas()?;
        let object_types = self.build_object_types()?;
        let standard_preflight =
            self.mode
                .standard_preflight()
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "checked standard function requires standard preparation evidence",
                })?;
        let owners = standard_preflight
            .function_identities
            .order()
            .iter()
            .map(|owner| {
                Ok((
                    *owner,
                    standard_preflight.function_identities.domain(*owner)?,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        let mut plans = HashMap::with_capacity(owners.len());
        for (owner, domain) in owners {
            let function_plan = match domain {
                FunctionDomain::Server => {
                    let checked = self
                        .checked
                        .server_functions()
                        .iter()
                        .find(|function| function.id() == owner)
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "checked standard function owners do not match declaration evidence",
                        })?
                        .clone();
                    let function = self.identities.function(checked.id())?;
                    let revision = self.initial_function_revision(checked.id(), function)?;
                    let compatibility_definition = self.function_definition(
                        &checked,
                        revision,
                        false,
                        CandidateTypeProjection::Compatibility,
                    )?;
                    let artifact = self.server_artifact(
                        &checked,
                        &compatibility_definition,
                        &object_types.compatibility,
                        &[],
                        &[],
                    )?;
                    let definition = self.function_definition(
                        &checked,
                        revision,
                        true,
                        CandidateTypeProjection::Durable,
                    )?;
                    let references = self.function_references(&checked, function, revision)?;
                    let semantic_hash_version = self.mode.semantic_hash_version(&references);
                    let plan = FunctionRevisionPlan::new(
                        self.active,
                        function,
                        FunctionRevisionPlanInput {
                            semantic_hash_version,
                            definition: &definition,
                            language_version: &artifact.language_version,
                            artifact: &artifact.artifact,
                            expressions: &self.expressions,
                            references: &references,
                            current_only: standard_upgrade_reuse_is_current_only(
                                semantic_hash_version,
                            ),
                            reuse_policy: FunctionRevisionReusePolicy::Complete,
                        },
                    )?;
                    let declaration_origin = self.source.origin(checked.location())?;
                    let declaration_content_hash = function_declaration_digest(
                        self.source
                            .declaration(self.parse_report, checked.location())?,
                    )?;
                    self.push_function_origins(&checked, function)?;
                    StandardUpgradeFunctionPlan {
                        revision: plan,
                        declaration_origin,
                        declaration_content_hash,
                    }
                }
                FunctionDomain::Client => {
                    let client = self.validated_client(owner)?.clone();
                    let function = self.identities.function(client.id)?;
                    let revision = self.initial_function_revision(client.id, function)?;
                    let definition = self.client_function_definition(
                        &client,
                        revision,
                        true,
                        CandidateTypeProjection::Durable,
                    )?;
                    let artifact = self.client_artifact(&client)?;
                    let references =
                        self.client_function_references(function, revision, &client)?;
                    let semantic_hash_version = self.mode.semantic_hash_version(&references);
                    let plan = FunctionRevisionPlan::new(
                        self.active,
                        function,
                        FunctionRevisionPlanInput {
                            semantic_hash_version,
                            definition: &definition,
                            language_version: &artifact.language_version,
                            artifact: &artifact.artifact,
                            expressions: &self.expressions,
                            references: &references,
                            current_only: standard_upgrade_reuse_is_current_only(
                                semantic_hash_version,
                            ),
                            reuse_policy: FunctionRevisionReusePolicy::Complete,
                        },
                    )?;
                    let declaration_origin = self.source.origin(&client.location)?;
                    let declaration_content_hash = function_declaration_digest(
                        self.source
                            .declaration(self.parse_report, &client.location)?,
                    )?;
                    self.push_client_function_origins(&client, function)?;
                    StandardUpgradeFunctionPlan {
                        revision: plan,
                        declaration_origin,
                        declaration_content_hash,
                    }
                }
            };
            let function = function_plan.revision.definition.id();
            if plans.insert(function, function_plan).is_some() {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "duplicate checked function",
                });
            }
        }
        if self
            .declaration_evidence
            .as_ref()
            .is_some_and(|evidence| !evidence.borrow().is_empty())
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked standard declaration type evidence was not consumed",
            });
        }

        let mut functions = Vec::with_capacity(plans.len());
        for definition in self.active.catalogue().functions() {
            let plan = plans.remove(&definition.id()).ok_or(
                PrepareError::InvalidCheckedBundle {
                    reason: "checked standard function owners do not match the active catalogue",
                },
            )?;
            functions.push(plan);
        }
        if !plans.is_empty() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked standard function owners do not match the active catalogue",
            });
        }
        Ok(StandardUpgradeLoweringPlan {
            source_template: self.source.revision,
            schemas,
            object_types: object_types.durable,
            expressions: self.expressions,
            origin_templates: self.origins,
            functions,
        })
    }

    fn build_server_function(
        &mut self,
        checked: &crate::CheckedServerFunction,
        object_types: &[ObjectTypeDefinition],
        enum_types: &[EnumTypeDefinition],
        record_value_types: &[RecordValueTypeDefinition],
    ) -> Result<(), PrepareError> {
        let function_id = self.identities.function(checked.id())?;
        let initial_revision = self.initial_function_revision(checked.id(), function_id)?;
        let compatibility_definition = self.function_definition(
            checked,
            initial_revision,
            false,
            CandidateTypeProjection::Compatibility,
        )?;
        let initial_definition = self.function_definition(
            checked,
            initial_revision,
            false,
            CandidateTypeProjection::Durable,
        )?;
        let prepared_artifact = self.server_artifact(
            checked,
            &compatibility_definition,
            object_types,
            enum_types,
            record_value_types,
        )?;
        let initial_references =
            self.function_references(checked, function_id, initial_revision)?;
        let (revision_id, current_revision) =
            self.finalise_function_revision(FunctionFinalisation {
                checked: checked.id(),
                location: checked.location(),
                function: function_id,
                initial_revision,
                definition: &initial_definition,
                prepared_artifact,
                references: &initial_references,
            })?;
        let definition =
            self.function_definition(checked, revision_id, true, CandidateTypeProjection::Durable)?;
        let references =
            self.rebind_function_references(function_id, revision_id, &initial_references);
        self.push_function_origins(checked, function_id)?;
        self.functions.push(definition);
        self.current_function_revisions.push(current_revision);
        self.references.extend(references);
        Ok(())
    }

    fn build_client_function(&mut self, validated: &ValidatedClient) -> Result<(), PrepareError> {
        if matches!(&self.mode, PreparationMode::LegacyV1)
            && matches!(
                &validated.body,
                ValidatedClientBody::StateBlock { states, .. } if !states.is_empty()
            )
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT state declarations require standard-backed preparation",
            });
        }

        let function_id = self.identities.function(validated.id)?;
        let initial_revision = self.initial_function_revision(validated.id, function_id)?;
        let initial_definition = self.client_function_definition(
            validated,
            initial_revision,
            false,
            CandidateTypeProjection::Durable,
        )?;
        let prepared_artifact = self.client_artifact(validated)?;
        let initial_references = if self.mode.signature_evidence().is_some()
            || matches!(self.mode, PreparationMode::Generic)
        {
            self.client_function_references(function_id, initial_revision, validated)?
        } else {
            Vec::new()
        };
        let (revision_id, current_revision) =
            self.finalise_function_revision(FunctionFinalisation {
                checked: validated.id,
                location: &validated.location,
                function: function_id,
                initial_revision,
                definition: &initial_definition,
                prepared_artifact,
                references: &initial_references,
            })?;

        let definition = self.client_function_definition(
            validated,
            revision_id,
            true,
            CandidateTypeProjection::Durable,
        )?;
        let references =
            self.rebind_function_references(function_id, revision_id, &initial_references);
        self.push_client_function_origins(validated, function_id)?;
        self.functions.push(definition);
        self.current_function_revisions.push(current_revision);
        self.references.extend(references);
        Ok(())
    }

    fn client_function_definition(
        &self,
        validated: &ValidatedClient,
        current_revision: FunctionRevisionId,
        consume_evidence: bool,
        projection: CandidateTypeProjection,
    ) -> Result<FunctionDefinition, PrepareError> {
        let return_type = self.client_return_type(validated, consume_evidence, projection)?;
        if let ValidatedClientBody::StateBlock { states, .. } = &validated.body {
            for (ordinal, state) in states.iter().enumerate() {
                let _ = self.declaration_type(
                    state.semantic_type(),
                    crate::CheckedTypeUseKind::State {
                        owner: validated.id,
                        ordinal: ordinal as u32,
                    },
                    consume_evidence,
                    projection,
                )?;
            }
        }
        let return_type = match validated.return_shape {
            CheckedClientReturnShape::Single => FunctionReturn::Single(return_type),
            CheckedClientReturnShape::Stream => FunctionReturn::Stream(return_type),
        };
        Ok(FunctionDefinition::new(
            self.identities.function(validated.id)?,
            validated.name.clone(),
            FunctionDomain::Client,
            validated
                .parameters
                .iter()
                .map(|parameter| {
                    Ok(ParameterDefinition::new(
                        self.identities.parameter(parameter.id())?,
                        parameter.name(),
                        parameter.ordinal(),
                        self.declaration_type(
                            parameter.semantic_type(),
                            crate::CheckedTypeUseKind::Parameter {
                                owner: validated.id,
                                parameter: parameter.id(),
                            },
                            consume_evidence,
                            projection,
                        )?,
                        None,
                    ))
                })
                .collect::<Result<Vec<_>, PrepareError>>()?,
            return_type,
            current_revision,
            validated.security,
            validated.transaction,
            validated.volatility,
        ))
    }
    fn client_artifact(
        &self,
        validated: &ValidatedClient,
    ) -> Result<PreparedFunctionArtifact, PrepareError> {
        let (version, payload, inner) = match &validated.body {
            ValidatedClientBody::BooleanLiteral(value) => {
                let plan = ClientPlan::return_boolean(*value);
                (
                    CLIENT_PLAN_VERSION,
                    plan.encode(),
                    InnerClientPlan::Boolean(plan),
                )
            }
            ValidatedClientBody::Expression(expression) => {
                let contains_action = client_expression_contains_action(expression);
                let contains_resource = client_expression_contains_resource(expression);
                let expression = self.client_expression_node(expression)?;
                if contains_action {
                    let plan = ActionClientPlan::new(match expression {
                        ClientExpressionNode::Action { operation } => operation,
                        _ => {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT action expression is not a root action",
                            });
                        }
                    });
                    let payload =
                        plan.encode()
                            .map_err(|_| PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT action plan exceeds client-plan limits",
                            })?;
                    (
                        CLIENT_PLAN_ACTION_VERSION,
                        payload,
                        InnerClientPlan::Action(plan),
                    )
                } else if contains_resource {
                    let plan = ResourceClientPlan::new(expression);
                    let payload =
                        plan.encode()
                            .map_err(|_| PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT resource plan exceeds client-plan limits",
                            })?;
                    (
                        CLIENT_PLAN_RESOURCE_VERSION,
                        payload,
                        InnerClientPlan::Resource(plan),
                    )
                } else {
                    let plan = ExpressionClientPlan::new(expression);
                    let payload =
                        plan.encode()
                            .map_err(|_| PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT expression exceeds client-plan limits",
                            })?;
                    (
                        plan.format_version(),
                        payload,
                        InnerClientPlan::Expression(plan),
                    )
                }
            }
            ValidatedClientBody::Procedural {
                locals,
                statements,
                return_expression,
            } => {
                if client_expression_contains_action(return_expression)
                    || statements
                        .iter()
                        .any(|statement| client_expression_contains_action(statement.expression()))
                {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT action is only supported in expression bodies",
                    });
                }
                let function_id = self.identities.function(validated.id)?;
                let local_ids: HashMap<u32, LocalId> = locals
                    .iter()
                    .map(|local| {
                        (
                            local.ordinal(),
                            durable_client_local_id(function_id, local.ordinal()),
                        )
                    })
                    .collect();
                let artifact_locals = locals
                    .iter()
                    .map(|local| {
                        let type_id = local
                            .standard_value_type()
                            .or_else(|| match local.semantic_type() {
                                SemanticType::Named(id)
                                | SemanticType::Reference { target: id } => {
                                    self.identities.type_id(id).ok()
                                }
                                SemanticType::Scalar(_) => None,
                            })
                            .ok_or(PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT local has no durable value type identity",
                            })?;
                        let local_id = *local_ids.get(&local.ordinal()).ok_or(
                            PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT local identity map is incomplete",
                            },
                        )?;
                        let kind = match local.kind() {
                            CheckedClientLocalKind::Value => ClientLocalKind::Value,
                            CheckedClientLocalKind::Resource(kind) => {
                                ClientLocalKind::Resource(kind)
                            }
                        };
                        Ok(ClientLocal::new(local_id, type_id, kind))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                let artifact_statements = statements
                    .iter()
                    .map(|statement| {
                        let local_id = *local_ids.get(&statement.local()).ok_or(
                            PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT statement targets an unknown local",
                            },
                        )?;
                        let expression = self.client_expression_node_with_locals(
                            statement.expression(),
                            &local_ids,
                        )?;
                        Ok(match statement {
                            CheckedClientStatement::Let { .. } => {
                                ClientStatement::let_(local_id, expression)
                            }
                            CheckedClientStatement::Assignment { .. } => {
                                ClientStatement::assignment(local_id, expression)
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                let return_expression =
                    self.client_expression_node_with_locals(return_expression, &local_ids)?;
                let plan = ProceduralClientPlan::new(
                    artifact_locals,
                    artifact_statements,
                    return_expression,
                );
                let payload = plan
                    .encode()
                    .map_err(|_| PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT procedural plan exceeds client-plan limits",
                    })?;
                (
                    CLIENT_PLAN_PROCEDURAL_VERSION,
                    payload,
                    InnerClientPlan::Procedural(plan),
                )
            }
            ValidatedClientBody::ControlFlow { locals, statements } => {
                let function_id = self.identities.function(validated.id)?;
                let local_ids: HashMap<u32, LocalId> = locals
                    .iter()
                    .map(|local| {
                        (
                            local.ordinal(),
                            durable_client_local_id(function_id, local.ordinal()),
                        )
                    })
                    .collect();
                let artifact_locals = locals
                    .iter()
                    .map(|local| {
                        let type_id = local
                            .standard_value_type()
                            .or_else(|| match local.semantic_type() {
                                SemanticType::Named(id)
                                | SemanticType::Reference { target: id } => {
                                    self.client_named_type_id(id).ok()
                                }
                                SemanticType::Scalar(scalar) => {
                                    client_control_flow_scalar_type_id(scalar)
                                }
                            })
                            .ok_or(PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT control-flow local has no durable value type identity",
                            })?;
                        let local_id = *local_ids.get(&local.ordinal()).ok_or(
                            PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT control-flow local identity map is incomplete",
                            },
                        )?;
                        let kind = match local.kind() {
                            CheckedClientLocalKind::Value => ClientLocalKind::Value,
                            CheckedClientLocalKind::Resource(kind) => {
                                ClientLocalKind::Resource(kind)
                            }
                        };
                        Ok(ClientLocal::new(local_id, type_id, kind))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                let artifact_statements =
                    self.client_control_flow_statements(statements, &local_ids)?;
                let plan = ControlFlowClientPlan::new(artifact_locals, artifact_statements);
                let payload = plan
                    .encode()
                    .map_err(|_| PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT control-flow plan exceeds client-plan limits",
                    })?;
                (
                    CLIENT_PLAN_CONTROL_FLOW_VERSION,
                    payload,
                    InnerClientPlan::ControlFlow(plan),
                )
            }

            ValidatedClientBody::StateBlock {
                return_expression,
                states,
            } => {
                let contains_action = client_expression_contains_action(return_expression);
                let contains_resource = client_expression_contains_resource(return_expression);
                let contains_inspect = client_expression_contains_inspect(return_expression);
                let expression = self.client_expression_node(return_expression)?;
                if states.is_empty() {
                    if contains_action {
                        let plan = ActionClientPlan::new(match expression {
                            ClientExpressionNode::Action { operation } => operation,
                            _ => {
                                return Err(PrepareError::InvalidCheckedBundle {
                                    reason: "checked CLIENT action expression is not a root action",
                                });
                            }
                        });
                        let payload =
                            plan.encode()
                                .map_err(|_| PrepareError::InvalidCheckedBundle {
                                    reason: "checked CLIENT action plan exceeds client-plan limits",
                                })?;
                        (
                            CLIENT_PLAN_ACTION_VERSION,
                            payload,
                            InnerClientPlan::Action(plan),
                        )
                    } else if contains_resource {
                        let plan = ResourceClientPlan::new(expression);
                        let payload = plan.encode().map_err(|_| {
                            PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT resource plan exceeds client-plan limits",
                            }
                        })?;
                        (
                            CLIENT_PLAN_RESOURCE_VERSION,
                            payload,
                            InnerClientPlan::Resource(plan),
                        )
                    } else {
                        let plan = ExpressionClientPlan::new(expression);
                        let payload =
                            plan.encode()
                                .map_err(|_| PrepareError::InvalidCheckedBundle {
                                    reason: "checked CLIENT expression exceeds client-plan limits",
                                })?;
                        (
                            plan.format_version(),
                            payload,
                            InnerClientPlan::Expression(plan),
                        )
                    }
                } else {
                    if contains_action {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT action is only supported in expression bodies",
                        });
                    }
                    if contains_inspect {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT state block cannot contain Inspector expressions",
                        });
                    }
                    let function_id = self.identities.function(validated.id)?;
                    let slots = states
                        .iter()
                        .enumerate()
                        .map(|(ordinal, state)| {
                            self.client_state_slot(state, function_id, validated.id, ordinal as u32)
                        })
                        .collect::<Result<Vec<_>, PrepareError>>()?;
                    let plan = StateClientPlan::new(expression, slots);
                    let payload =
                        plan.encode()
                            .map_err(|_| PrepareError::InvalidCheckedBundle {
                                reason: "checked CLIENT state plan exceeds client-plan limits",
                            })?;
                    (
                        CLIENT_PLAN_STATE_VERSION,
                        payload,
                        InnerClientPlan::State(plan),
                    )
                }
            }
            ValidatedClientBody::ExternalContract(identity) => {
                let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
                    identity: identity.clone(),
                });
                let payload = plan
                    .encode()
                    .map_err(|_| PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT external contract exceeds client-plan limits",
                    })?;
                (
                    CLIENT_PLAN_EXPRESSION_VERSION,
                    payload,
                    InnerClientPlan::Expression(plan),
                )
            }
        };
        // A function with checked capability requirements persists them in the
        // version-5 envelope around its inner plan; a function with none keeps
        // the exact version 1-4 artefact.
        let (version, payload) = if validated.capabilities.is_empty() {
            (version, payload)
        } else {
            let requirements = validated
                .capabilities
                .iter()
                .map(client_capability_requirement)
                .collect();
            let payload = CapabilityClientPlan::new(inner, requirements)
                .encode()
                .map_err(|_| PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT capability plan exceeds client-plan limits",
                })?;
            (CLIENT_PLAN_CAPABILITY_VERSION, payload)
        };
        let hash = artifact_payload_digest(&payload)?;
        Ok(PreparedFunctionArtifact {
            artifact: ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                CLIENT_PLAN_FORMAT,
                version,
                payload,
                hash,
            )?,
            language_version: CLIENT_PLAN_LANGUAGE_VERSION.to_owned(),
        })
    }

    fn client_state_slot(
        &self,
        state: &CheckedClientStateSlot,
        function_id: FunctionId,
        owner: CheckedFunctionId,
        ordinal: u32,
    ) -> Result<StateSlot, PrepareError> {
        let expected_state_slot_id = durable_state_slot_id(function_id, state.name());
        let state_slot_id = match state.id() {
            crate::resolver::CheckedStateSlotId::Existing(id) if id == expected_state_slot_id => id,
            crate::resolver::CheckedStateSlotId::Existing(_) => {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT state slot identity does not match its function and name",
                });
            }
            crate::resolver::CheckedStateSlotId::Provisional(_) => expected_state_slot_id,
        };
        let resolved_type = self.declaration_type(
            state.semantic_type(),
            crate::CheckedTypeUseKind::State { owner, ordinal },
            false,
            CandidateTypeProjection::Durable,
        )?;
        let type_id = resolved_type
            .value_type()
            .or_else(|| state.standard_value_type())
            .or_else(|| match state.semantic_type() {
                SemanticType::Named(id) | SemanticType::Reference { target: id } => {
                    self.identities.type_id(id).ok()
                }
                SemanticType::Scalar(_) => None,
            })
            .ok_or(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT state slot has no durable value type identity",
            })?;
        let scope = match state.scope() {
            CheckedStateScope::Local => StateScope::Local,
            CheckedStateScope::Session => StateScope::Session,
            CheckedStateScope::User => StateScope::User,
        };
        let default = match state.default() {
            CheckedStateDefault::Unset => StateDefault::Unset,
            CheckedStateDefault::Null => StateDefault::Null,
            CheckedStateDefault::Expression(expression) => {
                if client_expression_contains_inspect(expression) {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT state default cannot contain Inspector expressions",
                    });
                }
                StateDefault::Expression(self.client_expression_node(expression)?)
            }
        };
        Ok(StateSlot::new(state_slot_id, type_id, scope, default))
    }

    fn client_expression_node(
        &self,
        expression: &CheckedClientExpression,
    ) -> Result<ClientExpressionNode, PrepareError> {
        self.client_expression_node_with_locals(expression, &HashMap::new())
    }

    fn client_expression_node_with_locals(
        &self,
        expression: &CheckedClientExpression,
        local_ids: &HashMap<u32, LocalId>,
    ) -> Result<ClientExpressionNode, PrepareError> {
        Ok(match expression {
            CheckedClientExpression::Call {
                function,
                arguments,
                ..
            } => ClientExpressionNode::Call {
                function: self.identities.function(*function)?,
                arguments: arguments
                    .iter()
                    .map(|(parameter, value)| {
                        Ok((
                            self.identities.parameter(*parameter)?,
                            self.client_expression_node_with_locals(value, local_ids)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?,
            },
            CheckedClientExpression::Await { expression, .. } => ClientExpressionNode::Await {
                expression: Box::new(
                    self.client_expression_node_with_locals(expression, local_ids)?,
                ),
            },
            CheckedClientExpression::Resource { operation } => ClientExpressionNode::Resource {
                operation: self.client_resource_operation(operation)?,
            },
            CheckedClientExpression::Action { operation } => ClientExpressionNode::Action {
                operation: self.client_action_operation(operation)?,
            },
            CheckedClientExpression::Inspect { operation } => {
                let operation = match operation {
                    CheckedInspectOperation::Snapshot {
                        target, options, ..
                    } => {
                        if options.is_some() {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "checked Inspector snapshot options are unsupported in Inspector v1",
                            });
                        }
                        InspectOperationNode::snapshot(
                            self.client_expression_node_with_locals(target, local_ids)?,
                        )
                    }
                    CheckedInspectOperation::Projection {
                        projection,
                        snapshot,
                        ..
                    } => {
                        let projection = match projection {
                            CheckedInspectProjection::InvocationNodes => {
                                InspectProjection::InvocationNodes
                            }
                            CheckedInspectProjection::Calls => InspectProjection::Calls,
                            CheckedInspectProjection::Resources => InspectProjection::Resources,
                            CheckedInspectProjection::StateCells => InspectProjection::StateCells,
                            CheckedInspectProjection::UiNodes => InspectProjection::UiNodes,
                            CheckedInspectProjection::PresentationCandidates => {
                                InspectProjection::PresentationCandidates
                            }
                            CheckedInspectProjection::RuntimeBindings => {
                                InspectProjection::RuntimeBindings
                            }
                            CheckedInspectProjection::SecurityDecisions => {
                                InspectProjection::SecurityDecisions
                            }
                        };
                        InspectOperationNode::Projection {
                            projection,
                            snapshot: Box::new(
                                self.client_expression_node_with_locals(snapshot, local_ids)?,
                            ),
                        }
                    }
                };
                ClientExpressionNode::Inspect { operation }
            }
            CheckedClientExpression::String { value, .. } => ClientExpressionNode::String {
                value: value.clone(),
            },
            CheckedClientExpression::Integer { value, .. } => {
                ClientExpressionNode::Integer { value: *value }
            }
            CheckedClientExpression::Boolean { value, .. } => {
                ClientExpressionNode::Boolean { value: *value }
            }
            CheckedClientExpression::ParameterRead { parameter, .. } => {
                ClientExpressionNode::ParameterRead {
                    parameter: self.identities.parameter(*parameter)?,
                }
            }
            CheckedClientExpression::LocalRead { local, .. } => {
                let local =
                    local_ids
                        .get(local)
                        .copied()
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT local read has no durable local identity",
                        })?;
                ClientExpressionNode::LocalRead { local }
            }
            CheckedClientExpression::FieldPath { root, fields, .. } => {
                ClientExpressionNode::FieldPath {
                    root: self.identities.parameter(*root)?,
                    fields: fields
                        .iter()
                        .map(|field| self.identities.field(*field))
                        .collect::<Result<Vec<_>, PrepareError>>()?,
                }
            }
            CheckedClientExpression::SourceIntrospection { .. } => {
                ClientExpressionNode::SourceIntrospection
            }
            CheckedClientExpression::Input { .. } => ClientExpressionNode::Input,
            CheckedClientExpression::Evaluate { expression, .. } => {
                ClientExpressionNode::Evaluate {
                    expression: Box::new(
                        self.client_expression_node_with_locals(expression, local_ids)?,
                    ),
                }
            }
            CheckedClientExpression::Unary {
                operator,
                expression,
                ..
            } => ClientExpressionNode::Unary {
                operator: *operator,
                expression: Box::new(
                    self.client_expression_node_with_locals(expression, local_ids)?,
                ),
            },
            CheckedClientExpression::Binary {
                operator,
                left,
                right,
                ..
            } => ClientExpressionNode::Binary {
                operator: *operator,
                left: Box::new(self.client_expression_node_with_locals(left, local_ids)?),
                right: Box::new(self.client_expression_node_with_locals(right, local_ids)?),
            },
            CheckedClientExpression::Parenthesized { expression, .. } => {
                self.client_expression_node_with_locals(expression, local_ids)?
            }

            CheckedClientExpression::Concat { left, right, .. } => ClientExpressionNode::Concat {
                left: Box::new(self.client_expression_node_with_locals(left, local_ids)?),
                right: Box::new(self.client_expression_node_with_locals(right, local_ids)?),
            },
        })
    }

    fn client_control_flow_statements(
        &self,
        statements: &[CheckedClientControlFlowStatement],
        local_ids: &HashMap<u32, LocalId>,
    ) -> Result<Vec<ControlFlowStatement>, PrepareError> {
        statements
            .iter()
            .map(|statement| self.client_control_flow_statement(statement, local_ids))
            .collect()
    }

    fn client_control_flow_statement(
        &self,
        statement: &CheckedClientControlFlowStatement,
        local_ids: &HashMap<u32, LocalId>,
    ) -> Result<ControlFlowStatement, PrepareError> {
        match statement {
            CheckedClientControlFlowStatement::Let {
                local, expression, ..
            } => Ok(ControlFlowStatement::let_(
                *local_ids
                    .get(local)
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT control-flow LET targets an unknown local",
                    })?,
                self.client_expression_node_with_locals(expression, local_ids)?,
            )),
            CheckedClientControlFlowStatement::Assignment {
                local, expression, ..
            } => Ok(ControlFlowStatement::assignment(
                *local_ids
                    .get(local)
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT control-flow assignment targets an unknown local",
                    })?,
                self.client_expression_node_with_locals(expression, local_ids)?,
            )),
            CheckedClientControlFlowStatement::Return { expression, .. } => {
                let expression = expression
                    .as_ref()
                    .map(|expression| {
                        self.client_expression_node_with_locals(expression, local_ids)
                    })
                    .transpose()?;
                Ok(ControlFlowStatement::return_(expression))
            }
            CheckedClientControlFlowStatement::If {
                branches,
                else_statements,
                ..
            } => {
                let branches = branches
                    .iter()
                    .map(|branch| {
                        Ok(ControlFlowIfBranch::new(
                            self.client_expression_node_with_locals(branch.condition(), local_ids)?,
                            self.client_control_flow_statements(branch.statements(), local_ids)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                let else_statements = else_statements
                    .as_ref()
                    .map(|statements| self.client_control_flow_statements(statements, local_ids))
                    .transpose()?;
                Ok(ControlFlowStatement::if_(ControlFlowIfStatement::new(
                    branches,
                    else_statements,
                )))
            }
            CheckedClientControlFlowStatement::While {
                condition,
                statements,
                ..
            } => Ok(ControlFlowStatement::while_(
                ControlFlowWhileStatement::new(
                    self.client_expression_node_with_locals(condition, local_ids)?,
                    self.client_control_flow_statements(statements, local_ids)?,
                ),
            )),
        }
    }

    fn resource_target_revision(
        &self,
        target: CheckedFunctionId,
        function: FunctionId,
    ) -> Result<RevisionPair, PrepareError> {
        // Every supported target is installed with this candidate pair. The
        // active and verified-standard catalogues below only establish that an
        // unchanged target remains resolvable after application.
        if self
            .checked
            .server_functions()
            .iter()
            .any(|candidate| candidate.id() == target)
            || self
                .checked
                .client_functions()
                .iter()
                .any(|candidate| candidate.id() == target)
        {
            return Ok(RevisionPair::new(
                self.source.revision.id(),
                self.catalogue_revision,
            ));
        }
        if self.active.catalogue().function_by_id(function).is_some()
            || self
                .active
                .catalogue_hash_context()
                .standard()
                .is_some_and(|standard| standard.catalogue().function_by_id(function).is_some())
        {
            return Ok(RevisionPair::new(
                self.source.revision.id(),
                self.catalogue_revision,
            ));
        }
        Err(existing_mismatch(DefinitionIdentity::Function(function)))
    }
    fn action_target_revision(
        &self,
        target: CheckedFunctionId,
        function: FunctionId,
    ) -> Result<RevisionPair, PrepareError> {
        // Actions are installed with the candidate. An unchanged active
        // target therefore uses the candidate pair, not the pair that was
        // active while the CLIENT source was checked.
        if self
            .checked
            .server_functions()
            .iter()
            .any(|candidate| candidate.id() == target)
            || self
                .checked
                .client_functions()
                .iter()
                .any(|candidate| candidate.id() == target)
            || self.active.catalogue().function_by_id(function).is_some()
            || self
                .active
                .catalogue_hash_context()
                .standard()
                .is_some_and(|standard| standard.catalogue().function_by_id(function).is_some())
        {
            return Ok(RevisionPair::new(
                self.source.revision.id(),
                self.catalogue_revision,
            ));
        }
        Err(existing_mismatch(DefinitionIdentity::Function(function)))
    }

    fn client_action_operation(
        &self,
        operation: &CheckedActionOperation,
    ) -> Result<ActionOperationNode, PrepareError> {
        let result_type = match operation.standard_result_type() {
            Some(type_id) => type_id,
            None => match operation.result_type() {
                SemanticType::Named(type_id) | SemanticType::Reference { target: type_id } => {
                    self.client_named_type_id(type_id)?
                }
                SemanticType::Scalar(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT action result has no durable value type identity",
                    });
                }
            },
        };
        let mut arguments = operation
            .arguments()
            .iter()
            .map(|(parameter, value)| {
                Ok((
                    self.identities.parameter(*parameter)?,
                    self.client_expression_node(value)?,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        arguments.sort_by_key(|(parameter, _)| *parameter);
        let target = self.identities.function(operation.target())?;
        let target_revision = self.action_target_revision(operation.target(), target)?;
        Ok(ActionOperationNode::new(
            operation.target_domain(),
            target,
            target_revision,
            operation.call_site(),
            arguments,
            result_type,
        ))
    }

    fn client_resource_operation(
        &self,
        operation: &CheckedResourceOperation,
    ) -> Result<ResourceOperationNode, PrepareError> {
        let result_type = match operation.standard_result_type() {
            Some(type_id) => type_id,
            None => match operation.result_type() {
                SemanticType::Named(type_id) | SemanticType::Reference { target: type_id } => {
                    self.client_named_type_id(type_id)?
                }
                SemanticType::Scalar(_) => {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked CLIENT resource result has no durable value type identity",
                    });
                }
            },
        };
        let mut arguments = operation
            .arguments()
            .iter()
            .map(|(parameter, value)| {
                Ok((
                    self.identities.parameter(*parameter)?,
                    self.client_expression_node(value)?,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        // The artifact contract is ordered by durable ParameterId. Resolver
        // identities may be provisional and declaration order is not a valid
        // substitute once the identity map allocates durable IDs.
        arguments.sort_by_key(|(parameter, _)| *parameter);
        let target = self.identities.function(operation.target())?;
        let target_is_server = self
            .checked
            .server_functions()
            .iter()
            .any(|candidate| candidate.id() == operation.target())
            || self
                .active
                .catalogue()
                .function_by_id(target)
                .is_some_and(|candidate| candidate.domain() == FunctionDomain::Server)
            || self
                .active
                .catalogue_hash_context()
                .standard()
                .is_some_and(|standard| {
                    standard
                        .catalogue()
                        .function_by_id(target)
                        .is_some_and(|candidate| candidate.domain() == FunctionDomain::Server)
                });
        if !target_is_server {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT resource target is not a SERVER function",
            });
        }
        let target_revision = self.resource_target_revision(operation.target(), target)?;
        Ok(ResourceOperationNode::new(
            operation.kind(),
            target,
            target_revision,
            operation.call_site(),
            arguments,
            result_type,
        ))
    }
    fn client_named_type_id(&self, id: CheckedTypeId) -> Result<TypeId, PrepareError> {
        if let CheckedTypeId::Existing(type_id) = id
            && is_sealed_inspect_type_id(type_id)
        {
            return Ok(type_id);
        }
        if let CheckedTypeId::Existing(type_id) = id
            && self
                .mode
                .durable_standard_catalogue()
                .is_some_and(|catalogue| catalogue.value_type_by_id(type_id).is_some())
        {
            return Ok(type_id);
        }
        self.identities.type_id(id)
    }

    fn client_call_reference_sequence(
        &self,
        body: &ValidatedClientBody,
    ) -> Result<Vec<(CheckedFunctionId, SourceLocation)>, PrepareError> {
        let mut calls = Vec::new();
        match body {
            ValidatedClientBody::BooleanLiteral(_) | ValidatedClientBody::ExternalContract(_) => {}
            ValidatedClientBody::Expression(expression) => {
                self.append_client_expression_call_references(expression, &mut calls)?;
            }
            ValidatedClientBody::Procedural {
                statements,
                return_expression,
                ..
            } => {
                for statement in statements {
                    self.append_client_expression_call_references(
                        statement.expression(),
                        &mut calls,
                    )?;
                }
                self.append_client_expression_call_references(return_expression, &mut calls)?;
            }
            ValidatedClientBody::ControlFlow { statements, .. } => {
                self.append_client_control_flow_call_references(statements, &mut calls)?;
            }

            ValidatedClientBody::StateBlock {
                return_expression,
                states,
            } => {
                for state in states {
                    if let CheckedStateDefault::Expression(expression) = state.default() {
                        self.append_client_expression_call_references(expression, &mut calls)?;
                    }
                }
                self.append_client_expression_call_references(return_expression, &mut calls)?;
            }
        }
        Ok(calls)
    }

    fn append_client_operation_call_references(
        &self,
        arguments: &[(CheckedParameterId, CheckedClientExpression)],
        calls: &mut Vec<(CheckedFunctionId, SourceLocation)>,
    ) -> Result<(), PrepareError> {
        let mut ordered = arguments
            .iter()
            .map(|(parameter, expression)| Ok((self.identities.parameter(*parameter)?, expression)))
            .collect::<Result<Vec<_>, PrepareError>>()?;
        ordered.sort_by_key(|(parameter, _)| *parameter);
        for (_, expression) in ordered {
            self.append_client_expression_call_references(expression, calls)?;
        }
        Ok(())
    }

    fn append_client_expression_call_references(
        &self,
        expression: &CheckedClientExpression,
        calls: &mut Vec<(CheckedFunctionId, SourceLocation)>,
    ) -> Result<(), PrepareError> {
        match expression {
            CheckedClientExpression::Call {
                function,
                arguments,
                location,
            } => {
                for (_, argument) in arguments {
                    self.append_client_expression_call_references(argument, calls)?;
                }
                calls.push((*function, location.clone()));
            }
            CheckedClientExpression::Await { expression, .. }
            | CheckedClientExpression::Unary { expression, .. }
            | CheckedClientExpression::Parenthesized { expression, .. }
            | CheckedClientExpression::Evaluate { expression, .. } => {
                self.append_client_expression_call_references(expression, calls)?;
            }
            CheckedClientExpression::Resource { operation } => {
                self.append_client_operation_call_references(operation.arguments(), calls)?;
                calls.push((operation.target(), operation.location().clone()));
            }
            CheckedClientExpression::Action { operation } => {
                self.append_client_operation_call_references(operation.arguments(), calls)?;
                calls.push((operation.target(), operation.location().clone()));
            }
            CheckedClientExpression::Inspect { operation } => match operation {
                CheckedInspectOperation::Snapshot {
                    target, options, ..
                } => {
                    self.append_client_expression_call_references(target, calls)?;
                    if let Some(options) = options {
                        self.append_client_expression_call_references(options, calls)?;
                    }
                }
                CheckedInspectOperation::Projection { snapshot, .. } => {
                    self.append_client_expression_call_references(snapshot, calls)?;
                }
            },
            CheckedClientExpression::Concat { left, right, .. }
            | CheckedClientExpression::Binary { left, right, .. } => {
                self.append_client_expression_call_references(left, calls)?;
                self.append_client_expression_call_references(right, calls)?;
            }
            CheckedClientExpression::Input { .. }
            | CheckedClientExpression::SourceIntrospection { .. }
            | CheckedClientExpression::String { .. }
            | CheckedClientExpression::Integer { .. }
            | CheckedClientExpression::Boolean { .. }
            | CheckedClientExpression::ParameterRead { .. }
            | CheckedClientExpression::LocalRead { .. }
            | CheckedClientExpression::FieldPath { .. } => {}
        }
        Ok(())
    }

    fn append_client_control_flow_call_references(
        &self,
        statements: &[CheckedClientControlFlowStatement],
        calls: &mut Vec<(CheckedFunctionId, SourceLocation)>,
    ) -> Result<(), PrepareError> {
        for statement in statements {
            match statement {
                CheckedClientControlFlowStatement::Let { expression, .. }
                | CheckedClientControlFlowStatement::Assignment { expression, .. } => {
                    self.append_client_expression_call_references(expression, calls)?;
                }
                CheckedClientControlFlowStatement::Return { expression, .. } => {
                    if let Some(expression) = expression {
                        self.append_client_expression_call_references(expression, calls)?;
                    }
                }
                CheckedClientControlFlowStatement::If {
                    branches,
                    else_statements,
                    ..
                } => {
                    for branch in branches {
                        self.append_client_expression_call_references(branch.condition(), calls)?;
                        self.append_client_control_flow_call_references(
                            branch.statements(),
                            calls,
                        )?;
                    }
                    if let Some(statements) = else_statements {
                        self.append_client_control_flow_call_references(statements, calls)?;
                    }
                }
                CheckedClientControlFlowStatement::While {
                    condition,
                    statements,
                    ..
                } => {
                    self.append_client_expression_call_references(condition, calls)?;
                    self.append_client_control_flow_call_references(statements, calls)?;
                }
            }
        }
        Ok(())
    }

    fn reorder_client_call_references(
        &self,
        validated: &ValidatedClient,
        remaining_references: &mut Vec<&crate::CheckedDefinitionReference>,
    ) -> Result<(), PrepareError> {
        let sequence = self.client_call_reference_sequence(&validated.body)?;
        let call_slots = remaining_references
            .iter()
            .enumerate()
            .filter_map(|(index, reference)| {
                (reference.kind() == DefinitionReferenceKind::FunctionCall).then_some(index)
            })
            .collect::<Vec<_>>();
        if call_slots.len() != sequence.len() {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function-call references do not match artifact calls",
            });
        }

        let mut used_slots = HashSet::with_capacity(call_slots.len());
        let mut ordered_references = Vec::with_capacity(sequence.len());
        for (target, location) in sequence {
            let expected_target = CheckedDefinitionReferenceTarget::Function(target);
            let Some(slot) = call_slots.iter().copied().find(|slot| {
                !used_slots.contains(slot)
                    && remaining_references[*slot].target() == expected_target
                    && remaining_references[*slot].location() == &location
            }) else {
                return Err(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT artifact call has no exact definition reference",
                });
            };
            used_slots.insert(slot);
            ordered_references.push(remaining_references[slot]);
        }

        for (slot, reference) in call_slots.into_iter().zip(ordered_references) {
            remaining_references[slot] = reference;
        }
        Ok(())
    }

    fn client_function_references(
        &self,
        function: FunctionId,
        revision: FunctionRevisionId,
        validated: &ValidatedClient,
    ) -> Result<Vec<DefinitionReference>, PrepareError> {
        let mut references =
            Vec::with_capacity(validated.references.len() + validated.parameters.len() + 1);
        let mut remaining_references = validated.references.iter().collect::<Vec<_>>();
        if let Some(signature_evidence) = self.mode.signature_evidence() {
            for signature_slot in signature_evidence.function_slots(validated.id) {
                let ordinal = u32::try_from(references.len()).map_err(|_| {
                    PrepareError::ReferenceCountExceedsU32 {
                        function: validated.id,
                        count: references.len(),
                    }
                })?;
                if signature_slot.flattened_ordinal != ordinal {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked standard CLIENT signature has a non-contiguous slot sequence",
                    });
                }
                let (target, kind, origin) = match signature_slot.target {
                    EvidenceTarget::Value(target) => (
                        DefinitionReferenceTarget::ValueType(target),
                        DefinitionReferenceKind::NamedType,
                        self.source.origin(&signature_slot.location)?,
                    ),
                    EvidenceTarget::Named(target) => (
                        DefinitionReferenceTarget::ValueType(self.client_named_type_id(target)?),
                        DefinitionReferenceKind::NamedType,
                        self.source.origin(&signature_slot.location)?,
                    ),
                    EvidenceTarget::ObjectReference(target) => {
                        let target = CheckedDefinitionReferenceTarget::ObjectType(target);
                        let Some(index) = remaining_references.iter().position(|reference| {
                            reference.target() == target
                                && reference.kind() == DefinitionReferenceKind::ObjectReference
                                && reference.location() == &signature_slot.location
                        }) else {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "checked standard CLIENT object signature has no exact definition reference",
                            });
                        };
                        let reference = remaining_references.remove(index);
                        (
                            self.identities.reference_target(reference.target())?,
                            reference.kind(),
                            self.source.origin(reference.location())?,
                        )
                    }
                    EvidenceTarget::Unknown => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked standard CLIENT signature has an unknown declaration use",
                        });
                    }
                };
                references.push(DefinitionReference::new(
                    function, revision, ordinal, target, kind, origin,
                ));
            }
        } else if !matches!(validated.return_target, EvidenceTarget::Unknown) {
            let return_target = match validated.return_target {
                EvidenceTarget::Value(type_id) => DefinitionReferenceTarget::ValueType(type_id),
                EvidenceTarget::Named(type_id) => {
                    DefinitionReferenceTarget::ValueType(self.client_named_type_id(type_id)?)
                }
                EvidenceTarget::ObjectReference(type_id) => {
                    DefinitionReferenceTarget::ObjectType(self.identities.type_id(type_id)?)
                }
                EvidenceTarget::Unknown => unreachable!(),
            };
            references.push(DefinitionReference::new(
                function,
                revision,
                0,
                return_target,
                if matches!(validated.return_target, EvidenceTarget::ObjectReference(_)) {
                    DefinitionReferenceKind::ObjectReference
                } else {
                    DefinitionReferenceKind::NamedType
                },
                self.source.origin(&validated.return_location)?,
            ));
        }
        self.reorder_client_call_references(validated, &mut remaining_references)?;
        for reference in remaining_references {
            let ordinal = u32::try_from(references.len()).map_err(|_| {
                PrepareError::ReferenceCountExceedsU32 {
                    function: validated.id,
                    count: validated.references.len() + validated.parameters.len() + 1,
                }
            })?;
            references.push(DefinitionReference::new(
                function,
                revision,
                ordinal,
                self.identities.reference_target(reference.target())?,
                reference.kind(),
                self.source.origin(reference.location())?,
            ));
        }
        Ok(references)
    }

    fn initial_function_revision(
        &self,
        checked: CheckedFunctionId,
        function: FunctionId,
    ) -> Result<FunctionRevisionId, PrepareError> {
        match checked {
            CheckedFunctionId::Existing(_) => self
                .active
                .catalogue()
                .function_by_id(function)
                .ok_or(existing_mismatch(DefinitionIdentity::Function(function)))
                .map(|definition| definition.current_revision()),
            CheckedFunctionId::Provisional(_) => Ok(FunctionRevisionId::new()),
        }
    }

    fn finalise_function_revision(
        &mut self,
        input: FunctionFinalisation<'_>,
    ) -> Result<(FunctionRevisionId, FunctionRevisionRecord), PrepareError> {
        let FunctionFinalisation {
            checked,
            location,
            function,
            initial_revision,
            definition,
            prepared_artifact,
            references,
        } = input;
        let semantic_hash_version = self.mode.semantic_hash_version(references);
        let calculated = FunctionRevisionPlan::new(
            self.active,
            function,
            FunctionRevisionPlanInput {
                semantic_hash_version,
                definition,
                language_version: &prepared_artifact.language_version,
                artifact: &prepared_artifact.artifact,
                expressions: &self.expressions,
                references,
                current_only: matches!(self.mode, PreparationMode::StandardV1Match { .. }),
                reuse_policy: if matches!(self.mode, PreparationMode::LegacyV1) {
                    FunctionRevisionReusePolicy::SemanticHashOnly
                } else {
                    FunctionRevisionReusePolicy::Complete
                },
            },
        )?;
        if matches!(self.mode, PreparationMode::StandardV1Match { .. }) {
            let current = self
                .active
                .function_revisions()
                .iter()
                .find(|revision| {
                    revision.function() == function && revision.id() == initial_revision
                })
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "matched active source has no current function revision",
                })?;
            let declaration_origin = self.source.origin(location)?;
            let declaration = self.source.declaration(self.parse_report, location)?;
            let expected = FunctionRevisionRecord::new(
                function,
                initial_revision,
                current.revision_number(),
                declaration_origin,
                function_declaration_digest(declaration)?,
                calculated.semantic_hash,
                prepared_artifact.language_version,
                prepared_artifact.artifact,
            )?
            .with_semantic_hash_version(semantic_hash_version);
            return Ok((expected.id(), expected));
        }
        let plan = calculated;
        if let Some(revision) = plan.reusable {
            return Ok((revision.id(), revision));
        }
        let revision_id = match checked {
            CheckedFunctionId::Existing(_) => FunctionRevisionId::new(),
            CheckedFunctionId::Provisional(_) => initial_revision,
        };
        let revision_number =
            plan.next_revision_number
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "checked standard function has no validated next revision number",
                })?;
        let declaration_origin = self.source.origin(location)?;
        let declaration = self.source.declaration(self.parse_report, location)?;
        let revision = FunctionRevisionRecord::new(
            function,
            revision_id,
            revision_number,
            declaration_origin,
            function_declaration_digest(declaration)?,
            plan.semantic_hash,
            prepared_artifact.language_version,
            prepared_artifact.artifact,
        )?
        .with_semantic_hash_version(plan.semantic_hash_version);
        self.new_function_revisions.push(revision.clone());
        Ok((revision_id, revision))
    }

    fn rebind_function_references(
        &self,
        function: FunctionId,
        revision: FunctionRevisionId,
        references: &[DefinitionReference],
    ) -> Vec<DefinitionReference> {
        references
            .iter()
            .map(|reference| {
                DefinitionReference::new(
                    function,
                    revision,
                    reference.ordinal(),
                    reference.target(),
                    reference.kind(),
                    reference.source_origin(),
                )
            })
            .collect()
    }

    fn validated_client(&self, owner: CheckedFunctionId) -> Result<ValidatedClient, PrepareError> {
        if matches!(self.mode, PreparationMode::Generic) {
            let checked = self
                .checked
                .client_functions()
                .iter()
                .find(|function| function.id() == owner)
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "checked CLIENT function is absent from the checked bundle",
                })?;
            return validate_generic_client_function(checked, self.active);
        }
        let Some(standard_preflight) = self.mode.standard_preflight() else {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function requires standard preparation evidence",
            });
        };
        standard_preflight
            .clients
            .get(&owner)
            .cloned()
            .ok_or(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function has no exact validated return evidence",
            })
    }

    fn client_return_type(
        &self,
        validated: &ValidatedClient,
        consume_evidence: bool,
        projection: CandidateTypeProjection,
    ) -> Result<ResolvedType, PrepareError> {
        Ok(self.mode.lower_candidate_type(
            self.client_candidate_return_type(validated, consume_evidence)?,
            projection,
        ))
    }

    fn client_candidate_return_type(
        &self,
        validated: &ValidatedClient,
        consume_evidence: bool,
    ) -> Result<CandidateResolvedType, PrepareError> {
        if matches!(self.mode, PreparationMode::Generic) {
            return match validated.return_semantic_type {
                SemanticType::Scalar(_) => Ok(CandidateResolvedType::LegacyScalar(
                    validated
                        .return_scalar
                        .ok_or(PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT scalar return has no compatibility type",
                        })?,
                )),
                SemanticType::Named(target) => Ok(CandidateResolvedType::Named(
                    self.client_named_type_id(target)?,
                )),
                SemanticType::Reference { target } => Ok(CandidateResolvedType::Reference(
                    self.identities.type_id(target)?,
                )),
            };
        }
        let evidence = if let Some(declaration_evidence) = &self.declaration_evidence {
            let kind = crate::CheckedTypeUseKind::Return {
                owner: validated.id,
                ordinal: 0,
            };
            if consume_evidence {
                declaration_evidence.borrow_mut().consume(kind)?
            } else {
                declaration_evidence.borrow().lookup(kind)?
            }
        } else {
            EvidenceUse {
                kind: crate::CheckedTypeUseKind::Return {
                    owner: validated.id,
                    ordinal: 0,
                },
                target: validated.return_target.clone(),
                location: validated.return_location.clone(),
            }
        };
        if evidence.target != validated.return_target
            || evidence.location != validated.return_location
        {
            return Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function return evidence does not match its validated slot",
            });
        }
        match (
            validated.return_semantic_type,
            validated.return_target.clone(),
        ) {
            (SemanticType::Scalar(_compatibility), EvidenceTarget::Value(type_id)) => {
                Ok(CandidateResolvedType::StandardValue {
                    type_id,
                    compatibility: validated.return_scalar.ok_or(
                        PrepareError::InvalidCheckedBundle {
                            reason: "checked CLIENT scalar return has no compatibility type",
                        },
                    )?,
                })
            }
            (SemanticType::Named(target), EvidenceTarget::Named(evidence))
                if target == evidence =>
            {
                if let CheckedTypeId::Existing(type_id) = target
                    && (is_sealed_inspect_type_id(type_id)
                        || self
                            .mode
                            .durable_standard_catalogue()
                            .is_some_and(|catalogue| catalogue.value_type_by_id(type_id).is_some()))
                {
                    Ok(CandidateResolvedType::StandardOpaqueValue(type_id))
                } else {
                    Ok(CandidateResolvedType::Named(
                        self.client_named_type_id(target)?,
                    ))
                }
            }
            (SemanticType::Reference { target }, EvidenceTarget::ObjectReference(evidence))
                if target == evidence =>
            {
                Ok(CandidateResolvedType::Reference(
                    self.identities.type_id(target)?,
                ))
            }
            _ => Err(PrepareError::InvalidCheckedBundle {
                reason: "checked CLIENT function return evidence does not match its semantic type",
            }),
        }
    }

    fn function_definition(
        &self,
        checked: &crate::CheckedServerFunction,
        current_revision: FunctionRevisionId,
        consume_evidence: bool,
        projection: CandidateTypeProjection,
    ) -> Result<FunctionDefinition, PrepareError> {
        let function_id = self.identities.function(checked.id())?;
        let parameters = checked
            .parameters()
            .iter()
            .map(|parameter| {
                Ok(ParameterDefinition::new(
                    self.identities.parameter(parameter.id())?,
                    parameter.name(),
                    parameter.ordinal(),
                    self.declaration_type(
                        parameter.semantic_type(),
                        crate::CheckedTypeUseKind::Parameter {
                            owner: checked.id(),
                            parameter: parameter.id(),
                        },
                        consume_evidence,
                        projection,
                    )?,
                    None,
                ))
            })
            .collect::<Result<Vec<_>, PrepareError>>()?;
        let return_type = match checked.return_type() {
            crate::resolver::CheckedServerFunctionReturn::Single { semantic_type, .. } => {
                FunctionReturn::Single(self.declaration_type(
                    *semantic_type,
                    crate::CheckedTypeUseKind::Return {
                        owner: checked.id(),
                        ordinal: 0,
                    },
                    consume_evidence,
                    projection,
                )?)
            }
            crate::resolver::CheckedServerFunctionReturn::Rows(columns) => {
                let return_columns = columns
                    .iter()
                    .map(|column| {
                        Ok(FunctionReturnColumnDefinition::new(
                            column.name(),
                            column.ordinal(),
                            self.declaration_type(
                                column.semantic_type(),
                                crate::CheckedTypeUseKind::Return {
                                    owner: checked.id(),
                                    ordinal: column.ordinal(),
                                },
                                consume_evidence,
                                projection,
                            )?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PrepareError>>()?;
                FunctionReturn::Rows(return_columns)
            }
            crate::resolver::CheckedServerFunctionReturn::Stream { semantic_type, .. } => {
                FunctionReturn::Stream(self.declaration_type(
                    *semantic_type,
                    crate::CheckedTypeUseKind::Return {
                        owner: checked.id(),
                        ordinal: 0,
                    },
                    consume_evidence,
                    projection,
                )?)
            }
        };

        Ok(FunctionDefinition::new(
            function_id,
            checked.name().clone(),
            FunctionDomain::Server,
            parameters,
            return_type,
            current_revision,
            checked.security(),
            checked.transaction(),
            checked.volatility(),
        ))
    }

    fn server_artifact(
        &self,
        checked: &crate::CheckedServerFunction,
        function: &FunctionDefinition,
        object_types: &[ObjectTypeDefinition],
        enum_types: &[EnumTypeDefinition],
        record_value_types: &[RecordValueTypeDefinition],
    ) -> Result<PreparedFunctionArtifact, PrepareError> {
        let query_function = query_planning_function(function);

        if let Some(checked_plan) = checked.unique_text_selected_query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let references = self.mapped_references(checked)?;
            let hash_context = self.mode.catalogue_hash_context();
            let standard = hash_context
                .standard()
                .map(VerifiedStandardLibrarySnapshot::catalogue);
            let encoded = unique_text_selected_query_plan(
                &plan,
                &query_function,
                object_types,
                standard,
                &references,
            )?;
            let payload = encoded.payload().to_vec();
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedFunctionArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    encoded.format_version(),
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION.to_owned(),
            });
        }

        if let Some(checked_plan) = checked.identity_selected_query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let references = self.mapped_references(checked)?;
            let encoded =
                identity_selected_query_plan(&plan, &query_function, object_types, &references)?;
            let payload = encoded.payload().to_vec();
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedFunctionArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    encoded.format_version(),
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION.to_owned(),
            });
        }

        if let Some(checked_plan) = checked.distinct_query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
            )?;
            let references = self.mapped_references(checked)?;
            let encoded = distinct_query_plan(&plan, &query_function, object_types, &references)?;
            let payload = encoded.payload().to_vec();
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedFunctionArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    encoded.format_version(),
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION.to_owned(),
            });
        }

        if let Some(checked_plan) = checked.query_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
            )?;
            let references = self.mapped_references(checked)?;
            let payload =
                version_one_query_plan(&plan, &query_function, object_types, &references)?;
            let hash = artifact_payload_digest(&payload)?;
            return Ok(PreparedFunctionArtifact {
                artifact: ExecutableArtifact::new(
                    ExecutableArtifactKind::Server,
                    SERVER_PLAN_FORMAT,
                    SERVER_PLAN_VERSION,
                    payload,
                    hash,
                )?,
                language_version: SERVER_PLAN_LANGUAGE_VERSION.to_owned(),
            });
        }

        let references = self.mapped_references(checked)?;

        let (format_version, payload) = if let Some(checked_plan) = checked.mutation_plan() {
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.field(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let context = self.mode.catalogue_hash_context();
            let standard = context
                .standard()
                .map(VerifiedStandardLibrarySnapshot::catalogue);
            let plan = server_mutation_plan(
                &plan,
                function,
                object_types,
                enum_types,
                record_value_types,
                standard,
                &references,
            )?;
            (plan.format_version(), plan.encode()?)
        } else {
            let checked_plan = checked
                .delete_plan()
                .ok_or(PrepareError::InvalidCheckedBundle {
                    reason: "checked SERVER function body cannot be prepared",
                })?;
            let plan = checked_plan.try_map_identities(
                |id| self.identities.type_id(id),
                |id| self.identities.function(id),
                |id| self.identities.parameter(id),
            )?;
            let plan = server_delete_plan(&plan, function, object_types, &references)?;
            (plan.format_version(), plan.encode()?)
        };
        let hash = artifact_payload_digest(&payload)?;
        Ok(PreparedFunctionArtifact {
            artifact: ExecutableArtifact::new(
                ExecutableArtifactKind::Server,
                SERVER_MUTATION_PLAN_FORMAT,
                format_version,
                payload,
                hash,
            )?,
            language_version: SERVER_MUTATION_PLAN_LANGUAGE_VERSION.to_owned(),
        })
    }

    fn mapped_references(
        &self,
        checked: &crate::CheckedServerFunction,
    ) -> Result<Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)>, PrepareError> {
        checked
            .references()
            .iter()
            .map(|reference| {
                Ok((
                    reference.kind(),
                    self.identities.reference_target(reference.target())?,
                ))
            })
            .collect()
    }

    fn function_references(
        &self,
        checked: &crate::CheckedServerFunction,
        function: FunctionId,
        revision: FunctionRevisionId,
    ) -> Result<Vec<DefinitionReference>, PrepareError> {
        let mut references = Vec::with_capacity(checked.references().len());
        let mut remaining_references = checked.references().iter().collect::<Vec<_>>();
        if let Some(signature_evidence) = self.mode.signature_evidence() {
            for signature_slot in signature_evidence.function_slots(checked.id()) {
                let ordinal = u32::try_from(references.len()).map_err(|_| {
                    PrepareError::ReferenceCountExceedsU32 {
                        function: checked.id(),
                        count: references.len(),
                    }
                })?;
                if signature_slot.flattened_ordinal != ordinal {
                    return Err(PrepareError::InvalidCheckedBundle {
                        reason: "checked standard signature has a non-contiguous slot sequence",
                    });
                }
                match signature_slot.target {
                    EvidenceTarget::Value(target) => {
                        references.push(DefinitionReference::new(
                            function,
                            revision,
                            ordinal,
                            DefinitionReferenceTarget::ValueType(target),
                            DefinitionReferenceKind::NamedType,
                            self.source.origin(&signature_slot.location)?,
                        ));
                    }
                    EvidenceTarget::Named(target) => {
                        references.push(DefinitionReference::new(
                            function,
                            revision,
                            ordinal,
                            DefinitionReferenceTarget::ValueType(self.identities.type_id(target)?),
                            DefinitionReferenceKind::NamedType,
                            self.source.origin(&signature_slot.location)?,
                        ));
                    }
                    EvidenceTarget::ObjectReference(target) => {
                        let target = CheckedDefinitionReferenceTarget::ObjectType(target);
                        let Some(index) = remaining_references.iter().position(|reference| {
                            reference.target() == target
                                && reference.kind() == DefinitionReferenceKind::ObjectReference
                                && reference.location() == &signature_slot.location
                        }) else {
                            return Err(PrepareError::InvalidCheckedBundle {
                                reason: "checked standard object signature has no exact definition reference",
                            });
                        };
                        let reference = remaining_references.remove(index);
                        references.push(DefinitionReference::new(
                            function,
                            revision,
                            ordinal,
                            self.identities.reference_target(reference.target())?,
                            reference.kind(),
                            self.source.origin(reference.location())?,
                        ));
                    }
                    EvidenceTarget::Unknown => {
                        return Err(PrepareError::InvalidCheckedBundle {
                            reason: "checked standard signature has an unknown declaration use",
                        });
                    }
                }
            }
        }
        let mut next_ordinal = u32::try_from(references.len()).map_err(|_| {
            PrepareError::ReferenceCountExceedsU32 {
                function: checked.id(),
                count: references.len(),
            }
        })?;
        for reference in remaining_references {
            let ordinal = next_ordinal;
            next_ordinal =
                next_ordinal
                    .checked_add(1)
                    .ok_or(PrepareError::ReferenceCountExceedsU32 {
                        function: checked.id(),
                        count: usize::MAX,
                    })?;
            references.push(DefinitionReference::new(
                function,
                revision,
                ordinal,
                self.identities.reference_target(reference.target())?,
                reference.kind(),
                self.source.origin(reference.location())?,
            ));
        }
        Ok(references)
    }

    fn push_function_origins(
        &mut self,
        checked: &crate::CheckedServerFunction,
        function: FunctionId,
    ) -> Result<(), PrepareError> {
        self.push_origin(DefinitionIdentity::Function(function), checked.location())?;
        for parameter in checked.parameters() {
            self.push_origin(
                DefinitionIdentity::Parameter {
                    owner: function,
                    parameter: self.identities.parameter(parameter.id())?,
                },
                parameter.location(),
            )?;
        }
        for column in checked.return_columns() {
            self.push_origin(
                DefinitionIdentity::FunctionReturnColumn {
                    owner: function,
                    ordinal: column.ordinal(),
                },
                column.location(),
            )?;
        }
        Ok(())
    }

    fn push_client_function_origins(
        &mut self,
        checked: &ValidatedClient,
        function: FunctionId,
    ) -> Result<(), PrepareError> {
        self.push_origin(DefinitionIdentity::Function(function), &checked.location)?;
        for parameter in &checked.parameters {
            self.push_origin(
                DefinitionIdentity::Parameter {
                    owner: function,
                    parameter: self.identities.parameter(parameter.id())?,
                },
                parameter.location(),
            )?;
        }
        Ok(())
    }

    fn push_origin(
        &mut self,
        identity: DefinitionIdentity,
        location: &SourceLocation,
    ) -> Result<(), PrepareError> {
        self.origins.push(DefinitionOrigin::new(
            identity,
            self.source.origin(location)?,
        ));
        Ok(())
    }
}

fn supports_unique_text_value_type(value_type: &ValueTypeDefinition) -> bool {
    value_type.kind() == ValueTypeKind::Primitive
        && value_type.mutability() == ValueTypeMutability::Immutable
        && value_type.persistence() == ValueTypePersistence::Persistable
        && value_type.representation_contract() == "orna.kernel.value.character-large-object@1"
}

pub(super) fn supports_durable_unique_field(
    resolved_type: ResolvedType,
    nullable: bool,
    standard: Option<&CatalogueSnapshot>,
) -> bool {
    if !nullable && resolved_type.reference_target().is_some() {
        return true;
    }
    match (standard, resolved_type) {
        (None, ResolvedType::Scalar(StandardScalar::CharacterLargeObject)) => true,
        (Some(standard), ResolvedType::Value(type_id)) => standard
            .value_type_by_id(type_id)
            .is_some_and(supports_unique_text_value_type),
        _ => false,
    }
}

pub(super) fn standard_upgrade_reuse_is_current_only(
    semantic_hash_version: FunctionSemanticHashVersion,
) -> bool {
    semantic_hash_version == FunctionSemanticHashVersion::Version1
}

pub(super) fn next_function_revision_number(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<u64, PrepareError> {
    active
        .function_revisions()
        .iter()
        .chain(active.historical_function_revisions())
        .filter(|revision| revision.function() == function)
        .map(FunctionRevisionRecord::revision_number)
        .max()
        .map_or(Ok(1), |maximum| {
            maximum
                .checked_add(1)
                .ok_or(PrepareError::FunctionRevisionNumberExhausted { function })
        })
}
