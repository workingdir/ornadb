use super::*;

impl StandardApplicationCheckReport {
    /// Returns the checked standard library that authorised this result.
    pub fn standard_library(&self) -> &CheckedStandardLibrary {
        &self.standard_library
    }

    /// Returns the retained parse report on success and failure.
    pub fn parse_report(&self) -> &ParseReport {
        &self.parse_report
    }

    /// Returns syntax and semantic diagnostics in source order.
    pub fn diagnostics(&self) -> &[CompilerDiagnostic] {
        &self.diagnostics
    }

    /// Returns whether checking produced any error-level diagnostics.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(CompilerDiagnostic::is_error)
    }

    /// Returns the number of error-level diagnostics.
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error())
            .count()
    }

    /// Returns the number of warning-level diagnostics.
    pub fn warning_count(&self) -> usize {
        self.diagnostics.len() - self.error_count()
    }

    /// Returns the distinct checked standard-application bundle on success.
    pub fn checked_bundle(&self) -> Option<&CheckedStandardApplicationBundle> {
        self.checked_bundle.as_ref()
    }

    /// Returns the crate-private data required for durable standard preparation.
    ///
    /// This deliberately exposes neither a legacy report nor a checked bundle
    /// outside the compiler crate.
    pub(crate) fn preparation_view(&self) -> Option<StandardApplicationPreparationView<'_>> {
        self.checked_bundle
            .as_ref()
            .map(StandardApplicationPreparationView::new)
    }

    #[cfg(test)]
    pub(crate) fn replace_type_uses_for_test(
        &mut self,
        uses: Vec<CheckedApplicationTypeUse>,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        bundle.use_indices = uses
            .iter()
            .enumerate()
            .map(|(index, type_use)| (type_use.kind(), index))
            .collect();
        bundle.uses = uses;
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_standard_type_references_for_test(
        &mut self,
        references: Vec<CheckedStandardTypeReference>,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        bundle.standard_type_references = references;
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_base_catalogue_revision_for_test(
        &mut self,
        revision: CatalogueRevisionId,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        bundle.inner.base_catalogue_revision = revision;
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_standard_context_for_test(
        &mut self,
        catalogue_revision: CatalogueRevisionId,
        library_revision: StandardLibraryRevisionId,
        digest: Sha256Digest,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        bundle.standard_catalogue_revision = catalogue_revision;
        bundle.standard_library_revision = library_revision;
        bundle.standard_library_digest = digest;
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_value_type_id_for_test(&mut self, index: usize, type_id: TypeId) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(CheckedApplicationTypeUse::Value(value)) = bundle.uses.get_mut(index) else {
            return false;
        };
        value.type_id = type_id;
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_object_reference_target_for_test(
        &mut self,
        index: usize,
        target: CheckedTypeId,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(CheckedApplicationTypeUse::ObjectReference(reference)) =
            bundle.uses.get_mut(index)
        else {
            return false;
        };
        reference.target = target;
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_type_use_location_for_test(
        &mut self,
        index: usize,
        location: SourceLocation,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(type_use) = bundle.uses.get_mut(index) else {
            return false;
        };
        match type_use {
            CheckedApplicationTypeUse::Value(value) => value.location = location,
            CheckedApplicationTypeUse::Named {
                location: current, ..
            } => *current = location,
            CheckedApplicationTypeUse::ObjectReference(reference) => reference.location = location,
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_type_use_kind_for_test(
        &mut self,
        index: usize,
        kind: CheckedTypeUseKind,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(type_use) = bundle.uses.get_mut(index) else {
            return false;
        };
        match type_use {
            CheckedApplicationTypeUse::Value(value) => value.kind = kind,
            CheckedApplicationTypeUse::Named { kind: current, .. } => *current = kind,
            CheckedApplicationTypeUse::ObjectReference(reference) => reference.kind = kind,
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_value_with_object_reference_for_test(
        &mut self,
        index: usize,
        target: CheckedTypeId,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(type_use) = bundle.uses.get_mut(index) else {
            return false;
        };
        let kind = type_use.kind();
        let location = type_use.location().clone();
        *type_use = CheckedApplicationTypeUse::ObjectReference(CheckedObjectReferenceUse {
            target,
            kind,
            location,
        });
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_standard_type_reference_for_test(
        &mut self,
        index: usize,
        owner: CheckedFunctionId,
        ordinal: u32,
        target: TypeId,
        location: SourceLocation,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(reference) = bundle.standard_type_references.get_mut(index) else {
            return false;
        };
        reference.owner = owner;
        reference.ordinal = ordinal;
        reference.target = target;
        reference.location = location;
        true
    }

    #[cfg(test)]
    fn with_first_client_for_test(
        &mut self,
        mutate: impl FnOnce(&mut CheckedClientFunction),
    ) -> bool {
        self.with_client_for_test(0, mutate)
    }

    #[cfg(test)]
    fn with_client_for_test(
        &mut self,
        index: usize,
        mutate: impl FnOnce(&mut CheckedClientFunction),
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(function) = bundle.inner.client_functions.get_mut(index) else {
            return false;
        };
        mutate(function);
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_domain_for_test(&mut self, domain: FunctionDomain) -> bool {
        self.with_first_client_for_test(|function| function.domain = domain)
    }

    #[cfg(test)]
    pub(crate) fn replace_client_domain_for_test(
        &mut self,
        index: usize,
        domain: FunctionDomain,
    ) -> bool {
        self.with_client_for_test(index, |function| function.domain = domain)
    }

    #[cfg(test)]
    pub(crate) fn append_first_client_parameter_for_test(&mut self) -> bool {
        self.with_first_client_for_test(|function| {
            function.parameters.push(CheckedServerFunctionParameter {
                id: CheckedParameterId::Existing(ParameterId::from_bytes([0xf1; 16])),
                name: "hostile".to_owned(),
                ordinal: 0,
                semantic_type: SemanticType::Scalar(StandardScalar::Boolean),
                location: function.location.clone(),
            });
        })
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_return_with_integer_for_test(&mut self) -> bool {
        self.with_first_client_for_test(|function| {
            function.return_type = SemanticType::Scalar(StandardScalar::Integer);
        })
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_security_for_test(
        &mut self,
        security: FunctionSecurity,
    ) -> bool {
        self.with_first_client_for_test(|function| function.security = security)
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_transaction_for_test(
        &mut self,
        transaction: Option<FunctionTransaction>,
    ) -> bool {
        self.with_first_client_for_test(|function| function.transaction = transaction)
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_volatility_for_test(
        &mut self,
        volatility: FunctionVolatility,
    ) -> bool {
        self.with_first_client_for_test(|function| function.volatility = volatility)
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_body_with_unsupported_for_test(&mut self) -> bool {
        self.with_first_client_for_test(|function| {
            function.body = CheckedClientFunctionBody::Unsupported;
        })
    }

    #[cfg(test)]
    pub(crate) fn append_first_client_reference_for_test(&mut self) -> bool {
        self.with_first_client_for_test(|function| {
            function.references.push(CheckedDefinitionReference {
                target: CheckedDefinitionReferenceTarget::Function(function.id),
                kind: DefinitionReferenceKind::NamedType,
                location: function.location.clone(),
            });
        })
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_location_for_test(
        &mut self,
        location: SourceLocation,
    ) -> bool {
        self.with_first_client_for_test(|function| function.location = location)
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_name_for_test(
        &mut self,
        name: QualifiedSemanticName,
    ) -> bool {
        self.with_first_client_for_test(|function| function.name = name)
    }

    #[cfg(test)]
    pub(crate) fn replace_first_client_id_with_evidence_for_test(
        &mut self,
        id: CheckedFunctionId,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(function) = bundle.inner.client_functions.first_mut() else {
            return false;
        };
        let previous = function.id;
        function.id = id;

        let rewrite_use = |type_use: &mut CheckedApplicationTypeUse| {
            let kind = match type_use.kind() {
                CheckedTypeUseKind::Field { owner, field } => {
                    CheckedTypeUseKind::Field { owner, field }
                }
                CheckedTypeUseKind::Parameter { owner, parameter } if owner == previous => {
                    CheckedTypeUseKind::Parameter {
                        owner: id,
                        parameter,
                    }
                }
                CheckedTypeUseKind::State { owner, ordinal } if owner == previous => {
                    CheckedTypeUseKind::State { owner: id, ordinal }
                }
                CheckedTypeUseKind::Return { owner, ordinal } if owner == previous => {
                    CheckedTypeUseKind::Return { owner: id, ordinal }
                }
                CheckedTypeUseKind::Expression { owner, ordinal } if owner == previous => {
                    CheckedTypeUseKind::Expression { owner: id, ordinal }
                }
                CheckedTypeUseKind::Result { owner, ordinal } if owner == previous => {
                    CheckedTypeUseKind::Result { owner: id, ordinal }
                }
                kind => kind,
            };
            match type_use {
                CheckedApplicationTypeUse::Value(value) => value.kind = kind,
                CheckedApplicationTypeUse::Named { kind: current, .. } => *current = kind,
                CheckedApplicationTypeUse::ObjectReference(reference) => reference.kind = kind,
            }
        };
        for type_use in &mut bundle.uses {
            rewrite_use(type_use);
        }
        for type_use in &mut bundle.preparation_evidence.declaration_uses {
            rewrite_use(type_use);
        }
        for type_use in &mut bundle.preparation_evidence.type_uses {
            rewrite_use(type_use);
        }
        for reference in &mut bundle.standard_type_references {
            if reference.owner == previous {
                reference.owner = id;
            }
        }
        for reference in &mut bundle.preparation_evidence.standard_type_references {
            if reference.owner == previous {
                reference.owner = id;
            }
        }
        bundle.use_indices = bundle
            .uses
            .iter()
            .enumerate()
            .map(|(index, type_use)| (type_use.kind(), index))
            .collect();
        true
    }

    /// Changes only the checked CLIENT identity.
    ///
    /// This intentionally leaves the canonical use and reference arenas
    /// unchanged so preparation tests can prove that gate 10 materialises
    /// their exact retained CLIENT-return evidence before gate 11.
    #[cfg(test)]
    pub(crate) fn replace_first_client_id_for_test(&mut self, id: CheckedFunctionId) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(function) = bundle.inner.client_functions.first_mut() else {
            return false;
        };
        function.id = id;
        true
    }

    /// Changes the retained CLIENT return-use kind in both canonical arenas.
    #[cfg(test)]
    pub(crate) fn replace_first_client_return_kind_for_test(
        &mut self,
        replacement: CheckedTypeUseKind,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(function) = bundle.inner.client_functions.first() else {
            return false;
        };
        let expected = CheckedTypeUseKind::Return {
            owner: function.id,
            ordinal: 0,
        };
        let mut changed = false;
        for type_uses in [
            &mut bundle.uses,
            &mut bundle.preparation_evidence.declaration_uses,
            &mut bundle.preparation_evidence.type_uses,
        ] {
            for type_use in type_uses
                .iter_mut()
                .filter(|type_use| type_use.kind() == expected)
            {
                match type_use {
                    CheckedApplicationTypeUse::Value(value) => value.kind = replacement,
                    CheckedApplicationTypeUse::Named { kind, .. } => *kind = replacement,
                    CheckedApplicationTypeUse::ObjectReference(reference) => {
                        reference.kind = replacement;
                    }
                }
                changed = true;
            }
        }
        if changed {
            bundle.use_indices = bundle
                .uses
                .iter()
                .enumerate()
                .map(|(index, type_use)| (type_use.kind(), index))
                .collect();
        }
        changed
    }

    /// Changes the retained CLIENT return-use target in both canonical arenas.
    #[cfg(test)]
    pub(crate) fn replace_first_client_return_type_id_for_test(&mut self, type_id: TypeId) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(function) = bundle.inner.client_functions.first() else {
            return false;
        };
        let expected = CheckedTypeUseKind::Return {
            owner: function.id,
            ordinal: 0,
        };
        let mut changed = false;
        for type_uses in [
            &mut bundle.uses,
            &mut bundle.preparation_evidence.declaration_uses,
            &mut bundle.preparation_evidence.type_uses,
        ] {
            for type_use in type_uses
                .iter_mut()
                .filter(|type_use| type_use.kind() == expected)
            {
                let CheckedApplicationTypeUse::Value(value) = type_use else {
                    return false;
                };
                value.type_id = type_id;
                changed = true;
            }
        }
        changed
    }

    /// Changes only the retained CLIENT return-use location in both type-use arenas.
    #[cfg(test)]
    pub(crate) fn replace_first_client_return_use_location_for_test(
        &mut self,
        location: SourceLocation,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(function) = bundle.inner.client_functions.first() else {
            return false;
        };
        let expected = CheckedTypeUseKind::Return {
            owner: function.id,
            ordinal: 0,
        };
        let mut changed = false;
        for type_uses in [
            &mut bundle.uses,
            &mut bundle.preparation_evidence.declaration_uses,
            &mut bundle.preparation_evidence.type_uses,
        ] {
            for type_use in type_uses
                .iter_mut()
                .filter(|type_use| type_use.kind() == expected)
            {
                match type_use {
                    CheckedApplicationTypeUse::Value(value) => value.location = location.clone(),
                    CheckedApplicationTypeUse::Named {
                        location: current, ..
                    } => *current = location.clone(),
                    CheckedApplicationTypeUse::ObjectReference(reference) => {
                        reference.location = location.clone();
                    }
                }
                changed = true;
            }
        }
        changed
    }

    /// Replaces one retained gate-11 location without changing its evidence.
    ///
    /// The selector is test-only. It gives preparation tests one narrow seam
    /// for the complete ordered location traversal.
    #[cfg(test)]
    pub(crate) fn replace_standard_preparation_location_for_test(
        &mut self,
        selector: &str,
        location: SourceLocation,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        match selector {
            "schema" => {
                let Some(schema) = bundle.inner.schemas.first_mut() else {
                    return false;
                };
                schema.location = location;
            }
            "object" => {
                let Some(object) = bundle.inner.object_types.first_mut() else {
                    return false;
                };
                object.location = location;
            }
            "field" => {
                let Some(field) = bundle
                    .inner
                    .object_types
                    .first_mut()
                    .and_then(|object| object.fields.first_mut())
                else {
                    return false;
                };
                field.location = location;
            }
            "default" => {
                let Some(default) = bundle
                    .inner
                    .object_types
                    .first_mut()
                    .and_then(|object| object.fields.first_mut())
                    .and_then(|field| field.default.as_mut())
                else {
                    return false;
                };
                default.location = location;
            }
            "server" => {
                let Some(function) = bundle.inner.server_functions.first_mut() else {
                    return false;
                };
                function.location = location;
            }
            "server parameter" => {
                let Some(parameter) = bundle
                    .inner
                    .server_functions
                    .first_mut()
                    .and_then(|function| function.parameters.first_mut())
                else {
                    return false;
                };
                parameter.location = location;
            }
            "server return" => {
                let Some(function) = bundle.inner.server_functions.first_mut() else {
                    return false;
                };
                match &mut function.return_type {
                    CheckedServerFunctionReturn::Single {
                        location: current, ..
                    } => {
                        *current = location;
                    }
                    CheckedServerFunctionReturn::Rows(columns) => {
                        let Some(column) = columns.first_mut() else {
                            return false;
                        };
                        column.location = location;
                    }
                    CheckedServerFunctionReturn::Stream { .. } => return false,
                }
            }
            "server reference" => {
                let Some(reference) = bundle
                    .inner
                    .server_functions
                    .first_mut()
                    .and_then(|function| function.references.first_mut())
                else {
                    return false;
                };
                reference.location = location;
            }
            "client" => {
                let Some(function) = bundle.inner.client_functions.first_mut() else {
                    return false;
                };
                function.location = location;
            }
            "client parameter" => {
                let Some(parameter) = bundle
                    .inner
                    .client_functions
                    .first_mut()
                    .and_then(|function| function.parameters.last_mut())
                else {
                    return false;
                };
                parameter.location = location;
            }
            "client return" => {
                let Some(function) = bundle.inner.client_functions.first() else {
                    return false;
                };
                let owner = function.id;
                let return_kind = CheckedTypeUseKind::Return { owner, ordinal: 0 };
                let mut changed = false;
                for type_uses in [
                    &mut bundle.uses,
                    &mut bundle.preparation_evidence.declaration_uses,
                    &mut bundle.preparation_evidence.type_uses,
                ] {
                    for type_use in type_uses
                        .iter_mut()
                        .filter(|type_use| type_use.kind() == return_kind)
                    {
                        match type_use {
                            CheckedApplicationTypeUse::Value(value) => {
                                value.location = location.clone()
                            }
                            CheckedApplicationTypeUse::Named {
                                location: current, ..
                            } => *current = location.clone(),
                            CheckedApplicationTypeUse::ObjectReference(reference) => {
                                reference.location = location.clone()
                            }
                        }
                        changed = true;
                    }
                }
                for references in [
                    &mut bundle.standard_type_references,
                    &mut bundle.preparation_evidence.standard_type_references,
                ] {
                    for reference in references
                        .iter_mut()
                        .filter(|reference| reference.owner == owner)
                    {
                        reference.location = location.clone();
                        changed = true;
                    }
                }
                if !changed {
                    return false;
                }
            }
            "client body" => {
                let Some(function) = bundle.inner.client_functions.first_mut() else {
                    return false;
                };
                let CheckedClientFunctionBody::BooleanLiteral {
                    location: body_location,
                    ..
                } = &mut function.body
                else {
                    return false;
                };
                *body_location = location;
            }
            "client reference" => {
                let Some(reference) = bundle
                    .inner
                    .client_functions
                    .first_mut()
                    .and_then(|function| function.references.last_mut())
                else {
                    return false;
                };
                reference.location = location;
            }
            _ => return false,
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_first_server_id_for_test(&mut self, id: CheckedFunctionId) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(function) = bundle.inner.server_functions.first_mut() else {
            return false;
        };
        function.id = id;
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_server_parameter_name_for_test(
        &mut self,
        index: usize,
        name: String,
    ) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(parameter) = bundle
            .inner
            .server_functions
            .first_mut()
            .and_then(|function| function.parameters.get_mut(index))
        else {
            return false;
        };
        parameter.name = name;
        true
    }

    #[cfg(test)]
    pub(crate) fn remove_first_server_declaration_evidence_for_test(&mut self) -> bool {
        let Some(bundle) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(owner) = bundle
            .inner
            .server_functions
            .first()
            .map(CheckedServerFunction::id)
        else {
            return false;
        };
        let belongs_to = |type_use: &CheckedApplicationTypeUse| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Parameter { owner: actual, .. }
                    | CheckedTypeUseKind::Return { owner: actual, .. }
                    if actual == owner
            )
        };
        bundle.uses.retain(|type_use| !belongs_to(type_use));
        bundle
            .preparation_evidence
            .declaration_uses
            .retain(|type_use| !belongs_to(type_use));
        bundle
            .preparation_evidence
            .type_uses
            .retain(|type_use| !belongs_to(type_use));
        bundle.use_indices = bundle
            .uses
            .iter()
            .enumerate()
            .map(|(index, type_use)| (type_use.kind(), index))
            .collect();
        true
    }
}
